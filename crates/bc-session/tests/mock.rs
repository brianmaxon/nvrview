//! Info queries and the channel list, against the in-process mock NVR.

use bc_protocol::session::Command;
use bc_session::{DeviceSession, QueryError, SessionConfig, TcpConnector};
use mock_nvr::fixture::Fixture;
use std::path::PathBuf;
use std::time::Duration;

fn fixture() -> Fixture {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    Fixture::load(&path).unwrap()
}

fn spawn_mock(channels: u8) -> mock_nvr::MockNvr {
    let mut cfg = mock_nvr::Config::new(fixture());
    cfg.channels = channels;
    mock_nvr::spawn(cfg).unwrap()
}

fn session(nvr: &mock_nvr::MockNvr, password: &str) -> DeviceSession {
    let mut cfg = SessionConfig::new("example-user", password);
    cfg.query_timeout = Duration::from_secs(5);
    DeviceSession::spawn(TcpConnector::new(nvr.addr()), cfg)
}

#[tokio::test]
async fn queries_are_answered_and_the_channel_list_arrives() {
    let nvr = spawn_mock(3);
    let s = session(&nvr, "example-password");

    // Sent before login completes: held until the session is logged in.
    let v = s.query(Command::VersionInfo, None).await.unwrap();
    assert_eq!(v.status, 200);
    assert!(
        !bc_protocol::xml::parse_version_info(&v.xml)
            .unwrap()
            .model
            .is_empty()
    );

    let c = s.query(Command::ChannelVersionInfo, Some(2)).await.unwrap();
    assert_eq!(c.status, 200);
    assert_eq!(
        bc_protocol::xml::parse_version_info(&c.xml)
            .unwrap()
            .channel,
        Some(2)
    );

    // Status 300 with a body, as real devices answer 146.
    let si = s.query(Command::StreamInfoList, None).await.unwrap();
    assert_eq!(si.status, 300);
    assert!(si.xml.contains("<StreamInfo>"));

    let mut channels = s.watch_channels();
    let list = tokio::time::timeout(Duration::from_secs(5), channels.wait_for(Option::is_some))
        .await
        .unwrap()
        .unwrap()
        .clone()
        .unwrap();
    assert_eq!(list.len(), 3);
    assert!(list.iter().all(|c| c.connected));
    s.close().await;
}

#[tokio::test]
async fn non_query_commands_are_refused_locally() {
    let nvr = spawn_mock(1);
    let s = session(&nvr, "example-password");
    for cmd in [
        Command::Login,
        Command::PreviewStart,
        Command::PreviewStop,
        Command::Keepalive,
    ] {
        assert_eq!(s.query(cmd, None).await, Err(QueryError::NotAllowed));
    }
    assert_eq!(
        s.query(Command::ChannelVersionInfo, None).await,
        Err(QueryError::NotAllowed)
    );
    s.close().await;
}

#[tokio::test]
async fn queries_fail_when_the_login_is_refused() {
    let nvr = spawn_mock(1);
    let s = session(&nvr, "wrong-password");
    assert_eq!(
        s.query(Command::VersionInfo, None).await,
        Err(QueryError::Closed)
    );
    s.close().await;
}
