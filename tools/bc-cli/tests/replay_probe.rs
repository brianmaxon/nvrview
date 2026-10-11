//! `replay-probe` against the in-process mock NVR on a paused clock: one
//! session, live view beside one to four plays, every play stopped and the
//! session's one login released on every path. How the mock refuses a play
//! is its own choice (docs/MILESTONE-P.md, B5, B6).

use bc_cli::replay_probe::{Outcome, ProbeConfig, replay_probe};
use bc_protocol::archive::DateTime;
use bc_session::{DeviceSession, FnConnector, SessionConfig};
use mock_nvr::archive::ArchiveConfig;
use mock_nvr::fixture::Fixture;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::Instant;

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn spawn_mock(tweak: impl FnOnce(&mut ArchiveConfig)) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 2;
    cfg.fps = 10;
    let mut a = ArchiveConfig::new(vec![d(8, 0, 0, 0)]);
    tweak(&mut a);
    cfg.archive = Some(a);
    mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current())
}

fn session(nvr: &mock_nvr::MockNvr) -> DeviceSession {
    let addr = nvr.addr();
    let mut cfg = SessionConfig::new("example-user", "example-password");
    cfg.max_plays = 4;
    cfg.query_timeout = Duration::from_secs(5);
    DeviceSession::spawn(FnConnector(move || mock_nvr::connect(addr)), cfg)
}

fn probe_cfg() -> ProbeConfig {
    let mut c = ProbeConfig::new(1);
    c.now = Some(d(8, 12, 0, 0));
    c
}

/// Closes the session and checks that nothing is left running on the
/// device and that the run used one login.
async fn closed_cleanly(nvr: &mock_nvr::MockNvr, s: DeviceSession) {
    s.close().await;
    for _ in 0..50 {
        if nvr.active_logins() == 0 && nvr.active_plays() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(nvr.active_plays(), 0, "plays left running");
    assert_eq!(nvr.active_logins(), 0, "login not released");
    assert!(
        nvr.play_log().iter().all(|p| p.speed == Some(1)),
        "normal speed only"
    );
}

#[tokio::test(start_paused = true)]
async fn four_stages_beside_live_view() {
    let nvr = spawn_mock(|_| {});
    let s = session(&nvr);
    let (_tx, rx) = watch::channel(false);
    let mut out = Vec::new();
    let t0 = Instant::now();
    let r = replay_probe(&s, &probe_cfg(), &mut out, rx).await.unwrap();
    assert_eq!(
        r.outcome,
        Outcome::Completed,
        "{}",
        String::from_utf8_lossy(&out)
    );
    assert!(t0.elapsed() < Duration::from_secs(90));
    assert_eq!(r.max_ok, 4);
    assert_eq!(r.stages.len(), 4);
    assert_eq!(r.plays_started, 1 + 2 + 3 + 4);
    assert!(r.baseline_live_fps >= 8.0, "{}", r.baseline_live_fps);
    assert!(r.tail_live_fps.unwrap() >= 8.0);
    for (i, st) in r.stages.iter().enumerate() {
        assert_eq!(st.plays, i + 1);
        assert_eq!(st.delivered, i + 1);
        assert!(st.seconds >= 7, "{st:?}");
        assert!(st.live_fps >= 8.0, "{st:?}");
        assert!(st.play_fps.iter().all(|f| *f > 0.0), "{st:?}");
        // Each play starts at the keyframe at or before its seek.
        assert!(
            st.start_offset.iter().all(|o| matches!(o, Some(-2..=0))),
            "{st:?}"
        );
    }
    // The most recent finished recording of at least a minute (60-minute
    // files: 10:00 to 11:00), seeks 5, 15, 25, 35 s into it.
    let log = nvr.play_log();
    assert_eq!(log.len(), 10);
    assert!(log.iter().all(|p| p.channel == 1));
    assert_eq!(log[9].at, d(8, 10, 0, 35));
    assert!(r.stops_sent >= 10, "{}", r.stops_sent);
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("stage 4 summary"), "{text}");
    closed_cleanly(&nvr, s).await;
}

/// A device that sends eight seconds at once and then nothing until real
/// time catches up is keeping pace, not stalled.
#[tokio::test(start_paused = true)]
async fn a_long_burst_is_not_a_stall() {
    let nvr = spawn_mock(|a| a.play_burst_secs = 8);
    let s = session(&nvr);
    let (_tx, rx) = watch::channel(false);
    let mut out = Vec::new();
    let r = replay_probe(&s, &probe_cfg(), &mut out, rx).await.unwrap();
    assert_eq!(
        r.outcome,
        Outcome::Completed,
        "{}",
        String::from_utf8_lossy(&out)
    );
    for st in &r.stages {
        assert!(st.video_secs.iter().all(|v| *v >= 7.0), "{st:?}");
    }
    closed_cleanly(&nvr, s).await;
}

#[tokio::test(start_paused = true)]
async fn stops_at_the_first_refusal() {
    let nvr = spawn_mock(|a| a.max_plays = Some(2));
    let s = session(&nvr);
    let (_tx, rx) = watch::channel(false);
    let mut out = Vec::new();
    let r = replay_probe(&s, &probe_cfg(), &mut out, rx).await.unwrap();
    assert_eq!(
        r.outcome,
        Outcome::Refused {
            stage: 3,
            command: 5,
            status: 400
        },
        "{}",
        String::from_utf8_lossy(&out)
    );
    assert_eq!(r.max_ok, 2);
    assert_eq!(r.stages.len(), 3);
    // No fourth stage, no tail.
    assert_eq!(r.plays_started, 1 + 2 + 3);
    assert!(r.tail_live_fps.is_none());
    closed_cleanly(&nvr, s).await;
}

#[tokio::test(start_paused = true)]
async fn an_interrupt_stops_every_play() {
    let nvr = spawn_mock(|_| {});
    let s = session(&nvr);
    let (tx, rx) = watch::channel(false);
    // In the middle of stage 3.
    let at = Instant::now() + Duration::from_secs(30);
    tokio::spawn(async move {
        tokio::time::sleep_until(at).await;
        let _ = tx.send(true);
    });
    let mut out = Vec::new();
    let r = replay_probe(&s, &probe_cfg(), &mut out, rx).await.unwrap();
    assert_eq!(r.outcome, Outcome::Interrupted);
    assert!(r.plays_started > 3 && r.plays_started < 10, "{r:?}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(nvr.active_plays(), 0);
    closed_cleanly(&nvr, s).await;
}

#[tokio::test(start_paused = true)]
async fn the_time_cap_ends_the_run() {
    let nvr = spawn_mock(|_| {});
    let s = session(&nvr);
    let (_tx, rx) = watch::channel(false);
    let mut cfg = probe_cfg();
    cfg.run_limit = Duration::from_secs(20);
    let t0 = Instant::now();
    let r = replay_probe(&s, &cfg, &mut Vec::new(), rx).await.unwrap();
    assert_eq!(r.outcome, Outcome::TimeCap);
    // The cap, plus the stops.
    assert!(t0.elapsed() < Duration::from_secs(30), "{:?}", t0.elapsed());
    closed_cleanly(&nvr, s).await;
}

#[tokio::test(start_paused = true)]
async fn no_recording_means_no_play() {
    let nvr = spawn_mock(|_| {});
    let s = session(&nvr);
    let (_tx, rx) = watch::channel(false);
    let mut cfg = probe_cfg();
    // Nothing on the day before, and nothing finished long enough ago.
    cfg.now = Some(d(8, 0, 1, 0));
    let r = replay_probe(&s, &cfg, &mut Vec::new(), rx).await.unwrap();
    assert_eq!(r.outcome, Outcome::NoRecording);
    assert_eq!(r.plays_started, 0);
    assert_eq!(nvr.command_count(5), 0);
    closed_cleanly(&nvr, s).await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_login_is_not_retried() {
    let nvr = spawn_mock(|_| {});
    let addr = nvr.addr();
    let s = DeviceSession::spawn(
        FnConnector(move || mock_nvr::connect(addr)),
        SessionConfig::new("example-user", "wrong-password"),
    );
    let (_tx, rx) = watch::channel(false);
    let r = replay_probe(&s, &probe_cfg(), &mut Vec::new(), rx).await;
    assert!(
        matches!(r, Err(bc_cli::CliError::LoginRefused { .. })),
        "{r:?}"
    );
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(matches!(
        *s.watch_state().borrow(),
        bc_session::ConnState::LoginFailed { .. }
    ));
    assert_eq!(nvr.command_count(5), 0);
    s.close().await;
}
