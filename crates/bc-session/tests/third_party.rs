//! A third-party (ONVIF) camera channel behind an NVR, against the mock:
//! previews answered with 400, then the stream on the message ID of a later
//! start (docs/protocol.md, "Records as the official client reads them").

mod common;

use bc_protocol::media::Record;
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, Item, SessionConfig, Subscription};
use common::mock_connector;
use mock_nvr::fixture::Fixture;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::Instant;

fn spawn_mock(refusals: u32) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 3;
    cfg.fps = 50;
    let mut tp = mock_nvr::ThirdPartyConfig::new(vec![1], refusals);
    tp.media_delay = Duration::from_millis(100);
    cfg.third_party = Some(tp);
    mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current())
}

fn session(nvr: &mock_nvr::MockNvr, retry_ms: u64, limit_ms: u64) -> DeviceSession {
    let mut cfg = SessionConfig::new("example-user", "example-password");
    cfg.preview_retry_interval = Duration::from_millis(retry_ms);
    cfg.preview_wait_limit = Duration::from_millis(limit_ms);
    DeviceSession::spawn(mock_connector(nvr), cfg)
}

async fn recv(sub: &mut Subscription) -> Item {
    tokio::time::timeout(Duration::from_secs(10), sub.recv())
        .await
        .expect("timed out waiting for an item")
        .expect("subscription ended")
}

#[tokio::test(start_paused = true)]
async fn the_stream_arrives_after_400s_and_restarts() {
    let nvr = spawn_mock(2);
    let s = session(&nvr, 200, 10_000);
    let mut sub = s.subscribe(1, StreamTier::Sub).await.unwrap();
    let t0 = Instant::now();
    assert_eq!(recv(&mut sub).await, Item::WaitingForDevice { status: 400 });
    assert!(matches!(
        recv(&mut sub).await,
        Item::Media(Record::Info { .. })
    ));
    // Keyframe first, then a running stream of `:0dc`/`:1dc` records.
    let mut videos = Vec::new();
    while videos.len() < 20 {
        match recv(&mut sub).await {
            Item::Media(Record::Video { keyframe, .. }) => videos.push(keyframe),
            Item::Media(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(videos[0]);
    assert!(videos.iter().any(|k| !k));
    // Two refused starts, each stopped 200 ms after its 400, then the third
    // streamed: three starts and two stops.
    assert_eq!(nvr.command_count(3), 3);
    assert_eq!(nvr.command_count(4), 2);
    assert!(t0.elapsed() >= Duration::from_millis(400));
    // The stream keeps running: no more restarts.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(nvr.command_count(3), 3);
    drop(sub);
    s.close().await;
    assert_eq!(nvr.command_count(4), 3);
}

#[tokio::test(start_paused = true)]
async fn a_channel_that_never_streams_is_refused_after_the_limit() {
    let nvr = spawn_mock(u32::MAX);
    let s = session(&nvr, 100, 800);
    let mut sub = s.subscribe(1, StreamTier::Main).await.unwrap();
    let t0 = Instant::now();
    assert_eq!(recv(&mut sub).await, Item::WaitingForDevice { status: 400 });
    assert_eq!(recv(&mut sub).await, Item::StartRejected { status: 400 });
    assert!(t0.elapsed() >= Duration::from_millis(700));
    let starts = nvr.command_count(3);
    assert!((4..=9).contains(&starts), "{starts} starts");
    // Every start was stopped.
    let deadline = Instant::now() + Duration::from_secs(5);
    while nvr.command_count(4) != starts {
        assert!(Instant::now() < deadline, "stops");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    s.close().await;
    assert_eq!(nvr.command_count(3), starts);
}

#[tokio::test(start_paused = true)]
async fn other_channels_play_while_one_waits() {
    let nvr = spawn_mock(u32::MAX);
    let s = session(&nvr, 100, 60_000);
    let mut waiting = s.subscribe(1, StreamTier::Sub).await.unwrap();
    let mut normal = s.subscribe(0, StreamTier::Sub).await.unwrap();
    assert_eq!(
        recv(&mut waiting).await,
        Item::WaitingForDevice { status: 400 }
    );
    assert!(matches!(
        recv(&mut normal).await,
        Item::Media(Record::Info { .. })
    ));
    assert!(matches!(
        recv(&mut normal).await,
        Item::Media(Record::Video { keyframe: true, .. })
    ));
    s.close().await;
    assert_eq!(nvr.active_logins(), 0);
}
