//! Smart alerts end to end against the mock NVR on a simulated clock: a
//! trigger wakes cameras, the core announces pictures (`verify_request`), a
//! scripted detector (boxes keyed by camera and picture) answers, and the
//! engine decides with a trace; woken streams stop when checks end and
//! episodes settle. Nothing here waits on real time.

mod mock_ha;
#[allow(unused_imports)]
#[path = "../../mqtt-triggers/tests/common/mod.rs"]
mod mqtt_broker;

use crate::common::{PASSWORD, Recorder, USER, fast_config, on_sim, sim_spawn, start, wait_until};
use serde_json::{Value, json};
use simtime::SimRuntime;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use viewer_core::{
    AlertError, AlertInput, Core, CoreConfig, ExternalTrigger, InputFactory, OutputFactory,
    RecordingOutput, StreamTier,
};

/// 2026-06-10 12:00 UTC (day without a location: night is 22:00-06:00).
const NOON_S: i64 = 1_781_092_800;
/// 2026-06-10 23:00 UTC (night).
const NIGHT_S: i64 = 1_781_132_400;

/// What the scripted detector answers for one picture.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Answer {
    /// A person standing in the middle of the picture.
    Person,
    /// Nothing found.
    Nothing,
    /// No answer at all (the app could not check it).
    Silent,
}

struct Harness {
    sim: SimRuntime,
    nvr: mock_nvr::MockNvr,
    core: Arc<Core>,
    rec: Arc<Recorder>,
    dev: u64,
    handled: HashSet<String>,
    /// Pictures answered per channel.
    pictures: HashMap<u64, usize>,
    max_woken: usize,
    max_logins: usize,
    /// Clock step between checks.
    step: Duration,
    _dir: Option<tempfile::TempDir>,
}

fn fixture() -> mock_nvr::fixture::Fixture {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    mock_nvr::fixture::Fixture::load(&path).unwrap()
}

impl Harness {
    fn new(channels: u8, wall_s: i64, data_dir: bool) -> Harness {
        Harness::with(channels, wall_s, data_dir, |_| {})
    }

    fn with(
        channels: u8,
        wall_s: i64,
        data_dir: bool,
        edit: impl FnOnce(&mut mock_nvr::Config),
    ) -> Harness {
        let sim = SimRuntime::new();
        let mut cfg = mock_nvr::Config::new(fixture());
        cfg.channels = channels;
        edit(&mut cfg);
        let nvr = sim_spawn(&sim, cfg);
        let dir = data_dir.then(|| tempfile::tempdir().unwrap());
        let (core, rec) = start(on_sim(
            &sim,
            CoreConfig {
                data_dir: dir.as_ref().map(|d| d.path().to_path_buf()),
                test_wall_clock_start_us: Some(wall_s * 1_000_000),
                ..fast_config()
            },
        ));
        let dev = core
            .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
            .unwrap();
        let mut h = Harness {
            sim,
            nvr,
            core,
            rec,
            dev,
            handled: HashSet::new(),
            pictures: HashMap::new(),
            max_woken: 0,
            max_logins: 0,
            step: Duration::from_millis(10),
            _dir: dir,
        };
        h.run_until("connected", 10, |h| {
            h.rec
                .events_of("device_state")
                .iter()
                .any(|e| e["state"] == "connected")
        });
        h
    }

    fn cam(&self, channel: u64) -> Value {
        json!({"device": self.dev, "channel": channel, "lens": 0})
    }

    fn configure(&self, mut settings: Value) {
        settings["utc_offset_min"] = json!(0);
        self.core.alerts_configure(&settings.to_string()).unwrap();
    }

    /// Advances the clock in 10 ms steps, answering verification requests
    /// with `detector`, until `cond` holds.
    fn run_until_with(
        &mut self,
        what: &str,
        secs: u64,
        detector: &mut dyn FnMut(u64, usize) -> Answer,
        mut cond: impl FnMut(&mut Harness) -> bool,
    ) {
        let end = self.sim.elapsed() + Duration::from_secs(secs);
        loop {
            self.core.flush_events();
            self.answer(detector);
            self.max_woken = self.max_woken.max(self.core.alerts_woken().len());
            self.max_logins = self.max_logins.max(self.nvr.active_logins());
            if cond(self) {
                return;
            }
            assert!(self.sim.elapsed() < end, "timed out waiting for {what}");
            self.sim.advance(self.step);
        }
    }

    fn run_until(&mut self, what: &str, secs: u64, cond: impl FnMut(&mut Harness) -> bool) {
        self.run_until_with(what, secs, &mut |_, _| Answer::Silent, cond);
    }

    /// Answers every new request: the frames must be complete Annex-B
    /// access units from a keyframe.
    fn answer(&mut self, detector: &mut dyn FnMut(u64, usize) -> Answer) {
        for r in self.rec.events_of("verify_request") {
            let key = r["request"].as_str().unwrap().to_owned();
            if !self.handled.insert(key.clone()) {
                continue;
            }
            let channel = r["camera"]["channel"].as_u64().unwrap();
            let n = self.pictures.entry(channel).or_insert(0);
            let answer = detector(channel, *n);
            *n += 1;
            let frames = match self.core.alert_frames_get(&key) {
                Ok(f) => f,
                // Expired or canceled meanwhile.
                Err(AlertError::UnknownId) => continue,
                Err(e) => panic!("frames: {e}"),
            };
            assert!(!frames.data.is_empty());
            assert_eq!(frames.sizes.iter().sum::<usize>(), frames.data.len());
            assert!(frames.data.starts_with(&[0, 0, 0, 1]) || frames.data.starts_with(&[0, 0, 1]));
            assert_eq!(frames.sizes.len(), r["frames"].as_array().unwrap().len());
            assert_eq!(r["frames"][0]["keyframe"], true);
            let boxes = match answer {
                Answer::Silent => continue,
                Answer::Nothing => json!({"detections": []}),
                Answer::Person => json!({"detections": [
                    {"class": "person", "score": 0.8, "box": {"x": 0.4, "y": 0.3, "w": 0.1, "h": 0.4}},
                    {"label": "kite", "score": 0.9, "box": {"x": 0.0, "y": 0.0, "w": 0.1, "h": 0.1}}
                ]}),
            };
            match self.core.alert_detections_put(&key, &boxes.to_string()) {
                Ok(()) | Err(AlertError::UnknownId) => {}
                Err(e) => panic!("answer: {e}"),
            }
        }
    }

    /// Shuts the core down while the simulated clock may run.
    fn shutdown(&self) {
        self.sim.run_free(|| self.core.shutdown());
    }

    fn decisions(&self) -> Vec<Value> {
        self.rec
            .events_of("alert")
            .into_iter()
            .filter(|e| e["state"] != "settled")
            .map(|e| e["decision"].clone())
            .collect()
    }

    fn main_previews(&self) -> usize {
        self.nvr
            .active_previews()
            .iter()
            .filter(|(_, t)| *t == StreamTier::Main)
            .count()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn step_names(decision: &Value) -> Vec<String> {
    decision["trace"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["step"].as_str().unwrap().to_owned())
        .collect()
}

/// An external sensor fires through the input seam; both mapped cameras
/// wake; the detector sees a person in two pictures; the decision is an
/// alert with the full trace, delivered to an output, logged, and the
/// streams stop once the episode settles.
#[test]
fn external_trigger_wakes_cameras_and_alerts_with_a_trace() {
    let mut h = Harness::new(2, NOON_S, true);
    let output = RecordingOutput::new();
    let o = output.clone();
    let factory: OutputFactory = Arc::new(move |_ctx, raw| {
        assert_eq!(raw["endpoint"], "http://192.0.2.10:8123");
        Ok(o.clone())
    });
    h.core.alerts_register_output("home_assistant", factory);
    let triggers = Arc::new(Mutex::new(None));
    let t2 = triggers.clone();
    let input: InputFactory = Arc::new(move |ctx, raw| {
        assert_eq!(raw["topic"], "example/topic");
        *t2.lock().unwrap() = Some(ctx.triggers.clone());
        struct Stub;
        impl AlertInput for Stub {
            fn stop(&mut self) {}
        }
        Ok(Box::new(Stub))
    });
    h.core.alerts_register_input("mqtt", input);
    h.configure(json!({
        "sources": [{"key": "sensor-1", "name": "Sensor 1",
                     "cameras": [{"camera": h.cam(0)}, {"camera": h.cam(1),
                                  "roi": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]}]}],
        "outputs": [{"kind": "desktop"},
                    {"kind": "home_assistant", "endpoint": "http://192.0.2.10:8123", "min_severity": "low"}],
        "inputs": [{"kind": "mqtt", "topic": "example/topic"}],
    }));
    h.core.flush_events();
    let configured = h.rec.events_of("alerts_configured");
    assert_eq!(configured.len(), 1, "{configured:?}");
    assert_eq!(configured[0]["outputs"][0]["ok"], true);
    assert_eq!(configured[0]["inputs"][0]["ok"], true);

    let ext = triggers.lock().unwrap().clone().expect("input built");
    ext.put(ExternalTrigger {
        source: "sensor-1".into(),
        kind: "sensor_motion".into(),
        state: alerts::trigger::TriggerState::Pulse,
        at_ms: None,
    })
    .unwrap();
    h.run_until_with("a decision", 20, &mut |_, _| Answer::Person, |h| {
        !h.decisions().is_empty()
    });
    let d = &h.decisions()[0];
    assert_eq!(d["decision"], "alert", "{d}");
    assert_eq!(d["class"], "person");
    assert_eq!(d["unverified"], false);
    let steps = step_names(d);
    for s in [
        "trigger_received",
        "gate",
        "sampling",
        "camera_woken",
        "frame_sampled",
        "progress",
        "checked",
        "rules",
        "timeliness",
        "decision",
    ] {
        assert!(steps.contains(&s.to_owned()), "{s} missing from {steps:?}");
    }
    assert!(d["explanation"].as_str().unwrap().contains("Sensor 1"));
    // The live view saw the steps as they happened.
    let live: Vec<Value> = h.rec.events_of("verify_step");
    assert!(live.iter().any(|e| e["step"]["step"] == "frame_sampled"));
    assert!(live.iter().any(|e| e["step"]["step"] == "decision"));
    // Requests offered crops: the frame, 4 tiles, and the region.
    let req = &h.rec.events_of("verify_request")[0];
    assert!(req["crops"].as_array().unwrap().len() >= 5);
    assert_eq!(req["classes"], json!(["person", "vehicle", "animal"]));

    // The output got it and its result is in the trace and the log.
    h.run_until("delivery", 5, |h| {
        h.rec
            .events_of("verify_step")
            .iter()
            .any(|e| e["step"]["step"] == "delivery")
    });
    let n = output.notices();
    assert_eq!(n.len(), 1);
    assert_eq!(n[0].state, viewer_core::NoticeState::New);
    let log: Value =
        serde_json::from_str(&h.core.alerts_get(r#"{"decision":["alert"]}"#).unwrap()).unwrap();
    let e = &log["entries"][0];
    assert_eq!(e["decision"]["alert_id"], d["alert_id"]);
    assert_eq!(e["source"], "external");
    assert_eq!(e["deliveries"][0]["ok"], true);
    assert_eq!(e["deliveries"][0]["kind"], "home_assistant");

    // Re-checks find nothing; the episode settles and every stream stops.
    h.step = Duration::from_millis(250);
    h.run_until_with("settled", 400, &mut |_, _| Answer::Nothing, |h| {
        h.rec
            .events_of("alert")
            .iter()
            .any(|e| e["state"] == "settled")
    });
    h.run_until("streams stopped", 10, |h| h.main_previews() == 0);
    assert!(h.core.alerts_woken().is_empty());
    assert!(
        output
            .notices()
            .iter()
            .any(|n| n.state == viewer_core::NoticeState::Settled)
    );
    assert_eq!(h.max_logins, 1, "never a second login");
    h.shutdown();
    assert!(output.was_shut_down());
}

/// The camera's AI says person; our check sees nothing: a quiet log entry,
/// and the stream it woke stops at once.
#[test]
fn camera_trigger_not_confirmed_is_logged_quietly() {
    let mut h = Harness::new(2, NOON_S, false);
    h.configure(
        json!({"cameras": [{"camera": h.cam(1), "exposure": "outside", "name": "Camera B"}]}),
    );
    assert!(h.nvr.set_alarm(1, "MD", "people"));
    h.run_until_with("a decision", 20, &mut |_, _| Answer::Nothing, |h| {
        h.decisions().iter().any(|d| d["class"] == "person")
    });
    let d = h
        .decisions()
        .into_iter()
        .find(|d| d["class"] == "person")
        .unwrap();
    assert_eq!(d["decision"], "quiet", "{d}");
    assert_eq!(d["reason"], "not_confirmed");
    assert_eq!(h.pictures.get(&1), Some(&2), "2 pictures checked");
    // Motion alone on a camera that does not check motion: quiet, no wake.
    assert!(h.decisions().iter().any(|d| d["reason"] == "motion_only"));
    h.run_until("streams stopped", 5, |h| h.main_previews() == 0);
    let log: Value = serde_json::from_str(
        &h.core
            .alerts_get(r#"{"decision":["quiet"],"text":"camera b"}"#)
            .unwrap(),
    )
    .unwrap();
    assert!(!log["entries"].as_array().unwrap().is_empty(), "{log}");
    assert_eq!(log["entries"][0]["source"], "device");
}

/// At night, a person on an outside camera is always-alert: with no answer
/// from the app it fails open (alert, unverified, critical).
#[test]
fn no_answer_fails_open_for_always_alert_cases() {
    let mut h = Harness::new(1, NIGHT_S, false);
    h.configure(
        json!({"cameras": [{"camera": h.cam(0), "exposure": "outside"}],
                       "acquisition": {"answer_timeout_ms": 1000}}),
    );
    let id = h
        .core
        .alerts_trigger_put(
            &json!({"source": "device", "camera": h.cam(0), "kind": "people"}).to_string(),
        )
        .unwrap();
    h.run_until("a decision", 30, |h| !h.decisions().is_empty());
    let d = &h.decisions()[0];
    assert_eq!(d["trigger_id"], id);
    assert_eq!(d["decision"], "alert", "{d}");
    assert_eq!(d["severity"], "critical");
    assert_eq!(d["unverified"], true);
    assert_eq!(d["always"], "night_outside_person");
    // Requests that timed out were reported.
    assert!(
        h.rec
            .events_of("verify_request_done")
            .iter()
            .any(|e| e["result"] == "timeout")
    );
}

/// Smoke and CO need no pictures: they alert at once, critical, without
/// waking anything; a mute (and only a mute) stops them.
#[test]
fn smoke_always_alerts_and_only_a_mute_stops_it() {
    let mut h = Harness::new(1, NOON_S, false);
    h.configure(json!({"cameras": [{"camera": h.cam(0), "exposure": "inside", "schedule": {"kind": "windows", "windows": []}}]}));
    h.core
        .alerts_trigger_put(
            &json!({"source": "local_audio", "camera": h.cam(0), "kind": "smoke_alarm"})
                .to_string(),
        )
        .unwrap();
    h.run_until("a decision", 5, |h| !h.decisions().is_empty());
    let d = &h.decisions()[0];
    assert_eq!(d["decision"], "alert", "{d}");
    assert_eq!(d["severity"], "critical");
    assert!(h.rec.events_of("verify_request").is_empty());
    assert_eq!(h.max_woken, 0);

    h.core.alert_mute(&h.cam(0).to_string(), 900).unwrap();
    h.core
        .alerts_trigger_put(
            &json!({"source": "local_audio", "camera": h.cam(0), "kind": "co_alarm"}).to_string(),
        )
        .unwrap();
    h.run_until("a second decision", 5, |h| h.decisions().len() == 2);
    assert_eq!(h.decisions()[1]["decision"], "suppress");
    assert_eq!(h.decisions()[1]["reason"], "muted");
    let status: Value = serde_json::from_str(&h.core.alerts_get("{}").unwrap()).unwrap();
    assert_eq!(status["status"]["muted"].as_array().unwrap().len(), 1);
}

/// Four cameras trigger at once on a device allowed two wakes: never more
/// than two streams are woken, and every check is still decided.
#[test]
fn wakes_are_capped_per_device_and_every_check_is_decided() {
    let mut h = Harness::new(4, NOON_S, false);
    let cams: Vec<Value> = (0..4)
        .map(|c| json!({"camera": h.cam(c), "exposure": "outside"}))
        .collect();
    h.configure(
        json!({"cameras": cams, "standby": {"max_wake_per_device": 2},
                       "acquisition": {"recheck": false}}),
    );
    for c in 0..4 {
        h.core
            .alerts_trigger_put(
                &json!({"source": "device", "camera": h.cam(c), "kind": "people"}).to_string(),
            )
            .unwrap();
    }
    h.run_until_with("four decisions", 60, &mut |_, _| Answer::Person, |h| {
        assert!(h.main_previews() <= 2, "more than two streams woken");
        h.decisions().len() == 4
    });
    assert_eq!(h.max_woken, 2);
    assert!(h.decisions().iter().all(|d| d["decision"] == "alert"));
    h.run_until("streams stopped", 5, |h| h.main_previews() == 0);
}

/// A connection drop in the middle of a check: the session reconnects on
/// its one login, the stream comes back, and the check still completes;
/// the stream stops afterwards.
#[test]
fn reconnect_during_an_episode() {
    let mut h = Harness::new(1, NOON_S, false);
    h.configure(
        json!({"cameras": [{"camera": h.cam(0), "exposure": "outside"}],
                       "acquisition": {"recheck": false}}),
    );
    h.core
        .alerts_trigger_put(
            &json!({"source": "device", "camera": h.cam(0), "kind": "people"}).to_string(),
        )
        .unwrap();
    // The first picture is answered; then the connection drops.
    h.run_until_with("first picture", 10, &mut |_, _| Answer::Person, |h| {
        h.pictures.get(&0) == Some(&1)
    });
    h.nvr.drop_connections();
    h.run_until_with("a decision", 30, &mut |_, _| Answer::Person, |h| {
        !h.decisions().is_empty()
    });
    assert_eq!(
        h.decisions()[0]["decision"],
        "alert",
        "{}",
        h.decisions()[0]
    );
    h.run_until("streams stopped", 10, |h| h.main_previews() == 0);
    assert_eq!(h.max_logins, 1);
}

/// A tile already showing main shares its stream: the check adds no second
/// preview, and the tile keeps its stream after the check.
#[test]
fn a_running_tile_stream_is_reused() {
    let mut h = Harness::new(1, NOON_S, false);
    let tile = h.core.open_tile(h.dev, 0, StreamTier::Main).unwrap();
    h.run_until("tile streaming", 10, |h| h.main_previews() == 1);
    h.configure(json!({"cameras": [{"camera": h.cam(0)}], "acquisition": {"recheck": false}}));
    h.core
        .alerts_trigger_put(
            &json!({"source": "device", "camera": h.cam(0), "kind": "people"}).to_string(),
        )
        .unwrap();
    h.run_until_with("a decision", 20, &mut |_, _| Answer::Person, |h| {
        assert!(h.main_previews() <= 1);
        !h.decisions().is_empty()
    });
    h.sim.advance(Duration::from_secs(2));
    assert_eq!(h.main_previews(), 1);
    h.core.close_tile(tile).unwrap();
}

/// Outputs that want pictures wait for the app's snapshot (briefly); the
/// snapshot, feedback, and log survive in the data directory.
#[test]
fn snapshots_feedback_and_the_log() {
    let mut h = Harness::new(1, NOON_S, true);
    let output = RecordingOutput::new();
    output.set_wants_picture(true);
    let o = output.clone();
    h.core
        .alerts_register_output("ntfy", Arc::new(move |_, _| Ok(o.clone())));
    h.configure(json!({"cameras": [{"camera": h.cam(0)}],
                       "outputs": [{"kind": "ntfy", "endpoint": "http://192.0.2.20/example"}],
                       "acquisition": {"recheck": false}}));
    h.core
        .alerts_trigger_put(
            &json!({"source": "device", "camera": h.cam(0), "kind": "people"}).to_string(),
        )
        .unwrap();
    h.run_until_with("a decision", 20, &mut |_, _| Answer::Person, |h| {
        !h.decisions().is_empty()
    });
    let alert_id = h.decisions()[0]["alert_id"].as_u64().unwrap();
    assert!(output.notices().is_empty(), "waits for the snapshot");
    h.core
        .alert_snapshot_put(alert_id, b"\xff\xd8example")
        .unwrap();
    h.run_until("delivered", 5, |_| !output.notices().is_empty());
    assert_eq!(
        output.notices()[0].snapshot.as_deref(),
        Some(&b"\xff\xd8example"[..])
    );
    assert_eq!(
        h.core.alert_snapshot_get(alert_id).unwrap(),
        b"\xff\xd8example"
    );
    assert_eq!(
        h.core.alert_snapshot_put(alert_id + 100, b"x"),
        Err(AlertError::UnknownId)
    );
    h.core
        .alert_feedback(
            &json!({"alert_id": alert_id, "vote": "down", "reason": "nothing_there"}).to_string(),
        )
        .unwrap();
    assert_eq!(
        h.core.alert_feedback(
            &json!({"alert_id": alert_id, "vote": "up", "reason": "duplicate"}).to_string()
        ),
        Err(AlertError::InvalidArgument)
    );
    let log: Value = serde_json::from_str(
        &h.core
            .alerts_get(&json!({"alert_id": alert_id}).to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(log["entries"][0]["snapshot"], true);
    assert_eq!(log["entries"][0]["feedback"]["reason"], "nothing_there");
    assert_eq!(h.core.alerts_get("[1]"), Err(AlertError::InvalidArgument));
}

/// A camera whose live stream gives no picture: after the first-picture
/// timeout the recording's cover image of the last seconds is checked
/// instead.
#[test]
fn a_cover_image_stands_in_when_the_stream_is_silent() {
    let mut h = Harness::with(1, NOON_S, false, |cfg| {
        let day = bc_protocol::archive::DateTime::new(2026, 6, 10, 0, 0, 0).unwrap();
        cfg.archive = Some(mock_nvr::archive::ArchiveConfig::new(vec![day]));
    });
    h.nvr.set_silent_previews(true);
    h.configure(json!({"cameras": [{"camera": h.cam(0)}], "acquisition": {"recheck": false}}));
    h.core
        .alerts_trigger_put(
            &json!({"source": "device", "camera": h.cam(0), "kind": "people"}).to_string(),
        )
        .unwrap();
    h.run_until_with("a decision", 30, &mut |_, _| Answer::Person, |h| {
        !h.decisions().is_empty()
    });
    let req = &h.rec.events_of("verify_request")[0];
    assert_eq!(req["from_cover"], true, "{req}");
    assert_eq!(h.nvr.command_count(298), 1);
    // Quiet decisions carry only a summary in events; the trace is in the log.
    let d = h.decisions()[0].clone();
    if d["decision"] != "alert" {
        assert!(d.get("trace").is_none(), "{d}");
        assert!(d["summary"].as_str().is_some_and(|t| !t.is_empty()), "{d}");
    }
    let q = json!({"trigger_id": d["trigger_id"]}).to_string();
    let log: Value = serde_json::from_str(&h.core.alerts_get(&q).unwrap()).unwrap();
    let steps = step_names(&log["entries"][0]["decision"]);
    assert!(steps.contains(&"frame_sampled".to_owned()), "{steps:?}");
}

/// Before the first configure nothing is checked; turning the service off
/// stops what it woke.
#[test]
fn idle_until_configured_and_off_stops_everything() {
    let mut h = Harness::new(1, NOON_S, false);
    assert!(h.nvr.set_alarm(0, "", "people"));
    h.sim.advance(Duration::from_secs(3));
    h.core.flush_events();
    assert!(h.rec.events_of("verify_request").is_empty());
    assert_eq!(
        h.core.alerts_trigger_put(
            &json!({"source": "device", "camera": h.cam(0), "kind": "people"}).to_string()
        ),
        Err(AlertError::NotConfigured)
    );
    h.configure(json!({"cameras": [{"camera": h.cam(0)}]}));
    h.core
        .alerts_trigger_put(
            &json!({"source": "device", "camera": h.cam(0), "kind": "people"}).to_string(),
        )
        .unwrap();
    h.run_until("woken", 10, |h| h.main_previews() == 1);
    h.configure(json!({"enabled": false}));
    h.run_until("stopped", 5, |h| h.main_previews() == 0);
    assert!(h.core.alerts_woken().is_empty());
}

// ---- The shipped modules: MQTT input and Home Assistant output ----

const MQTT_USER: &str = "example-user";
const MQTT_SECRET: &str = "example-mqtt-secret-0123";
const HA_SECRET: &str = "example-ha-token-0123456789";
const HELPER: &str = "input_boolean.example_alert";

/// Settings with an MQTT input on `broker` and a Home Assistant output on
/// `ha_url` that pulses `HELPER` for any alert (no cooldown, no pulse gap).
fn glue_settings(h: &Harness, broker_port: u16, ha_url: &str) -> Value {
    json!({
        "sources": [{"key": "sensor-1", "name": "Sensor 1",
                     "cameras": [{"camera": h.cam(0)}, {"camera": h.cam(1)}]}],
        "outputs": [{"kind": "desktop"},
                    {"kind": "home_assistant", "id": "ha", "endpoint": ha_url,
                     "min_severity": "low", "pulse_gap_ms": 0, "cooldown_ms": 0,
                     "rules": [{"action": {"kind": "pulse", "entity_id": HELPER}}]}],
        "inputs": [{"kind": "mqtt", "id": "sensor-1", "trigger_kind": "sensor_motion",
                    "username": MQTT_USER, "topic": "example/topic",
                    "broker": {"host": "127.0.0.1", "port": broker_port, "tls": null}}],
        "secrets": {"sensor-1": MQTT_SECRET, "ha": HA_SECRET},
    })
}

fn broker_with_login() -> mqtt_broker::Broker {
    mqtt_broker::Broker::start(mqtt_broker::BrokerOpts {
        auth: Some((MQTT_USER.to_owned(), MQTT_SECRET.to_owned())),
        ..mqtt_broker::BrokerOpts::default()
    })
}

impl Harness {
    /// Waits (in real time, for threads doing real I/O; the simulated clock
    /// stands still) until `cond` holds.
    fn wait_real(&self, what: &str, mut cond: impl FnMut(&Harness) -> bool) {
        wait_until(what, 30, || {
            self.core.flush_events();
            cond(self)
        });
    }

    fn mqtt_status(&self) -> Value {
        let log: Value =
            serde_json::from_str(&self.core.alerts_get(r#"{"limit":1}"#).unwrap()).unwrap();
        log["status"]["input_status"][0]["status"].clone()
    }

    fn delivery_steps(&self) -> Vec<Value> {
        self.rec
            .events_of("verify_step")
            .into_iter()
            .map(|e| e["step"].clone())
            .filter(|s| s["step"] == "delivery")
            .collect()
    }

    fn sub_previews(&self) -> usize {
        self.nvr
            .active_previews()
            .iter()
            .filter(|(_, t)| *t == StreamTier::Sub)
            .count()
    }

    /// Every event, the alert log answer, and every file the core wrote,
    /// as one text.
    fn everything_written(&self) -> String {
        let mut all = String::new();
        for e in self.rec.events() {
            all.push_str(&e.to_string());
        }
        all.push_str(&self.core.alerts_get("{}").unwrap());
        if let Some(dir) = &self._dir {
            let mut stack = vec![dir.path().to_path_buf()];
            while let Some(p) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&p) else {
                    continue;
                };
                for e in rd.flatten() {
                    let path = e.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if let Ok(text) = std::fs::read(&path) {
                        all.push_str(&String::from_utf8_lossy(&text));
                    }
                }
            }
        }
        all
    }
}

/// An MQTT "ON" wakes the mapped cameras, the scripted detector confirms a
/// person, the alert is delivered to Home Assistant (the helper is pulsed
/// over the real HTTP client with the token from the secrets), the
/// re-checks run on the sub stream, and no credential leaks anywhere.
#[test]
fn mqtt_on_confirmed_pulses_home_assistant() {
    let broker = broker_with_login();
    let ha = mock_ha::MockHa::start(HA_SECRET);
    ha.set(HELPER, "off");
    let mut h = Harness::new(2, NOON_S, true);
    h.configure(glue_settings(&h, broker.port, &ha.url()));
    h.core.flush_events();
    let configured = h.rec.events_of("alerts_configured");
    assert_eq!(configured[0]["inputs"][0]["ok"], true, "{configured:?}");
    assert_eq!(configured[0]["outputs"][0]["ok"], true, "{configured:?}");

    // The client logged in with the secret (the broker checks it).
    let connected = broker.wait(|e| matches!(e, mqtt_broker::Event::Connected { .. }));
    assert!(matches!(
        connected,
        mqtt_broker::Event::Connected { username: Some(ref u), .. } if u == MQTT_USER
    ));
    broker.wait_subscribed();
    broker.publish("example/topic", b"ON", false);
    h.wait_real("the trigger to arrive", |h| {
        h.mqtt_status()["triggers"] == 1
    });
    assert_eq!(h.mqtt_status()["status"], "connected");

    h.run_until_with("a decision", 20, &mut |_, _| Answer::Person, |h| {
        !h.decisions().is_empty()
    });
    let d = h.decisions()[0].clone();
    assert_eq!(d["decision"], "alert", "{d}");
    assert_eq!(d["unverified"], false);
    assert!(d["explanation"].as_str().unwrap().contains("Sensor 1"));
    h.wait_real("the Home Assistant delivery", |h| {
        !h.delivery_steps().is_empty()
    });
    let step = h.delivery_steps()[0].clone();
    assert_eq!(step["ok"], true, "{step}");
    assert_eq!(step["kind"], "home_assistant");
    assert!(step["summary"].as_str().unwrap().contains(HELPER), "{step}");
    // One pulse: read, on, off, with the token accepted.
    assert_eq!(ha.turn_ons(HELPER), 1, "{:?}", ha.log());
    assert_eq!(ha.state_of(HELPER).as_deref(), Some("off"));
    assert!(
        ha.log().iter().all(|l| !l.contains("bad token")),
        "{:?}",
        ha.log()
    );
    // The result is in the log with the decision.
    let log: Value =
        serde_json::from_str(&h.core.alerts_get(r#"{"decision":["alert"]}"#).unwrap()).unwrap();
    assert_eq!(log["entries"][0]["deliveries"][0]["ok"], true);
    assert_eq!(log["entries"][0]["source"], "external");

    // Re-checks of the alerted camera read the sub stream, not the main one.
    h.step = Duration::from_millis(250);
    h.run_until_with(
        "re-checks on the sub stream",
        120,
        &mut |_, _| Answer::Person,
        |h| h.sub_previews() >= 1 && h.main_previews() == 0,
    );
    h.run_until_with("a re-check request", 60, &mut |_, _| Answer::Person, |h| {
        h.rec
            .events_of("verify_request")
            .iter()
            .any(|e| e["purpose"] == "recheck")
    });

    // No credential in any event, the log, or any file.
    let all = h.everything_written();
    for secret in [MQTT_SECRET, HA_SECRET] {
        assert!(!all.contains(secret), "a credential was written out");
    }
    h.shutdown();
    broker.wait(|e| matches!(e, mqtt_broker::Event::Disconnected));
}

/// The detector sees nothing: a quiet decision and no pulse. A later,
/// confirmed alert pulses exactly once, so the first did not.
#[test]
fn mqtt_on_not_confirmed_does_not_pulse() {
    let broker = broker_with_login();
    let ha = mock_ha::MockHa::start(HA_SECRET);
    ha.set(HELPER, "off");
    let mut h = Harness::new(2, NOON_S, false);
    let mut settings = glue_settings(&h, broker.port, &ha.url());
    settings["cameras"] = json!([{"camera": h.cam(0)}, {"camera": h.cam(1)}]);
    h.configure(settings.clone());
    broker.wait_subscribed();
    broker.publish("example/topic", b"ON", false);
    h.wait_real("the trigger to arrive", |h| {
        h.mqtt_status()["triggers"] == 1
    });
    h.run_until_with("a decision", 20, &mut |_, _| Answer::Nothing, |h| {
        !h.decisions().is_empty()
    });
    let d = h.decisions()[0].clone();
    assert_eq!(d["decision"], "quiet", "{d}");
    assert!(
        d.get("trace").is_none(),
        "quiet events carry a summary only: {d}"
    );
    assert!(d["summary"].as_str().unwrap().contains("Sensor 1"), "{d}");
    h.core.flush_events();
    assert!(h.delivery_steps().is_empty());

    // A confirmed alert afterwards is the only pulse.
    h.core
        .alerts_trigger_put(
            &json!({"source": "device", "camera": h.cam(0), "kind": "people"}).to_string(),
        )
        .unwrap();
    h.run_until_with("a second decision", 30, &mut |_, _| Answer::Person, |h| {
        h.decisions().len() == 2
    });
    assert_eq!(
        h.decisions()[1]["decision"],
        "alert",
        "{}",
        h.decisions()[1]
    );
    h.wait_real("the delivery", |h| !h.delivery_steps().is_empty());
    assert_eq!(h.delivery_steps().len(), 1);
    assert_eq!(ha.turn_ons(HELPER), 1, "{:?}", ha.log());

    // Reconfiguring without the input stops it (the broker sees the
    // disconnect); the output, given its secret again, keeps working.
    let mut without = settings;
    without["inputs"] = json!([]);
    h.configure(without);
    broker.wait(|e| matches!(e, mqtt_broker::Event::Disconnected));
    h.shutdown();
}

/// A broker that rejects the login is not retried; the status says so and
/// nothing about the credential appears.
#[test]
fn mqtt_login_rejected_is_reported_and_not_retried() {
    let broker = broker_with_login();
    let ha = mock_ha::MockHa::start(HA_SECRET);
    let h = Harness::new(1, NOON_S, false);
    let mut settings = glue_settings(&h, broker.port, &ha.url());
    settings["secrets"] = json!({"sensor-1": "example-wrong-secret", "ha": HA_SECRET});
    h.configure(settings);
    broker.wait(|e| matches!(e, mqtt_broker::Event::Rejected));
    h.wait_real("the status", |h| h.mqtt_status()["status"] == "auth_failed");
    assert_eq!(h.mqtt_status()["auth_failures"], 1);
    assert!(!h.everything_written().contains("example-wrong-secret"));
}

/// An output with no token is left out with a clear message.
#[test]
fn home_assistant_without_a_token_is_reported() {
    let ha = mock_ha::MockHa::start(HA_SECRET);
    let h = Harness::new(1, NOON_S, false);
    let mut settings = glue_settings(&h, 1, &ha.url());
    settings["inputs"] = json!([]);
    settings["secrets"] = json!({});
    h.configure(settings);
    h.core.flush_events();
    let configured = h.rec.events_of("alerts_configured");
    let out = &configured[0]["outputs"][0];
    assert_eq!(out["ok"], false, "{configured:?}");
    assert!(out["error"].as_str().unwrap().contains("token"));
}

// ---- Debugging and review: message log, diagnostics, tests, evidence ----

impl Harness {
    fn messages(&self, extra: Value) -> Value {
        let mut req = json!({"input": "sensor-1"});
        for (k, v) in extra.as_object().unwrap() {
            req[k] = v.clone();
        }
        serde_json::from_str(&self.core.alerts_messages(&req.to_string()).unwrap()).unwrap()
    }

    fn test_decisions(&self) -> Vec<Value> {
        self.rec
            .events_of("alert")
            .into_iter()
            .filter(|e| e["state"] != "settled" && e["test"] == true)
            .collect()
    }
}

/// The sensor's message log has an outcome per message, is cleared on
/// request, and its payloads are never written to events, the alert log, or
/// any file.
#[test]
fn mqtt_messages_are_logged_with_outcomes_and_stay_in_memory() {
    let broker = broker_with_login();
    let ha = mock_ha::MockHa::start(HA_SECRET);
    let h = Harness::new(2, NOON_S, true);
    h.configure(glue_settings(&h, broker.port, &ha.url()));
    broker.wait_subscribed();
    broker.publish("example/topic", b"ON", false);
    broker.publish("example/topic", b"example-private-payload", false);
    h.wait_real("both messages", |h| h.mqtt_status()["messages"] == 2);
    let page = h.messages(json!({}));
    let outcomes: Vec<&str> = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["outcome"]["code"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, ["matched", "not_a_trigger_value"]);
    assert_eq!(
        page["entries"][0]["outcome"]["text"],
        "matched sensor sensor-1 as sensor_motion (start)"
    );
    assert_eq!(page["entries"][1]["payload"], "example-private-payload");
    // Paging by sequence number.
    let next = h.messages(json!({"after": page["next_seq"]}));
    assert_eq!(next["entries"].as_array().unwrap().len(), 0);
    assert_eq!(h.messages(json!({"limit": 1}))["more"], true);
    // Unknown input, bad request.
    assert_eq!(
        h.core.alerts_messages(r#"{"input":"nope"}"#),
        Err(AlertError::UnknownId)
    );
    assert_eq!(
        h.core.alerts_messages("{}"),
        Err(AlertError::InvalidArgument)
    );
    // Memory only.
    h.core.flush_events();
    assert!(!h.everything_written().contains("example-private-payload"));
    assert_eq!(h.messages(json!({"clear": true}))["total"], 0);
}

/// A broker nothing listens on is reported with its reason in the input's
/// state, with the log line.
#[test]
fn mqtt_connection_failure_names_its_reason() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let ha = mock_ha::MockHa::start(HA_SECRET);
    let h = Harness::new(1, NOON_S, false);
    h.configure(glue_settings(&h, port, &ha.url()));
    h.wait_real("the reason", |h| {
        h.mqtt_status()["last_error_kind"] == "refused"
    });
    let status = h.mqtt_status();
    assert!(
        status["last_error"]
            .as_str()
            .unwrap()
            .contains("nothing is listening")
    );
    assert!(status["last_error_ms"].as_i64().unwrap() > 0);
    assert!(status["last_success_ms"].is_null());
    h.core.flush_events();
    let log_lines: Vec<String> = h
        .rec
        .events_of("alerts_input_status")
        .iter()
        .filter_map(|e| e["status"]["log"].as_str().map(str::to_owned))
        .collect();
    assert!(
        log_lines.iter().any(|l| l.contains("nothing is listening")),
        "{log_lines:?}"
    );
}

/// A simulated trigger runs the whole chain, is marked as a test in the
/// decision, the log and the events, and reaches Home Assistant only when
/// asked.
#[test]
fn a_simulated_trigger_is_a_test_and_skips_home_assistant_unless_ticked() {
    let broker = broker_with_login();
    let ha = mock_ha::MockHa::start(HA_SECRET);
    ha.set(HELPER, "off");
    let mut h = Harness::new(2, NOON_S, false);
    h.configure(glue_settings(&h, broker.port, &ha.url()));
    h.core
        .alerts_trigger_put(
            &json!({"source": "external", "origin": "sensor-1", "kind": "sensor_motion",
                    "test": true, "only_camera": h.cam(1)})
            .to_string(),
        )
        .unwrap();
    h.run_until_with("a test decision", 20, &mut |_, _| Answer::Person, |h| {
        !h.test_decisions().is_empty()
    });
    let requests = h.rec.events_of("verify_request");
    assert!(!requests.is_empty());
    assert!(
        requests.iter().all(|r| r["camera"]["channel"] == 1),
        "only the chosen camera"
    );
    let ev = &h.test_decisions()[0];
    assert_eq!(ev["decision"]["decision"], "alert", "{ev}");
    let log: Value = serde_json::from_str(&h.core.alerts_get("{}").unwrap()).unwrap();
    assert_eq!(log["entries"][0]["test"], true);
    // The desktop output (the app) sees the alert; Home Assistant does not.
    h.core.flush_events();
    assert!(
        h.delivery_steps()
            .iter()
            .all(|s| s["kind"] != "home_assistant")
    );
    assert_eq!(ha.turn_ons(HELPER), 0, "{:?}", ha.log());
    drop(h);

    // Ticked: Home Assistant receives it.
    let ha = mock_ha::MockHa::start(HA_SECRET);
    ha.set(HELPER, "off");
    let mut h = Harness::new(2, NOON_S, false);
    h.configure(glue_settings(&h, broker.port, &ha.url()));
    h.core
        .alerts_trigger_put(
            &json!({"source": "external", "origin": "sensor-1", "kind": "sensor_motion",
                    "test": true, "send_to_home_assistant": true})
            .to_string(),
        )
        .unwrap();
    h.run_until_with("a test decision", 20, &mut |_, _| Answer::Person, |h| {
        !h.test_decisions().is_empty()
    });
    h.wait_real("the delivery", |h| !h.delivery_steps().is_empty());
    assert_eq!(ha.turn_ons(HELPER), 1, "{:?}", ha.log());
    // A real trigger afterwards is not marked.
    assert_eq!(
        h.core.alerts_trigger_put(
            &json!({"source": "external", "origin": "sensor-1", "kind": "sensor_motion",
                    "only_camera": h.cam(0)})
            .to_string()
        ),
        Err(AlertError::InvalidArgument),
        "only_camera is for tests"
    );
}

/// "Send test alert" delivers through the output now and reports the HTTP
/// status; a missing entity reports 404.
#[test]
fn a_test_alert_goes_through_home_assistant_and_reports_the_result() {
    let broker = broker_with_login();
    let ha = mock_ha::MockHa::start(HA_SECRET);
    ha.set(HELPER, "off");
    let h = Harness::new(1, NOON_S, false);
    h.configure(glue_settings(&h, broker.port, &ha.url()));
    assert_eq!(
        h.core.alerts_output_test(r#"{"output":7}"#),
        Err(AlertError::UnknownId)
    );
    h.core.alerts_output_test(r#"{"output":1}"#).unwrap();
    h.wait_real("the result", |h| {
        !h.rec.events_of("alerts_output_test").is_empty()
    });
    let ev = h.rec.events_of("alerts_output_test")[0].clone();
    assert_eq!(
        (ev["ok"].clone(), ev["http_status"].clone()),
        (json!(true), json!(200)),
        "{ev}"
    );
    assert_eq!(ev["kind"], "home_assistant");
    assert!(ev["at_ms"].as_i64().unwrap() > 0);
    assert_eq!(ha.turn_ons(HELPER), 1);
    // A second test straight away is not held back by a cooldown.
    ha.set(HELPER, "off");
    h.core.alerts_output_test(r#"{"output":1}"#).unwrap();
    h.wait_real("the second result", |h| {
        h.rec.events_of("alerts_output_test").len() == 2
    });
    assert_eq!(ha.turn_ons(HELPER), 2);
    // The desktop entry cannot send a test: reported, not an error.
    h.core.alerts_output_test(r#"{"output":0}"#).ok();
    drop(ha);
}

/// Evidence is kept with the decision, expires on the (simulated) clock,
/// and a pin keeps a set.
#[test]
fn evidence_is_kept_with_the_decision_expires_and_can_be_pinned() {
    let mut h = Harness::new(1, NOON_S, true);
    h.configure(
        json!({"cameras": [{"camera": h.cam(0), "exposure": "outside"}],
                       "evidence": {"retention_min": 30},
                       "acquisition": {"recheck": false}}),
    );
    let jpeg = b"\xff\xd8example-frame\xff\xd9";
    let mut ids = Vec::new();
    for _ in 0..2 {
        let id = h
            .core
            .alerts_trigger_put(
                &json!({"source": "device", "camera": h.cam(0), "kind": "people"}).to_string(),
            )
            .unwrap();
        h.core
            .alert_evidence_put(
                &json!({"trigger_id": id, "frame": 0,
                        "doc": {"methods": {"camera_ai": {"claimed": "person"},
                                            "detector": {"boxes": []}}}})
                .to_string(),
                jpeg,
            )
            .unwrap();
        let n = ids.len() + 1;
        h.run_until_with("a decision", 20, &mut |_, _| Answer::Person, |h| {
            h.decisions().len() == n
        });
        // Out of the cooldown for the next one.
        h.sim.advance(Duration::from_secs(600));
        ids.push(id);
    }
    let set: Value = serde_json::from_str(
        &h.core
            .alert_evidence_get(&json!({"trigger_id": ids[0]}).to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(set["decision"]["decision"], "alert", "{set}");
    assert_eq!(set["methods"]["camera_ai"]["claimed"], "person");
    assert_eq!(set["frames"][0]["bytes"], jpeg.len());
    assert_eq!(set["pinned"], false);
    assert_eq!(h.core.alert_evidence_frame_get(ids[0], 0).unwrap(), jpeg);
    // Bad input.
    assert_eq!(
        h.core
            .alert_evidence_put(r#"{"trigger_id":999999,"frame":0}"#, jpeg),
        Err(AlertError::UnknownId)
    );
    assert_eq!(
        h.core
            .alert_evidence_put(&json!({"trigger_id": ids[0]}).to_string(), b""),
        Err(AlertError::InvalidArgument)
    );
    let list: Value = serde_json::from_str(&h.core.alert_evidence_get("{}").unwrap()).unwrap();
    assert_eq!(list["sets"].as_array().unwrap().len(), 2);
    assert_eq!(list["retention_min"], 30);
    // Keep the first one; the clock passes the retention.
    h.core.alert_evidence_pin(ids[0]).unwrap();
    let snapshot_alert = set["decision"]["alert_id"].as_u64().unwrap();
    assert_eq!(h.core.alert_snapshot_get(snapshot_alert).unwrap(), jpeg);
    h.sim.advance(Duration::from_secs(31 * 60));
    assert_eq!(
        h.core
            .alert_evidence_get(&json!({"trigger_id": ids[1]}).to_string()),
        Err(AlertError::UnknownId)
    );
    assert_eq!(
        h.core.alert_evidence_frame_get(ids[1], 0),
        Err(AlertError::UnknownId)
    );
    let kept: Value = serde_json::from_str(
        &h.core
            .alert_evidence_get(&json!({"trigger_id": ids[0]}).to_string())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(kept["pinned"], true);
    assert_eq!(
        h.core.alert_evidence_pin(ids[1]),
        Err(AlertError::UnknownId)
    );
}

mod property {
    use super::*;
    use proptest::prelude::*;

    #[derive(Clone, Debug)]
    enum Op {
        Trigger(u8),
        External,
        Advance(u16),
        Drop,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u8..3).prop_map(Op::Trigger),
            Just(Op::External),
            (1u16..4000).prop_map(Op::Advance),
            Just(Op::Drop),
        ]
    }

    fn answer() -> impl Strategy<Value = Answer> {
        prop_oneof![
            Just(Answer::Person),
            Just(Answer::Nothing),
            Just(Answer::Silent)
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 6, ..ProptestConfig::default() })]

        /// Whatever triggers, answers, and drops happen, every stream the
        /// service woke stops once its checks and episodes are over.
        #[test]
        fn woken_streams_always_stop(ops in proptest::collection::vec(op(), 1..8),
                                     answers in proptest::collection::vec(answer(), 1..8)) {
            let mut h = Harness::new(3, NOON_S, false);
            let cams: Vec<Value> = (0..3).map(|c| json!({"camera": h.cam(c), "exposure": "outside"})).collect();
            h.configure(json!({"cameras": cams, "standby": {"max_wake_per_device": 2},
                               "sources": [{"key": "s1", "cameras": [{"camera": h.cam(0)}, {"camera": h.cam(2)}]}],
                               "timing": {"recheck_limit_s": 60}}));
            let mut i = 0usize;
            let mut det = move |_c: u64, _n: usize| { i += 1; answers[i % answers.len()] };
            for op in ops {
                match op {
                    Op::Trigger(c) => {
                        let _ = h.core.alerts_trigger_put(&json!({"source": "device", "camera": h.cam(u64::from(c)), "kind": "people"}).to_string());
                    }
                    Op::External => {
                        let _ = h.core.alerts_trigger_put(&json!({"source": "external", "origin": "s1", "kind": "beam"}).to_string());
                    }
                    Op::Advance(ms) => {
                        let target = h.sim.elapsed() + Duration::from_millis(u64::from(ms));
                        h.run_until_with("advance", 10, &mut det, |h| h.sim.elapsed() >= target);
                    }
                    Op::Drop => h.nvr.drop_connections(),
                }
            }
            // Re-checks last at most a minute, episodes settle after two
            // idle minutes: well within ten.
            h.step = Duration::from_millis(250);
            h.run_until_with("streams stopped", 600, &mut det, |h| {
                h.main_previews() == 0 && h.core.alerts_woken().is_empty()
            });
            prop_assert!(h.max_logins <= 1);
        }
    }
}
