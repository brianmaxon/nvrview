//! Switching a tile's stream tier with generations (docs/FFI.md, "Switching
//! a tile's stream tier"), against the in-process mock NVR.

mod common;

use common::*;
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
    let nvr = mock(2);
    let (core, rec) = start(fast_config());
    let dev = add(&core, &nvr);
    let t = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    wait_until("sub-stream pictures", 10, || rec.decodes(t) >= 10);
    let starts_before = nvr.command_count(3);
    let stops_before = nvr.command_count(4);

    core.set_tile_tier(t, StreamTier::Main).unwrap();
    wait_until("the switch", 10, || {
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
    assert!(timing[0]["ns"].as_u64().unwrap() > 0);

    // Generation 1 is a complete decoder protocol of its own, and its first
    // picture was decoded while generation 0 was still being fed; then
    // generation 0 got a final Reset and nothing after it.
    wait_until("main-stream pictures", 10, || {
        rec.seen_of(t, 1)
            .iter()
            .filter(|s| matches!(s, Seen::Decode { .. }))
            .count()
            >= 5
    });
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
    wait_until("the sub-stream stop", 5, || {
        nvr.command_count(4) == stops_before + 1
    });
    assert_eq!(nvr.command_count(3), starts_before + 1);
    assert_eq!(nvr.active_logins(), 1);
    assert_eq!(core.decoded(t, 0, 0), Ok(()), "a late result is harmless");

    // And back to the sub-stream: generation 2.
    core.set_tile_tier(t, StreamTier::Sub).unwrap();
    wait_until("the switch back", 10, || {
        tier_events(&rec, t).contains(&("sub".into(), "active".into(), 2))
    });
    wait_until("sub-stream pictures again", 10, || {
        rec.seen_of(t, 2)
            .iter()
            .filter(|s| matches!(s, Seen::Decode { .. }))
            .count()
            >= 3
    });
    check_protocol(&rec.seen_of(t, 2));
    assert!(rec.seen_of(t, 1).contains(&Seen::Reset));
    wait_until("tile_stats of generation 2", 5, || {
        rec.events_of("tile_stats")
            .iter()
            .any(|e| e["tile"] == t && e["generation"] == 2 && e["tier"] == "sub")
    });

    // Asking for the tier already shown reports it active again.
    core.set_tile_tier(t, StreamTier::Sub).unwrap();
    wait_until("reaffirmed", 5, || {
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
    wait_until("tile_closed", 5, || {
        !rec.events_of("tile_closed").is_empty()
    });
    assert_eq!(rec.check_closed_once(&[t]), ["closed"]);
    core.shutdown();
    wait_until("logged out", 5, || nvr.active_logins() == 0);
}

#[test]
fn a_switch_shares_a_stream_another_tile_already_shows() {
    let nvr = mock(2);
    let (core, rec) = start(fast_config());
    let dev = add(&core, &nvr);
    let grid = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    let big = core.open_tile(dev, 0, StreamTier::Main).unwrap();
    for t in [grid, big] {
        wait_until("pictures", 10, || rec.decodes(t) >= 5);
    }
    let (starts, stops) = (nvr.command_count(3), nvr.command_count(4));
    core.set_tile_tier(grid, StreamTier::Main).unwrap();
    wait_until("the switch", 10, || {
        tier_events(&rec, grid).contains(&("main".into(), "active".into(), 1))
    });
    // The main stream was already running: no new start; the sub-stream
    // had no other viewer: stopped.
    wait_until("the sub-stream stop", 5, || {
        nvr.command_count(4) == stops + 1
    });
    assert_eq!(nvr.command_count(3), starts);
    // The other tile never noticed.
    assert!(rec.seen_gen(big).iter().all(|(g, _)| *g == 0));
    core.shutdown();
}

#[test]
fn a_newer_request_replaces_a_switch_in_progress() {
    let nvr = mock(1);
    let (core, rec) = start(fast_config());
    let dev = add(&core, &nvr);
    let t = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    wait_until("pictures", 10, || rec.decodes(t) >= 5);
    // A decoder that stops answering: the new generation never shows.
    rec.set_behavior(
        t,
        Behavior {
            silent: true,
            ..Behavior::default()
        },
    );
    core.set_tile_tier(t, StreamTier::Main).unwrap();
    wait_until("main decodes submitted", 10, || {
        rec.seen_of(t, 1)
            .iter()
            .any(|s| matches!(s, Seen::Decode { .. }))
    });
    core.set_tile_tier(t, StreamTier::Sub).unwrap();
    wait_until("back to the sub-stream", 5, || {
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
    core.shutdown();
}

#[test]
fn a_switch_that_shows_nothing_in_time_fails_and_the_old_stream_stays() {
    let nvr = mock(1);
    let (core, rec) = start(CoreConfig {
        tier_switch_timeout: Duration::from_millis(500),
        ..fast_config()
    });
    let dev = add(&core, &nvr);
    let t = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    wait_until("pictures", 10, || rec.decodes(t) >= 5);
    let stops = nvr.command_count(4);
    rec.set_behavior(
        t,
        Behavior {
            silent: true,
            ..Behavior::default()
        },
    );
    core.set_tile_tier(t, StreamTier::Main).unwrap();
    wait_until("the timeout", 5, || {
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
    wait_until("the main-stream stop", 5, || {
        nvr.command_count(4) == stops + 1
    });
    assert!(
        !rec.events_of("timing")
            .iter()
            .any(|e| e["what"] == "tier_switch")
    );
    core.shutdown();
}

#[test]
fn the_first_picture_is_timed_once() {
    let nvr = mock(2);
    let (core, rec) = start(fast_config());
    let dev = add(&core, &nvr);
    let a = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    let b = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    for t in [a, b] {
        wait_until("pictures", 10, || rec.decodes(t) >= 3);
    }
    let first: Vec<_> = rec
        .events_of("timing")
        .into_iter()
        .filter(|e| e["what"] == "first_picture")
        .collect();
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(first[0]["tile"] == a || first[0]["tile"] == b);
    assert!(first[0]["ns"].as_u64().unwrap() > 0);
    core.shutdown();
}
