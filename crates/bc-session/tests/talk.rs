//! Talk-back against the in-process mock NVR's doorbell model: permission,
//! capability checks, rate limit, pacing, and the close on every exit path.

mod common;

use bc_protocol::talk::{AdpcmEncoder, RECORD_DURATION_US, SAMPLES_PER_RECORD, decode_record};
use bc_session::{DeviceSession, Refusal, SessionConfig, StopReason, Talk, TalkState};
use common::mock_connector;
use mock_nvr::fixture::Fixture;
use mock_nvr::talk::{TalkEnd, TalkLog};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::Instant;

/// The doorbell's channel; channel 0 has no speaker.
const BELL: u8 = 1;

fn spawn_mock() -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 3;
    cfg.talk_channels = vec![BELL];
    mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current())
}

fn session(nvr: &mock_nvr::MockNvr, tweak: impl FnOnce(&mut SessionConfig)) -> DeviceSession {
    let mut cfg = SessionConfig::new("example-user", "example-password");
    cfg.query_timeout = Duration::from_secs(5);
    tweak(&mut cfg);
    DeviceSession::spawn(mock_connector(nvr), cfg)
}

fn tone(n: usize) -> Vec<i16> {
    (0..n)
        .map(|i| ((i as f64 * 2.0 * std::f64::consts::PI * 440.0 / 16_000.0).sin() * 7000.0) as i16)
        .collect()
}

/// What the device should decode from `src` sent as records (the last one
/// padded with silence).
fn expected(src: &[i16]) -> Vec<i16> {
    let mut enc = AdpcmEncoder::new();
    let mut out = Vec::new();
    for chunk in src.chunks(SAMPLES_PER_RECORD) {
        let mut block = [0i16; SAMPLES_PER_RECORD];
        block[..chunk.len()].copy_from_slice(chunk);
        out.extend(decode_record(&enc.encode_record(&block)).unwrap().samples);
    }
    out
}

/// Waits for the mock's talk log to satisfy `f`.
async fn wait_log(nvr: &mock_nvr::MockNvr, what: &str, f: impl Fn(&[TalkLog]) -> bool) {
    let t0 = Instant::now();
    while !f(&nvr.talk_log()) {
        assert!(t0.elapsed() < Duration::from_secs(5), "{what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn ended(talk: &mut Talk) -> TalkState {
    tokio::time::timeout(Duration::from_secs(10), talk.ended())
        .await
        .expect("talk did not end")
}

#[tokio::test(start_paused = true)]
async fn refused_without_permission_and_nothing_reaches_the_device() {
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    assert!(!s.motion_allowed(BELL));
    assert_eq!(s.talk(BELL).await.unwrap_err(), Refusal::NotPermitted);
    // Not even the read-only checks went out.
    assert_eq!(nvr.command_count(10), 0);
    assert_eq!(nvr.command_count(201), 0);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_channel_without_a_speaker_is_refused_before_any_talk_command() {
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(0, true);
    assert_eq!(s.talk(0).await.unwrap_err(), Refusal::NotSupported);
    assert_eq!(nvr.command_count(201), 0);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn audio_arrives_decodable_and_in_real_time_then_finishes_with_a_close() {
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    let mut talk = s.talk(BELL).await.unwrap();
    assert_eq!(talk.state(), TalkState::Active);
    assert_eq!(nvr.command_count(10), 1);
    // A text-to-speech reply pushed in one go: 1.5 s, not a whole number
    // of records.
    let src = tone(24_000);
    assert_eq!(talk.push(&src), src.len());
    talk.finish();
    assert_eq!(
        ended(&mut talk).await,
        TalkState::Stopped(StopReason::Finished)
    );
    wait_log(&nvr, "talk closed", |l| {
        l.len() == 1 && l[0].end == TalkEnd::Closed
    })
    .await;
    let log = &nvr.talk_log()[0];
    let n = src.len().div_ceil(SAMPLES_PER_RECORD);
    assert_eq!(log.records.len(), n);
    assert_eq!(talk.stats().records_sent, n as u64);
    // The close waited for the device to acknowledge every record.
    assert_eq!(talk.stats().records_acked, n as u64);
    assert!(log.continuous());
    assert_eq!(log.samples().unwrap(), expected(&src));
    // Paced by a 64 ms clock: never ahead of it, and the whole span within
    // a generous margin of real time (the test machine may be loaded).
    let t0 = log.records[0].at;
    let rec = Duration::from_micros(RECORD_DURATION_US);
    for (i, r) in log.records.iter().enumerate() {
        let ideal = rec * i as u32;
        let at = r.at.duration_since(t0);
        assert!(
            at + Duration::from_millis(15) >= ideal,
            "record {i} at {at:?}, before {ideal:?}"
        );
    }
    let span = log.records[n - 1].at.duration_since(t0);
    let ideal = rec * (n as u32 - 1);
    assert!(
        span < ideal + Duration::from_millis(400),
        "{span:?} vs {ideal:?}"
    );
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn dropping_the_talk_sends_the_close() {
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    let talk = s.talk(BELL).await.unwrap();
    talk.push(&tone(4096));
    drop(talk);
    wait_log(&nvr, "closed after drop", |l| {
        l.len() == 1 && l[0].end == TalkEnd::Closed
    })
    .await;
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn stop_and_session_close_both_send_the_close() {
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    let mut talk = s.talk(BELL).await.unwrap();
    talk.push(&tone(2048));
    talk.stop();
    assert_eq!(
        ended(&mut talk).await,
        TalkState::Stopped(StopReason::Stopped)
    );
    wait_log(&nvr, "first closed", |l| {
        l.len() == 1 && l[0].end == TalkEnd::Closed
    })
    .await;
    // Keep the second talk open while the session closes.
    let mut talk = s.talk(BELL).await.unwrap();
    talk.push(&tone(16_000));
    s.close().await;
    assert_eq!(
        ended(&mut talk).await,
        TalkState::Stopped(StopReason::SessionClosed)
    );
    wait_log(&nvr, "second closed", |l| {
        l.len() == 2 && l[1].end == TalkEnd::Closed
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_device_error_ends_the_talk_with_a_close() {
    let nvr = spawn_mock();
    nvr.set_talk_audio_status(500);
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    let mut talk = s.talk(BELL).await.unwrap();
    talk.push(&tone(4096));
    assert_eq!(
        ended(&mut talk).await,
        TalkState::Stopped(StopReason::DeviceError(500))
    );
    wait_log(&nvr, "closed after error", |l| {
        l.len() == 1 && l[0].end == TalkEnd::Closed
    })
    .await;
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_open_reports_the_status() {
    let nvr = spawn_mock();
    nvr.set_talk_open_status(503);
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    assert_eq!(s.talk(BELL).await.unwrap_err(), Refusal::DeviceRefused(503));
    assert!(nvr.talk_log().is_empty());
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn starts_are_rate_limited_per_channel() {
    let nvr = spawn_mock();
    let s = session(&nvr, |c| c.talk_starts_per_minute = 2);
    s.set_motion_allowed(BELL, true);
    for _ in 0..2 {
        let mut t = s.talk(BELL).await.unwrap();
        t.stop();
        ended(&mut t).await;
    }
    match s.talk(BELL).await.unwrap_err() {
        Refusal::RateLimited { retry_after } => {
            assert!(
                retry_after > Duration::from_secs(50) && retry_after <= Duration::from_secs(60)
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(nvr.command_count(201), 2);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn one_talk_per_channel() {
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    let first = s.talk(BELL).await.unwrap();
    assert_eq!(s.talk(BELL).await.unwrap_err(), Refusal::Busy);
    drop(first);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn withdrawing_permission_stops_the_talk() {
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    let mut talk = s.talk(BELL).await.unwrap();
    talk.push(&tone(16_000));
    s.set_motion_allowed(BELL, false);
    assert_eq!(
        ended(&mut talk).await,
        TalkState::Stopped(StopReason::PermissionRevoked)
    );
    wait_log(&nvr, "closed after revoke", |l| {
        l.len() == 1 && l[0].end == TalkEnd::Closed
    })
    .await;
    assert_eq!(s.talk(BELL).await.unwrap_err(), Refusal::NotPermitted);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn an_idle_talk_stops_and_the_buffer_is_bounded() {
    let nvr = spawn_mock();
    let s = session(&nvr, |c| {
        c.talk_idle_timeout = Duration::from_millis(300);
        c.talk_buffer = 4096;
    });
    s.set_motion_allowed(BELL, true);
    let mut talk = s.talk(BELL).await.unwrap();
    let accepted = talk.push(&tone(10_000));
    assert!(accepted <= 4096, "{accepted}");
    assert_eq!(talk.stats().samples_dropped, (10_000 - accepted) as u64);
    assert_eq!(ended(&mut talk).await, TalkState::Stopped(StopReason::Idle));
    wait_log(&nvr, "closed when idle", |l| {
        l.len() == 1 && l[0].end == TalkEnd::Closed
    })
    .await;
    // Only the whole records of what was accepted went out.
    assert_eq!(
        nvr.talk_log()[0].records.len(),
        accepted / SAMPLES_PER_RECORD
    );
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_lost_connection_ends_the_talk() {
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    let mut talk = s.talk(BELL).await.unwrap();
    talk.push(&tone(16_000));
    nvr.shutdown();
    assert_eq!(
        ended(&mut talk).await,
        TalkState::Stopped(StopReason::ConnectionLost)
    );
    // Nothing more is accepted.
    assert_eq!(talk.push(&tone(100)), 0);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn microphone_style_pushes_go_out_as_they_fill_records() {
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    let mut talk = s.talk(BELL).await.unwrap();
    // 20 ms chunks every 20 ms for 1.28 s: 20 records' worth.
    let src = tone(SAMPLES_PER_RECORD * 20);
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    for chunk in src.chunks(320) {
        tick.tick().await;
        assert_eq!(talk.push(chunk), chunk.len());
    }
    talk.finish();
    assert_eq!(
        ended(&mut talk).await,
        TalkState::Stopped(StopReason::Finished)
    );
    wait_log(&nvr, "closed", |l| {
        l.len() == 1 && l[0].end == TalkEnd::Closed
    })
    .await;
    let log = &nvr.talk_log()[0];
    assert_eq!(log.records.len(), 20);
    assert_eq!(log.samples().unwrap(), expected(&src));
    let span = log.records[19].at.duration_since(log.records[0].at);
    // 19 gaps of 64 ms, give or take the 20 ms push granularity and load.
    assert!(
        span > Duration::from_millis(1100) && span < Duration::from_millis(1600),
        "{span:?}"
    );
    s.close().await;
}
