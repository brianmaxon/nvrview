//! Talk-back through the core against the mock NVR's doorbell: the
//! per-camera permission, `talk` events, the app's 48 kHz float audio
//! arriving at the device as decodable 16 kHz ADPCM, and the close on stop,
//! tile close, and permission withdrawal.

mod common;

use common::{PASSWORD, USER, fast_config, start, wait_until};
use mock_nvr::talk::TalkEnd;
use std::path::PathBuf;
use viewer_core::{CoreError, StreamTier};

/// The doorbell's channel; channel 0 has no speaker.
const BELL: u8 = 1;

fn mock() -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 2;
    cfg.talk_channels = vec![BELL];
    mock_nvr::spawn(cfg).unwrap()
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
    let nvr = mock();
    let (core, rec) = start(fast_config());
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let tile = core.open_tile(dev, BELL, StreamTier::Sub).unwrap();

    // Off by default: refused with a clear status, nothing sent.
    assert_eq!(core.talk_start(tile), Err(CoreError::NotPermitted));
    wait_until("refused event", 5, || states(&rec, tile) == ["refused"]);
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
    wait_until("active", 10, || {
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
    wait_until("stopped", 10, || {
        states(&rec, tile).last().map(String::as_str) == Some("stopped")
    });
    let last = talk_events(&rec, tile).pop().unwrap();
    assert_eq!(last["reason"], "finished");
    wait_until("closed at the device", 5, || {
        nvr.talk_log()
            .first()
            .is_some_and(|t| t.end == TalkEnd::Closed)
    });
    let log = &nvr.talk_log()[0];
    assert_eq!(log.channel, BELL);
    assert!(log.continuous());
    // 1 s at 48 kHz is 16000 samples at 16 kHz: 15 or 16 records,
    // depending on the resampler's delay.
    let n = log.records.len();
    assert!((15..=16).contains(&n), "{n} records");
    assert_eq!(last["sent_ms"], (n * 64) as u64);
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
    wait_until("not supported", 10, || {
        states(&rec, other).last().map(String::as_str) == Some("refused")
    });
    assert_eq!(
        talk_events(&rec, other).pop().unwrap()["reason"],
        "not_supported"
    );
    assert_eq!(nvr.command_count(201), 1);
    core.shutdown();
}

#[test]
fn closing_the_tile_or_withdrawing_permission_closes_the_talk() {
    let nvr = mock();
    let (core, rec) = start(fast_config());
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    core.set_talk_permission(dev, BELL, true).unwrap();

    let tile = core.open_tile(dev, BELL, StreamTier::Sub).unwrap();
    core.talk_start(tile).unwrap();
    wait_until("active", 10, || {
        states(&rec, tile).last().map(String::as_str) == Some("active")
    });
    core.talk_push(tile, &tone(16_000, 300.0, 8000), 16_000)
        .unwrap();
    core.close_tile(tile).unwrap();
    wait_until("closed at the device", 5, || {
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
    wait_until("active again", 10, || {
        states(&rec, tile).last().map(String::as_str) == Some("active")
    });
    core.talk_push(tile, &tone(16_000, 300.0, 8000), 16_000)
        .unwrap();
    core.set_talk_permission(dev, BELL, false).unwrap();
    wait_until("stopped on revoke", 10, || {
        states(&rec, tile).last().map(String::as_str) == Some("stopped")
    });
    assert_eq!(
        talk_events(&rec, tile).pop().unwrap()["reason"],
        "permission_revoked"
    );
    wait_until("second closed", 5, || {
        nvr.talk_log()
            .get(1)
            .is_some_and(|t| t.end == TalkEnd::Closed)
    });
    assert_eq!(core.talk_start(tile), Err(CoreError::NotPermitted));

    // Shutting down with a talk open closes it too.
    core.set_talk_permission(dev, BELL, true).unwrap();
    core.talk_start(tile).unwrap();
    wait_until("active a third time", 10, || {
        states(&rec, tile).last().map(String::as_str) == Some("active")
    });
    core.shutdown();
    wait_until("third closed", 5, || {
        nvr.talk_log()
            .get(2)
            .is_some_and(|t| t.end == TalkEnd::Closed)
    });
}
