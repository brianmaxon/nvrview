//! bc-cli's library against the in-process mock NVR.

use bc_cli::{CliError, RecordLimits, list_channels, record, wait_logged_in};
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, SessionConfig, TcpConnector};
use mock_nvr::fixture::Fixture;
use std::path::PathBuf;
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(10);

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name)
}

fn spawn_mock(fixture: &str, channels: u8) -> mock_nvr::MockNvr {
    let mut cfg = mock_nvr::Config::new(Fixture::load(&fixture_path(fixture)).unwrap());
    cfg.channels = channels;
    cfg.fps = 100;
    mock_nvr::spawn(cfg).unwrap()
}

fn session(nvr: &mock_nvr::MockNvr, password: &str) -> DeviceSession {
    DeviceSession::spawn(
        TcpConnector::new(nvr.addr()),
        SessionConfig::new("example-user", password),
    )
}

#[tokio::test]
async fn lists_channels_with_names() {
    let nvr = spawn_mock("h264-basic.h264", 3);
    let s = session(&nvr, "example-password");
    let listing = list_channels(&s, LIMIT).await.unwrap();
    assert!(!listing.device.model.is_empty());
    assert_eq!(listing.channels.len(), 3);
    for (i, c) in listing.channels.iter().enumerate() {
        assert_eq!(usize::from(c.channel), i);
        assert!(c.connected);
        assert!(c.tiers.contains(&StreamTier::Sub));
        assert_eq!(c.version.as_ref().unwrap().channel, Some(c.channel));
    }
    s.close().await;
    // The session's one login is released.
    for _ in 0..50 {
        if nvr.active_logins() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("login slot not released");
}

/// The recorded file is the fixture's elementary stream from its first
/// keyframe on, byte for byte, looping.
async fn records_fixture(name: &str) {
    let nvr = spawn_mock(name, 2);
    let fixture = Fixture::load(&fixture_path(name)).unwrap();
    let s = session(&nvr, "example-password");
    wait_logged_in(&s, LIMIT).await.unwrap();
    let mut out = Vec::new();
    let frames = 30;
    let stats = record(
        &s,
        1,
        StreamTier::Sub,
        &mut out,
        Default::default(),
        RecordLimits {
            frames: Some(frames),
        },
        std::future::pending(),
    )
    .await
    .unwrap();
    s.close().await;

    assert_eq!(stats.codec, Some(fixture.codec()));
    assert_eq!(stats.frames, frames);
    assert!(stats.keyframes >= 1);
    let expected: Vec<u8> = fixture.frames()[fixture.first_keyframe()..]
        .iter()
        .cycle()
        .take(frames as usize)
        .flat_map(|f| f.data.iter().copied())
        .collect();
    assert_eq!(out.len(), expected.len());
    assert!(out == expected, "recorded bytes differ from the fixture");
}

#[tokio::test]
async fn records_h264_from_the_first_keyframe() {
    records_fixture("h264-leading-p.h264").await;
}

#[tokio::test]
async fn records_h265_from_the_first_keyframe() {
    records_fixture("h265-leading-p.h265").await;
}

#[tokio::test]
async fn stops_when_asked() {
    let nvr = spawn_mock("h264-basic.h264", 1);
    let s = session(&nvr, "example-password");
    wait_logged_in(&s, LIMIT).await.unwrap();
    let mut out = Vec::new();
    let stats = record(
        &s,
        0,
        StreamTier::Main,
        &mut out,
        Default::default(),
        RecordLimits::default(),
        tokio::time::sleep(Duration::from_millis(500)),
    )
    .await
    .unwrap();
    s.close().await;
    assert!(stats.frames > 0);
    assert_eq!(out.len() as u64, stats.bytes);
}

#[tokio::test]
async fn refused_login_is_reported_and_not_retried() {
    let nvr = spawn_mock("h264-basic.h264", 1);
    let s = session(&nvr, "wrong-password");
    match wait_logged_in(&s, LIMIT).await {
        Err(CliError::LoginRefused { status: 401, .. }) => {}
        other => panic!("{other:?}"),
    }
    s.close().await;
}
