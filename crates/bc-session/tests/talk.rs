//! Talk-back against the in-process mock NVR's doorbell model: permission,
//! capability checks, rate limit, pacing, and the close on every exit path.

use crate::common::mock_connector;
use bc_protocol::talk::{AdpcmEncoder, RECORD_DURATION_US, SAMPLES_PER_RECORD, decode_record};
use bc_session::{
    DeviceSession, Refusal, SessionConfig, StopReason, Talk, TalkObserver, TalkState,
};
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
    let s = session(&nvr, |c| c.talk_tail = Duration::ZERO);
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
    let s = session(&nvr, |c| c.talk_tail = Duration::ZERO);
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

/// Records of `log` that decode to silence from where they start.
fn silent_records(log: &TalkLog) -> usize {
    log.records
        .iter()
        .rev()
        .take_while(|r| r.decode().unwrap().samples.iter().all(|&x| x.abs() < 64))
        .count()
}

#[tokio::test(start_paused = true)]
async fn a_finished_talk_sends_a_silent_tail_before_the_close() {
    // The device plays about a second behind and drops what it has not
    // played at the close: the default tail keeps the talk open 1.5 s
    // after the last audio, sending silence at the same pace.
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    let mut talk = s.talk(BELL).await.unwrap();
    // A reply rendered to 3.5 s, pushed in one go.
    let src = tone(56_000);
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
    let audio = src.len().div_ceil(SAMPLES_PER_RECORD);
    // ceil(1500 / 64) silent records follow the audio.
    let tail = 24;
    assert_eq!(log.records.len(), audio + tail);
    assert_eq!(talk.stats().tail_records, tail as u64);
    assert_eq!(talk.stats().records_sent, (audio + tail) as u64);
    assert!(log.continuous());
    // The audio as pushed, then silence in the same codec.
    let mut with_tail = src.clone();
    with_tail.resize((audio + tail) * SAMPLES_PER_RECORD, 0);
    assert_eq!(log.samples().unwrap(), expected(&with_tail));
    assert!(silent_records(log) >= tail, "{}", silent_records(log));
    // Paced like the audio, and the close comes after the whole tail.
    let rec = Duration::from_micros(RECORD_DURATION_US);
    let last_audio = log.records[audio - 1].at;
    let last = log.records[audio + tail - 1].at;
    let span = last.duration_since(last_audio);
    assert!(
        span + Duration::from_millis(15) >= rec * tail as u32
            && span < rec * tail as u32 + Duration::from_millis(200),
        "{span:?}"
    );
    let closed = log.ended.unwrap();
    assert!(closed >= last);
    assert!(
        closed.duration_since(last_audio) >= Duration::from_millis(1500),
        "{:?}",
        closed.duration_since(last_audio)
    );
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_push_during_the_tail_continues_the_same_talk() {
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    let mut talk = s.talk(BELL).await.unwrap();
    let first = tone(SAMPLES_PER_RECORD * 4);
    talk.push(&first);
    talk.finish();
    // Wait until a few silent records went out.
    wait_log(&nvr, "tail started", |l| l[0].records.len() >= 8).await;
    assert_eq!(talk.state(), TalkState::Active);
    let second = tone(SAMPLES_PER_RECORD * 3);
    assert_eq!(talk.push(&second), second.len());
    talk.finish();
    assert_eq!(
        ended(&mut talk).await,
        TalkState::Stopped(StopReason::Finished)
    );
    wait_log(&nvr, "talk closed", |l| {
        l.len() == 1 && l[0].end == TalkEnd::Closed
    })
    .await;
    // One talk: one open, one close.
    assert_eq!(nvr.command_count(201), 1);
    assert_eq!(nvr.command_count(11), 1);
    let log = &nvr.talk_log()[0];
    assert!(log.continuous());
    // The second reply went out after some silence, then a full tail.
    let n = log.records.len();
    let tail = talk.stats().tail_records as usize;
    assert!(tail >= 4 + 24, "{tail}");
    assert_eq!(n, 4 + 3 + tail);
    assert_eq!(talk.stats().records_sent as usize, n);
    assert!(silent_records(log) >= 20, "{}", silent_records(log));
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn stopping_closes_at_once_without_a_tail() {
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    let mut talk = s.talk(BELL).await.unwrap();
    talk.push(&tone(16_000));
    wait_log(&nvr, "audio flowing", |l| l[0].records.len() >= 3).await;
    let at = Instant::now();
    talk.stop();
    assert_eq!(
        ended(&mut talk).await,
        TalkState::Stopped(StopReason::Stopped)
    );
    wait_log(&nvr, "closed", |l| l[0].end == TalkEnd::Closed).await;
    let log = &nvr.talk_log()[0];
    assert!(log.ended.unwrap().duration_since(at) < Duration::from_millis(100));
    assert_eq!(talk.stats().tail_records, 0);
    assert_eq!(silent_records(log), 0);
    s.close().await;
}

#[test]
fn the_default_limit_is_twenty_starts_a_minute() {
    let cfg = SessionConfig::new("example-user", "example-password");
    assert_eq!(cfg.talk_starts_per_minute, 20);
    assert_eq!(cfg.talk_tail, Duration::from_millis(1500));
}

async fn talk_once(s: &DeviceSession) -> Result<(), Refusal> {
    let mut t = s.talk(BELL).await?;
    t.stop();
    ended(&mut t).await;
    Ok(())
}

fn limited(r: Result<(), Refusal>) -> bool {
    matches!(r, Err(Refusal::RateLimited { .. }))
}

#[tokio::test(start_paused = true)]
async fn a_conversation_lifts_the_limit_until_it_ends() {
    let nvr = spawn_mock();
    let s = session(&nvr, |c| c.talk_starts_per_minute = 2);
    s.set_motion_allowed(BELL, true);
    // Outside a conversation: two, then refused.
    talk_once(&s).await.unwrap();
    talk_once(&s).await.unwrap();
    assert!(limited(talk_once(&s).await));
    // In a conversation: as many as the user wants.
    s.set_talk_conversation(BELL, true);
    assert!(s.talk_conversation(BELL));
    for _ in 0..5 {
        talk_once(&s).await.unwrap();
    }
    assert_eq!(nvr.command_count(201), 7);
    // Ended: the limit applies again (the exempt starts did not count, and
    // the first two are still within the minute).
    s.set_talk_conversation(BELL, false);
    assert!(limited(talk_once(&s).await));
    // A minute later the window is clear again.
    tokio::time::sleep(Duration::from_secs(61)).await;
    talk_once(&s).await.unwrap();
    talk_once(&s).await.unwrap();
    // Withdrawing the permission clears the mark.
    s.set_talk_conversation(BELL, true);
    s.set_motion_allowed(BELL, false);
    assert!(!s.talk_conversation(BELL));
    s.set_motion_allowed(BELL, true);
    assert!(limited(talk_once(&s).await));
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn an_observer_sees_the_sent_samples_and_the_silent_tail() {
    let nvr = spawn_mock();
    let s = session(&nvr, |_| {});
    s.set_motion_allowed(BELL, true);
    let obs = TalkObserver::new(1000);
    s.attach_talk_observer(BELL, &obs);
    let mut talk = s.talk(BELL).await.unwrap();
    let src = tone(5 * SAMPLES_PER_RECORD);
    assert_eq!(talk.push(&src), src.len());
    talk.finish();
    assert_eq!(
        ended(&mut talk).await,
        TalkState::Stopped(StopReason::Finished)
    );
    let recs = obs.drain();
    assert_eq!(recs.len() as u64, talk.stats().records_sent);
    let audio = 5;
    assert!(recs.len() > audio);
    let heard: Vec<i16> = recs[..audio]
        .iter()
        .flat_map(|r| r.samples.iter().copied())
        .collect();
    assert_eq!(heard, src);
    assert!(recs[..audio].iter().all(|r| !r.tail));
    // Paced one record apart on the core clock.
    for w in recs.windows(2) {
        assert_eq!(
            w[1].at.duration_since(w[0].at),
            Duration::from_micros(RECORD_DURATION_US)
        );
    }
    // The tail is silence and marked.
    assert!(recs[audio..].iter().all(|r| r.tail
        && r.samples.len() == SAMPLES_PER_RECORD
        && r.samples.iter().all(|&x| x == 0)));
    assert_eq!(obs.dropped(), 0);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_full_observer_drops_the_oldest_and_counts() {
    let nvr = spawn_mock();
    let s = session(&nvr, |c| c.talk_tail = Duration::ZERO);
    s.set_motion_allowed(BELL, true);
    let obs = TalkObserver::new(3);
    s.attach_talk_observer(BELL, &obs);
    let mut talk = s.talk(BELL).await.unwrap();
    let src = tone(8 * SAMPLES_PER_RECORD);
    talk.push(&src);
    talk.finish();
    ended(&mut talk).await;
    assert_eq!(obs.len(), 3);
    assert_eq!(obs.dropped(), 5);
    let recs = obs.drain();
    // The newest three survive.
    let heard: Vec<i16> = recs
        .iter()
        .flat_map(|r| r.samples.iter().copied())
        .collect();
    assert_eq!(heard, src[5 * SAMPLES_PER_RECORD..]);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn detaching_stops_delivery() {
    let nvr = spawn_mock();
    let s = session(&nvr, |c| c.talk_tail = Duration::ZERO);
    s.set_motion_allowed(BELL, true);
    let obs = TalkObserver::new(100);
    s.attach_talk_observer(BELL, &obs);
    let mut talk = s.talk(BELL).await.unwrap();
    talk.push(&tone(2 * SAMPLES_PER_RECORD));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(obs.len(), 2);
    s.detach_talk_observer(BELL);
    talk.push(&tone(2 * SAMPLES_PER_RECORD));
    talk.finish();
    ended(&mut talk).await;
    assert_eq!(talk.stats().records_sent, 4);
    assert_eq!(obs.len(), 2);
    s.close().await;
}
