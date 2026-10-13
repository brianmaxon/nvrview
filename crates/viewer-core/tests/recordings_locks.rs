//! Locked incidents in the recordings owner (docs/FFI-RECORDINGS.md,
//! section 3, "Locked incidents") against in-process mock NVRs on a
//! simulated clock: locking queues the download (a growing incident's once
//! it stops growing), a lock follows a camera group change with its edits,
//! survives a restart, keeps its edits and index days past the age limit,
//! and keeps its day when the index evicts by size.

mod common;

use bc_protocol::archive::DateTime;
use common::*;
use mock_nvr::archive::{ArchiveConfig, MockEvent};
use serde_json::{Value, json};
use simtime::SimRuntime;
use std::path::{Path, PathBuf};
use viewer_core::{Core, CoreConfig, RecError};

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn t(day: u8, h: u8, m: u8, s: u8) -> i64 {
    d(day, h, m, s).to_naive_secs()
}

fn day(n: u8) -> i64 {
    t(n, 0, 0, 0)
}

const DAY: i64 = 86_400;

/// A device with two channels and recordings on 8, 9, and 10 October
/// (today is the 10th): a person on channel 0 on the 8th, a vehicle on
/// channel 1 on the 9th, and motion on channel 1 that runs from just before
/// noon on the 10th (when the clock starts) for a minute.
fn archive_mock(sim: &SimRuntime, file_secs: u32) -> mock_nvr::MockNvr {
    archive_mock_with(sim, file_secs, None)
}

/// [`archive_mock`] whose file searches each take `search_delay`.
fn archive_mock_with(
    sim: &SimRuntime,
    file_secs: u32,
    search_delay: Option<std::time::Duration>,
) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 2;
    let mut a = ArchiveConfig::new(vec![d(8, 0, 0, 0), d(9, 0, 0, 0), d(10, 0, 0, 0)]);
    a.events_per_day = 0;
    a.file_secs = file_secs;
    a.search_delay = search_delay;
    let ev = |channel: u8, kind: &str, start: DateTime, end: DateTime| MockEvent {
        channel,
        kind: kind.into(),
        start,
        end,
    };
    a.events = vec![
        ev(0, "people", d(8, 20, 0, 0), d(8, 20, 0, 15)),
        ev(1, "vehicle", d(9, 7, 42, 5), d(9, 7, 42, 25)),
        ev(1, "md", d(10, 11, 59, 50), d(10, 12, 1, 0)),
    ];
    cfg.archive = Some(a);
    sim_spawn(sim, cfg)
}

/// Noon on the 10th plus `later` seconds, UTC (offset 0).
fn config(sim: &SimRuntime, data_dir: Option<&Path>, later: i64) -> CoreConfig {
    on_sim(
        sim,
        CoreConfig {
            data_dir: data_dir.map(Path::to_path_buf),
            test_wall_clock_start_us: Some((t(10, 12, 0, 0) + later) * 1_000_000),
            ..fast_config()
        },
    )
}

fn add(core: &Core, nvr: &mock_nvr::MockNvr) -> String {
    let addr = nvr.addr();
    let id = core
        .add_device(&addr.ip().to_string(), addr.port(), USER, PASSWORD)
        .unwrap();
    core.device_key(id).unwrap()
}

fn cam(key: &str, ch: u8) -> Value {
    json!({"device": key, "channel": ch, "lens": 0})
}

fn parse(s: Result<String, RecError>) -> Value {
    serde_json::from_str(&s.unwrap()).unwrap()
}

const ENABLE: &str = r#"{"channels":[{"channel":0},{"channel":1}]}"#;

/// Incidents from the 1st to the 10th, newest first.
fn incidents(core: &Core) -> Vec<Value> {
    let q = json!({"start": day(1), "end": day(10) + DAY - 1});
    parse(core.incidents_get(&q.to_string()))["incidents"]
        .as_array()
        .unwrap()
        .clone()
}

fn by_start(core: &Core, start: i64) -> Option<Value> {
    incidents(core).into_iter().find(|i| i["start"] == start)
}

fn key_at(core: &Core, start: i64) -> String {
    by_start(core, start).unwrap()["key"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Asks for `days` of both cameras and waits until `done`.
fn fill(
    sim: &SimRuntime,
    core: &Core,
    key: &str,
    days: &[i64],
    force: bool,
    done: impl FnMut() -> bool,
) {
    let want = json!({"cameras": [cam(key, 0), cam(key, 1)], "days": days, "force": force});
    core.recordings_want(&want.to_string()).unwrap();
    sim_until(sim, core, "the fill", 60, done);
}

/// The index's day files of the one device under `data_dir`.
fn day_files(data_dir: &Path) -> Vec<String> {
    let index = data_dir.join("recordings").join("index");
    let mut out = Vec::new();
    for dev in std::fs::read_dir(index).unwrap() {
        for f in std::fs::read_dir(dev.unwrap().path().join("days")).unwrap() {
            out.push(f.unwrap().file_name().to_string_lossy().into_owned());
        }
    }
    out.sort();
    out
}

#[test]
fn locking_queues_the_download_and_a_growing_incident_waits_until_it_settles() {
    let sim = SimRuntime::new();
    let nvr = archive_mock(&sim, 3600);
    let (core, rec) = start(config(&sim, None, 0));
    let out = tempfile::tempdir().unwrap();
    assert_eq!(
        core.incident_lock("i-1", true),
        Err(RecError::NotConfigured)
    );
    core.recordings_configure(&json!({"downloads_dir": out.path()}).to_string())
        .unwrap();
    let key = add(&core, &nvr);
    core.recordings_enable(&key, Some(ENABLE)).unwrap();
    fill(&sim, &core, &key, &[day(9), day(10)], false, || {
        incidents(&core).len() == 2
    });
    let (k9, k10) = (
        key_at(&core, t(9, 7, 42, 5)),
        key_at(&core, t(10, 11, 59, 50)),
    );
    assert_eq!(
        core.incident_lock("i-0-nope", true),
        Err(RecError::UnknownId)
    );
    assert!(incidents(&core).iter().all(|i| i["locked"] == false));

    // A settled incident: locked, its download queued.
    let changed = rec.events_of("incidents_changed").len();
    let r = parse(core.incident_lock(&k9, true));
    assert_eq!(r["keys"], json!([k9]));
    assert_eq!(r["download_status"], "queued");
    let id = r["download"].as_str().unwrap().to_owned();
    assert!(PathBuf::from(r["folder"].as_str().unwrap()).starts_with(out.path()));
    assert_eq!(by_start(&core, t(9, 7, 42, 5)).unwrap()["locked"], true);
    sim_until(&sim, &core, "incidents_changed", 5, || {
        rec.events_of("incidents_changed").len() > changed
    });
    assert_eq!(
        rec.events_of("incidents_changed").last().unwrap()["revision"],
        r["revision"]
    );
    // Again: nothing changes, and the download is the same one.
    let again = parse(core.incident_lock(&k9, true));
    assert_eq!(again["keys"], json!([]));
    assert_eq!(
        (&again["download"], &again["download_status"]),
        (&json!(id), &json!("existing"))
    );

    // A growing incident: locked now, downloaded once it stops growing.
    let r = parse(core.incident_lock(&k10, true));
    assert_eq!(r["keys"], json!([k10]));
    assert_eq!(r["download_status"], "when_settled");
    assert_eq!((&r["download"], &r["folder"]), (&Value::Null, &Value::Null));
    let queued_for = |k: &str| {
        rec.events_of("download")
            .iter()
            .any(|e| e["incident"] == k && e["state"].is_string())
    };
    assert!(!queued_for(&k10));
    // It ends at 12:01:00; with a 10 s gap it settles at 12:01:10 (the
    // clock started at 12:00:00), and only then is it queued: the queue is
    // watched every step, and when the download shows up the incident is
    // no longer growing. (Reading the incidents every step instead cost
    // most of this test's time.)
    sim_until(&sim, &core, "queued once settled", 110, || queued_for(&k10));
    assert!(
        sim.elapsed() >= std::time::Duration::from_secs(70),
        "queued at {:?}, before it settled",
        sim.elapsed()
    );
    assert_eq!(
        by_start(&core, t(10, 11, 59, 50)).unwrap()["growing"],
        false,
        "queued while growing"
    );
    assert_eq!(
        by_start(&core, t(10, 11, 59, 50)).unwrap()["downloads"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Unlocking leaves the downloads alone.
    let r = parse(core.incident_lock(&k9, false));
    assert_eq!(r["keys"], json!([k9]));
    assert!(r.get("download_status").is_none());
    assert_eq!(by_start(&core, t(9, 7, 42, 5)).unwrap()["locked"], false);
    assert_eq!(parse(core.incident_lock(&k9, false))["keys"], json!([]));
    assert!(
        parse(core.downloads_get())["downloads"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["download"] == id.as_str())
    );
    sim.run_free(|| core.shutdown());
}

#[test]
fn a_lock_survives_group_changes_and_restarts_and_keeps_its_edits_and_days_past_the_age_limit() {
    let sim = SimRuntime::new();
    let nvr = archive_mock(&sim, 3600);
    let dir = tempfile::tempdir().unwrap();
    let (core, _rec) = start(config(&sim, Some(dir.path()), 0));
    core.recordings_configure("{}").unwrap();
    let key = add(&core, &nvr);
    core.recordings_enable(&key, Some(ENABLE)).unwrap();
    fill(&sim, &core, &key, &[day(8), day(9)], false, || {
        incidents(&core).len() == 2
    });
    let (k8, k9) = (key_at(&core, t(8, 20, 0, 0)), key_at(&core, t(9, 7, 42, 5)));
    for (k, name) in [(&k8, "Parcel"), (&k9, "Delivery")] {
        core.incident_edit(&json!({"op": "rename", "key": k, "name": name}).to_string())
            .unwrap();
    }
    // Without downloads_dir the lock is stored and says so.
    let r = parse(core.incident_lock(&k9, true));
    assert_eq!(r["download_status"], "not_configured");

    // A camera group changes both keys: the locked incident keeps its lock
    // and its name; the unlocked one loses its name (its edit no longer
    // matches).
    let groups = json!({"groups": [{"name": "Group 1", "cameras": [cam(&key, 0), cam(&key, 1)]}]});
    core.camera_groups_put(&groups.to_string()).unwrap();
    let i9 = by_start(&core, t(9, 7, 42, 5)).unwrap();
    let i8 = by_start(&core, t(8, 20, 0, 0)).unwrap();
    let k9g = i9["key"].as_str().unwrap().to_owned();
    assert_ne!(k9g, k9);
    assert_eq!(
        (&i9["locked"], &i9["name"]),
        (&json!(true), &json!("Delivery"))
    );
    assert_eq!((&i8["locked"], &i8["name"]), (&json!(false), &Value::Null));
    sim.run_free(|| core.shutdown());
    drop(core);

    // 100 days later: the lock is still there, before the device is added.
    let (core, _rec) = start(config(&sim, Some(dir.path()), 100 * DAY));
    core.recordings_configure("{}").unwrap();
    core.recordings_enable(&key, Some(ENABLE)).unwrap();
    let i9 = by_start(&core, t(9, 7, 42, 5)).unwrap();
    assert_eq!(i9["key"], k9g.as_str());
    assert_eq!(
        (&i9["locked"], &i9["name"]),
        (&json!(true), &json!("Delivery"))
    );
    // Searching today prunes the index past the 90-day limit: the unlocked
    // day goes, the locked one stays, and so do its incident and edits.
    let key2 = add(&core, &nvr);
    assert_eq!(key2, key);
    fill(&sim, &core, &key, &[day(10) + 100 * DAY], false, || {
        by_start(&core, t(8, 20, 0, 0)).is_none()
    });
    assert_eq!(day_files(dir.path()), vec!["2026-10-09.json"]);
    let i9 = by_start(&core, t(9, 7, 42, 5)).unwrap();
    assert_eq!(
        (&i9["locked"], &i9["name"]),
        (&json!(true), &json!("Delivery"))
    );
    assert_eq!(i9["edited"], true);
    let edits: Value = serde_json::from_slice(
        &std::fs::read(dir.path().join("recordings").join("incident-edits.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(edits["edits"].as_array().unwrap().len(), 1, "{edits}");
    assert_eq!(edits["edits"][0]["key"], k9g.as_str());
    // Unlocked, the age limit applies again at the next prune.
    core.incident_lock(&k9g, false).unwrap();
    fill(&sim, &core, &key, &[day(10) + 100 * DAY], true, || {
        by_start(&core, t(9, 7, 42, 5)).is_none()
    });
    assert_eq!(day_files(dir.path()), vec!["2027-01-18.json"], "today only");
    sim.run_free(|| core.shutdown());
}

#[test]
fn a_locked_day_is_evicted_last_when_the_index_is_over_its_size() {
    let sim = SimRuntime::new();
    // One-minute files make each camera-day's file list a few hundred KB.
    let nvr = archive_mock(&sim, 60);
    let dir = tempfile::tempdir().unwrap();
    let (core, _rec) = start(config(&sim, Some(dir.path()), 0));
    core.recordings_configure(r#"{"index": {"max_bytes": 1048576}}"#)
        .unwrap();
    let key = add(&core, &nvr);
    core.recordings_enable(&key, Some(ENABLE)).unwrap();
    fill(&sim, &core, &key, &[day(8)], false, || {
        by_start(&core, t(8, 20, 0, 0)).is_some()
    });
    let k8 = key_at(&core, t(8, 20, 0, 0));
    core.incident_lock(&k8, true).unwrap();
    // The 9th makes the index too large. Oldest first would drop the 8th;
    // it is locked, so the 9th goes.
    fill(&sim, &core, &key, &[day(9)], false, || {
        day_files(dir.path()) == vec!["2026-10-08.json"]
            && parse(core.recordings_status())["devices"][0]["pending_days"] == 0
    });
    let size = |name: &str| {
        let index = dir.path().join("recordings").join("index");
        let dev = std::fs::read_dir(index).unwrap().next().unwrap().unwrap();
        std::fs::metadata(dev.path().join("days").join(name))
            .unwrap()
            .len()
    };
    assert!(size("2026-10-08.json") * 2 > 1_048_576, "the 9th was over");
    assert_eq!(by_start(&core, t(8, 20, 0, 0)).unwrap()["locked"], true);
    sim.run_free(|| core.shutdown());
}

/// Incidents are derived from the index as it is when they are asked for:
/// once the index returns an event, the incidents group it too, even while
/// the owner holds back `recordings_changed` (at most one a second while
/// filling) for the camera-day that brought it.
#[test]
fn incidents_include_every_event_the_index_returns() {
    let sim = SimRuntime::new();
    // Searches 200 ms apart: each camera-day is indexed well within a
    // second of the one before.
    let nvr = archive_mock_with(&sim, 3600, Some(std::time::Duration::from_millis(200)));
    let (core, _rec) = start(config(&sim, None, 0));
    core.recordings_configure("{}").unwrap();
    let key = add(&core, &nvr);
    core.recordings_enable(&key, Some(ENABLE)).unwrap();
    let q = json!({"start": day(8), "end": day(9) + DAY - 1}).to_string();
    // (events the index returns, events in incidents) at every step of the
    // fill, until both are in or the incidents lag. The lanes write the
    // index on their own threads, so an event can land between the two
    // queries: asked in this order, the incidents may be ahead of the
    // events but never behind them.
    let mut last = (0, 0);
    fill(&sim, &core, &key, &[day(8), day(9)], false, || {
        let events = parse(core.recordings_events(&q))["events"]
            .as_array()
            .unwrap()
            .len();
        let grouped: usize = incidents(&core)
            .iter()
            .filter(|i| i["start"].as_i64().unwrap() < day(10))
            .map(|i| i["events"].as_array().unwrap().len())
            .sum();
        last = (events, grouped);
        grouped < events || events == 2
    });
    sim.run_free(|| core.shutdown());
    assert_eq!(last, (2, 2), "the incidents lag the index");
}
