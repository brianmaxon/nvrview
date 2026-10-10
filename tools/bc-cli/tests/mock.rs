//! bc-cli's library against the in-process mock NVR, on tokio's paused
//! clock: the device runs on the test's runtime (`mock_nvr::spawn_in`),
//! reached through in-memory pipes, so captures, stagger, restarts, and
//! timeouts take simulated time.

use bc_cli::{
    CliError, RecordLimits, info_command, info_query, list_channels, record, wait_logged_in,
};
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, FnConnector, SessionConfig};
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
    mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current())
}

/// Connects to `nvr` through in-memory pipes.
fn connector(nvr: &mock_nvr::MockNvr) -> impl bc_session::Connector + use<> {
    let addr = nvr.addr();
    FnConnector(move || mock_nvr::connect(addr))
}

fn session(nvr: &mock_nvr::MockNvr, password: &str) -> DeviceSession {
    DeviceSession::spawn(connector(nvr), SessionConfig::new("example-user", password))
}

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
async fn records_h264_from_the_first_keyframe() {
    records_fixture("h264-leading-p.h264").await;
}

#[tokio::test(start_paused = true)]
async fn records_h265_from_the_first_keyframe() {
    records_fixture("h265-leading-p.h265").await;
}

#[tokio::test(start_paused = true)]
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

#[tokio::test(start_paused = true)]
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
#[tokio::test(start_paused = true)]
async fn captures_tiers_and_analyzes_them() {
    use bc_cli::analyze::Analysis;
    use bc_cli::{CaptureConfig, Restart, capture};
    use bc_protocol::recording::TierWriter;
    use mock_nvr::replay::{Replay, ReplayConfig};

    let mut cfg = mock_nvr::Config::new(Fixture::load(&fixture_path("h264-basic.h264")).unwrap());
    cfg.replay = Some(ReplayConfig::new(
        Replay::from_reader(&two_tier_recording()[..]).unwrap(),
    ));
    let nvr = mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current());
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

#[tokio::test(start_paused = true)]
async fn info_queries_return_the_reply_body() {
    let nvr = spawn_mock("h264-basic.h264", 2);
    let s = session(&nvr, "example-password");
    for (n, ch, tag) in [
        (80, None, "<VersionInfo"),
        (146, None, "<StreamInfoList"),
        (199, None, "<Support"),
        (318, Some(1), "<channelId>1</channelId>"),
    ] {
        let cmd = info_command(n, ch).unwrap();
        let r = info_query(&s, cmd, ch, LIMIT).await.unwrap();
        assert!(r.xml.contains(tag), "{n}: {} {}", r.status, r.xml);
    }
    // Only the read-only queries: nothing else can be named.
    for (n, ch) in [(318, None), (80, Some(1)), (3, None), (31, None), (0, None)] {
        assert!(info_command(n, ch).is_none(), "{n}");
    }
    s.close().await;
}

/// A third-party camera channel: the device answers each preview with 400
/// and streams only after the session restarts it.
fn spawn_third_party_mock() -> mock_nvr::MockNvr {
    let mut cfg = mock_nvr::Config::new(Fixture::load(&fixture_path("h264-basic.h264")).unwrap());
    cfg.channels = 2;
    cfg.fps = 100;
    let mut tp = mock_nvr::ThirdPartyConfig::new(vec![1], 1);
    tp.media_delay = Duration::from_millis(100);
    cfg.third_party = Some(tp);
    mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current())
}

fn quick_retry_session(nvr: &mock_nvr::MockNvr) -> DeviceSession {
    let mut cfg = SessionConfig::new("example-user", "example-password");
    cfg.preview_retry_interval = Duration::from_millis(200);
    DeviceSession::spawn(connector(nvr), cfg)
}

#[tokio::test(start_paused = true)]
async fn records_a_channel_whose_preview_is_answered_400() {
    let nvr = spawn_third_party_mock();
    let s = quick_retry_session(&nvr);
    wait_logged_in(&s, LIMIT).await.unwrap();
    let mut out = Vec::new();
    let stats = record(
        &s,
        1,
        StreamTier::Sub,
        &mut out,
        Default::default(),
        RecordLimits { frames: Some(20) },
        tokio::time::sleep(LIMIT),
    )
    .await
    .unwrap();
    s.close().await;
    assert_eq!(stats.waits, 1);
    assert_eq!(stats.frames, 20);
    assert!(stats.keyframes >= 1);
    // The file starts at the fixture's first keyframe.
    let fixture = Fixture::load(&fixture_path("h264-basic.h264")).unwrap();
    let first = &fixture.frames()[fixture.first_keyframe()].data;
    assert!(out.starts_with(first));
    assert_eq!(nvr.command_count(3), 2);
}

#[tokio::test(start_paused = true)]
async fn captures_a_channel_whose_preview_is_answered_400() {
    use bc_cli::{CaptureConfig, capture};
    use bc_protocol::recording::TierWriter;

    let nvr = spawn_third_party_mock();
    let s = quick_retry_session(&nvr);
    wait_logged_in(&s, LIMIT).await.unwrap();
    let mut out = TierWriter::new(Vec::new()).unwrap();
    let cc = CaptureConfig {
        channel: 1,
        tiers: vec![StreamTier::Sub],
        stagger: Duration::ZERO,
        restart: None,
    };
    let stats = capture(
        &s,
        &cc,
        &mut out,
        tokio::time::sleep(Duration::from_millis(1_500)),
    )
    .await
    .unwrap();
    s.close().await;
    let sub = &stats.tiers[0].1;
    assert_eq!(sub.waits, 1, "{sub:?}");
    assert!(sub.videos >= 50 && sub.keyframes >= 1, "{sub:?}");
}
