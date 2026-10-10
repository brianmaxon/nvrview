//! The core against the in-process mock NVR and a fake platform decoder.
//! Most tests run the core on a simulated clock; the ones about threads
//! (answers from other threads, a slow `on_action`, calls from callbacks)
//! keep the core's own runtime and tile threads, in real time.

mod common;

use common::*;
use simtime::SimRuntime;
use std::sync::atomic::Ordering;
use std::time::Duration;
use viewer_core::{CoreConfig, CoreError, DecodeFailureKind, StreamTier, TileStatus};

#[test]
fn tiles_share_one_login_and_play() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 4);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let addr = nvr.addr();
    let dev = core
        .add_device(&addr.ip().to_string(), addr.port(), USER, PASSWORD)
        .unwrap();
    let tiles = [
        core.open_tile(dev, 0, StreamTier::Sub).unwrap(),
        core.open_tile(dev, 1, StreamTier::Sub).unwrap(),
        core.open_tile(dev, 2, StreamTier::Main).unwrap(),
    ];
    assert!(tiles.iter().all(|t| *t != 0 && *t != dev));
    for t in tiles {
        sim_until(&sim, &core, "decodes on every tile", 10, || {
            rec.decodes(t) >= 20
        });
    }
    assert_eq!(nvr.active_logins(), 1, "one session carries every tile");
    for t in tiles {
        let seen = rec.seen(t);
        check_protocol(&seen);
        // Configure first (status changes aside), then a keyframe.
        let first = seen.iter().find(|s| !matches!(s, Seen::Status(_))).unwrap();
        assert!(
            matches!(
                first,
                Seen::Configure {
                    width: 320,
                    height: 240,
                    ..
                }
            ),
            "{first:?}"
        );
        assert_eq!(rec.last_status(t), Some(TileStatus::Playing));
        assert!(
            !seen.contains(&Seen::Flush),
            "a clean stream needs no flush"
        );
    }
    assert!(
        !rec.overlap.load(Ordering::SeqCst),
        "on_action overlapped for a tile"
    );
    sim_until(&sim, &core, "tile_stats", 5, || {
        rec.events_of("tile_stats")
            .iter()
            .any(|e| e["tile"] == tiles[0] && e["decoded"].as_u64() > Some(0))
    });
    let s = rec
        .events_of("tile_stats")
        .into_iter()
        .rev()
        .find(|e| e["tile"] == tiles[0])
        .unwrap();
    for key in [
        "received", "decoded", "gated", "dropped", "failures", "resets",
    ] {
        assert!(s[key].is_u64(), "tile_stats.{key}");
    }
    let states: Vec<_> = rec
        .events_of("device_state")
        .iter()
        .map(|e| e["state"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(states[..2], ["connecting", "connected"]);

    sim.run_free(|| core.shutdown());
    let after = (
        rec.actions.load(Ordering::SeqCst),
        rec.events_count.load(Ordering::SeqCst),
    );
    sim_until(&sim, &core, "the session to close", 5, || {
        nvr.active_logins() == 0
    });
    sim.advance(Duration::from_millis(200));
    core.flush_events();
    assert_eq!(
        after,
        (
            rec.actions.load(Ordering::SeqCst),
            rec.events_count.load(Ordering::SeqCst)
        ),
        "no callback after shutdown"
    );
    assert_eq!(
        core.open_tile(dev, 0, StreamTier::Sub),
        Err(CoreError::Closed)
    );
    assert_eq!(core.decoded(tiles[0], 0, 0), Err(CoreError::Closed));
}

#[test]
fn answers_from_other_threads_keep_tiles_playing() {
    let nvr = mock(2);
    let (core, rec) = start(fast_config());
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let t = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    rec.set_behavior(
        t,
        Behavior {
            answer_from_thread: Some(Duration::from_millis(15)),
            ..Behavior::default()
        },
    );
    wait_until("decodes", 10, || rec.decodes(t) >= 50);
    let seen = rec.seen(t);
    check_protocol(&seen);
    assert_eq!(rec.last_status(t), Some(TileStatus::Playing));
    assert!(
        !seen.contains(&Seen::Status(TileStatus::Recovering)),
        "{seen:?}"
    );
    core.shutdown();
}

#[test]
fn failure_kinds_drive_recovery() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 4);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    // Open tiles one at a time so each one's behavior is set before its
    // first action.
    let open = |ch: u8, b: Behavior| {
        let t = core.open_tile(dev, ch, StreamTier::Sub).unwrap();
        rec.set_behavior(t, b);
        t
    };
    // Two corrupt pictures: flush, wait for a keyframe, play again.
    let flaky = open(
        0,
        Behavior {
            fail_decodes: Some((5..7, DecodeFailureKind::DecodeError)),
            ..Behavior::default()
        },
    );
    // Rejected parameter sets: undecodable, never decodes.
    let rejected = open(
        1,
        Behavior {
            fail_configures: vec![(0, DecodeFailureKind::InvalidParameterSets)],
            ..Behavior::default()
        },
    );
    // The first decoder cannot be created: reset, cooldown, then it plays.
    let transient = open(
        2,
        Behavior {
            fail_configures: vec![(0, DecodeFailureKind::SessionCreateFailed)],
            ..Behavior::default()
        },
    );
    sim_until(&sim, &core, "flaky tile recovers", 15, || {
        let seen = rec.seen(flaky);
        seen.contains(&Seen::Flush)
            && seen.contains(&Seen::Status(TileStatus::Recovering))
            && rec.last_status(flaky) == Some(TileStatus::Playing)
    });
    sim_until(&sim, &core, "rejected tile gives up", 10, || {
        rec.last_status(rejected) == Some(TileStatus::Undecodable)
    });
    sim_until(&sim, &core, "transient tile plays", 15, || {
        rec.last_status(transient) == Some(TileStatus::Playing) && rec.decodes(transient) > 10
    });
    sim.advance(Duration::from_millis(500));
    core.flush_events();
    for t in [flaky, rejected, transient] {
        check_protocol(&rec.seen(t));
    }
    // Answers to Decodes that the behavior missed are none here; the
    // rejected tile never decoded and stays quiet.
    assert_eq!(rec.decodes(rejected), 0);
    assert_eq!(rec.last_status(rejected), Some(TileStatus::Undecodable));
    let tr = rec.seen(transient);
    let configures = tr
        .iter()
        .filter(|s| matches!(s, Seen::Configure { .. }))
        .count();
    assert_eq!(configures, 2, "{tr:?}");
    assert!(tr.contains(&Seen::Reset));
    let stats = rec
        .events_of("tile_stats")
        .into_iter()
        .rev()
        .find(|e| e["tile"] == flaky)
        .unwrap();
    assert!(stats["failures"].as_u64() >= Some(2), "{stats}");
    sim.run_free(|| core.shutdown());
}

#[test]
fn closing_a_tile_stops_its_preview_and_its_callbacks() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 2);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let tap = Tap::sim(nvr.addr());
    let dev = core.add_device_with(tap.clone(), USER, PASSWORD).unwrap();
    let a = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    let b = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "both tiles play", 10, || {
        rec.decodes(a) >= 10 && rec.decodes(b) >= 10
    });
    assert_eq!(tap.count(3), 2, "two preview starts");
    assert_eq!(tap.count(4), 0);

    core.close_tile(a).unwrap();
    let seen_at_close = rec.seen(a).len();
    let b_at_close = rec.decodes(b);
    sim_until(&sim, &core, "preview stop", 5, || tap.count(4) == 1);
    sim.advance(Duration::from_millis(300));
    core.flush_events();
    assert_eq!(rec.seen(a).len(), seen_at_close, "no on_action after close");
    assert!(rec.decodes(b) > b_at_close, "the other tile keeps playing");
    assert_eq!(core.close_tile(a), Err(CoreError::UnknownId));
    assert_eq!(core.decoded(a, 0, 0), Err(CoreError::UnknownId));
    sim_until(&sim, &core, "tile_closed", 5, || {
        rec.events_of("tile_closed")
            .iter()
            .any(|e| e["tile"] == a && e["reason"] == "closed")
    });
    assert_eq!(nvr.active_logins(), 1);
    assert_eq!(tap.connects.load(Ordering::SeqCst), 1);
    sim.run_free(|| core.shutdown());
    sim_until(&sim, &core, "the session to close", 5, || {
        nvr.active_logins() == 0
    });
    assert_eq!(tap.count(4), 2, "shutdown stopped the remaining preview");
}

#[test]
fn removing_a_device_closes_its_tiles_and_socket() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 2);
    let other = sim_mock(&sim, 1);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let keep = core
        .add_device("127.0.0.1", other.addr().port(), USER, PASSWORD)
        .unwrap();
    let t1 = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    let t2 = core.open_tile(dev, 1, StreamTier::Main).unwrap();
    let t3 = core.open_tile(keep, 0, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "tiles play", 10, || {
        [t1, t2, t3].iter().all(|t| rec.decodes(*t) >= 5)
    });

    core.remove_device(dev).unwrap();
    let (n1, n2, n3) = (rec.seen(t1).len(), rec.seen(t2).len(), rec.decodes(t3));
    sim_until(
        &sim,
        &core,
        "the removed device's session to close",
        5,
        || nvr.active_logins() == 0,
    );
    sim_until(&sim, &core, "tile_closed for both tiles", 5, || {
        let closed = rec.events_of("tile_closed");
        [t1, t2].iter().all(|t| {
            closed
                .iter()
                .any(|e| e["tile"] == *t && e["reason"] == "device_removed")
        })
    });
    // Events are asynchronous: the device logs out (freeing its slot on the
    // mock) before the core reports it closed.
    sim_until(&sim, &core, "the device's closed event", 5, || {
        rec.events_of("device_state")
            .iter()
            .any(|e| e["device"] == dev && e["state"] == "closed")
    });
    sim.advance(Duration::from_millis(300));
    core.flush_events();
    assert_eq!((rec.seen(t1).len(), rec.seen(t2).len()), (n1, n2));
    assert!(rec.decodes(t3) > n3, "the other device keeps playing");
    assert_eq!(other.active_logins(), 1);
    assert_eq!(core.remove_device(dev), Err(CoreError::UnknownId));
    assert_eq!(
        core.open_tile(dev, 0, StreamTier::Sub),
        Err(CoreError::UnknownId)
    );
    drop(core);
    sim.advance_until(
        "dropping the core closes the rest",
        Duration::from_secs(5),
        Duration::from_millis(10),
        || other.active_logins() == 0,
    );
}

#[test]
fn refused_login_is_reported_once_and_not_retried() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 1);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let tap = Tap::sim(nvr.addr());
    let dev = core
        .add_device_with(tap.clone(), USER, "wrong-password")
        .unwrap();
    let t = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "login_failed", 10, || {
        rec.events_of("device_state")
            .iter()
            .any(|e| e["state"] == "login_failed")
    });
    let e = rec
        .events_of("device_state")
        .into_iter()
        .find(|e| e["state"] == "login_failed")
        .unwrap();
    assert_eq!(e["device"], dev);
    assert_eq!(e["status"], 401);
    assert_eq!(e["remain_times"], 2);
    // Several backoff periods later: still one attempt, no new states.
    sim.advance(Duration::from_millis(800));
    core.flush_events();
    assert_eq!(tap.connects.load(Ordering::SeqCst), 1);
    let states: Vec<_> = rec
        .events_of("device_state")
        .iter()
        .map(|e| e["state"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(states, ["connecting", "login_failed"]);
    assert_eq!(rec.decodes(t), 0);
    assert_eq!(nvr.active_logins(), 0);
    // The device's tiles end with it, and so does one opened afterward.
    let late = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "tile_closed for both tiles", 5, || {
        rec.events_of("tile_closed").len() == 2
    });
    // The app removes the device and adds it again with new credentials.
    core.remove_device(dev).unwrap();
    assert_eq!(core.close_tile(t), Err(CoreError::UnknownId));
    sim.advance(Duration::from_millis(100));
    core.flush_events();
    assert_eq!(
        rec.check_closed_once(&[t, late]),
        ["login_failed", "login_failed"]
    );
    let again = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    assert_ne!(again, dev);
    let t = core.open_tile(again, 0, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "decodes after new credentials", 10, || {
        rec.decodes(t) > 5
    });
    sim.run_free(|| core.shutdown());
}

#[test]
fn channels_and_device_info_events() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 3);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    sim_until(&sim, &core, "named channels", 10, || {
        rec.events_of("channels")
            .iter()
            .any(|e| e["channels"][0].get("name").is_some())
    });
    let all = rec.events_of("channels");
    let named = all.last().unwrap();
    assert_eq!(named["device"], dev);
    let list = named["channels"].as_array().unwrap();
    assert_eq!(list.len(), 3);
    for (i, c) in list.iter().enumerate() {
        assert_eq!(c["channel"], i);
        assert_eq!(c["connected"], true);
        assert_eq!(c["tiers"], serde_json::json!(["main", "sub", "extern"]));
        assert_eq!(c["name"], format!("Channel {}", i + 1));
        assert_eq!(c["model"], "MOCK-CAM");
    }
    // The slot list came first, before any name was known.
    assert!(all[0]["channels"][0].get("name").is_none(), "{all:?}");
    sim_until(&sim, &core, "device_info", 5, || {
        !rec.events_of("device_info").is_empty()
    });
    let info = &rec.events_of("device_info")[0];
    assert_eq!(info["device"], dev);
    assert_eq!(info["name"], "Mock NVR");
    assert_eq!(info["model"], "MOCK-NVR");
    sim.run_free(|| core.shutdown());
}

#[test]
fn a_slow_tile_does_not_stall_another() {
    let nvr = mock(2);
    let (core, rec) = start(fast_config());
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let slow = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    rec.set_behavior(
        slow,
        Behavior {
            slow: Some(Duration::from_millis(250)),
            ..Behavior::default()
        },
    );
    let fast = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    wait_until("both tiles start", 10, || {
        rec.decodes(slow) >= 1 && rec.decodes(fast) >= 1
    });
    let (s0, f0) = (rec.decodes(slow), rec.decodes(fast));
    std::thread::sleep(Duration::from_secs(3));
    let (s1, f1) = (rec.decodes(slow) - s0, rec.decodes(fast) - f0);
    // The mock sends 25 pictures a second per stream.
    assert!(f1 >= 50, "fast tile decoded only {f1} in 3 s");
    assert!(s1 <= 13, "slow tile cannot keep up: {s1}");
    check_protocol(&rec.seen(slow));
    check_protocol(&rec.seen(fast));
    let fast_stats = rec
        .events_of("tile_stats")
        .into_iter()
        .rev()
        .find(|e| e["tile"] == fast)
        .unwrap();
    assert_eq!(fast_stats["dropped"], 0, "{fast_stats}");
    wait_until("the slow tile reports drops", 5, || {
        rec.events_of("tile_stats")
            .iter()
            .any(|e| e["tile"] == slow && e["dropped"].as_u64() > Some(0))
    });
    // The session stayed healthy throughout.
    assert_eq!(
        rec.events_of("device_state")
            .iter()
            .filter(|e| e["state"] == "connected")
            .count(),
        1
    );
    core.shutdown();
}

#[test]
fn calls_from_inside_callbacks_are_refused() {
    let nvr = mock(1);
    let (core, rec) = start(fast_config());
    rec.try_reentry.store(true, Ordering::SeqCst);
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let t = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    wait_until("decodes", 10, || rec.decodes(t) >= 3);
    rec.try_reentry.store(false, Ordering::SeqCst);
    let attempts = rec.reentrant.lock().unwrap().clone();
    assert!(!attempts.is_empty());
    assert!(
        attempts
            .iter()
            .all(|r| *r == Err(CoreError::InvalidArgument))
    );
    core.close_tile(t).unwrap();
    core.remove_device(dev).unwrap();
    core.shutdown();
}

#[test]
fn arguments_and_limits() {
    let (core, _rec) = start(CoreConfig {
        max_devices: 1,
        max_tiles: 1,
        ..fast_config()
    });
    assert_eq!(
        core.add_device("", 9000, USER, PASSWORD),
        Err(CoreError::InvalidArgument)
    );
    assert_eq!(
        core.add_device("127.0.0.1", 0, USER, PASSWORD),
        Err(CoreError::InvalidArgument)
    );
    assert_eq!(
        core.add_device("127.0.0.1", 9, "", PASSWORD),
        Err(CoreError::InvalidArgument)
    );
    // Port 9 (discard) on loopback: nothing listens; the device backs off.
    let dev = core.add_device("127.0.0.1", 9, USER, PASSWORD).unwrap();
    assert_eq!(
        core.add_device("127.0.0.1", 9, USER, PASSWORD),
        Err(CoreError::Limit)
    );
    assert_eq!(
        core.open_tile(dev + 100, 0, StreamTier::Sub),
        Err(CoreError::UnknownId)
    );
    let t = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    assert_eq!(
        core.open_tile(dev, 1, StreamTier::Sub),
        Err(CoreError::Limit)
    );
    assert_eq!(core.decoded(t + 100, 0, 0), Err(CoreError::UnknownId));
    core.close_tile(t).unwrap();
    core.remove_device(dev).unwrap();
    core.shutdown();
    core.shutdown();
    assert_eq!(
        core.add_device("127.0.0.1", 9, USER, PASSWORD),
        Err(CoreError::Closed)
    );
}
