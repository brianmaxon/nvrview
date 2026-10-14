//! Sound analysis of a recorded day from downloaded files
//! (docs/FFI-RECORDINGS.md, section 2a): the core reads the audio of a
//! finished download, decodes it, and runs the same loudness meter and
//! alarm detector as live listening. The mock's archive carries a synthetic
//! smoke alarm (fixtures/synthetic/aac-smoke-t3.aac) as the recorded audio.
//! Everything runs on the simulated clock.

use crate::common::{PASSWORD, Recorder, USER, fast_config, on_sim, sim_spawn, sim_until, start};
use bc_protocol::archive::DateTime;
use mock_nvr::archive::{ArchiveConfig, MockEvent};
use serde_json::{Value, json};
use simtime::SimRuntime;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use viewer_core::{Core, CoreConfig, RecError};

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn day(n: u8) -> i64 {
    d(n, 0, 0, 0).to_naive_secs()
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name)
}

/// Recordings on the 8th, 9th, and 10th, with audio, and one motion event
/// on the 9th on channel 0 that a download can be made for.
fn archive_mock(sim: &SimRuntime) -> mock_nvr::MockNvr {
    let video = mock_nvr::fixture::Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let mut cfg = mock_nvr::Config::new(video);
    cfg.channels = 2;
    let mut a = ArchiveConfig::new(vec![d(8, 0, 0, 0), d(9, 0, 0, 0), d(10, 0, 0, 0)]);
    a.events_per_day = 0;
    a.events = vec![MockEvent {
        channel: 0,
        kind: "md".into(),
        start: d(9, 7, 42, 5),
        end: d(9, 7, 42, 25),
    }];
    a.audio = Some(Arc::new(
        mock_nvr::audio::AudioFixture::load(&fixture_path("aac-smoke-t3.aac")).unwrap(),
    ));
    cfg.archive = Some(a);
    sim_spawn(sim, cfg)
}

fn config(sim: &SimRuntime, data_dir: Option<&Path>) -> CoreConfig {
    on_sim(
        sim,
        CoreConfig {
            data_dir: data_dir.map(Path::to_path_buf),
            test_wall_clock_start_us: Some(d(10, 12, 0, 0).to_naive_secs() * 1_000_000),
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

fn analyze_events(rec: &Recorder, request: &Value) -> Vec<Value> {
    rec.events_of("sound_analyze")
        .into_iter()
        .filter(|e| e["request"] == *request)
        .collect()
}

fn final_state(rec: &Recorder, request: &Value) -> Option<Value> {
    analyze_events(rec, request)
        .into_iter()
        .find(|e| matches!(e["state"].as_str(), Some("done" | "failed" | "canceled")))
}

fn ask(core: &Core, key: &str, ch: u8, d: i64) -> Value {
    parse(core.sound_analyze_day(&json!({"camera": cam(key, ch), "day": d + 5}).to_string()))
}

/// A device, its index filled for the three days, and one finished
/// download (channel 0 on the 9th). Returns the key.
fn downloaded(sim: &SimRuntime, core: &Core, rec: &Recorder, nvr: &mock_nvr::MockNvr) -> String {
    let addr = nvr.addr();
    let id = core
        .add_device(&addr.ip().to_string(), addr.port(), USER, PASSWORD)
        .unwrap();
    let key = core.device_key(id).unwrap();
    core.recordings_enable(&key, Some(r#"{"channels":[{"channel":0},{"channel":1}]}"#))
        .unwrap();
    let want = json!({"cameras": [cam(&key, 0), cam(&key, 1)], "days": [day(8), day(9), day(10)]});
    core.recordings_want(&want.to_string()).unwrap();
    sim_until(sim, core, "the index", 30, || {
        rec.events_of("recordings_index")
            .iter()
            .rev()
            .find(|e| e["device"] == key.as_str())
            .is_some_and(|e| e["state"] == "idle" && e["pending_days"] == 0)
    });
    let q = json!({"start": day(8), "end": day(10) + 86_399});
    let incidents = parse(core.incidents_get(&q.to_string()));
    let inc = incidents["incidents"][0]["key"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = parse(core.download_incident(&json!({"incident": inc}).to_string()));
    let id = r["download"].clone();
    sim_until(sim, core, "the download", 120, || {
        rec.events_of("download")
            .iter()
            .any(|e| e["download"] == id && e["state"] == "done")
    });
    key
}

/// A downloaded day is analyzed from its file: the alarm label and the
/// loudness appear at the device-local times of the recording; a day with
/// no download says so; cancel works; asking again does not double labels.
#[test]
fn a_downloaded_day_is_analyzed_from_its_file() {
    let sim = SimRuntime::new();
    let nvr = archive_mock(&sim);
    let out = tempfile::tempdir().unwrap();
    let (core, rec) = start(config(&sim, None));
    core.recordings_configure(
        &json!({"downloads_dir": out.path(), "downloads": {"chunk_s": 5}}).to_string(),
    )
    .unwrap();
    let key = downloaded(&sim, &core, &rec, &nvr);

    // Nothing is read until asked.
    let q = json!({"start": day(9), "end": day(9) + 86_399, "kinds": ["smoke_alarm"]});
    let labels = |core: &Core| {
        parse(core.recordings_events(&q.to_string()))["events"]
            .as_array()
            .unwrap()
            .clone()
    };
    assert!(labels(&core).is_empty());

    let r = ask(&core, &key, 0, day(9));
    let id = r["request"].clone();
    sim_until(&sim, &core, "the analysis", 60, || {
        final_state(&rec, &id).is_some()
    });
    let end = final_state(&rec, &id).unwrap();
    assert_eq!(end["state"], "done", "{end}");
    assert!(end["seconds"].as_u64().unwrap() >= 20, "{end}");
    assert!(end["labels"].as_u64().unwrap() >= 1, "{end}");
    let states: Vec<_> = analyze_events(&rec, &id)
        .iter()
        .map(|e| e["state"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(states, ["queued", "running", "done"]);

    // The label is at the recording's time of day, from local audio.
    let found = labels(&core);
    assert!(!found.is_empty());
    let from = day(9) + 7 * 3600 + 40 * 60;
    for e in &found {
        assert_eq!(e["source"], "local_audio");
        assert_eq!(e["analyzer"], "pattern-v1");
        let s = e["start"].as_i64().unwrap();
        assert!((from..from + 180).contains(&s), "{e}");
    }
    // The loudness of the tone, about -10 dBFS, on the same seconds.
    let l = parse(
        core.recordings_loudness(
            &json!({"start": from, "end": from + 179, "bucket_s": 1, "cameras": [cam(&key, 0)]})
                .to_string(),
        ),
    );
    let peaks: Vec<Option<i64>> = l["lanes"][0]["peak_db"]
        .as_array()
        .unwrap()
        .iter()
        .map(Value::as_i64)
        .collect();
    assert!(
        peaks.iter().flatten().any(|p| (-13..=-7).contains(p)),
        "{peaks:?}"
    );
    // Other days are untouched.
    let other = parse(core.recordings_loudness(
        &json!({"start": day(8), "end": day(8) + 86_399, "bucket_s": 3600, "cameras": [cam(&key, 0)]})
            .to_string(),
    ));
    assert!(
        other["lanes"][0]["peak_db"]
            .as_array()
            .unwrap()
            .iter()
            .all(Value::is_null)
    );

    // Again: the same labels, not doubled.
    let n = found.len();
    let again = ask(&core, &key, 0, day(9));
    assert_ne!(again["request"], id);
    sim_until(&sim, &core, "the second analysis", 60, || {
        final_state(&rec, &again["request"]).is_some()
    });
    assert_eq!(labels(&core).len(), n);

    // A camera or day with nothing downloaded has no audio. (Canceling a
    // queued or running request is covered by recorded_days_are_analyzed_
    // only_on_request and the unit tests of the analysis: the work is too
    // quick here to cancel at a chosen moment.)
    let none = ask(&core, &key, 1, day(9));
    let first = ask(&core, &key, 0, day(8));
    sim_until(&sim, &core, "the requests", 60, || {
        final_state(&rec, &none["request"]).is_some()
            && final_state(&rec, &first["request"]).is_some()
    });
    for (r, want) in [(&none, "no_audio"), (&first, "no_audio")] {
        let f = final_state(&rec, &r["request"]).unwrap();
        assert_eq!(
            (&f["state"], &f["reason"]),
            (&json!("failed"), &json!(want))
        );
    }
    sim.run_free(|| core.shutdown());
}

/// Without downloads set up there is no source at all.
#[test]
fn without_downloads_there_is_no_source() {
    let sim = SimRuntime::new();
    let nvr = archive_mock(&sim);
    let (core, rec) = start(config(&sim, None));
    let addr = nvr.addr();
    let id = core
        .add_device(&addr.ip().to_string(), addr.port(), USER, PASSWORD)
        .unwrap();
    let key = core.device_key(id).unwrap();
    core.recordings_configure("{}").unwrap();
    core.recordings_enable(&key, Some(r#"{"channels":[{"channel":0}]}"#))
        .unwrap();
    let r = ask(&core, &key, 0, day(9));
    sim_until(&sim, &core, "the failure", 30, || {
        final_state(&rec, &r["request"]).is_some()
    });
    let f = final_state(&rec, &r["request"]).unwrap();
    assert_eq!(
        (&f["state"], &f["reason"]),
        (&json!("failed"), &json!("no_source"))
    );
    sim.run_free(|| core.shutdown());
}

/// A loudness gap says why it is empty, and "analyze this gap" measures
/// only that span of the downloaded video.
#[test]
fn gaps_say_why_and_a_span_is_analyzed_alone() {
    let sim = SimRuntime::new();
    let nvr = archive_mock(&sim);
    let out = tempfile::tempdir().unwrap();
    let (core, rec) = start(config(&sim, None));
    core.recordings_configure(
        &json!({"downloads_dir": out.path(), "downloads": {"chunk_s": 5}}).to_string(),
    )
    .unwrap();
    let key = downloaded(&sim, &core, &rec, &nvr);
    let gaps = |d: i64| -> Vec<Value> {
        let l = parse(
            core.recordings_loudness(
                &json!({"start": d, "end": d + 86_399, "bucket_s": 60, "gaps": true,
                        "cameras": [cam(&key, 0)]})
                .to_string(),
            ),
        );
        l["lanes"][0]["gaps"].as_array().unwrap().clone()
    };
    let reasons = |g: &[Value]| -> Vec<(String, bool)> {
        g.iter()
            .map(|g| {
                (
                    g["reason"].as_str().unwrap().to_owned(),
                    g["analyzable"].as_bool().unwrap(),
                )
            })
            .collect()
    };
    // The 8th is recorded but not downloaded: not analyzed, not analyzable.
    let g8 = gaps(day(8));
    assert!(
        reasons(&g8).contains(&("not_analyzed".to_owned(), false)),
        "{g8:?}"
    );
    assert!(
        reasons(&g8).iter().all(|(_, analyzable)| !analyzable),
        "{g8:?}"
    );
    // The 9th has a download: a gap there can be analyzed.
    let g9 = gaps(day(9));
    let open = g9
        .iter()
        .find(|g| g["reason"] == "not_analyzed" && g["analyzable"] == true)
        .unwrap_or_else(|| panic!("{g9:?}"))
        .clone();
    let (s, e) = (
        open["start"].as_i64().unwrap(),
        open["end"].as_i64().unwrap(),
    );
    let to = e.min(s + 59);
    let r = parse(core.sound_analyze_day(
        &json!({"camera": cam(&key, 0), "day": day(9), "start": s, "end": to}).to_string(),
    ));
    sim_until(&sim, &core, "the span analysis", 60, || {
        final_state(&rec, &r["request"]).is_some()
    });
    let f = final_state(&rec, &r["request"]).unwrap();
    assert_eq!(f["state"], "done", "{f}");
    assert_eq!(
        (f["start"].as_i64(), f["end"].as_i64()),
        (Some(s), Some(to))
    );
    assert!(f["seconds"].as_u64().unwrap() <= (to - s + 1) as u64, "{f}");
    // Only the span has loudness now.
    let l = parse(
        core.recordings_loudness(
            &json!({"start": day(9), "end": day(9) + 86_399, "bucket_s": 1,
                    "cameras": [cam(&key, 0)]})
            .to_string(),
        ),
    );
    let heard: Vec<i64> = l["lanes"][0]["peak_db"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, v)| !v.is_null())
        .map(|(i, _)| day(9) + i as i64)
        .collect();
    assert!(!heard.is_empty());
    assert!(heard.iter().all(|t| (s..=to).contains(t)), "{heard:?}");
    // A span outside the day is refused.
    let bad = core.sound_analyze_day(
        &json!({"camera": cam(&key, 0), "day": day(9), "start": day(9) - 5, "end": day(9)})
            .to_string(),
    );
    assert!(matches!(bad, Err(RecError::InvalidArgument)));
    sim.run_free(|| core.shutdown());
}
