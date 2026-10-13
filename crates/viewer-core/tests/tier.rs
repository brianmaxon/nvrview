//! Switching a tile's stream tier with generations (docs/FFI.md, "Switching
//! a tile's stream tier"), against the in-process mock NVR, on a simulated
//! clock: switch timeouts and the alignment cap are measured in simulated
//! time.

use crate::common::*;
use simtime::SimRuntime;
use std::time::Duration;
use viewer_core::{CoreConfig, CoreError, StreamTier};

fn tier_events(rec: &Recorder, tile: u64) -> Vec<(String, String, u64)> {
    rec.events_of("tile_tier")
        .iter()
        .filter(|e| e["tile"] == tile)
        .map(|e| {
            (
                e["tier"].as_str().unwrap().to_owned(),
                e["state"].as_str().unwrap().to_owned(),
                e["generation"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn position(seen: &[(u32, Seen)], f: impl Fn(&(u32, Seen)) -> bool) -> Option<usize> {
    seen.iter().position(f)
}

fn add(core: &viewer_core::Core, nvr: &mock_nvr::MockNvr) -> u64 {
    let addr = nvr.addr();
    core.add_device(&addr.ip().to_string(), addr.port(), USER, PASSWORD)
        .unwrap()
}

#[test]
fn a_tile_switches_to_main_without_a_gap_and_back() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 2);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = add(&core, &nvr);
    let t = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "sub-stream pictures", 10, || {
        rec.decodes(t) >= 10
    });
    let starts_before = nvr.command_count(3);
    let stops_before = nvr.command_count(4);

    core.set_tile_tier(t, StreamTier::Main).unwrap();
    sim_until(&sim, &core, "the switch", 10, || {
        tier_events(&rec, t).contains(&("main".into(), "active".into(), 1))
    });
    assert_eq!(
        tier_events(&rec, t),
        vec![
            ("main".into(), "starting".into(), 1),
            ("main".into(), "active".into(), 1)
        ]
    );
    let timing: Vec<_> = rec
        .events_of("timing")
        .into_iter()
        .filter(|e| e["what"] == "tier_switch")
        .collect();
    assert_eq!(timing.len(), 1);
    assert_eq!(timing[0]["tile"], t);
    // The mock's tiers share one clock, as a device's do, so the main
    // stream's first picture is not older than the one shown and is shown
    // at once, not at the alignment cap (the cap has tests of its own
    // below); on the simulated clock that may take no time at all.
    assert_eq!(timing[0]["capped"], false, "{}", timing[0]);
    let ns = timing[0]["ns"].as_u64().unwrap();
    assert!(
        Duration::from_nanos(ns) < fast_config().tier_switch_align_cap,
        "{ns} ns"
    );

    // Generation 1 is a complete decoder protocol of its own, and its first
    // picture was decoded while generation 0 was still being fed; then
    // generation 0 got a final Reset and nothing after it.
    // (The new generation may decode several pictures before it is shown,
    // while it catches up with the picture on screen.)
    sim_until(
        &sim,
        &core,
        "main-stream pictures after the switch",
        10,
        || {
            let seen = rec.seen_gen(t);
            position(&seen, |(g, s)| *g == 0 && *s == Seen::Reset).is_some_and(|r| {
                seen[r..]
                    .iter()
                    .filter(|(g, s)| *g == 1 && matches!(s, Seen::Decode { .. }))
                    .count()
                    >= 5
            })
        },
    );
    check_protocol(&rec.seen_of(t, 1));
    let seen = rec.seen_gen(t);
    let first_new = position(&seen, |(g, s)| *g == 1 && matches!(s, Seen::Decode { .. })).unwrap();
    let old_reset = position(&seen, |(g, s)| *g == 0 && *s == Seen::Reset).unwrap();
    assert!(first_new < old_reset, "{seen:?}");
    assert!(
        seen[old_reset + 1..].iter().all(|(g, _)| *g == 1),
        "generation 0 acted after its Reset"
    );
    assert_eq!(seen.last().unwrap().0, 1);

    // Start the new stream, then stop the old one; still one login.
    sim_until(&sim, &core, "the sub-stream stop", 5, || {
        nvr.command_count(4) == stops_before + 1
    });
    assert_eq!(nvr.command_count(3), starts_before + 1);
    assert_eq!(nvr.active_logins(), 1);
    assert_eq!(core.decoded(t, 0, 0), Ok(()), "a late result is harmless");

    // And back to the sub-stream: generation 2.
    core.set_tile_tier(t, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "the switch back", 10, || {
        tier_events(&rec, t).contains(&("sub".into(), "active".into(), 2))
    });
    sim_until(&sim, &core, "sub-stream pictures again", 10, || {
        rec.seen_of(t, 2)
            .iter()
            .filter(|s| matches!(s, Seen::Decode { .. }))
            .count()
            >= 3
    });
    check_protocol(&rec.seen_of(t, 2));
    assert!(rec.seen_of(t, 1).contains(&Seen::Reset));
    sim_until(&sim, &core, "tile_stats of generation 2", 5, || {
        rec.events_of("tile_stats")
            .iter()
            .any(|e| e["tile"] == t && e["generation"] == 2 && e["tier"] == "sub")
    });

    // Asking for the tier already shown reports it active again.
    core.set_tile_tier(t, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "reaffirmed", 5, || {
        tier_events(&rec, t)
            .iter()
            .filter(|e| **e == ("sub".to_owned(), "active".to_owned(), 2))
            .count()
            == 2
    });
    assert_eq!(
        core.set_tile_tier(t + 100, StreamTier::Main),
        Err(CoreError::UnknownId)
    );
    core.close_tile(t).unwrap();
    sim_until(&sim, &core, "tile_closed", 5, || {
        !rec.events_of("tile_closed").is_empty()
    });
    assert_eq!(rec.check_closed_once(&[t]), ["closed"]);
    sim.run_free(|| core.shutdown());
    sim_until(&sim, &core, "logged out", 5, || nvr.active_logins() == 0);
}

#[test]
fn a_switch_shares_a_stream_another_tile_already_shows() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 2);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = add(&core, &nvr);
    let grid = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    let big = core.open_tile(dev, 0, StreamTier::Main).unwrap();
    for t in [grid, big] {
        sim_until(&sim, &core, "pictures", 10, || rec.decodes(t) >= 5);
    }
    let (starts, stops) = (nvr.command_count(3), nvr.command_count(4));
    core.set_tile_tier(grid, StreamTier::Main).unwrap();
    sim_until(&sim, &core, "the switch", 10, || {
        tier_events(&rec, grid).contains(&("main".into(), "active".into(), 1))
    });
    // The main stream was already running: no new start; the sub-stream
    // had no other viewer: stopped.
    sim_until(&sim, &core, "the sub-stream stop", 5, || {
        nvr.command_count(4) == stops + 1
    });
    assert_eq!(nvr.command_count(3), starts);
    // The other tile never noticed.
    assert!(rec.seen_gen(big).iter().all(|(g, _)| *g == 0));
    sim.run_free(|| core.shutdown());
}

#[test]
fn a_newer_request_replaces_a_switch_in_progress() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 1);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = add(&core, &nvr);
    let t = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "pictures", 10, || rec.decodes(t) >= 5);
    // A decoder that stops answering: the new generation never shows.
    rec.set_behavior(
        t,
        Behavior {
            silent: true,
            ..Behavior::default()
        },
    );
    core.set_tile_tier(t, StreamTier::Main).unwrap();
    sim_until(&sim, &core, "main decodes submitted", 10, || {
        rec.seen_of(t, 1)
            .iter()
            .any(|s| matches!(s, Seen::Decode { .. }))
    });
    core.set_tile_tier(t, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "back to the sub-stream", 5, || {
        tier_events(&rec, t).contains(&("sub".into(), "active".into(), 0))
    });
    let failed: Vec<_> = rec
        .events_of("tile_tier")
        .into_iter()
        .filter(|e| e["tile"] == t && e["state"] == "failed")
        .collect();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["generation"], 1);
    assert_eq!(failed[0]["reason"], "superseded");
    assert_eq!(rec.seen_of(t, 1).last(), Some(&Seen::Reset));
    sim.run_free(|| core.shutdown());
}

#[test]
fn a_switch_that_shows_nothing_in_time_fails_and_the_old_stream_stays() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 1);
    let (core, rec) = start(on_sim(
        &sim,
        CoreConfig {
            tier_switch_timeout: Duration::from_millis(500),
            ..fast_config()
        },
    ));
    let dev = add(&core, &nvr);
    let t = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "pictures", 10, || rec.decodes(t) >= 5);
    let stops = nvr.command_count(4);
    rec.set_behavior(
        t,
        Behavior {
            silent: true,
            ..Behavior::default()
        },
    );
    core.set_tile_tier(t, StreamTier::Main).unwrap();
    sim_until(&sim, &core, "the timeout", 5, || {
        tier_events(&rec, t).contains(&("main".into(), "failed".into(), 1))
    });
    let failed = rec
        .events_of("tile_tier")
        .into_iter()
        .find(|e| e["state"] == "failed")
        .unwrap();
    assert_eq!(failed["reason"], "timeout");
    assert_eq!(rec.seen_of(t, 1).last(), Some(&Seen::Reset));
    assert!(!rec.seen_of(t, 0).contains(&Seen::Reset));
    // The abandoned main stream is stopped; the sub-stream keeps running.
    sim_until(&sim, &core, "the main-stream stop", 5, || {
        nvr.command_count(4) == stops + 1
    });
    assert!(
        !rec.events_of("timing")
            .iter()
            .any(|e| e["what"] == "tier_switch")
    );
    sim.run_free(|| core.shutdown());
}

#[test]
fn the_first_picture_is_timed_once() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 2);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    // The core has been running a while when the app opens its tiles.
    sim.advance(Duration::from_millis(250));
    let dev = add(&core, &nvr);
    let a = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    let b = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    for t in [a, b] {
        sim_until(&sim, &core, "pictures", 10, || rec.decodes(t) >= 3);
    }
    let first: Vec<_> = rec
        .events_of("timing")
        .into_iter()
        .filter(|e| e["what"] == "first_picture")
        .collect();
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0]["tile"] == a || first[0]["tile"] == b);
    // Timed from the core's start, on its clock.
    assert!(first[0]["ns"].as_u64().unwrap() >= 250_000_000, "{first:?}");
    sim.run_free(|| core.shutdown());
}

// --- Alignment of a switch between tiers of one camera ---------------------
//
// A made-up `NVRREC02` recording served by the mock's replay (several tiers
// on one replay clock): the sub-stream from the start, the other tier only
// from 3 s on, so a switch requested in the first second starts it at its
// first keyframe, at a known moment. Both tiers take pictures every 40 ms on
// one picture clock (`CLOCK0` plus capture time), unless a test shifts it.

const CLOCK0: u64 = 7_000_000_000;
const PICTURE_US: u64 = 40_000;
const SUB_LATENCY_US: u64 = 60_000;
const OTHER_FROM_US: u64 = 3_000_000;
const RECORDING_US: u64 = 8_000_000;

/// Frames of the synthetic fixture in playback order, cycling, from its
/// first keyframe.
struct Frames {
    fixture: mock_nvr::fixture::Fixture,
    next: usize,
}

impl Frames {
    fn new() -> Frames {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic/h264-basic.h264");
        let fixture = mock_nvr::fixture::Fixture::load(&path).unwrap();
        let next = fixture.first_keyframe();
        Frames { fixture, next }
    }

    fn take(&mut self) -> (bool, Vec<u8>) {
        let f = &self.fixture.frames()[self.next];
        self.next = self.fixture.next_index(self.next);
        (f.keyframe, f.data.clone())
    }
}

/// The recording: sub arrives `SUB_LATENCY_US` after capture; `tier` is
/// recorded from `OTHER_FROM_US` on, picture `k` arriving at `arrival(k,
/// capture)` and stamped `CLOCK0 + capture + shift`.
fn alignment_recording(
    tier: viewer_core::StreamTier,
    shift: i64,
    arrival: impl Fn(u64, u64) -> u64,
) -> Vec<u8> {
    use bc_protocol::media::{Codec, Record};
    use bc_protocol::recording::TierWriter;
    let info = Record::Info {
        width: 320,
        height: 240,
        fps: 25,
    };
    let mut items: Vec<(u64, viewer_core::StreamTier, Record)> =
        vec![(0, StreamTier::Sub, info.clone())];
    let mut sub = Frames::new();
    let mut c = 0;
    while c < RECORDING_US {
        let (keyframe, data) = sub.take();
        items.push((
            c + SUB_LATENCY_US,
            StreamTier::Sub,
            Record::Video {
                codec: Codec::H264,
                keyframe,
                pts_us: CLOCK0 + c,
                data,
                extra_header: Vec::new(),
            },
        ));
        c += PICTURE_US;
    }
    items.push((OTHER_FROM_US - 50_000, tier, info));
    let mut other = Frames::new();
    let (mut k, mut last) = (0, 0);
    let mut c = OTHER_FROM_US;
    while c < RECORDING_US {
        let (keyframe, data) = other.take();
        let at = arrival(k, c).max(last);
        last = at;
        items.push((
            at,
            tier,
            Record::Video {
                codec: Codec::H264,
                keyframe,
                pts_us: (CLOCK0 as i64 + c as i64 + shift) as u64,
                data,
                extra_header: Vec::new(),
            },
        ));
        k += 1;
        c += PICTURE_US;
    }
    items.sort_by_key(|i| i.0);
    let mut w = TierWriter::new(Vec::new()).unwrap();
    w.start_requested(StreamTier::Sub, 0).unwrap();
    w.start_requested(tier, OTHER_FROM_US - 100_000).unwrap();
    for (at, t, r) in items {
        w.record(t, at, &r).unwrap();
    }
    w.finish().unwrap()
}

fn replay_mock(sim: &SimRuntime, recording: &[u8]) -> mock_nvr::MockNvr {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 1;
    cfg.replay = Some(mock_nvr::replay::ReplayConfig::new(
        mock_nvr::replay::Replay::from_reader(recording).unwrap(),
    ));
    sim_spawn(sim, cfg)
}

/// The `tier_switch` timing event of a switch from sub to `tier`, after
/// checking that the old generation kept decoding until its Reset.
fn switch_with(recording: &[u8], tier: viewer_core::StreamTier) -> (serde_json::Value, usize) {
    let sim = SimRuntime::new();
    let nvr = replay_mock(&sim, recording);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = add(&core, &nvr);
    let t = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "sub-stream pictures", 10, || {
        rec.decodes(t) >= 5
    });
    core.set_tile_tier(t, tier).unwrap();
    sim_until(&sim, &core, "the switch", 10, || {
        rec.events_of("timing")
            .iter()
            .any(|e| e["what"] == "tier_switch")
    });
    let timing = rec
        .events_of("timing")
        .into_iter()
        .find(|e| e["what"] == "tier_switch")
        .unwrap();
    assert_eq!(timing["tile"], t);
    // Old pictures decoded while the new generation already decoded: the
    // old generation kept presenting until its final Reset.
    let seen = rec.seen_gen(t);
    let first_new = position(&seen, |(g, s)| *g == 1 && matches!(s, Seen::Decode { .. })).unwrap();
    let old_reset = position(&seen, |(g, s)| *g == 0 && *s == Seen::Reset).unwrap();
    assert!(first_new < old_reset, "{seen:?}");
    let old_meanwhile = seen[first_new..old_reset]
        .iter()
        .filter(|(g, s)| *g == 0 && matches!(s, Seen::Decode { .. }))
        .count();
    check_protocol(&rec.seen_of(t, 1));
    sim.run_free(|| core.shutdown());
    (timing, old_meanwhile)
}

fn ms_field(e: &serde_json::Value, key: &str) -> f64 {
    e[key].as_f64().unwrap_or_else(|| panic!("{key} in {e}"))
}

#[test]
fn a_main_stream_ahead_of_the_picture_shown_switches_at_once() {
    // Main arrives 20 ms after capture, 40 ms before sub's picture of the
    // same moment (as measured, main usually arrives earlier).
    let rec = alignment_recording(StreamTier::Main, 0, |_, c| c + 20_000);
    let (e, _) = switch_with(&rec, StreamTier::Main);
    assert_eq!(e["capped"], false, "{e}");
    assert!(ms_field(&e, "held_ms") < 30.0, "{e}");
    let delta = ms_field(&e, "delta_ms");
    assert!((0.0..=120.0).contains(&delta), "{e}");
}

#[test]
fn a_main_stream_behind_is_held_until_it_catches_up() {
    // Main's first picture arrives 250 ms after capture, then pictures come
    // twice as fast as taken until main is 20 ms behind capture: it starts
    // 190 ms behind the sub picture on screen and catches up in about
    // 160 ms.
    let rec = alignment_recording(StreamTier::Main, 0, |k, c| {
        (OTHER_FROM_US + 250_000 + k * PICTURE_US / 2).max(c + 20_000)
    });
    let (e, old_meanwhile) = switch_with(&rec, StreamTier::Main);
    assert_eq!(e["capped"], false, "{e}");
    let held = ms_field(&e, "held_ms");
    assert!((60.0..290.0).contains(&held), "{e}");
    let delta = ms_field(&e, "delta_ms");
    assert!((0.0..=80.0).contains(&delta), "{e}");
    assert!(old_meanwhile >= 1, "the old picture kept updating: {e}");
}

#[test]
fn a_main_stream_far_behind_switches_at_the_cap() {
    // Main 600 ms after capture: it never catches up with sub's 60 ms.
    let rec = alignment_recording(StreamTier::Main, 0, |_, c| c + 600_000);
    let (e, old_meanwhile) = switch_with(&rec, StreamTier::Main);
    assert_eq!(e["capped"], true, "{e}");
    let held = ms_field(&e, "held_ms");
    assert!((300.0..450.0).contains(&held), "{e}");
    assert!(ms_field(&e, "delta_ms") < -300.0, "{e}");
    assert!(old_meanwhile >= 3, "{e}");
}

#[test]
fn a_tier_of_unknown_clock_or_another_clock_is_not_aligned() {
    // The extern tier, whose clock is not known to be sub's, as far behind
    // as above: no waiting, it shows at its first picture.
    let rec = alignment_recording(StreamTier::Extern, 0, |_, c| c + 600_000);
    let (e, _) = switch_with(&rec, StreamTier::Extern);
    assert_eq!(e["capped"], false, "{e}");
    assert!(ms_field(&e, "held_ms") < 1.0, "{e}");
    assert!(ms_field(&e, "delta_ms") < -300.0, "{e}");

    // Main with timestamps 100 s behind sub's: evidently another clock.
    let rec = alignment_recording(StreamTier::Main, -100_000_000, |_, c| c + 20_000);
    let (e, _) = switch_with(&rec, StreamTier::Main);
    assert_eq!(e["capped"], false, "{e}");
    assert!(ms_field(&e, "held_ms") < 1.0, "{e}");
    assert!(ms_field(&e, "delta_ms") < -90_000.0, "{e}");
}
