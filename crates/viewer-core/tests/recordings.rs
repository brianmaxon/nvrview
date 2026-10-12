//! The recordings owner (docs/FFI-RECORDINGS.md, sections 1 to 5 and 8)
//! against in-process mock NVRs with a recording store, on a simulated
//! clock: the index fills only when asked, today stays fresh only while
//! held, events are coalesced, device removal and a refused login stop the
//! lane, thumbnails, downloads (dedupe, pause, retry, forget, trash), and
//! what survives a restart.

mod common;

use bc_protocol::archive::DateTime;
use common::*;
use mock_nvr::archive::{ArchiveConfig, MockEvent};
use serde_json::{Value, json};
use simtime::SimRuntime;
use std::path::{Path, PathBuf};
use std::time::Duration;
use viewer_core::{Core, CoreConfig, RecError, ThumbKind, Thumbnail, ThumbnailAnswer};

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn t(day: u8, h: u8, m: u8, s: u8) -> i64 {
    d(day, h, m, s).to_naive_secs()
}

fn day(n: u8) -> i64 {
    t(n, 0, 0, 0)
}

/// A device with three channels and recordings on 8, 9, and 10 October
/// (today is the 10th): a person on channel 0 on the 8th, and a vehicle on
/// channel 1 with motion on channel 0 at the same moment on the 9th.
fn archive_mock(
    sim: &SimRuntime,
    search_delay: Option<Duration>,
    rate: Option<u64>,
) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 3;
    let mut a = ArchiveConfig::new(vec![d(8, 0, 0, 0), d(9, 0, 0, 0), d(10, 0, 0, 0)]);
    a.events_per_day = 0;
    let ev = |channel: u8, kind: &str, start: DateTime, end: DateTime| MockEvent {
        channel,
        kind: kind.into(),
        start,
        end,
    };
    a.events = vec![
        ev(0, "people", d(8, 20, 0, 0), d(8, 20, 0, 15)),
        ev(1, "vehicle", d(9, 7, 42, 5), d(9, 7, 42, 25)),
        ev(0, "md", d(9, 7, 42, 10), d(9, 7, 42, 30)),
    ];
    a.search_delay = search_delay;
    a.download_rate = rate;
    cfg.archive = Some(a);
    sim_spawn(sim, cfg)
}

/// Noon on the 10th, UTC: the device's today (offset 0) is the 10th.
fn config(sim: &SimRuntime, data_dir: Option<&Path>) -> CoreConfig {
    on_sim(
        sim,
        CoreConfig {
            data_dir: data_dir.map(Path::to_path_buf),
            test_wall_clock_start_us: Some(t(10, 12, 0, 0) * 1_000_000),
            ..fast_config()
        },
    )
}

fn add_with(core: &Core, nvr: &mock_nvr::MockNvr, password: &str) -> (u64, String) {
    let addr = nvr.addr();
    let id = core
        .add_device(&addr.ip().to_string(), addr.port(), USER, password)
        .unwrap();
    (id, core.device_key(id).unwrap())
}

fn add(core: &Core, nvr: &mock_nvr::MockNvr) -> (u64, String) {
    add_with(core, nvr, PASSWORD)
}

fn cam(key: &str, ch: u8) -> Value {
    json!({"device": key, "channel": ch, "lens": 0})
}

fn parse(s: Result<String, RecError>) -> Value {
    serde_json::from_str(&s.unwrap()).unwrap()
}

fn of(rec: &Recorder, kind: &str, key: &str) -> Vec<Value> {
    rec.events_of(kind)
        .into_iter()
        .filter(|e| e["device"] == key)
        .collect()
}

/// Events with the simulated time they were first seen (within a 10 ms
/// step).
#[derive(Default)]
struct Log {
    seen: usize,
    items: Vec<(Duration, Value)>,
}

impl Log {
    fn until(
        &mut self,
        sim: &SimRuntime,
        core: &Core,
        rec: &Recorder,
        what: &str,
        secs: u64,
        mut cond: impl FnMut(&[(Duration, Value)]) -> bool,
    ) {
        sim_until(sim, core, what, secs, || {
            let ev = rec.events();
            for e in &ev[self.seen..] {
                self.items.push((sim.elapsed(), e.clone()));
            }
            self.seen = ev.len();
            cond(&self.items)
        });
    }
}

fn idle_with_nothing_pending(rec: &Recorder, key: &str) -> bool {
    of(rec, "recordings_index", key)
        .last()
        .is_some_and(|e| e["state"] == "idle" && e["pending_days"] == 0)
}

#[test]
fn days_fill_on_demand_events_are_coalesced_and_queries_answer_from_memory() {
    let sim = SimRuntime::new();
    // Each file search takes 400 ms, so filling four camera-days takes
    // longer than the one-second coalescing interval.
    let nvr = archive_mock(&sim, Some(Duration::from_millis(400)), None);
    let (core, rec) = start(config(&sim, None));
    assert_eq!(
        core.recordings_enable("d-x", Some("{}")),
        Err(RecError::NotConfigured)
    );
    assert_eq!(
        core.recordings_configure("[1]"),
        Err(RecError::InvalidArgument)
    );
    core.recordings_configure("{}").unwrap();
    let (_dev, key) = add(&core, &nvr);
    core.recordings_enable(&key, Some(r#"{"channels":[{"channel":0},{"channel":1}]}"#))
        .unwrap();
    // Nothing is searched until a view asks (no background fill).
    sim.advance(Duration::from_secs(20));
    assert_eq!(nvr.command_count(142) + nvr.command_count(14), 0);

    let (c0, c1) = (cam(&key, 0), cam(&key, 1));
    let want = json!({"cameras": [c0, c1], "days": [day(8) + 3600, day(9)]});
    core.recordings_want(&want.to_string()).unwrap();
    let mut log = Log::default();
    log.until(&sim, &core, &rec, "the fill", 30, |_| {
        nvr.command_count(14) >= 4 && idle_with_nothing_pending(&rec, &key)
    });

    // recordings_changed: fewer than one per camera-day, at least a second
    // apart while busy (the end of a fill is reported at once), together
    // covering both days, with growing revisions.
    let changed: Vec<&(Duration, Value)> = log
        .items
        .iter()
        .filter(|(_, e)| e["type"] == "recordings_changed" && e["device"] == key.as_str())
        .collect();
    assert!((2..4).contains(&changed.len()), "{changed:?}");
    for w in changed.windows(2) {
        let (t1, t2) = (w[0].0, w[1].0);
        let fill_ended = log
            .items
            .iter()
            .any(|(t, e)| *t == t2 && e["type"] == "recordings_index" && e["state"] != "busy");
        assert!(
            t2 - t1 >= Duration::from_millis(990) || fill_ended,
            "{t1:?} {t2:?}"
        );
        assert!(w[1].1["revision"].as_u64() >= w[0].1["revision"].as_u64());
    }
    let first = changed
        .iter()
        .map(|(_, e)| e["start"].as_i64().unwrap())
        .min();
    let last = changed
        .iter()
        .map(|(_, e)| e["end"].as_i64().unwrap())
        .max();
    assert_eq!(first, Some(day(8)));
    assert_eq!(last, Some(day(9) + 86_399));
    // recordings_index: busy, then idle; busy reports a second apart.
    let index: Vec<&(Duration, Value)> = log
        .items
        .iter()
        .filter(|(_, e)| e["type"] == "recordings_index" && e["device"] == key.as_str())
        .collect();
    assert!(index.iter().any(|(_, e)| e["state"] == "busy"));
    let busy: Vec<Duration> = index
        .iter()
        .filter(|(_, e)| e["state"] == "busy")
        .map(|(t, _)| *t)
        .collect();
    for w in busy.windows(2) {
        assert!(w[1] - w[0] >= Duration::from_millis(990), "{busy:?}");
    }
    assert!(!rec.events_of("incidents_changed").is_empty());

    // Queries answer from memory.
    let searches = nvr.command_count(14);
    let month = json!({"start": day(1), "end": t(31, 23, 59, 59)}).to_string();
    let cal = parse(core.recordings_calendar(&month));
    assert_eq!(cal["days"].as_array().unwrap().len(), 2);
    for c in cal["days"].as_array().unwrap() {
        assert_eq!(c["days"], json!([day(8), day(9), day(10)]), "{cal}");
    }
    let spans = parse(core.recordings_spans(
        &json!({"start": day(8), "end": day(8) + 86_399, "cameras": [c0]}).to_string(),
    ));
    let s0 = &spans["spans"][0];
    assert_eq!(s0["camera"], c0);
    assert_eq!(s0["spans"].as_array().unwrap().len(), 24, "{spans}");
    assert_eq!(s0["spans"][0]["tier"], "main");
    let two_days = json!({"start": day(8), "end": day(9) + 86_399});
    let ev = parse(core.recordings_events(&two_days.to_string()));
    let kinds: Vec<&str> = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["people", "vehicle", "motion"], "md is motion: {ev}");
    assert_eq!(ev["truncated"], false);
    assert_eq!(ev["events"][1]["camera"], c1);
    assert_eq!(ev["events"][1]["source"], "device");
    let motion = parse(core.recordings_events(
        &json!({"start": day(8), "end": day(9) + 86_399, "kinds": ["motion"]}).to_string(),
    ));
    assert_eq!(motion["events"].as_array().unwrap().len(), 1);
    let one = parse(core.recordings_events(
        &json!({"start": day(8), "end": day(9) + 86_399, "limit": 1}).to_string(),
    ));
    assert_eq!(one["truncated"], true);
    assert!(one["revision"].as_u64().unwrap() >= 4);
    let inc = parse(core.incidents_get(&two_days.to_string()));
    let list = inc["incidents"].as_array().unwrap();
    assert_eq!(list.len(), 2, "{inc}");
    assert_eq!(list[0]["kinds"], json!(["motion", "vehicle"]));
    assert_eq!(list[0]["cameras"].as_array().unwrap().len(), 2);
    assert_eq!(list[0]["growing"], false);
    assert_eq!(list[0]["start"], t(9, 7, 42, 5));
    assert_eq!(list[1]["kinds"], json!(["people"]));
    let key9 = list[0]["key"].as_str().unwrap();
    let one = parse(core.incident_get(key9));
    assert_eq!(one["incident"]["id"], list[0]["id"]);
    assert_eq!(core.incident_get("i-0-nope"), Err(RecError::UnknownId));
    assert_eq!(nvr.command_count(14), searches, "queries never search");

    // Days already indexed are not searched again unless forced.
    core.recordings_want(&want.to_string()).unwrap();
    sim.advance(Duration::from_secs(5));
    assert_eq!(nvr.command_count(14), searches);
    let forced = json!({"cameras": [c0], "days": [day(9)], "force": true});
    core.recordings_want(&forced.to_string()).unwrap();
    sim_until(&sim, &core, "the forced refresh", 10, || {
        nvr.command_count(14) == searches + 1
    });

    // Limits and refusals: nothing is queued.
    let many: Vec<i64> = (0..32).map(|n| day(1) + n * 86_400).collect();
    let too_many = json!({"cameras": [c0], "days": many});
    assert_eq!(
        core.recordings_want(&too_many.to_string()),
        Err(RecError::Limit)
    );
    let not_enabled = json!({"cameras": [cam(&key, 2)], "days": [day(9)]});
    assert_eq!(
        core.recordings_want(&not_enabled.to_string()),
        Err(RecError::InvalidArgument)
    );
    let other = json!({"cameras": [cam("d-other", 0)], "days": [day(9)]});
    assert_eq!(
        core.recordings_want(&other.to_string()),
        Err(RecError::InvalidArgument)
    );
    let status = parse(core.recordings_status());
    assert_eq!(status["devices"][0]["device"], key.as_str());
    sim.run_free(|| core.shutdown());
}

#[test]
fn today_stays_fresh_only_while_held() {
    let sim = SimRuntime::new();
    let nvr = archive_mock(&sim, None, None);
    let (core, _rec) = start(config(&sim, None));
    core.recordings_configure(r#"{"index":{"refresh_today_s":5}}"#)
        .unwrap();
    let (_dev, key) = add(&core, &nvr);
    core.recordings_enable(&key, Some(r#"{"channels":[{"channel":0}]}"#))
        .unwrap();
    let hold = json!({"cameras": [cam(&key, 0)]}).to_string();
    assert_eq!(
        core.recordings_hold_today(0, Some(&hold)),
        Err(RecError::InvalidArgument)
    );
    let bad = json!({"cameras": [cam(&key, 1)]}).to_string();
    assert_eq!(
        core.recordings_hold_today(7, Some(&bad)),
        Err(RecError::InvalidArgument)
    );
    core.recordings_hold_today(7, Some(&hold)).unwrap();
    sim_until(&sim, &core, "today searched", 20, || {
        nvr.command_count(14) >= 1
    });
    let held = nvr.command_count(14);
    sim.advance(Duration::from_secs(30));
    let refreshed = nvr.command_count(14) - held;
    assert!(refreshed >= 2, "searched again while held: {refreshed}");
    core.recordings_hold_today(7, None).unwrap();
    sim.advance(Duration::from_secs(1));
    let released = nvr.command_count(14);
    sim.advance(Duration::from_secs(60));
    assert_eq!(nvr.command_count(14), released, "not after the release");
    sim.run_free(|| core.shutdown());
}

#[test]
fn a_removed_device_parks_requests_and_a_refused_login_stops_everything() {
    let sim = SimRuntime::new();
    let nvr = archive_mock(&sim, None, None);
    let (core, rec) = start(config(&sim, None));
    core.recordings_configure("{}").unwrap();
    let (dev, key) = add(&core, &nvr);
    core.recordings_enable(
        &key,
        Some(r#"{"channels":[{"channel":0},{"channel":1}],"background":{"days_back":2,"pass_interval_s":10}}"#),
    )
    .unwrap();
    // The background block fills the last two days by itself.
    sim_until(&sim, &core, "background fill", 30, || {
        nvr.command_count(14) >= 4 && idle_with_nothing_pending(&rec, &key)
    });
    let ev = parse(
        core.recordings_events(&json!({"start": day(9), "end": day(10) + 86_399}).to_string()),
    );
    assert_eq!(ev["events"].as_array().unwrap().len(), 2, "{ev}");

    // Removed: the lane stops, requests wait for the device.
    sim.run_free(|| core.remove_device(dev).unwrap());
    sim_until(&sim, &core, "waiting_device", 5, || {
        of(&rec, "recordings_index", &key)
            .last()
            .is_some_and(|e| e["state"] == "waiting_device")
    });
    let searches = nvr.command_count(14);
    let want = json!({"cameras": [cam(&key, 0)], "days": [day(8)]}).to_string();
    core.recordings_want(&want).unwrap();
    let status = parse(core.recordings_status());
    assert_eq!(status["devices"][0]["state"], "waiting_device");
    assert_eq!(status["devices"][0]["pending_days"], 1);
    sim.advance(Duration::from_secs(30));
    assert_eq!(
        nvr.command_count(14),
        searches,
        "nothing without the device"
    );

    // Added again: the parked day is searched on the new session.
    let (dev, again) = add(&core, &nvr);
    assert_eq!(again, key);
    sim_until(&sim, &core, "parked day searched", 20, || {
        nvr.command_count(14) > searches && idle_with_nothing_pending(&rec, &key)
    });
    let day8 = parse(
        core.recordings_events(&json!({"start": day(8), "end": day(8) + 86_399}).to_string()),
    );
    assert_eq!(day8["events"].as_array().unwrap().len(), 1);

    // A refused login ends the lane: every request fails at once, nothing
    // is retried, and the background fill stops asking.
    sim.run_free(|| core.remove_device(dev).unwrap());
    let logins = nvr.command_count(1);
    let (_bad, same) = add_with(&core, &nvr, "wrong-password");
    assert_eq!(same, key);
    sim_until(&sim, &core, "ended", 10, || {
        of(&rec, "recordings_index", &key)
            .last()
            .is_some_and(|e| e["state"] == "ended")
    });
    let failed_before = of(&rec, "recordings_day_failed", &key).len();
    let want = json!({"cameras": [cam(&key, 0), cam(&key, 1)], "days": [day(7)], "force": true});
    core.recordings_want(&want.to_string()).unwrap();
    sim_until(&sim, &core, "failed days", 10, || {
        of(&rec, "recordings_day_failed", &key).len() == failed_before + 2
    });
    let f = of(&rec, "recordings_day_failed", &key).pop().unwrap();
    assert_eq!(f["day"], day(7));
    assert!(f["reason"].is_string());
    let thumb = json!({"camera": cam(&key, 0), "start": t(9, 7, 42, 5), "end": t(9, 7, 42, 25)});
    assert_eq!(
        core.thumbnail_get(&thumb.to_string()),
        Ok(ThumbnailAnswer::Pending)
    );
    sim_until(&sim, &core, "thumbnail failed", 10, || {
        rec.events_of("thumbnail")
            .iter()
            .any(|e| e["state"] == "failed")
    });
    sim.advance(Duration::from_secs(60));
    assert_eq!(
        of(&rec, "recordings_day_failed", &key).len(),
        failed_before + 2,
        "not retried, and no background requests"
    );
    assert!(nvr.command_count(1) <= logins + 1, "one refused login only");
    sim.run_free(|| core.shutdown());
}

#[test]
fn thumbnails_come_through_the_lane_into_the_cache() {
    let sim = SimRuntime::new();
    let nvr = archive_mock(&sim, None, None);
    let (core, rec) = start(config(&sim, None));
    core.recordings_configure(r#"{"thumbnails":{"max_bytes":1048576}}"#)
        .unwrap();
    let (_dev, key) = add(&core, &nvr);
    core.recordings_enable(&key, Some(r#"{"channels":[{"channel":0}]}"#))
        .unwrap();
    let k = json!({"camera": cam(&key, 0), "tier": "sub", "start": t(9, 7, 42, 5),
                   "end": t(9, 7, 42, 25)})
    .to_string();
    assert_eq!(core.thumbnail_get(&k), Ok(ThumbnailAnswer::Pending));
    sim_until(&sim, &core, "thumbnail ready", 10, || {
        rec.events_of("thumbnail")
            .iter()
            .any(|e| e["state"] == "ready")
    });
    let e = rec.events_of("thumbnail").pop().unwrap();
    assert_eq!(e["key"]["camera"], cam(&key, 0));
    assert_eq!(e["key"]["tier"], "sub");
    let ThumbnailAnswer::Ready(th) = core.thumbnail_get(&k).unwrap() else {
        panic!("not cached");
    };
    assert_eq!(th.kind, ThumbKind::H264Keyframe);
    assert!(!th.data.is_empty());
    let small = Thumbnail {
        kind: ThumbKind::Image,
        width: 160,
        height: 90,
        pts_us: th.pts_us,
        data: vec![0xff, 0xd8, 0xff, 0xd9],
    };
    core.thumbnail_put(&k, &small).unwrap();
    assert_eq!(core.thumbnail_get(&k), Ok(ThumbnailAnswer::Ready(small)));
    let huge = Thumbnail {
        kind: ThumbKind::Image,
        width: 1,
        height: 1,
        pts_us: 0,
        data: vec![0; 2 << 20],
    };
    assert_eq!(core.thumbnail_put(&k, &huge), Err(RecError::Limit));
    // No recording in the range: empty.
    let none =
        json!({"camera": cam(&key, 0), "start": t(11, 7, 0, 0), "end": t(11, 7, 0, 9)}).to_string();
    assert_eq!(core.thumbnail_get(&none), Ok(ThumbnailAnswer::Pending));
    sim_until(&sim, &core, "thumbnail empty", 10, || {
        rec.events_of("thumbnail")
            .iter()
            .any(|e| e["state"] == "empty")
    });
    // A device that is not enabled: failed.
    let other = json!({"camera": cam("d-other", 0), "start": 1, "end": 2}).to_string();
    assert_eq!(core.thumbnail_get(&other), Ok(ThumbnailAnswer::Pending));
    sim_until(&sim, &core, "thumbnail failed", 10, || {
        rec.events_of("thumbnail")
            .iter()
            .any(|e| e["state"] == "failed")
    });
    core.thumbnail_cancel(Some(&k)).unwrap();
    core.thumbnail_cancel(None).unwrap();
    assert_eq!(
        core.thumbnail_get(r#"{"camera":{"device":"x","channel":0},"start":5,"end":1}"#),
        Err(RecError::InvalidArgument)
    );
    sim.run_free(|| core.shutdown());
}

/// What makes a download event urgent: its and its cameras' states.
fn states(e: &Value) -> String {
    let mut s = format!("{}:{}", e["state"], e["paused"]);
    for c in e["cameras"].as_array().unwrap() {
        s.push_str(&format!("|{}:{}", c["state"], c["paused"]));
    }
    s
}

fn incident_keys(core: &Core) -> Vec<(String, Value)> {
    let v =
        parse(core.incidents_get(&json!({"start": day(8), "end": day(10) + 86_399}).to_string()));
    v["incidents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["key"].as_str().unwrap().to_owned(), i.clone()))
        .collect()
}

#[test]
fn downloads_are_deduplicated_coalesced_controlled_and_trashed() {
    let sim = SimRuntime::new();
    // Throttled so a download takes several seconds.
    let nvr = archive_mock(&sim, None, Some(200_000));
    let (core, rec) = start(config(&sim, None));
    let out = tempfile::tempdir().unwrap();
    core.recordings_configure(&json!({"downloads": {"chunk_s": 5}}).to_string())
        .unwrap();
    assert_eq!(
        core.download_incident(r#"{"incident":"i-1"}"#),
        Err(RecError::NotConfigured),
        "no downloads_dir"
    );
    core.recordings_configure(
        &json!({"downloads_dir": out.path(), "downloads": {"chunk_s": 5}}).to_string(),
    )
    .unwrap();
    let (dev, key) = add(&core, &nvr);
    core.recordings_enable(&key, Some(r#"{"channels":[{"channel":0},{"channel":1}]}"#))
        .unwrap();
    let want = json!({"cameras": [cam(&key, 0), cam(&key, 1)], "days": [day(8), day(9)]});
    core.recordings_want(&want.to_string()).unwrap();
    sim_until(&sim, &core, "the fill", 20, || {
        idle_with_nothing_pending(&rec, &key) && incident_keys(&core).len() == 2
    });
    let incidents = incident_keys(&core);
    let (key9, key8) = (incidents[0].0.clone(), incidents[1].0.clone());
    assert_eq!(
        core.download_incident(r#"{"incident":"i-0-nope"}"#),
        Err(RecError::UnknownId)
    );
    let r1 = parse(core.download_incident(&json!({"incident": key9}).to_string()));
    assert_eq!(r1["existing"], false);
    let id = r1["download"].as_str().unwrap().to_owned();
    let folder = PathBuf::from(r1["folder"].as_str().unwrap());
    assert!(folder.starts_with(out.path()));
    let r2 = parse(core.download_incident(&json!({"incident": key9}).to_string()));
    assert_eq!(
        (&r2["download"], &r2["existing"]),
        (&r1["download"], &json!(true))
    );

    // Control: unknown downloads and cameras, pause and resume.
    let c2 = cam(&key, 2).to_string();
    assert_eq!(
        core.download_pause("nope", None, true),
        Err(RecError::UnknownId)
    );
    assert_eq!(
        core.download_pause(&id, Some(&c2), true),
        Err(RecError::UnknownId)
    );
    assert_eq!(core.download_retry("nope", None), Err(RecError::UnknownId));
    core.download_retry(&id, None).unwrap();
    let c1 = cam(&key, 1).to_string();
    core.download_pause(&id, Some(&c1), true).unwrap();
    let mut log = Log::default();
    let mine = |e: &Value| e["type"] == "download" && e["download"] == id.as_str();
    log.until(&sim, &core, &rec, "camera paused", 10, |items| {
        items.iter().any(|(_, e)| {
            mine(e)
                && e["cameras"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c["camera"]["channel"] == 1 && c["paused"] == true)
        })
    });
    core.download_pause(&id, Some(&c1), false).unwrap();
    // The device goes away while downloading: its cameras wait.
    log.until(&sim, &core, &rec, "running", 10, |items| {
        items
            .iter()
            .any(|(_, e)| mine(e) && e["state"] == "running")
    });
    sim.run_free(|| core.remove_device(dev).unwrap());
    log.until(&sim, &core, &rec, "waiting_device", 10, |items| {
        items.iter().any(|(_, e)| {
            mine(e)
                && e["cameras"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| c["state"] == "waiting_device")
        })
    });
    let (_dev, _) = add(&core, &nvr);
    log.until(&sim, &core, &rec, "download done", 120, |items| {
        items.iter().any(|(_, e)| mine(e) && e["state"] == "done")
    });
    // Coalescing: progress at most every 250 ms; state changes at once.
    let events: Vec<&(Duration, Value)> = log.items.iter().filter(|(_, e)| mine(e)).collect();
    assert!(events.len() > 4, "{}", events.len());
    for w in events.windows(2) {
        if states(&w[0].1) == states(&w[1].1) {
            assert!(
                w[1].0 - w[0].0 >= Duration::from_millis(240),
                "{:?} {:?}",
                w[0],
                w[1]
            );
        }
    }
    let done = &events.last().unwrap().1;
    for c in done["cameras"].as_array().unwrap() {
        let output = PathBuf::from(c["output"].as_str().unwrap());
        assert!(output.is_file(), "{output:?}");
        assert_eq!(c["chunks_done"], c["chunks_total"]);
    }
    assert_eq!(done["incident"], key9.as_str());
    let listed = parse(core.downloads_get());
    assert_eq!(listed["downloads"][0]["download"], id.as_str());
    assert_eq!(listed["downloads"][0]["state"], "done");
    let inc = incident_keys(&core);
    assert_eq!(inc[0].1["downloads"], json!([id]));
    // Finished and unchanged: the same download.
    let r3 = parse(core.download_incident(&json!({"incident": key9}).to_string()));
    assert_eq!(
        (&r3["download"], &r3["existing"]),
        (&r1["download"], &json!(true))
    );

    // Trash: forgotten, reported once nothing writes, files untouched.
    let trash = parse(core.download_trash(&id));
    assert_eq!(trash["folder"], folder.to_string_lossy().as_ref());
    sim_until(&sim, &core, "download_removed", 10, || {
        !rec.events_of("download_removed").is_empty()
    });
    let removed = rec.events_of("download_removed").pop().unwrap();
    assert_eq!(
        removed,
        json!({"type": "download_removed", "download": id, "trash": true,
               "folder": folder.to_string_lossy()})
    );
    assert!(
        folder.join("incident.json").is_file(),
        "the core deletes nothing"
    );
    assert_eq!(core.download_trash(&id), Err(RecError::UnknownId));
    assert_eq!(parse(core.downloads_get())["downloads"], json!([]));

    // Forget with the partial files: pause first.
    let not_in = json!({"incident": key8, "cameras": [{"camera": cam(&key, 1)}]});
    assert_eq!(
        core.download_incident(&not_in.to_string()),
        Err(RecError::InvalidArgument)
    );
    let r8 = parse(core.download_incident(&json!({"incident": key8}).to_string()));
    let id8 = r8["download"].as_str().unwrap().to_owned();
    let folder8 = PathBuf::from(r8["folder"].as_str().unwrap());
    core.download_pause(&id8, None, true).unwrap();
    sim_until(&sim, &core, "forgotten", 10, || {
        core.download_forget(&id8, true).is_ok()
    });
    sim_until(&sim, &core, "removed", 10, || {
        rec.events_of("download_removed")
            .iter()
            .any(|e| e["download"] == id8.as_str() && e["trash"] == false)
    });
    assert!(!folder8.join(".parts").exists());
    assert_eq!(core.download_forget(&id8, false), Err(RecError::UnknownId));
    sim.run_free(|| core.shutdown());
}

#[test]
fn edits_groups_index_and_downloads_survive_a_restart() {
    let sim = SimRuntime::new();
    let nvr = archive_mock(&sim, None, None);
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("downloads");
    let settings = json!({"downloads_dir": downloads}).to_string();
    let (core, rec) = start(config(&sim, Some(dir.path())));
    core.recordings_configure(&settings).unwrap();
    let (_dev, key) = add(&core, &nvr);
    let enable = r#"{"channels":[{"channel":0},{"channel":1}]}"#;
    core.recordings_enable(&key, Some(enable)).unwrap();
    let want = json!({"cameras": [cam(&key, 0), cam(&key, 1)], "days": [day(8), day(9)]});
    core.recordings_want(&want.to_string()).unwrap();
    sim_until(&sim, &core, "the fill", 20, || {
        idle_with_nothing_pending(&rec, &key) && incident_keys(&core).len() == 2
    });

    // Camera groups: limits, then one group of both cameras (keys change).
    let too_many: Vec<Value> = (0..65)
        .map(|n| json!({"name": format!("Group {n}"), "cameras": []}))
        .collect();
    assert_eq!(
        core.camera_groups_put(&json!({"groups": too_many}).to_string()),
        Err(RecError::Limit)
    );
    let groups = json!({"groups": [{"name": "Group 1", "cameras": [cam(&key, 0), cam(&key, 1)]}],
                        "camera_names": [{"camera": cam(&key, 1), "name": "Side"}]});
    let before = rec.events_of("incidents_changed").len();
    core.camera_groups_put(&groups.to_string()).unwrap();
    sim_until(&sim, &core, "groups changed", 5, || {
        !rec.events_of("camera_groups_changed").is_empty()
            && rec.events_of("incidents_changed").len() > before
    });
    assert_eq!(rec.events_of("camera_groups_changed")[0]["revision"], 1);
    let incidents = incident_keys(&core);
    let (key9, inc9) = &incidents[0];
    assert_eq!(inc9["group"], "Group 1");
    let side = inc9["cameras"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["camera"]["channel"] == 1)
        .unwrap();
    assert_eq!(side["name"], "Side");

    // Edits: refusals, then a rename.
    assert_eq!(
        core.incident_edit(r#"{"op":"rename","key":"i-0-nope","name":"x"}"#),
        Err(RecError::UnknownId)
    );
    let keys: Vec<String> = (0..1001).map(|n| format!("i-{n}")).collect();
    assert_eq!(
        core.incident_edit(&json!({"op": "merge", "keys": keys}).to_string()),
        Err(RecError::Limit)
    );
    assert_eq!(
        core.incident_edit(r#"{"op":"fly"}"#),
        Err(RecError::InvalidArgument)
    );
    let r = parse(
        core.incident_edit(&json!({"op": "rename", "key": key9, "name": "Delivery"}).to_string()),
    );
    assert_eq!(r["keys"], json!([key9]));
    assert_eq!(incident_keys(&core)[0].1["name"], "Delivery");
    assert_eq!(incident_keys(&core)[0].1["edited"], true);

    // A paused download stays unfinished across the restart.
    let r8 = parse(core.download_incident(&json!({"incident": incidents[1].0}).to_string()));
    let id8 = r8["download"].as_str().unwrap().to_owned();
    core.download_pause(&id8, None, true).unwrap();
    sim.run_free(|| core.shutdown());
    drop(core);

    let (core, rec) = start(config(&sim, Some(dir.path())));
    core.recordings_configure(&settings).unwrap();
    // Enabled before the device is added: the index answers from disk.
    core.recordings_enable(&key, Some(enable)).unwrap();
    let ev = parse(
        core.recordings_events(&json!({"start": day(8), "end": day(9) + 86_399}).to_string()),
    );
    assert_eq!(ev["events"].as_array().unwrap().len(), 3, "{ev}");
    let incidents = incident_keys(&core);
    assert_eq!(incidents[0].0, *key9);
    assert_eq!(incidents[0].1["name"], "Delivery");
    let g = parse(core.camera_groups_get());
    assert_eq!(g["revision"], 1);
    assert_eq!(g["groups"][0]["name"], "Group 1");
    sim_until(&sim, &core, "download resumed", 10, || {
        parse(core.downloads_get())["downloads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["download"] == id8.as_str() && d["state"] == "paused")
    });
    // A want before the device is added waits for it.
    let want10 = json!({"cameras": [cam(&key, 0)], "days": [day(10)]});
    core.recordings_want(&want10.to_string()).unwrap();
    assert_eq!(
        parse(core.recordings_status())["devices"][0]["pending_days"],
        1
    );
    let (_dev, again) = add(&core, &nvr);
    assert_eq!(again, key);
    core.download_pause(&id8, None, false).unwrap();
    sim_until(&sim, &core, "resumed download done", 60, || {
        rec.events_of("download")
            .iter()
            .any(|e| e["download"] == id8.as_str() && e["state"] == "done")
    });
    sim_until(&sim, &core, "parked day", 20, || {
        idle_with_nothing_pending(&rec, &key)
    });
    sim.run_free(|| core.shutdown());
}

#[test]
fn caps_refuse_whole_requests_and_store_nothing() {
    let sim = SimRuntime::new();
    let (core, _rec) = start(config(&sim, None));
    core.recordings_configure("{}").unwrap();
    // A device that is not added: requests wait, within the queue's cap.
    let channels: Vec<Value> = (0..140).map(|c| json!({"channel": c})).collect();
    core.recordings_enable(
        "d-offline",
        Some(&json!({"channels": channels}).to_string()),
    )
    .unwrap();
    let cams: Vec<Value> = (0..140).map(|c| cam("d-offline", c)).collect();
    let want = |n: i64| {
        let days: Vec<i64> = (0..n).map(|k| day(1) + k * 86_400).collect();
        json!({"cameras": cams, "days": days}).to_string()
    };
    assert_eq!(core.recordings_want(&want(31)), Err(RecError::Limit));
    let pending = || parse(core.recordings_status())["devices"][0]["pending_days"].clone();
    assert_eq!(pending(), 0, "nothing queued");
    core.recordings_want(&want(29)).unwrap();
    assert_eq!(pending(), 29 * 140);
    assert_eq!(core.recordings_want(&want(1)), Err(RecError::Limit));
    assert_eq!(pending(), 29 * 140);

    // Camera groups and names.
    let big: Vec<Value> = (0..65).map(|c| cam("d-offline", c)).collect();
    let g = json!({"groups": [{"name": "All", "cameras": big}]});
    assert_eq!(core.camera_groups_put(&g.to_string()), Err(RecError::Limit));
    let g = json!({"groups": [{"name": "x".repeat(257), "cameras": []}]});
    assert_eq!(core.camera_groups_put(&g.to_string()), Err(RecError::Limit));
    let g = json!({"groups": [{"name": "A", "cameras": []}, {"name": "A", "cameras": []}]});
    assert_eq!(
        core.camera_groups_put(&g.to_string()),
        Err(RecError::InvalidArgument)
    );
    assert_eq!(parse(core.camera_groups_get())["revision"], 0);
    // Disabled: its requests are gone.
    core.recordings_enable("d-offline", None).unwrap();
    assert_eq!(parse(core.recordings_status())["devices"], json!([]));
    assert_eq!(
        core.recordings_want(&want(1)),
        Err(RecError::InvalidArgument)
    );
    sim.run_free(|| core.shutdown());
}

#[test]
fn months_alone_fill_the_calendar_and_cameras_carry_reported_names() {
    let sim = SimRuntime::new();
    let nvr = archive_mock(&sim, None, None);
    let (core, rec) = start(config(&sim, None));
    core.recordings_configure("{}").unwrap();
    let (_dev, key) = add(&core, &nvr);
    core.recordings_enable(&key, Some(r#"{"channels":[{"channel":0},{"channel":1}]}"#))
        .unwrap();
    let (c0, c1) = (cam(&key, 0), cam(&key, 1));
    let september = DateTime::new(2026, 9, 14, 0, 0, 0).unwrap().to_naive_secs();
    let september_first = DateTime::new(2026, 9, 1, 0, 0, 0).unwrap().to_naive_secs();

    // A calendar paged to two months: their calendars only (one 142 each
    // for both cameras), no day searched; recordings_changed covers them.
    let months = json!({"cameras": [c0, c1], "months": [september, day(20)]});
    core.recordings_want(&months.to_string()).unwrap();
    sim_until(&sim, &core, "the months", 20, || {
        nvr.command_count(142) == 2 && idle_with_nothing_pending(&rec, &key)
    });
    assert_eq!(nvr.command_count(14), 0);
    let october = json!({"start": day(1), "end": t(31, 23, 59, 59)}).to_string();
    let cal = parse(core.recordings_calendar(&october));
    assert_eq!(cal["days"].as_array().unwrap().len(), 2, "{cal}");
    for c in cal["days"].as_array().unwrap() {
        assert_eq!(c["days"], json!([day(8), day(9), day(10)]), "{cal}");
    }
    sim_until(&sim, &core, "the change event", 5, || {
        let changed = of(&rec, "recordings_changed", &key);
        let start = changed.iter().filter_map(|e| e["start"].as_i64()).min();
        let end = changed.iter().filter_map(|e| e["end"].as_i64()).max();
        start <= Some(september_first) && end >= Some(day(10))
    });
    // A past month fetched after it ended is final; force asks again.
    core.recordings_want(&months.to_string()).unwrap();
    sim.advance(Duration::from_secs(5));
    assert_eq!(nvr.command_count(142), 2);
    let forced = json!({"cameras": [c0], "months": [september], "force": true});
    core.recordings_want(&forced.to_string()).unwrap();
    sim_until(&sim, &core, "the forced month", 10, || {
        nvr.command_count(142) == 3
    });
    // Limits: more than a year of months; neither days nor months.
    let many: Vec<i64> = (0..13).map(|n| day(1) - n * 32 * 86_400).collect();
    assert_eq!(
        core.recordings_want(&json!({"cameras": [c0], "months": many}).to_string()),
        Err(RecError::Limit)
    );
    assert_eq!(
        core.recordings_want(&json!({"cameras": [c0]}).to_string()),
        Err(RecError::InvalidArgument)
    );

    // Camera names: the names the device reports, once known; the user's
    // override wins.
    sim_until(&sim, &core, "channel names", 10, || {
        rec.events_of("channels")
            .iter()
            .any(|e| e["channels"][0]["name"].is_string())
    });
    core.recordings_want(&json!({"cameras": [c0, c1], "days": [day(9)]}).to_string())
        .unwrap();
    sim_until(&sim, &core, "the day", 20, || {
        idle_with_nothing_pending(&rec, &key) && !incident_keys(&core).is_empty()
    });
    let names = |core: &Core| -> Vec<(i64, String)> {
        let (_, inc) = incident_keys(core).remove(0);
        inc["cameras"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                (
                    c["camera"]["channel"].as_i64().unwrap(),
                    c["name"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };
    assert_eq!(
        names(&core),
        vec![(0, "Channel 1".to_owned()), (1, "Channel 2".to_owned())]
    );
    let groups = json!({"groups": [], "camera_names": [{"camera": c1, "name": "Side"}]});
    core.camera_groups_put(&groups.to_string()).unwrap();
    assert_eq!(
        names(&core),
        vec![(0, "Channel 1".to_owned()), (1, "Side".to_owned())]
    );
    sim.run_free(|| core.shutdown());
}
