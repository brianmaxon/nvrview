//! Plays of recordings (123, 5, 7) against the in-process mock NVR on the
//! test's paused clock, beside live view on the same session. What the mock
//! does past the observed facts (how a play ends, refusals) is the mock's
//! choice, not a device fact; see docs/MILESTONE-P.md, B5 and B6.

mod common;

use bc_protocol::archive::{DateTime, PlayRequest};
use bc_protocol::media::{Record, wall_clock_secs};
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, Item, Play, PlayStart, QueryError, SearchError, SessionConfig};
use common::mock_connector;
use mock_nvr::archive::ArchiveConfig;
use mock_nvr::fixture::Fixture;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::Instant;

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn spawn_mock(tweak: impl FnOnce(&mut ArchiveConfig)) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 3;
    let mut a = ArchiveConfig::new(vec![d(8, 0, 0, 0)]);
    tweak(&mut a);
    cfg.archive = Some(a);
    mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current())
}

fn session(nvr: &mock_nvr::MockNvr, tweak: impl FnOnce(&mut SessionConfig)) -> DeviceSession {
    let mut cfg = SessionConfig::new("example-user", "example-password");
    cfg.query_timeout = Duration::from_secs(5);
    tweak(&mut cfg);
    DeviceSession::spawn(mock_connector(nvr), cfg)
}

/// A play of channel `ch` from `at`, naming the mock's file that holds it
/// (60-minute files from midnight).
fn start(ch: u8, at: DateTime) -> PlayStart {
    let hour = DateTime::new(at.year, at.month, at.day, at.hour, 0, 0).unwrap();
    PlayStart {
        request: PlayRequest {
            channel: ch,
            uid: None,
            stream: StreamTier::Sub,
            file_id: format!("mock-file-{ch}-{}", hour.to_naive_secs()),
            file_name: format!("file-{ch}"),
        },
        at,
    }
}

/// Reads video until `secs` of it arrived: (first keyframe's wall
/// seconds, first and last pts).
async fn read_video(p: &mut Play, secs: u64) -> (Option<u32>, u64, u64) {
    read(p, secs, true).await
}

/// Like [`read_video`]; `from_start` checks that the first picture is a
/// keyframe (a play's first).
async fn read(p: &mut Play, secs: u64, from_start: bool) -> (Option<u32>, u64, u64) {
    let (mut first_wall, mut first, mut last) = (None, None, 0);
    while let Some(r) = p.next().await.unwrap() {
        if let Record::Video {
            pts_us,
            keyframe,
            extra_header,
            ..
        } = r
        {
            if first.is_none() {
                assert!(keyframe || !from_start, "a play starts at a keyframe");
                first_wall = wall_clock_secs(&extra_header);
                first = Some(pts_us);
            }
            assert!(pts_us >= last, "timestamps keep rising");
            last = pts_us;
            if last - first.unwrap() >= secs * 1_000_000 {
                break;
            }
        }
    }
    (first_wall, first.unwrap_or(0), last)
}

#[tokio::test(start_paused = true)]
async fn a_play_seeks_bursts_paces_and_crosses_files() {
    let nvr = spawn_mock(|a| a.file_minutes = 1);
    let s = session(&nvr, |_| {});
    // 20 s before a file boundary; the GOP is 2 s, so the play starts at
    // the keyframe at or before the seek.
    let at = d(8, 7, 41, 41);
    let t0 = Instant::now();
    let mut p = s.play(&start(1, at)).await.unwrap();
    let (wall, first, _) = read_video(&mut p, 3).await;
    assert_eq!(wall, Some(at.to_naive_secs() as u32 - 1));
    // The burst (3 s in the mock) arrives at once...
    assert!(
        t0.elapsed() < Duration::from_millis(500),
        "{:?}",
        t0.elapsed()
    );
    // ...then about real time, and on across the file boundary at 7.42.
    let (_, _, last) = read(&mut p, 30, false).await;
    assert!(last - first >= 30_000_000);
    let el = t0.elapsed();
    assert!(
        el >= Duration::from_secs(25) && el <= Duration::from_secs(32),
        "{el:?}"
    );
    let log = nvr.play_log();
    assert_eq!(log.len(), 1);
    assert_eq!((log[0].channel, log[0].at, log[0].speed), (1, at, Some(1)));
    assert_eq!(log[0].seq, p.seq());
    assert_eq!(nvr.command_count(123), 1);

    // Dropping it stops it (7).
    drop(p);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(nvr.command_count(7), 1);
    assert_eq!(nvr.active_plays(), 0);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn plays_share_the_session_with_live_view_and_are_bounded() {
    let nvr = spawn_mock(|_| {});
    let s = session(&nvr, |c| c.max_plays = 2);
    let mut live = s.subscribe(0, StreamTier::Sub).await.unwrap();
    let mut a = s.play(&start(1, d(8, 7, 0, 0))).await.unwrap();
    let mut b = s.play(&start(2, d(8, 9, 30, 0))).await.unwrap();
    // A third is refused at once.
    assert!(matches!(
        s.play(&start(1, d(8, 8, 0, 0))).await,
        Err(SearchError::Query(QueryError::Busy))
    ));
    let (wa, ..) = read_video(&mut a, 5).await;
    let (wb, ..) = read_video(&mut b, 5).await;
    assert_eq!(wa, Some(d(8, 7, 0, 0).to_naive_secs() as u32));
    assert_eq!(wb, Some(d(8, 9, 30, 0).to_naive_secs() as u32));
    // Live view kept flowing meanwhile.
    let mut frames = 0;
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        if let Ok(Some(Item::Media(Record::Video { .. }))) =
            tokio::time::timeout(Duration::from_secs(1), live.recv()).await
        {
            frames += 1;
        }
    }
    assert!(frames > 5, "live frames: {frames}");
    // Stopping one frees its permit.
    assert!(a.stop(Duration::from_secs(1)).await);
    let mut c = s.play(&start(1, d(8, 8, 0, 0))).await.unwrap();
    read_video(&mut c, 1).await;
    drop((b, c, live));
    // Closing the session stops what still runs.
    s.close().await;
    assert_eq!(nvr.command_count(7), 3);
    assert_eq!(nvr.active_plays(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_play_ends_at_the_end_of_the_recordings_and_bad_files_are_refused() {
    let nvr = spawn_mock(|_| {});
    let s = session(&nvr, |_| {});
    // Three seconds before the recorded day ends.
    let mut p = s.play(&start(0, d(8, 23, 59, 56))).await.unwrap();
    let mut video = 0;
    while let Some(r) = p.next().await.unwrap() {
        if matches!(r, Record::Video { .. }) {
            video += 1;
        }
    }
    assert!(video > 0);
    // Ended: the same answer again, and nothing left to stop.
    assert!(p.next().await.unwrap().is_none());
    assert!(!p.stop(Duration::from_millis(200)).await);

    let mut bad = start(0, d(8, 7, 0, 0));
    bad.request.file_id = "unknown".into();
    let mut p = s.play(&bad).await.unwrap();
    assert!(p.next().await.is_err());
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_group_seeks_together_pauses_then_plays_together() {
    let nvr = spawn_mock(|_| {});
    let s = session(&nvr, |c| c.max_plays = 3);
    let at = d(8, 7, 41, 41);
    let starts = [start(0, at), start(1, at), start(2, at)];
    let t0 = Instant::now();
    let mut plays = s.play_group(&starts, at).await.unwrap();
    // The seeks, the pause (500 ms by default), then the plays.
    assert!(
        t0.elapsed() >= Duration::from_millis(500),
        "{:?}",
        t0.elapsed()
    );
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    assert_eq!(plays.len(), 3);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(nvr.command_count(123), 3);
    assert_eq!(nvr.command_count(5), 3);
    // Consecutive seq numbers, one message ID per play.
    let seq0 = plays[0].seq();
    assert!(
        plays
            .iter()
            .enumerate()
            .all(|(i, p)| p.seq() == seq0 + i as u32)
    );
    let ids: Vec<_> = plays.iter().map(|p| p.message_id().unwrap()).collect();
    assert!(
        ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2],
        "{ids:?}"
    );
    // Every play started at the one seek time and was sent as a sub-stream
    // play; the device saw the last seek when each 5 arrived.
    let log = nvr.play_log();
    assert_eq!(log.len(), 3);
    let last = plays[2].seq();
    for (i, l) in log.iter().enumerate() {
        assert_eq!((l.channel, l.at, l.seq), (i as u8, at, last));
        assert_eq!(l.support_sub, Some(1));
    }
    // The media of every play carries the last seek's seq; each is routed
    // by its message ID and delivers.
    for p in &mut plays {
        let (wall, ..) = read_video(p, 3).await;
        assert_eq!(wall, Some(at.to_naive_secs() as u32 - 1));
    }
    // Dropping the plays stops them all.
    drop(plays);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(nvr.command_count(7), 3);
    assert_eq!(nvr.active_plays(), 0);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn the_pause_between_seeks_and_plays_is_configurable() {
    let nvr = spawn_mock(|_| {});
    let s = session(&nvr, |c| {
        c.max_plays = 2;
        c.play_group_delay = Duration::from_secs(3);
    });
    let at = d(8, 9, 0, 0);
    let t0 = Instant::now();
    let plays = s
        .play_group(&[start(0, at), start(1, at)], at)
        .await
        .unwrap();
    assert!(t0.elapsed() >= Duration::from_secs(3), "{:?}", t0.elapsed());
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(plays);
    s.close().await;
    assert_eq!(nvr.command_count(7), 2);
}

#[tokio::test(start_paused = true)]
async fn a_group_respects_max_plays_and_sends_nothing_when_it_does_not_fit() {
    let nvr = spawn_mock(|_| {});
    let s = session(&nvr, |c| c.max_plays = 3);
    let at = d(8, 7, 0, 0);
    let one = s.play(&start(0, at)).await.unwrap();
    // Two more fit; three do not, and nothing goes out for them.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let seeks = nvr.command_count(123);
    assert!(matches!(
        s.play_group(&[start(0, at), start(1, at), start(2, at)], at)
            .await,
        Err(SearchError::Query(QueryError::Busy))
    ));
    assert_eq!(nvr.command_count(123), seeks);
    assert_eq!(nvr.command_count(5), 1);
    // The refused group released its permits: two still fit.
    let two = s
        .play_group(&[start(1, at), start(2, at)], at)
        .await
        .unwrap();
    assert_eq!(two.len(), 2);
    assert!(s.play_group(&[], at).await.unwrap().is_empty());
    drop((one, two));
    s.close().await;
    assert_eq!(nvr.active_plays(), 0);
}
