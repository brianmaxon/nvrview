//! The second-lens probe against the mock's channel kinds (docs/protocol.md,
//! "Second lenses").

use bc_session::{ConnState, DeviceSession, LensProbe, SessionConfig, TcpConnector};
use mock_nvr::ExternMode;
use mock_nvr::fixture::Fixture;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_millis(800);

fn spawn_mock() -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 5;
    cfg.extern_modes = vec![
        ExternMode::Listed,
        ExternMode::Hidden,
        ExternMode::InfoOnly,
        ExternMode::Refused,
        ExternMode::Silent,
    ];
    mock_nvr::spawn(cfg).unwrap()
}

async fn connected(nvr: &mock_nvr::MockNvr) -> DeviceSession {
    let cfg = SessionConfig::new("example-user", "example-password");
    let s = DeviceSession::spawn(TcpConnector::new(nvr.addr()), cfg);
    let mut st = s.watch_state();
    tokio::time::timeout(
        Duration::from_secs(10),
        st.wait_for(|s| *s == ConnState::Connected),
    )
    .await
    .unwrap()
    .unwrap();
    s
}

/// Waits until every preview start has had its stop.
async fn all_stopped(nvr: &mock_nvr::MockNvr) {
    let until = Instant::now() + Duration::from_secs(5);
    while nvr.command_count(4) != nvr.command_count(3) {
        assert!(Instant::now() < until, "a probe left its preview running");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn each_kind_of_channel_gets_its_answer_and_every_probe_stops() {
    let nvr = spawn_mock();
    let s = connected(&nvr).await;
    let t = Instant::now();
    match s.probe_second_lens(1, WINDOW).await {
        LensProbe::SecondLens { after } => assert!(after < WINDOW),
        other => panic!("hidden second lens: {other:?}"),
    }
    assert!(t.elapsed() < WINDOW, "a found lens ends the probe early");
    all_stopped(&nvr).await;

    assert_eq!(
        s.probe_second_lens(2, WINDOW).await,
        LensProbe::NoVideo { info: true }
    );
    all_stopped(&nvr).await;

    // A 400 ends the probe at once: no third-party wait.
    let t = Instant::now();
    assert_eq!(
        s.probe_second_lens(3, WINDOW).await,
        LensProbe::Refused { status: 400 }
    );
    assert!(t.elapsed() < WINDOW);
    all_stopped(&nvr).await;

    assert_eq!(
        s.probe_second_lens(4, WINDOW).await,
        LensProbe::NoVideo { info: false }
    );
    all_stopped(&nvr).await;
    // One start per probe, no restarts.
    assert_eq!(nvr.command_count(3), 4);

    // The same probe again works: nothing was left behind.
    assert!(matches!(
        s.probe_second_lens(1, WINDOW).await,
        LensProbe::SecondLens { .. }
    ));
    all_stopped(&nvr).await;
    assert_eq!(nvr.active_logins(), 1);
    s.close().await;
}

#[tokio::test]
async fn a_session_that_is_not_logged_in_is_inconclusive() {
    let cfg = SessionConfig::new("example-user", "example-password");
    // Nothing listens on this port (a documentation address).
    let s = DeviceSession::spawn(TcpConnector::new("192.0.2.1:9".parse().unwrap()), cfg);
    assert_eq!(
        s.probe_second_lens(0, WINDOW).await,
        LensProbe::Inconclusive
    );
}
