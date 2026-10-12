//! Camera motion control through the core against the mock NVR's PTZ
//! model, on a simulated clock: `ptz` events, the info JSON, and the stop
//! when the app stops renewing, when the tile closes, when the device is
//! removed, and at shutdown.

mod common;

use common::{PASSWORD, USER, fast_config, on_sim, sim_spawn, sim_until, start};
use mock_nvr::ptz::PtzCamera;
use simtime::SimRuntime;
use std::path::PathBuf;
use std::time::Duration;
use viewer_core::{CoreError, LensAxis, MotionOrigin, PtzDirection, StreamTier};

const PTZ: u8 = 1;
const LENS: u8 = 2;

fn mock(sim: &SimRuntime) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 3;
    cfg.ptz.insert(PTZ, PtzCamera::pan_tilt_zoom(2));
    cfg.ptz.insert(LENS, PtzCamera::lens_only());
    sim_spawn(sim, cfg)
}

fn ptz_events(rec: &common::Recorder, tile: u64) -> Vec<serde_json::Value> {
    rec.events_of("ptz")
        .into_iter()
        .filter(|e| e["tile"] == tile)
        .collect()
}

/// `action/state/reason` of each `ptz` event of `tile`.
fn summary(rec: &common::Recorder, tile: u64) -> Vec<String> {
    ptz_events(rec, tile)
        .iter()
        .map(|e| {
            let mut s = format!(
                "{}/{}",
                e["action"].as_str().unwrap(),
                e["state"].as_str().unwrap()
            );
            if let Some(r) = e["reason"].as_str() {
                s.push('/');
                s.push_str(r);
            }
            s
        })
        .collect()
}

fn controls(nvr: &mock_nvr::MockNvr, channel: u8) -> Vec<String> {
    nvr.ptz_log()
        .iter()
        .filter(|e| e.channel == Some(channel))
        .filter_map(|e| e.control().map(str::to_owned))
        .collect()
}

fn moving(nvr: &mock_nvr::MockNvr, channel: u8) -> bool {
    nvr.ptz_state(channel).unwrap().moving.is_some()
}

#[test]
fn moves_report_events_and_stop_when_the_app_stops_renewing() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let tile = core.open_tile(dev, PTZ, StreamTier::Sub).unwrap();

    // Held for 2 s, renewed every 400 ms: one move sent, one event.
    core.ptz_move(tile, PtzDirection::Left, 20, MotionOrigin::Manual)
        .unwrap();
    for _ in 0..5 {
        sim.advance(Duration::from_millis(400));
        core.ptz_move(tile, PtzDirection::Left, 20, MotionOrigin::Manual)
            .unwrap();
    }
    sim_until(&sim, &core, "moving", 5, || moving(&nvr, PTZ));
    // Released: the stop follows the move, in order.
    core.ptz_stop(tile, MotionOrigin::Manual).unwrap();
    sim_until(&sim, &core, "stopped", 5, || {
        summary(&rec, tile) == ["move/sent", "stop/sent/released"]
    });
    assert!(!moving(&nvr, PTZ));
    assert_eq!(controls(&nvr, PTZ), ["left", "stop"]);

    // The app stops renewing (a frozen window): the core stops the camera
    // 1 s later and says why.
    core.ptz_move(tile, PtzDirection::Up, 30, MotionOrigin::Manual)
        .unwrap();
    sim_until(&sim, &core, "moving up", 5, || moving(&nvr, PTZ));
    let t0 = sim.now();
    sim_until(&sim, &core, "dead-man stop", 5, || !moving(&nvr, PTZ));
    let after = sim.now() - t0;
    assert!(after <= Duration::from_millis(1100), "{after:?}");
    sim_until(&sim, &core, "dead-man event", 5, || {
        summary(&rec, tile).last().map(String::as_str) == Some("stop/sent/deadman")
    });

    // Arguments are checked before anything is sent.
    for speed in [0, 65] {
        assert_eq!(
            core.ptz_move(tile, PtzDirection::Left, speed, MotionOrigin::Manual),
            Err(CoreError::InvalidArgument)
        );
    }
    assert_eq!(
        core.ptz_move(99, PtzDirection::Left, 20, MotionOrigin::Manual),
        Err(CoreError::UnknownId)
    );
    sim.run_free(|| core.shutdown());
    assert_eq!(nvr.ptz_config_requests(), 0);
}

#[test]
fn info_presets_and_lens_moves() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let tile = core.open_tile(dev, PTZ, StreamTier::Sub).unwrap();
    let lens = core.open_tile(dev, LENS, StreamTier::Sub).unwrap();

    let info: serde_json::Value =
        serde_json::from_str(&sim.run_free(|| core.ptz_info(tile)).unwrap()).unwrap();
    assert_eq!(info["pan_tilt"], true);
    assert_eq!(
        info["zoom"],
        serde_json::json!({"min": 1000, "max": 6000, "cur": 1000})
    );
    assert_eq!(
        info["presets"],
        serde_json::json!([{"id": 0, "name": "Preset 1"}, {"id": 1, "name": "Preset 2"}])
    );
    assert_eq!(info["speeds"], serde_json::json!([1, 64]));
    let info: serde_json::Value =
        serde_json::from_str(&sim.run_free(|| core.ptz_info(lens)).unwrap()).unwrap();
    assert_eq!(info["pan_tilt"], false);
    assert_eq!(info["focus"]["max"], 164);
    assert_eq!(info["presets"], serde_json::json!([]));

    core.ptz_preset(tile, 1, MotionOrigin::Manual).unwrap();
    core.ptz_preset(tile, 7, MotionOrigin::Manual).unwrap();
    sim_until(&sim, &core, "preset events", 5, || {
        summary(&rec, tile) == ["preset/sent", "preset/refused/unknown_preset"]
    });
    assert_eq!(nvr.ptz_state(PTZ).unwrap().last_preset, Some(1));

    core.ptz_lens(lens, LensAxis::Zoom, 50, MotionOrigin::Manual)
        .unwrap();
    core.ptz_move(lens, PtzDirection::Left, 20, MotionOrigin::Manual)
        .unwrap();
    sim_until(&sim, &core, "lens events", 5, || {
        ptz_events(&rec, lens).len() == 2
    });
    let e = ptz_events(&rec, lens);
    assert_eq!(
        (e[0]["action"].as_str(), e[0]["position"].as_i64()),
        (Some("lens"), Some(28))
    );
    assert_eq!(summary(&rec, lens)[1], "move/refused/not_supported");
    assert_eq!(nvr.ptz_state(LENS).unwrap().zoom, Some(28));
    sim.run_free(|| core.shutdown());
}

#[test]
fn automation_needs_a_grant() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let tile = core.open_tile(dev, PTZ, StreamTier::Sub).unwrap();
    let api = MotionOrigin::Automation("example-client".to_owned());
    core.ptz_preset(tile, 0, api.clone()).unwrap();
    sim_until(&sim, &core, "refused", 5, || {
        summary(&rec, tile) == ["preset/refused/not_granted"]
    });
    assert!(nvr.ptz_log().iter().all(|e| e.control().is_none()));
    assert_eq!(nvr.ptz_state(PTZ).unwrap().last_preset, None);

    for bad in ["", &"x".repeat(65), "a\nb"] {
        assert_eq!(
            core.set_motion_grant(dev, PTZ, bad, true),
            Err(CoreError::InvalidArgument)
        );
        assert_eq!(
            core.ptz_stop(tile, MotionOrigin::Automation(bad.to_owned())),
            Err(CoreError::InvalidArgument)
        );
    }
    assert_eq!(
        core.set_motion_grant(99, PTZ, "example-client", true),
        Err(CoreError::UnknownId)
    );
    core.set_motion_grant(dev, PTZ, "example-client", true)
        .unwrap();
    core.ptz_preset(tile, 0, api).unwrap();
    sim_until(&sim, &core, "sent", 5, || {
        summary(&rec, tile).last().map(String::as_str) == Some("preset/sent")
    });
    assert_eq!(nvr.ptz_state(PTZ).unwrap().last_preset, Some(0));
    sim.run_free(|| core.shutdown());
}

#[test]
fn tile_close_device_removal_and_shutdown_stop_the_camera() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();

    // Tile close (also what a closing window does to its tiles).
    let tile = core.open_tile(dev, PTZ, StreamTier::Sub).unwrap();
    core.ptz_move(tile, PtzDirection::Right, 20, MotionOrigin::Manual)
        .unwrap();
    sim_until(&sim, &core, "moving", 5, || moving(&nvr, PTZ));
    core.close_tile(tile).unwrap();
    // Well before the dead-man timer would have.
    sim_until(&sim, &core, "stopped on close", 1, || !moving(&nvr, PTZ));
    assert_eq!(controls(&nvr, PTZ), ["right", "stop"]);
    assert_eq!(summary(&rec, tile), ["move/sent"]);

    // Another tile on the same camera that never moved it: closing it
    // sends nothing.
    let a = core.open_tile(dev, PTZ, StreamTier::Sub).unwrap();
    let b = core.open_tile(dev, PTZ, StreamTier::Main).unwrap();
    core.ptz_move(a, PtzDirection::Down, 20, MotionOrigin::Manual)
        .unwrap();
    sim_until(&sim, &core, "moving down", 5, || moving(&nvr, PTZ));
    core.close_tile(b).unwrap();
    sim.advance(Duration::from_millis(100));
    assert!(moving(&nvr, PTZ));

    // Device removal.
    sim.run_free(|| core.remove_device(dev)).unwrap();
    sim_until(&sim, &core, "stopped on removal", 1, || !moving(&nvr, PTZ));
    assert_eq!(controls(&nvr, PTZ), ["right", "stop", "down", "stop"]);

    // Shutdown.
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let tile = core.open_tile(dev, PTZ, StreamTier::Sub).unwrap();
    core.ptz_move(tile, PtzDirection::Left, 20, MotionOrigin::Manual)
        .unwrap();
    sim_until(&sim, &core, "moving left", 5, || moving(&nvr, PTZ));
    sim.run_free(|| core.shutdown());
    assert!(!moving(&nvr, PTZ));
    assert_eq!(
        controls(&nvr, PTZ),
        ["right", "stop", "down", "stop", "left", "stop"]
    );
}

#[test]
fn a_second_lens_is_addressed_with_the_sub_channel() {
    use bc_protocol::ptz::Cam;
    let sim = SimRuntime::new();
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 2;
    // Channel 1 is a multi-lens camera whose second lens pans and tilts;
    // channel 0 is a plain pan-and-tilt camera.
    cfg.ptz_lens1.insert(1, PtzCamera::pan_tilt_zoom(2));
    cfg.ptz.insert(0, PtzCamera::pan_tilt_zoom(1));
    let nvr = sim_spawn(&sim, cfg);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let second = core.open_tile(dev, 1, StreamTier::Extern).unwrap();
    let plain = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    // The extern stream of a single-lens camera is its middle quality, not a
    // second lens: its tile steers the first lens.
    let middle = core.open_tile(dev, 0, StreamTier::Extern).unwrap();

    // The second lens's tile reads and moves lens 1.
    let info: serde_json::Value =
        serde_json::from_str(&sim.run_free(|| core.ptz_info(second)).unwrap()).unwrap();
    assert_eq!(info["presets"].as_array().unwrap().len(), 2);
    core.ptz_move(second, PtzDirection::Right, 20, MotionOrigin::Manual)
        .unwrap();
    sim_until(&sim, &core, "lens 1 moving", 5, || {
        nvr.ptz_state(Cam::with_lens(1, 1))
            .unwrap()
            .moving
            .is_some()
    });
    core.ptz_stop(second, MotionOrigin::Manual).unwrap();
    core.ptz_preset(second, 1, MotionOrigin::Manual).unwrap();
    core.ptz_move(plain, PtzDirection::Left, 20, MotionOrigin::Manual)
        .unwrap();
    core.ptz_stop(plain, MotionOrigin::Manual).unwrap();
    core.ptz_move(middle, PtzDirection::Up, 20, MotionOrigin::Manual)
        .unwrap();
    core.ptz_stop(middle, MotionOrigin::Manual).unwrap();
    sim_until(&sim, &core, "stopped and recalled", 5, || {
        summary(&rec, plain).len() == 2
            && summary(&rec, middle).len() == 2
            && summary(&rec, second).len() == 3
    });
    assert!(nvr.ptz_state(Cam::with_lens(1, 0)).is_none());
    assert_eq!(
        nvr.ptz_state(Cam::with_lens(1, 1)).unwrap().last_preset,
        Some(1)
    );

    // Every request of the second lens carried the lens, every request of
    // the plain camera did not, and all were accepted.
    let log = nvr.ptz_log();
    assert!(log.iter().any(|e| e.control() == Some("right")));
    for e in &log {
        match e.channel {
            Some(1) => assert_eq!(e.lens, 1, "{e:?}"),
            Some(0) => assert_eq!(e.lens, 0, "{e:?}"),
            other => panic!("unexpected channel {other:?}"),
        }
        assert_eq!(e.status, 200, "{e:?}");
    }
    sim.run_free(|| core.shutdown());
    assert_eq!(nvr.ptz_config_requests(), 0);
}
