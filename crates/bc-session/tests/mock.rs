//! Info queries and the channel list, against the in-process mock NVR.

mod common;

use bc_protocol::session::Command;
use bc_session::{DeviceSession, QueryError, SessionConfig};
use common::mock_connector;
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
    mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current())
}

fn session(nvr: &mock_nvr::MockNvr, password: &str) -> DeviceSession {
    let mut cfg = SessionConfig::new("example-user", password);
    cfg.query_timeout = Duration::from_secs(5);
    DeviceSession::spawn(mock_connector(nvr), cfg)
}

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
async fn queries_fail_when_the_login_is_refused() {
    let nvr = spawn_mock(1);
    let s = session(&nvr, "wrong-password");
    assert_eq!(
        s.query(Command::VersionInfo, None).await,
        Err(QueryError::Closed)
    );
    s.close().await;
}

async fn next(
    rx: &mut tokio::sync::broadcast::Receiver<std::sync::Arc<bc_session::AlarmPush>>,
) -> std::sync::Arc<bc_session::AlarmPush> {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("an alarm push in time")
        .expect("the session is open")
}

#[tokio::test(start_paused = true)]
async fn detection_pushes_arrive_after_the_subscription() {
    let mut cfg = mock_nvr::Config::new(fixture());
    cfg.channels = 3;
    let nvr = mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current());
    let s = session(&nvr, "example-password");
    let mut alarms = s.alarms();
    // The first push after 192 is a full, idle snapshot.
    let first = next(&mut alarms).await;
    assert_eq!(first.connection, 1);
    assert_eq!(first.states.len(), 3);
    assert!(first.states.iter().all(|st| st.is_idle()));
    assert_eq!(nvr.alarm_subscribers(), 1);
    assert_eq!(nvr.command_count(192), 1);

    assert!(nvr.set_alarm(2, "MD", "people"));
    let p = next(&mut alarms).await;
    assert!(p.states[2].motion());
    assert_eq!(p.states[2].ai_types, vec!["people"]);
    assert!(p.states[0].is_idle());
    // No change, no push; then back to idle.
    assert!(nvr.set_alarm(2, "MD", "people"));
    assert!(nvr.set_alarm(2, "", ""));
    let p = next(&mut alarms).await;
    assert!(p.states[2].is_idle());
    assert!(!nvr.set_alarm(9, "MD", ""));
    s.close().await;
    // The mock forgets the subscription when its side of the session ends.
    let end = tokio::time::Instant::now() + Duration::from_secs(5);
    while nvr.alarm_subscribers() != 0 {
        assert!(
            tokio::time::Instant::now() < end,
            "subscription not dropped"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn no_detection_subscription_when_turned_off() {
    let nvr = spawn_mock(1);
    let mut cfg = SessionConfig::new("example-user", "example-password");
    cfg.alarm_report = false;
    let s = DeviceSession::spawn(mock_connector(&nvr), cfg);
    // A query round trip proves the session is logged in.
    s.query(Command::VersionInfo, None).await.unwrap();
    assert_eq!(nvr.command_count(192), 0);
    assert_eq!(nvr.alarm_subscribers(), 0);
    s.close().await;
}

/// A subscription made while the session ends for good (the login is
/// refused) must end too, whichever side wins the race: the supervisor
/// closing its request channel, or the subscriber's request landing in it.
/// The subscriber runs on its own thread so the two really are concurrent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_subscription_racing_a_refused_login_still_ends() {
    use bc_protocol::stream::StreamTier;
    let rt = tokio::runtime::Handle::current();
    for _ in 0..300 {
        let nvr = spawn_mock(1);
        let s = std::sync::Arc::new(session(&nvr, "wrong-password"));
        let subscriber = {
            let (s, rt) = (s.clone(), rt.clone());
            std::thread::spawn(move || {
                let mut sub = rt.block_on(s.subscribe(0, StreamTier::Sub)).ok()?;
                // The 60 s only bounds a hang if the bug is back; a correct
                // run ends as soon as the refusal is processed.
                Some(rt.block_on(async {
                    tokio::time::timeout(Duration::from_secs(60), sub.recv()).await
                }))
            })
        };
        let ended = tokio::task::spawn_blocking(move || subscriber.join().unwrap())
            .await
            .unwrap();
        if let Some(r) = ended {
            assert!(r.expect("the subscription ended").is_none());
        }
    }
}
