//! Tiles of a third-party (ONVIF) camera channel behind an NVR: the device
//! answers each preview with 400 and its stream may follow (docs/protocol.md,
//! "Records as the official client reads them"). Against the mock NVR's
//! third-party mode, on a simulated clock: retry intervals and wait limits
//! take simulated time.

mod common;

use common::*;
use simtime::SimRuntime;
use std::path::PathBuf;
use std::time::Duration;
use viewer_core::{CoreConfig, StreamTier, TileStatus};

fn third_party_mock(sim: &SimRuntime, refusals: u32) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 2;
    let mut tp = mock_nvr::ThirdPartyConfig::new(vec![1], refusals);
    tp.media_delay = Duration::from_millis(100);
    cfg.third_party = Some(tp);
    sim_spawn(sim, cfg)
}

fn config(retry_ms: u64, limit_ms: u64) -> CoreConfig {
    CoreConfig {
        preview_retry_interval: Duration::from_millis(retry_ms),
        preview_wait_limit: Duration::from_millis(limit_ms),
        ..fast_config()
    }
}

fn waiting_states(rec: &Recorder, tile: u64) -> Vec<String> {
    rec.events_of("tile_waiting")
        .iter()
        .filter(|e| e["tile"] == tile)
        .map(|e| e["state"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_tile_plays_after_the_device_answers_400() {
    let sim = SimRuntime::new();
    let nvr = third_party_mock(&sim, 2);
    let (core, rec) = start(on_sim(&sim, config(200, 10_000)));
    let tap = Tap::sim(nvr.addr());
    let dev = core.add_device_with(tap.clone(), USER, PASSWORD).unwrap();
    let tile = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "decodes", 10, || rec.decodes(tile) >= 20);
    let seen = rec.seen(tile);
    check_protocol(&seen);
    let first_decode = seen
        .iter()
        .find(|s| matches!(s, Seen::Decode { .. }))
        .unwrap();
    assert!(
        matches!(first_decode, Seen::Decode { keyframe: true, .. }),
        "{first_decode:?}"
    );
    assert_eq!(rec.last_status(tile), Some(TileStatus::Playing));
    // Waiting, then streaming, each once.
    assert_eq!(waiting_states(&rec, tile), ["waiting", "streaming"]);
    let w = rec
        .events_of("tile_waiting")
        .into_iter()
        .find(|e| e["state"] == "waiting")
        .unwrap();
    assert_eq!(w["status"], 400);
    assert_eq!(w["tier"], "sub");
    assert_eq!(w["generation"], 0);
    assert!(rec.events_of("tile_error").is_empty());
    // Two refused starts, each stopped and restarted, then the stream.
    assert_eq!(tap.count(3), 3);
    assert_eq!(tap.count(4), 2);
    sim.run_free(|| core.shutdown());
}

#[test]
fn a_tile_whose_device_never_streams_reports_start_rejected() {
    let sim = SimRuntime::new();
    let nvr = third_party_mock(&sim, u32::MAX);
    let (core, rec) = start(on_sim(&sim, config(100, 800)));
    let tap = Tap::sim(nvr.addr());
    let dev = core.add_device_with(tap.clone(), USER, PASSWORD).unwrap();
    let t0 = sim.now();
    let tile = core.open_tile(dev, 1, StreamTier::Main).unwrap();
    sim_until(&sim, &core, "tile_error", 10, || {
        rec.events_of("tile_error")
            .iter()
            .any(|e| e["tile"] == tile && e["reason"] == "start_rejected" && e["status"] == 400)
    });
    assert!(sim.now() - t0 >= Duration::from_millis(700));
    assert_eq!(waiting_states(&rec, tile), ["waiting"]);
    assert_eq!(rec.decodes(tile), 0);
    let starts = tap.count(3);
    assert!((4..=9).contains(&starts), "{starts} starts");
    sim_until(&sim, &core, "every start stopped", 5, || {
        tap.count(4) == starts
    });
    // Refused for good: no more starts.
    sim.advance(Duration::from_millis(400));
    core.flush_events();
    assert_eq!(tap.count(3), starts);
    sim.run_free(|| core.shutdown());
}

#[test]
fn closing_a_waiting_tile_stops_its_preview() {
    let sim = SimRuntime::new();
    let nvr = third_party_mock(&sim, u32::MAX);
    let (core, rec) = start(on_sim(&sim, config(100, 60_000)));
    let tap = Tap::sim(nvr.addr());
    let dev = core.add_device_with(tap.clone(), USER, PASSWORD).unwrap();
    let tile = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "waiting", 10, || {
        waiting_states(&rec, tile) == ["waiting"] && tap.count(3) >= 2
    });
    core.close_tile(tile).unwrap();
    sim_until(&sim, &core, "the stop", 5, || tap.count(4) == tap.count(3));
    let starts = tap.count(3);
    sim.advance(Duration::from_millis(400));
    core.flush_events();
    assert_eq!(tap.count(3), starts, "no restart after the close");
    assert_eq!(tap.count(4), starts);
    assert_eq!(nvr.active_logins(), 1);
    sim.run_free(|| core.shutdown());
    sim_until(&sim, &core, "the session to close", 5, || {
        nvr.active_logins() == 0
    });
}

#[test]
fn a_tier_switch_waits_for_the_device_too() {
    let sim = SimRuntime::new();
    let nvr = third_party_mock(&sim, 1);
    let (core, rec) = start(on_sim(&sim, config(200, 10_000)));
    let dev = core
        .add_device(
            &nvr.addr().ip().to_string(),
            nvr.addr().port(),
            USER,
            PASSWORD,
        )
        .unwrap();
    let tile = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "decodes", 10, || rec.decodes(tile) >= 10);
    core.set_tile_tier(tile, StreamTier::Main).unwrap();
    sim_until(&sim, &core, "the switch", 10, || {
        rec.events_of("tile_tier")
            .iter()
            .any(|e| e["tile"] == tile && e["state"] == "active" && e["generation"] == 1)
    });
    let gen1: Vec<String> = rec
        .events_of("tile_waiting")
        .iter()
        .filter(|e| e["tile"] == tile && e["generation"] == 1)
        .map(|e| e["state"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(gen1, ["waiting", "streaming"]);
    assert!(
        rec.events_of("tile_tier")
            .iter()
            .all(|e| e["state"] != "failed")
    );
    sim.run_free(|| core.shutdown());
}
