//! Smart alerts end to end against the mock NVR on a simulated clock: a
//! trigger wakes cameras, the core announces pictures (`verify_request`), a
//! scripted detector (boxes keyed by camera and picture) answers, and the
//! engine decides with a trace; woken streams stop when checks end and
//! episodes settle. Nothing here waits on real time.

mod common;

use common::{PASSWORD, Recorder, USER, fast_config, on_sim, sim_spawn, start};
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
        let sim = SimRuntime::new();
        let mut cfg = mock_nvr::Config::new(fixture());
        cfg.channels = channels;
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
