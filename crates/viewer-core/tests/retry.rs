//! A tile whose stream is up but silent recovers by itself, and the app's
//! Retry (`Core::retry_tile`) recovers it at once, against the in-process
//! mock NVR on a simulated clock.

mod common;

use common::*;
use simtime::SimRuntime;
use std::time::Duration;
use viewer_core::{CoreConfig, StreamTier};

fn add(core: &viewer_core::Core, nvr: &mock_nvr::MockNvr) -> u64 {
    let addr = nvr.addr();
    core.add_device(&addr.ip().to_string(), addr.port(), USER, PASSWORD)
        .unwrap()
}

#[test]
fn a_silent_stream_after_a_reconnect_recovers_without_the_user() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 2);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = add(&core, &nvr);
    let t = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "pictures", 10, || rec.decodes(t) >= 10);
    // The connection drops and the device then accepts starts and sends
    // nothing: the tile used to wait for video for ever.
    nvr.set_silent_previews(true);
    nvr.drop_connections();
    let before = rec.decodes(t);
    sim.advance(Duration::from_secs(30));
    core.flush_events();
    assert!(rec.decodes(t) <= before + 2);
    // The device recovers; the next watchdog restart brings the picture.
    nvr.set_silent_previews(false);
    sim_until(&sim, &core, "pictures again", 130, || {
        rec.decodes(t) >= before + 20
    });
    core.shutdown();
}

#[test]
fn retry_restarts_a_silent_tile_now_and_keeps_its_floor() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 2);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = add(&core, &nvr);
    let t = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "pictures", 10, || rec.decodes(t) >= 10);
    nvr.set_silent_previews(true);
    nvr.drop_connections();
    sim.advance(Duration::from_secs(5));
    core.flush_events();
    let before = rec.decodes(t);
    nvr.set_silent_previews(false);
    let starts = nvr.command_count(3);
    core.retry_tile(t).unwrap();
    // Asked again at once (a double click): nothing more happens.
    core.retry_tile(t).unwrap();
    sim_until(&sim, &core, "pictures after the retry", 5, || {
        rec.decodes(t) >= before + 10
    });
    assert_eq!(nvr.command_count(3), starts + 1, "one restart");
    // An unknown tile is reported.
    assert!(core.retry_tile(t + 100).is_err());
    core.shutdown();
}

#[test]
fn retry_cuts_the_reconnect_backoff_short() {
    let sim = SimRuntime::new();
    let nvr = sim_mock(&sim, 2);
    let mut cfg = fast_config();
    cfg.backoff = viewer_core::BackoffConfig {
        initial: Duration::from_secs(60),
        max: Duration::from_secs(60),
        jitter: 0.0,
        stable_after: Duration::from_secs(30),
    };
    let (core, rec) = start(on_sim(&sim, CoreConfig { ..cfg }));
    let dev = add(&core, &nvr);
    let t = core.open_tile(dev, 1, StreamTier::Sub).unwrap();
    sim_until(&sim, &core, "pictures", 10, || rec.decodes(t) >= 10);
    nvr.drop_connections();
    sim.advance(Duration::from_secs(3));
    core.flush_events();
    let before = rec.decodes(t);
    // Still waiting out the 60 s backoff.
    sim.advance(Duration::from_secs(5));
    core.flush_events();
    assert_eq!(rec.decodes(t), before);
    core.retry_tile(t).unwrap();
    sim_until(&sim, &core, "pictures after the retry", 10, || {
        rec.decodes(t) >= before + 10
    });
    core.shutdown();
}
