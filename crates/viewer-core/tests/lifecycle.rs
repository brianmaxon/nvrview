//! Ordering rules of docs/FFI.md: nothing about an object before its ID is
//! published, close never waits on another tile's callback, and exactly one
//! `tile_closed` per tile after its last action.

mod common;

use common::*;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use viewer_core::StreamTier;

#[test]
fn device_events_wait_for_the_id() {
    let nvr = mock(2);
    let (core, rec) = start(fast_config());
    let pending = core
        .add_device_deferred("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let id = pending.id();
    // The session logs in meanwhile; its events are held back.
    wait_until("login", 5, || nvr.active_logins() == 1);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        rec.events().iter().all(|e| e["device"] != id),
        "{:?}",
        rec.events()
    );
    assert_eq!(pending.publish(), id);
    wait_until("channels after publishing", 5, || {
        !rec.events_of("channels").is_empty()
    });
    let states: Vec<_> = rec
        .events_of("device_state")
        .iter()
        .map(|e| e["state"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(states, ["connecting", "connected"]);
    core.shutdown();
}

#[test]
fn tile_actions_wait_for_the_id() {
    let nvr = mock(2);
    let (core, rec) = start(fast_config());
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let pending = core.open_tile_deferred(dev, 0, StreamTier::Sub).unwrap();
    let t = pending.id();
    wait_until("the device connects", 5, || nvr.active_logins() == 1);
    std::thread::sleep(Duration::from_millis(500));
    assert!(rec.seen(t).is_empty(), "action before the ID was published");
    assert!(rec.events().iter().all(|e| e["tile"] != t));
    pending.publish();
    wait_until("decodes after publishing", 5, || rec.decodes(t) >= 5);
    let seen = rec.seen(t);
    check_protocol(&seen);
    core.shutdown();
}

#[test]
fn a_dropped_pending_tile_is_released_not_stuck() {
    let nvr = mock(1);
    let (core, rec) = start(fast_config());
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let pending = core.open_tile_deferred(dev, 0, StreamTier::Sub).unwrap();
    let t = pending.id();
    drop(pending);
    wait_until("decodes", 5, || rec.decodes(t) >= 2);
    core.close_tile(t).unwrap();
    core.shutdown();
}

#[test]
fn opening_a_tile_never_waits_on_an_action() {
    let nvr = mock(3);
    let (core, rec) = start(fast_config());
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let pending = core.open_tile_deferred(dev, 0, StreamTier::Sub).unwrap();
    let slow = pending.id();
    rec.set_behavior(
        slow,
        Behavior {
            slow: Some(Duration::from_millis(1500)),
            ..Behavior::default()
        },
    );
    pending.publish();
    // The first Decode callback is now sleeping.
    wait_until("the slow callback", 5, || rec.decodes(slow) == 1);
    let closer = {
        let core = core.clone();
        std::thread::spawn(move || {
            let start = Instant::now();
            core.close_tile(slow).unwrap();
            start.elapsed()
        })
    };
    std::thread::sleep(Duration::from_millis(50));
    let start = Instant::now();
    let other = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    let open_took = start.elapsed();
    assert!(
        open_took < Duration::from_millis(500),
        "nv_tile_open waited {open_took:?}"
    );
    let close_took = closer.join().unwrap();
    assert!(
        close_took >= Duration::from_millis(900),
        "close returned while the callback ran: {close_took:?}"
    );
    wait_until("the new tile plays", 5, || rec.decodes(other) >= 3);
    core.shutdown();
}

#[test]
fn tile_closed_is_sent_once_for_every_reason() {
    let nvr = mock(4);
    let bad = mock(1);
    let (core, rec) = start(fast_config());
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let removed = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let refused = core
        .add_device("127.0.0.1", bad.addr().port(), USER, "wrong-password")
        .unwrap();
    let closed = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    let kept = [
        core.open_tile(dev, 1, StreamTier::Sub).unwrap(),
        core.open_tile(dev, 2, StreamTier::Main).unwrap(),
    ];
    let gone = [
        core.open_tile(removed, 0, StreamTier::Sub).unwrap(),
        core.open_tile(removed, 3, StreamTier::Sub).unwrap(),
    ];
    let failed = core.open_tile(refused, 0, StreamTier::Sub).unwrap();
    for t in [closed, kept[0], kept[1], gone[0], gone[1]] {
        wait_until("every tile plays", 10, || rec.decodes(t) >= 3);
    }
    core.close_tile(closed).unwrap();
    core.remove_device(removed).unwrap();
    wait_until("the refused device's tile ends", 5, || {
        rec.events_of("tile_closed")
            .iter()
            .any(|e| e["tile"] == failed)
    });
    // Closing a tile that already ended on its own sends nothing more.
    core.close_tile(failed).unwrap();
    core.shutdown();
    let after = rec.events_count.load(Ordering::SeqCst);

    let all = [closed, gone[0], gone[1], failed, kept[0], kept[1]];
    let reasons = rec.check_closed_once(&all);
    assert_eq!(
        reasons,
        [
            "closed",
            "device_removed",
            "device_removed",
            "login_failed",
            "core_shutdown",
            "core_shutdown"
        ]
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(rec.events_count.load(Ordering::SeqCst), after);
}
