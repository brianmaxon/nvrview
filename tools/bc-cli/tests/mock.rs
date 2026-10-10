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

/// A made-up two-tier recording on one picture clock: sub pictures every
/// 100 ms (keyframe every 10) arriving 30 ms after capture, main pictures
/// every 50 ms (keyframe every 20) arriving 150 ms after capture; every
/// picture carries wall-clock seconds in its extra header.
fn two_tier_recording() -> Vec<u8> {
    use bc_protocol::media::{Codec, Record};
    use bc_protocol::recording::TierWriter;
    let clock0 = 3_000_000_000u64;
    let pic = |keyframe: bool, capture: u64| {
        let mut extra = (1_000 + ((clock0 + capture) / 1_000_000) as u32)
            .to_le_bytes()
            .to_vec();
        extra.extend([0u8; 4]);
        Record::Video {
            codec: Codec::H264,
            keyframe,
            pts_us: clock0 + capture,
            data: vec![0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }, 9, 9, 9],
            extra_header: extra,
        }
    };
    let info = |w, h, fps| Record::Info {
        width: w,
        height: h,
        fps,
    };
    let mut items = vec![
        (1_000u64, StreamTier::Sub, info(64, 48, 10)),
        (1_000, StreamTier::Main, info(640, 480, 20)),
    ];
    for i in 0..300u64 {
        items.push((
            i * 100_000 + 30_000,
            StreamTier::Sub,
            pic(i % 10 == 0, i * 100_000),
        ));
    }
    for i in 0..600u64 {
        items.push((
            i * 50_000 + 150_000,
            StreamTier::Main,
            pic(i % 20 == 0, i * 50_000),
        ));
    }
    items.sort_by_key(|(at, t, _)| (*at, t.number()));
    let mut w = TierWriter::new(Vec::new()).unwrap();
    for (at, t, r) in &items {
        w.record(*t, *at, r).unwrap();
    }
    w.finish().unwrap()
}

/// Capture sub and main from a mock replaying a two-tier recording, with
/// main restarted, then analyze the capture: one clock, main about 120 ms
/// behind sub, a start sample per main run.
#[tokio::test]
async fn captures_tiers_and_analyzes_them() {
    use bc_cli::analyze::Analysis;
    use bc_cli::{CaptureConfig, Restart, capture};
    use bc_protocol::recording::TierWriter;
    use mock_nvr::replay::{Replay, ReplayConfig};

    let mut cfg = mock_nvr::Config::new(Fixture::load(&fixture_path("h264-basic.h264")).unwrap());
    cfg.replay = Some(ReplayConfig::new(
        Replay::from_reader(&two_tier_recording()[..]).unwrap(),
    ));
    let nvr = mock_nvr::spawn(cfg).unwrap();
    let s = session(&nvr, "example-password");
    wait_logged_in(&s, LIMIT).await.unwrap();
    let mut out = TierWriter::new(Vec::new()).unwrap();
    let cc = CaptureConfig {
        channel: 0,
        tiers: vec![StreamTier::Sub, StreamTier::Main],
        stagger: Duration::from_millis(1_000),
        restart: Some(Restart {
            tier: StreamTier::Main,
            every: Duration::from_millis(2_000),
            gap: Duration::from_millis(700),
        }),
    };
    let stats = capture(
        &s,
        &cc,
        &mut out,
        tokio::time::sleep(Duration::from_millis(6_500)),
    )
    .await
    .unwrap();
    s.close().await;
    let bytes = out.finish().unwrap();

    let (sub, main) = (&stats.tiers[0].1, &stats.tiers[1].1);
    assert_eq!(sub.starts, 1);
    assert!(main.starts >= 2 && main.stops >= 1, "{main:?}");
    assert!(sub.videos >= 50 && main.videos >= 40, "{stats:?}");
    assert_eq!(sub.dropped + main.dropped, 0);

    let md = Analysis::read(&bytes[..]).unwrap().to_markdown();
    eprintln!("{md}");
    assert!(md.contains("shared clock (main/sub): yes"), "{md}");
    assert!(md.contains("wall-clock windows overlap"), "{md}");
    let line = md.lines().find(|l| l.starts_with("| median |")).unwrap();
    let lag: f64 = line.split('|').nth(2).unwrap().trim().parse().unwrap();
    assert!((95.0..=150.0).contains(&lag), "{line}");
    // Main joins at its latest keyframe: the pictures since then come at
    // once, as a burst.
    let row = md.lines().find(|l| l.starts_with("| main | 1 |")).unwrap();
    let cells: Vec<&str> = row.split('|').map(str::trim).collect();
    assert_eq!(cells[6], "yes", "{row}");
    let burst: usize = cells[8].parse().unwrap();
    assert!(burst >= 5, "{row}\n{md}");
}
