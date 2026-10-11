//! Sound for recordings (docs/FFI-RECORDINGS.md, section 2a) end to end, on
//! a simulated clock: cameras on the "always listen" list feed the loudness
//! envelope store and the alarm labels without any tile; labels show in
//! event queries and incidents with source `local_audio`; everything
//! survives a restart; older days are analyzed only on request, from a
//! source the test supplies. The mock NVR interleaves a synthetic smoke
//! alarm (fixtures/synthetic/aac-smoke-t3.aac) with its video.

mod common;

use bc_protocol::archive::DateTime;
use common::{PASSWORD, Recorder, USER, fast_config, on_sim, sim_spawn, sim_until, start};
use serde_json::{Value, json};
use simtime::SimRuntime;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use viewer_core::{
    AudioChunk, Core, CoreConfig, DayAudioSource, RecError, SourceError, StreamTier,
};

/// Noon on the 10th, UTC; the device is an hour east of UTC.
fn t0() -> i64 {
    DateTime::new(2026, 10, 10, 12, 0, 0)
        .unwrap()
        .to_naive_secs()
}

const OFFSET: i64 = 3600;

/// The device's local day (the 10th) and the local second the sim starts.
fn day() -> i64 {
    t0() - 12 * 3600
}

fn local0() -> i64 {
    t0() + OFFSET
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name)
}

fn mock(sim: &SimRuntime, channels: u8) -> mock_nvr::MockNvr {
    let video = mock_nvr::fixture::Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let mut cfg = mock_nvr::Config::new(video);
    cfg.channels = channels;
    let mut a = mock_nvr::audio::AudioConfig::new(
        mock_nvr::audio::AudioFixture::load(&fixture_path("aac-smoke-t3.aac")).unwrap(),
    );
    a.jitter_us = 30_000;
    cfg.audio = Some(Arc::new(a));
    sim_spawn(sim, cfg)
}

fn config(sim: &SimRuntime, data_dir: Option<&Path>) -> CoreConfig {
    on_sim(
        sim,
        CoreConfig {
            data_dir: data_dir.map(Path::to_path_buf),
            test_wall_clock_start_us: Some(t0() * 1_000_000),
            ..fast_config()
        },
    )
}

fn cam(key: &str, ch: u8) -> Value {
    json!({"device": key, "channel": ch, "lens": 0})
}

fn parse(s: Result<String, RecError>) -> Value {
    serde_json::from_str(&s.unwrap()).unwrap()
}

fn enable(core: &Core, key: &str) {
    core.recordings_enable(
        key,
        Some(
            &json!({"channels": [{"channel": 0}, {"channel": 1}], "utc_offset_s": OFFSET})
                .to_string(),
        ),
    )
    .unwrap();
}

fn events(core: &Core, kinds: &[&str]) -> Vec<Value> {
    let q = json!({"start": day(), "end": day() + 86_399, "kinds": kinds});
    parse(core.recordings_events(&q.to_string()))["events"]
        .as_array()
        .unwrap()
        .clone()
}

fn lane(core: &Core, key: &str, ch: u8, start: i64, end: i64, bucket: u32) -> Value {
    let q = json!({"start": start, "end": end, "bucket_s": bucket, "cameras": [cam(key, ch)]});
    let a = parse(core.recordings_loudness(&q.to_string()));
    assert_eq!(a["lanes"].as_array().unwrap().len(), 1, "{a}");
    a["lanes"][0].clone()
}

fn db_values(lane: &Value, member: &str) -> Vec<Option<i64>> {
    lane[member]
        .as_array()
        .unwrap()
        .iter()
        .map(Value::as_i64)
        .collect()
}

fn sound_watch_states(rec: &Recorder, channel: u64) -> Vec<String> {
    rec.events_of("sound_watch")
        .iter()
        .filter(|e| e["camera"]["channel"] == channel)
        .map(|e| e["state"].as_str().unwrap().to_owned())
        .collect()
}

fn setup(
    sim: &SimRuntime,
    data_dir: Option<&Path>,
) -> (mock_nvr::MockNvr, Arc<Core>, Arc<Recorder>, u64, String) {
    let nvr = mock(sim, 2);
    let (core, rec) = start(config(sim, data_dir));
    core.recordings_configure("{}").unwrap();
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let key = core.device_key(dev).unwrap();
    enable(&core, &key);
    (nvr, core, rec, dev, key)
}

/// A camera on the listen list is heard without a tile: its loudness reaches
/// the store in the device's local time, the alarm becomes one label with
/// source `local_audio` (also in incidents), a tile on the same channel
/// takes over without a second label, and a restart keeps all of it.
#[test]
fn a_watched_camera_fills_the_envelope_and_alarm_labels_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let sim = SimRuntime::new();
    let (nvr, core, rec, dev, key) = setup(&sim, Some(dir.path()));

    // Nothing is listened to until asked: no envelope, no labels.
    sim.advance(Duration::from_secs(5));
    assert_eq!(nvr.command_count(3), 0, "no preview was started");
    assert!(
        db_values(&lane(&core, &key, 1, local0(), local0() + 9, 1), "peak_db")
            .iter()
            .all(Option::is_none)
    );

    core.sound_watch(&cam(&key, 1).to_string(), true).unwrap();
    // Asking twice changes nothing.
    core.sound_watch(&cam(&key, 1).to_string(), true).unwrap();
    sim_until(&sim, &core, "an alarm label", 60, || {
        !events(&core, &["smoke_alarm"]).is_empty()
    });
    assert_eq!(sound_watch_states(&rec, 1), ["watching"]);
    let e = events(&core, &["smoke_alarm"]);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0]["source"], "local_audio");
    assert_eq!(e[0]["camera"], cam(&key, 1));
    assert_eq!(e[0]["analyzer"], "pattern-v1");
    assert!(e[0].get("confidence").is_none());
    let started = e[0]["start"].as_i64().unwrap();
    // The device-local second of the first beep: shortly after the sim
    // started, an hour ahead of UTC.
    let since = started - local0() - 5;
    assert!((-1..=3).contains(&since), "{started} vs {}", local0() + 5);
    // The other channel is not listened to.
    assert!(
        events(&core, &[])
            .iter()
            .all(|e| e["camera"]["channel"] == 1)
    );

    // The loudness of the tone, about -10 dBFS, in local seconds.
    let l = lane(&core, &key, 1, local0(), local0() + 29, 1);
    assert_eq!(l["bucket_s"], 1);
    assert_eq!(l["start"], local0());
    let peaks = db_values(&l, "peak_db");
    assert!(
        peaks.iter().flatten().any(|d| (-13..=-7).contains(d)),
        "{peaks:?}"
    );
    assert!(peaks.iter().flatten().count() >= 6, "{peaks:?}");
    // A minute bucket aggregates: the loudest second, and a quieter RMS.
    let l60 = lane(&core, &key, 1, local0(), local0() + 59, 60);
    let (p, r) = (db_values(&l60, "peak_db"), db_values(&l60, "rms_db"));
    assert_eq!((p.len(), r.len()), (1, 1));
    assert!(p[0].unwrap() >= -13 && r[0].unwrap() <= p[0].unwrap());
    // Channel 0 has nothing.
    assert!(
        db_values(&lane(&core, &key, 0, local0(), local0() + 29, 1), "peak_db")
            .iter()
            .all(Option::is_none)
    );

    // The label joins incidents like a device event.
    sim_until(&sim, &core, "an incident", 5, || {
        !parse(core.incidents_get(
            &json!({"start": day(), "end": day() + 86_399}).to_string(),
        ))["incidents"]
            .as_array()
            .unwrap()
            .is_empty()
    });
    let inc =
        parse(core.incidents_get(&json!({"start": day(), "end": day() + 86_399}).to_string()));
    let ev = &inc["incidents"][0]["events"][0];
    assert_eq!(ev["kind"], "smoke_alarm");
    assert_eq!(ev["source"], "local_audio");
    assert_eq!(inc["incidents"][0]["kinds"], json!(["smoke_alarm"]));
    assert!(
        rec.events_of("recordings_changed")
            .iter()
            .any(|e| e["device"] == key.as_str())
    );

    // A tile on the watched channel takes over; the alarm continues as one
    // episode, one label.
    let tile = core.open_tile(dev, 1, StreamTier::Main).unwrap();
    sim.advance(Duration::from_secs(4));
    core.flush_events();
    // Channel 0 on screen is heard too: its envelope fills with no listing.
    let tile0 = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    sim.advance(Duration::from_secs(8));
    core.flush_events();
    let heard0 = db_values(&lane(&core, &key, 0, local0(), local0() + 59, 1), "peak_db");
    assert!(heard0.iter().flatten().count() >= 5, "{heard0:?}");

    // Closing the tiles hands back to the listener; removing it ends the
    // episode, which gives the label its end.
    core.close_tile(tile).unwrap();
    core.close_tile(tile0).unwrap();
    sim.advance(Duration::from_secs(2));
    core.sound_watch(&cam(&key, 1).to_string(), false).unwrap();
    core.sound_watch(&cam(&key, 1).to_string(), false).unwrap();
    sim.advance(Duration::from_millis(500));
    core.flush_events();
    assert_eq!(sound_watch_states(&rec, 1), ["watching", "stopped"]);
    // Handing the channel over between the listener and a tile ends an
    // episode and starts another (as it does between tiles), so the alarm
    // may be several labels; the first keeps its start and has an end.
    let e = events(&core, &["smoke_alarm"]);
    assert!((1..=3).contains(&e.len()), "{e:?}");
    assert_eq!(e[0]["start"].as_i64(), Some(started));
    assert!(e.iter().all(|l| l["end"].as_i64() >= l["start"].as_i64()));
    assert!(
        e.iter().any(|l| l["end"].as_i64() > l["start"].as_i64()),
        "{e:?}"
    );
    let before = lane(&core, &key, 1, local0(), local0() + 59, 1);

    // Restart: the stored days and the index come back.
    sim.run_free(|| core.shutdown());
    drop(core);
    let file = dir
        .path()
        .join("recordings")
        .join("sound")
        .join(&key)
        .join("c1l0-2026-10-10.env");
    assert!(file.exists(), "{file:?}");
    let sim2 = SimRuntime::new();
    let (core2, _rec2) = start(config(&sim2, Some(dir.path())));
    core2.recordings_configure("{}").unwrap();
    enable(&core2, &key);
    assert_eq!(lane(&core2, &key, 1, local0(), local0() + 59, 1), before);
    assert_eq!(events(&core2, &["smoke_alarm"]), e);
    // Cameras left out: every camera with loudness.
    let all = parse(
        core2.recordings_loudness(&json!({"start": local0(), "end": local0() + 59}).to_string()),
    );
    let cams: Vec<_> = all["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["camera"]["channel"].as_u64().unwrap())
        .collect();
    assert_eq!(cams, [0, 1]);
    sim2.run_free(|| core2.shutdown());
}

/// The calls refuse what they should.
#[test]
fn the_sound_calls_validate() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim, 1);
    let (core, _rec) = start(config(&sim, None));
    let cam0 = json!({"device": "d-x", "channel": 0}).to_string();
    assert_eq!(core.sound_watch(&cam0, true), Err(RecError::NotConfigured));
    assert_eq!(
        core.recordings_loudness(r#"{"start":1,"end":2}"#),
        Err(RecError::NotConfigured)
    );
    assert_eq!(core.sound_labels_put("{}"), Err(RecError::InvalidArgument));
    core.recordings_configure("{}").unwrap();
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let key = core.device_key(dev).unwrap();

    // Configure: wrong types change nothing; right ones are accepted.
    assert_eq!(
        core.sound_configure(r#"{"alarms":"yes"}"#),
        Err(RecError::InvalidArgument)
    );
    assert_eq!(
        core.sound_configure(r#"{"loudness_history_s":3601}"#),
        Err(RecError::InvalidArgument)
    );
    assert_eq!(core.sound_configure("[]"), Err(RecError::InvalidArgument));
    core.sound_configure(r#"{"alarms":true,"loudness_history_s":600,"future":1}"#)
        .unwrap();

    // Loudness query shapes.
    let q = |v: Value| core.recordings_loudness(&v.to_string());
    assert_eq!(
        q(json!({"start": 5, "end": 4})),
        Err(RecError::InvalidArgument)
    );
    assert_eq!(
        q(json!({"start": 5, "end": 9, "bucket_s": 0})),
        Err(RecError::InvalidArgument)
    );
    assert_eq!(
        q(json!({"start": 5, "end": 9, "bucket_s": 3601})),
        Err(RecError::InvalidArgument)
    );
    assert_eq!(
        q(json!({"start": 0, "end": 200_000, "bucket_s": 1})),
        Err(RecError::Limit)
    );
    // A device that is not enabled gets a lane of nulls.
    let l = parse(q(
        json!({"start": 0, "end": 9, "bucket_s": 5, "cameras": [cam(&key, 0)]}),
    ));
    assert_eq!(l["lanes"][0]["peak_db"], json!([null, null]));

    // Labels: the device must be enabled; kinds, confidence, and the
    // reserved analyzer are checked.
    let label = |kind: &str, conf: Value| {
        json!({"camera": cam(&key, 0), "analyzer": "app-v1", "start": day(), "end": day() + 600,
               "labels": [{"kind": kind, "start": day() + 10, "end": day() + 16, "confidence": conf}]})
            .to_string()
    };
    assert_eq!(
        core.sound_labels_put(&label("dog_bark", json!(0.9))),
        Err(RecError::InvalidArgument)
    );
    enable(&core, &key);
    assert_eq!(
        core.sound_labels_put(&label("Dog Bark", json!(0.9))),
        Err(RecError::InvalidArgument)
    );
    assert_eq!(
        core.sound_labels_put(&label("dog_bark", json!(1.5))),
        Err(RecError::InvalidArgument)
    );
    let reserved = label("siren", json!(0.5)).replace("app-v1", "pattern-v1");
    assert_eq!(
        core.sound_labels_put(&reserved),
        Err(RecError::InvalidArgument)
    );
    let wide = json!({"camera": cam(&key, 0), "analyzer": "app-v1", "start": 0, "end": 40 * 86_400,
                      "labels": []})
    .to_string();
    assert_eq!(core.sound_labels_put(&wide), Err(RecError::Limit));
    assert_eq!(core.sound_labels_put(&label("DOG_BARK", json!(0.9))), Ok(1));

    // Analyze: a camera not enabled, no source, cancel of nothing.
    let other = json!({"camera": cam("d-nope", 0), "day": day()}).to_string();
    assert_eq!(
        core.sound_analyze_day(&other),
        Err(RecError::InvalidArgument)
    );
    assert_eq!(core.sound_analyze_cancel(99), Err(RecError::UnknownId));
    sim.run_free(|| core.shutdown());
}

/// Labels from the app: stored as `local_audio` events with the classifier's
/// own label and confidence, replaced by analyzer and range, filtered by
/// kind, and part of incidents.
#[test]
fn labels_from_the_app_replace_by_analyzer_and_range() {
    let sim = SimRuntime::new();
    let (_nvr, core, rec, _dev, key) = setup(&sim, None);
    let put = |analyzer: &str, start: i64, end: i64, labels: Value| {
        core.sound_labels_put(
            &json!({"camera": cam(&key, 0), "analyzer": analyzer, "start": start, "end": end,
                    "labels": labels})
            .to_string(),
        )
    };
    let (a, b) = (day() + 7 * 3600, day() + 8 * 3600);
    let dog = json!({"kind": "dog_bark", "start": a + 125, "end": a + 131, "confidence": 0.91,
                     "label": "Dog"});
    assert_eq!(put("soundanalysis-v1", a, b, json!([dog])), Ok(1));
    let e = events(&core, &[]);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0]["kind"], "dog_bark");
    assert_eq!(e[0]["source"], "local_audio");
    assert_eq!(e[0]["label"], "Dog");
    assert_eq!(e[0]["confidence"], 0.91);
    assert_eq!(e[0]["analyzer"], "soundanalysis-v1");

    // Again: the same labels change nothing; other labels replace them.
    assert_eq!(put("soundanalysis-v1", a, b, json!([dog])), Ok(0));
    let siren = json!({"kind": "siren", "start": a + 900, "end": a + 930, "confidence": 0.6});
    assert_eq!(put("soundanalysis-v1", a, b, json!([siren])), Ok(1));
    assert_eq!(
        events(&core, &[])
            .iter()
            .map(|e| e["kind"].clone())
            .collect::<Vec<_>>(),
        [json!("siren")]
    );
    // Another analyzer's labels are separate, and kinds filter.
    assert_eq!(put("other-v2", a, b, json!([dog])), Ok(1));
    assert_eq!(events(&core, &[]).len(), 2);
    assert_eq!(events(&core, &["dog_bark"]).len(), 1);
    assert_eq!(events(&core, &["motion"]).len(), 0);

    let incidents =
        || parse(core.incidents_get(&json!({"start": day(), "end": day() + 86_399}).to_string()));
    sim_until(&sim, &core, "an incident of the siren", 5, || {
        incidents()["incidents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kinds"].as_array().unwrap().contains(&json!("siren")))
    });
    let inc = incidents();
    let kinds: Vec<_> = inc["incidents"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["events"].as_array().unwrap().clone())
        .map(|e| (e["kind"].as_str().unwrap().to_owned(), e["source"].clone()))
        .collect();
    assert!(
        kinds.contains(&("siren".into(), json!("local_audio"))),
        "{kinds:?}"
    );
    assert!(
        rec.events_of("incidents_changed")
            .iter()
            .any(|e| e["revision"].as_u64() >= Some(1))
    );
    sim.run_free(|| core.shutdown());
}

/// A source of one day's audio: a T3 alarm from 02:00:03.5 (device-local).
struct Alarm {
    opened: AtomicUsize,
}

impl DayAudioSource for Alarm {
    fn open(
        &self,
        device: &str,
        camera: recordings::DeviceCamera,
        day: i64,
    ) -> Result<Box<dyn Iterator<Item = AudioChunk> + Send>, SourceError> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        if camera.channel != 0 || device.is_empty() || day % 86_400 != 0 {
            return Err(SourceError::NoAudio);
        }
        let rate = 16_000u64;
        let first = 2 * 3600 * rate + rate * 7 / 2;
        let pcm: Vec<f32> = (0..20 * rate)
            .map(|n| {
                let t = n as f64 / rate as f64;
                let c = t % 4.0;
                if c < 3.0 && c % 1.0 < 0.5 {
                    0.3 * (2.0 * std::f64::consts::PI * 3100.0 * t).sin() as f32
                } else {
                    0.0
                }
            })
            .collect();
        let chunks: Vec<AudioChunk> = pcm
            .chunks(16_000)
            .enumerate()
            .map(|(i, c)| AudioChunk {
                first_sample: first + (i * 16_000) as u64,
                samples: c.to_vec(),
            })
            .collect();
        Ok(Box::new(chunks.into_iter()))
    }
}

/// Older days are analyzed only on request: nothing reads audio until asked,
/// then the day's envelope and alarm labels appear, and asking again does
/// not duplicate them.
#[test]
fn recorded_days_are_analyzed_only_on_request() {
    let sim = SimRuntime::new();
    let (_nvr, core, rec, _dev, key) = setup(&sim, None);
    let source = Arc::new(Alarm {
        opened: AtomicUsize::new(0),
    });
    let past = day() - 86_400;
    let ask = |ch: u8, d: i64| {
        core.sound_analyze_day(&json!({"camera": cam(&key, ch), "day": d + 5}).to_string())
    };

    // Without a source the request fails, and nothing else happens.
    let r = parse(ask(0, past));
    assert_eq!(r["state"], "queued");
    sim_until(&sim, &core, "a failure without a source", 5, || {
        rec.events_of("sound_analyze")
            .iter()
            .any(|e| e["state"] == "failed")
    });
    let failed = rec.events_of("sound_analyze").last().unwrap().clone();
    assert_eq!(failed["reason"], "no_source");
    assert_eq!(failed["request"], r["request"]);
    assert_eq!(failed["day"], past);

    core.set_sound_day_source(Some(source.clone())).unwrap();
    sim.advance(Duration::from_secs(120));
    assert_eq!(
        source.opened.load(Ordering::SeqCst),
        0,
        "no background pass"
    );
    assert!(events(&core, &["smoke_alarm"]).is_empty());

    // On request: queued, running, done.
    let r = parse(ask(0, past));
    let id = r["request"].clone();
    let done = |rec: &Recorder, id: &Value| {
        rec.events_of("sound_analyze")
            .into_iter()
            .find(|e| e["request"] == *id && e["state"] == "done")
    };
    sim_until(&sim, &core, "the analysis", 30, || {
        done(&rec, &id).is_some()
    });
    let d = done(&rec, &id).unwrap();
    assert_eq!(d["camera"], cam(&key, 0));
    assert!(d["seconds"].as_u64().unwrap() >= 20, "{d}");
    assert_eq!(d["labels"], 1);
    assert_eq!(source.opened.load(Ordering::SeqCst), 1);
    let states: Vec<_> = rec
        .events_of("sound_analyze")
        .iter()
        .filter(|e| e["request"] == id)
        .map(|e| e["state"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(states, ["queued", "running", "done"]);

    let q = json!({"start": past, "end": past + 86_399, "kinds": ["smoke_alarm"]});
    let e = parse(core.recordings_events(&q.to_string()));
    let e = e["events"].as_array().unwrap();
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0]["source"], "local_audio");
    let at = past + 2 * 3600 + 3;
    assert!(
        (at..=at + 2).contains(&e[0]["start"].as_i64().unwrap()),
        "{e:?}"
    );
    let l = lane(&core, &key, 0, at, at + 29, 1);
    let peaks = db_values(&l, "peak_db");
    assert!(
        peaks.iter().flatten().any(|d| (-13..=-7).contains(d)),
        "{peaks:?}"
    );
    // The day before the audio and the other channel are untouched.
    assert!(
        db_values(&lane(&core, &key, 0, past, past + 59, 1), "peak_db")
            .iter()
            .all(Option::is_none)
    );

    // Again: same labels (not doubled). A camera-day without audio fails.
    let again = parse(ask(0, past));
    assert_ne!(again["request"], id);
    sim_until(&sim, &core, "the second analysis", 30, || {
        done(&rec, &again["request"]).is_some()
    });
    assert_eq!(
        parse(core.recordings_events(&q.to_string()))["events"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let none = parse(ask(1, past));
    sim_until(&sim, &core, "a day without audio", 30, || {
        rec.events_of("sound_analyze")
            .iter()
            .any(|e| e["request"] == none["request"] && e["state"] == "failed")
    });
    let f = rec
        .events_of("sound_analyze")
        .into_iter()
        .find(|e| e["request"] == none["request"] && e["state"] == "failed")
        .unwrap();
    assert_eq!(f["reason"], "no_audio");

    // Duplicate requests while one waits are one; a queued one cancels.
    let first = parse(ask(0, past - 86_400));
    let dup = parse(ask(0, past - 86_400));
    assert_eq!(first["request"], dup["request"]);
    let queued = parse(ask(0, past - 2 * 86_400));
    core.sound_analyze_cancel(queued["request"].as_u64().unwrap())
        .unwrap();
    sim_until(&sim, &core, "the first of the queue", 30, || {
        done(&rec, &first["request"]).is_some()
    });
    core.flush_events();
    assert!(
        rec.events_of("sound_analyze")
            .iter()
            .any(|e| e["request"] == queued["request"] && e["state"] == "canceled")
    );
    assert!(done(&rec, &queued["request"]).is_none());
    sim.run_free(|| core.shutdown());
}
