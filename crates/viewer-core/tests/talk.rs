//! Talk-back through the core against the mock NVR's doorbell: the
//! per-camera permission, `talk` events, the app's 48 kHz float audio
//! arriving at the device as decodable 16 kHz ADPCM, and the close on stop,
//! tile close, and permission withdrawal. On a simulated clock: the talk's
//! pacing and the device's acknowledgments take simulated time.

mod common;

use common::{PASSWORD, USER, fast_config, on_sim, sim_spawn, sim_until, start};
use mock_nvr::talk::TalkEnd;
use simtime::SimRuntime;
use std::path::PathBuf;
use viewer_core::{CoreError, StreamTier};

/// The doorbell's channel; channel 0 has no speaker.
const BELL: u8 = 1;

fn mock(sim: &SimRuntime) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 2;
    cfg.talk_channels = vec![BELL];
    sim_spawn(sim, cfg)
}

fn tone(rate: u32, freq: f64, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            (0.4 * (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(rate)).sin()) as f32
        })
        .collect()
}

fn talk_events(rec: &common::Recorder, tile: u64) -> Vec<serde_json::Value> {
    rec.events_of("talk")
        .into_iter()
        .filter(|e| e["tile"] == tile)
        .collect()
}

fn states(rec: &common::Recorder, tile: u64) -> Vec<String> {
    talk_events(rec, tile)
        .iter()
        .map(|e| e["state"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn talk_back_needs_the_permission_and_reaches_the_doorbell() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let tile = core.open_tile(dev, BELL, StreamTier::Sub).unwrap();

    // Off by default: refused with a clear status, nothing sent.
    assert_eq!(core.talk_start(tile), Err(CoreError::NotPermitted));
    sim_until(&sim, &core, "refused event", 5, || {
        states(&rec, tile) == ["refused"]
    });
    assert_eq!(talk_events(&rec, tile)[0]["reason"], "not_permitted");
    assert_eq!(nvr.command_count(201), 0);
    assert_eq!(nvr.command_count(10), 0);

    // Enabled: starting, active.
    assert_eq!(
        core.set_talk_permission(99, BELL, true),
        Err(CoreError::UnknownId)
    );
    core.set_talk_permission(dev, BELL, true).unwrap();
    core.talk_start(tile).unwrap();
    // Audio pushed while starting is kept and sent once active.
    let src = tone(48_000, 500.0, 48_000);
    core.talk_push(tile, &src[..9600], 48_000).unwrap();
    sim_until(&sim, &core, "active", 10, || {
        states(&rec, tile).last().map(String::as_str) == Some("active")
    });
    assert_eq!(states(&rec, tile), ["refused", "starting", "active"]);
    for chunk in src[9600..].chunks(960) {
        core.talk_push(tile, chunk, 48_000).unwrap();
    }
    assert_eq!(
        core.talk_push(tile, &src[..10], 12),
        Err(CoreError::InvalidArgument)
    );
    core.talk_stop(tile, true).unwrap();
    sim_until(&sim, &core, "stopped", 10, || {
        states(&rec, tile).last().map(String::as_str) == Some("stopped")
    });
    let last = talk_events(&rec, tile).pop().unwrap();
    assert_eq!(last["reason"], "finished");
    sim_until(&sim, &core, "closed at the device", 5, || {
        nvr.talk_log()
            .first()
            .is_some_and(|t| t.end == TalkEnd::Closed)
    });
    let log = &nvr.talk_log()[0];
    assert_eq!(log.channel, BELL);
    assert!(log.continuous());
    // 1 s at 48 kHz is 16000 samples at 16 kHz: 15 or 16 records,
    // depending on the resampler's delay; then the silent tail (1.5 s:
    // 24 records), so the device plays the end before the close.
    let tail = last["tail_ms"].as_u64().unwrap() as usize / 64;
    assert_eq!(tail, 24);
    let n = log.records.len() - tail;
    assert!((15..=16).contains(&n), "{n} records");
    assert_eq!(last["sent_ms"], (n * 64) as u64);
    let closed = log.ended.unwrap().duration_since(log.records[n - 1].at);
    assert!(
        closed >= std::time::Duration::from_millis(1500),
        "closed {closed:?} after the last audio"
    );
    assert!(
        log.records[n..].iter().skip(1).all(|r| r
            .decode()
            .unwrap()
            .samples
            .iter()
            .all(|x| x.abs() < 64))
    );
    // What arrived is the 500 Hz tone: check its frequency by counting
    // zero crossings in the middle of the decoded audio.
    let pcm = log.samples().unwrap();
    let mid = &pcm[2000..14000];
    let crossings = mid.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    let freq = crossings as f64 / 2.0 / (mid.len() as f64 / 16_000.0);
    assert!((freq - 500.0).abs() < 15.0, "{freq} Hz");
    let peak = mid.iter().map(|s| i32::from(*s).abs()).max().unwrap();
    assert!((10_000..=16_000).contains(&peak), "peak {peak}");

    // A channel without a speaker is refused by the session's checks.
    let other = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    core.set_talk_permission(dev, 0, true).unwrap();
    core.talk_start(other).unwrap();
    sim_until(&sim, &core, "not supported", 10, || {
        states(&rec, other).last().map(String::as_str) == Some("refused")
    });
    assert_eq!(
        talk_events(&rec, other).pop().unwrap()["reason"],
        "not_supported"
    );
    assert_eq!(nvr.command_count(201), 1);
    sim.run_free(|| core.shutdown());
}

#[test]
fn closing_the_tile_or_withdrawing_permission_closes_the_talk() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    core.set_talk_permission(dev, BELL, true).unwrap();

    let tile = core.open_tile(dev, BELL, StreamTier::Sub).unwrap();
    core.talk_start(tile).unwrap();
    sim_until(&sim, &core, "active", 10, || {
        states(&rec, tile).last().map(String::as_str) == Some("active")
    });
    core.talk_push(tile, &tone(16_000, 300.0, 8000), 16_000)
        .unwrap();
    core.close_tile(tile).unwrap();
    sim_until(&sim, &core, "closed at the device", 5, || {
        nvr.talk_log()
            .first()
            .is_some_and(|t| t.end == TalkEnd::Closed)
    });
    assert_eq!(states(&rec, tile), ["starting", "active", "stopped"]);
    assert_eq!(talk_events(&rec, tile)[2]["reason"], "tile_closed");
    // The stop came before the tile's own end.
    let pos =
        |pred: &dyn Fn(&serde_json::Value) -> bool| rec.events().iter().position(pred).unwrap();
    assert!(
        pos(&|e| e["type"] == "talk" && e["state"] == "stopped" && e["tile"] == tile)
            < pos(&|e| e["type"] == "tile_closed" && e["tile"] == tile)
    );
    assert_eq!(
        core.talk_push(tile, &[0.0; 10], 16_000),
        Err(CoreError::UnknownId)
    );

    let tile = core.open_tile(dev, BELL, StreamTier::Sub).unwrap();
    core.talk_start(tile).unwrap();
    sim_until(&sim, &core, "active again", 10, || {
        states(&rec, tile).last().map(String::as_str) == Some("active")
    });
    core.talk_push(tile, &tone(16_000, 300.0, 8000), 16_000)
        .unwrap();
    core.set_talk_permission(dev, BELL, false).unwrap();
    sim_until(&sim, &core, "stopped on revoke", 10, || {
        states(&rec, tile).last().map(String::as_str) == Some("stopped")
    });
    assert_eq!(
        talk_events(&rec, tile).pop().unwrap()["reason"],
        "permission_revoked"
    );
    sim_until(&sim, &core, "second closed", 5, || {
        nvr.talk_log()
            .get(1)
            .is_some_and(|t| t.end == TalkEnd::Closed)
    });
    assert_eq!(core.talk_start(tile), Err(CoreError::NotPermitted));

    // Shutting down with a talk open closes it too.
    core.set_talk_permission(dev, BELL, true).unwrap();
    core.talk_start(tile).unwrap();
    sim_until(&sim, &core, "active a third time", 10, || {
        states(&rec, tile).last().map(String::as_str) == Some("active")
    });
    sim.run_free(|| core.shutdown());
    sim_until(&sim, &core, "third closed", 5, || {
        nvr.talk_log()
            .get(2)
            .is_some_and(|t| t.end == TalkEnd::Closed)
    });
}

/// One start and stop on `tile`: the final state's reason (`stopped` when
/// it ran, else the refusal).
fn start_and_stop(
    sim: &SimRuntime,
    core: &viewer_core::Core,
    rec: &common::Recorder,
    tile: u64,
) -> String {
    let before = talk_events(rec, tile).len();
    core.talk_start(tile).unwrap();
    sim_until(sim, core, "talk started or refused", 10, || {
        talk_events(rec, tile)[before..]
            .iter()
            .any(|e| e["state"] == "active" || e["state"] == "refused")
    });
    core.talk_stop(tile, false).unwrap();
    sim_until(sim, core, "talk ended", 10, || {
        talk_events(rec, tile)[before..]
            .iter()
            .any(|e| e["state"] == "stopped" || e["state"] == "refused")
    });
    let last = talk_events(rec, tile).pop().unwrap();
    last["reason"].as_str().unwrap().to_owned()
}

#[test]
fn an_open_conversation_lifts_the_start_limit_until_it_ends() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let (core, rec) = start(on_sim(&sim, fast_config()));
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    core.set_talk_permission(dev, BELL, true).unwrap();
    let tile = core.open_tile(dev, BELL, StreamTier::Sub).unwrap();
    assert_eq!(
        core.set_talk_conversation(tile + 100, true),
        Err(CoreError::UnknownId)
    );

    // Outside a conversation: 20 starts a minute, then refused.
    for i in 0..20 {
        assert_eq!(
            start_and_stop(&sim, &core, &rec, tile),
            "stopped",
            "start {i}"
        );
    }
    assert_eq!(start_and_stop(&sim, &core, &rec, tile), "rate_limited");

    // A call is open: no limit.
    core.set_talk_conversation(tile, true).unwrap();
    assert!(core.talk_conversation(tile));
    for i in 0..5 {
        assert_eq!(
            start_and_stop(&sim, &core, &rec, tile),
            "stopped",
            "start {i}"
        );
    }
    // It ended: limited again.
    core.set_talk_conversation(tile, false).unwrap();
    assert!(!core.talk_conversation(tile));
    assert_eq!(start_and_stop(&sim, &core, &rec, tile), "rate_limited");

    // Withdrawing the permission ends the conversation's exemption.
    core.set_talk_conversation(tile, true).unwrap();
    core.set_talk_permission(dev, BELL, false).unwrap();
    assert!(!core.talk_conversation(tile));
    core.set_talk_permission(dev, BELL, true).unwrap();
    assert_eq!(start_and_stop(&sim, &core, &rec, tile), "rate_limited");

    // Closing the tile ends it too: a new tile on the camera is limited.
    core.set_talk_conversation(tile, true).unwrap();
    assert_eq!(start_and_stop(&sim, &core, &rec, tile), "stopped");
    core.close_tile(tile).unwrap();
    assert!(!core.talk_conversation(tile));
    let tile = core.open_tile(dev, BELL, StreamTier::Sub).unwrap();
    assert_eq!(start_and_stop(&sim, &core, &rec, tile), "rate_limited");
    assert_eq!(nvr.command_count(201), 26);
    sim.run_free(|| core.shutdown());
}
