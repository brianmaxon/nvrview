//! Recording searches, cover images, and downloads against the in-process
//! mock NVR's fake recording store, alongside live view on the same session.

use bc_protocol::archive::{
    CoverRequest, DateTime, DayRecordChannel, DownloadRequest, EventSearch, FileSearch,
};
use bc_protocol::media::Record;
use bc_protocol::stream::StreamTier;
use bc_session::{
    DeviceSession, Download, DownloadError, DownloadEvent, Item, SessionConfig, TcpConnector,
};
use mock_nvr::archive::{ArchiveConfig, DownloadFault, FaultKind, MockEvent};
use mock_nvr::fixture::Fixture;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn spawn_mock(tweak: impl FnOnce(&mut ArchiveConfig)) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 3;
    let mut a = ArchiveConfig::new(vec![d(8, 0, 0, 0), d(9, 0, 0, 0)]);
    tweak(&mut a);
    cfg.archive = Some(a);
    mock_nvr::spawn(cfg).unwrap()
}

fn session(nvr: &mock_nvr::MockNvr, tweak: impl FnOnce(&mut SessionConfig)) -> DeviceSession {
    let mut cfg = SessionConfig::new("example-user", "example-password");
    cfg.query_timeout = Duration::from_secs(5);
    cfg.download_stall_timeout = Duration::from_millis(700);
    tweak(&mut cfg);
    DeviceSession::spawn(TcpConnector::new(nvr.addr()), cfg)
}

fn request(ch: u8, a: DateTime, b: DateTime) -> DownloadRequest {
    DownloadRequest {
        channel: ch,
        uid: None,
        stream: StreamTier::Main,
        start: a,
        end: b,
    }
}

/// Reads a download to its end: (estimate, video frame count, records).
async fn drain(dl: &mut Download) -> Result<(Option<u64>, usize, usize), DownloadError> {
    let (mut est, mut video, mut recs) = (None, 0, 0);
    while let Some(ev) = dl.next().await? {
        match ev {
            DownloadEvent::Estimate(n) => est = Some(n),
            DownloadEvent::Record(r) => {
                recs += 1;
                if matches!(r, Record::Video { .. }) {
                    video += 1;
                }
            }
        }
    }
    Ok((est, video, recs))
}

#[tokio::test]
async fn searches_return_typed_results() {
    let nvr = spawn_mock(|a| {
        a.file_page = 7;
        a.event_page = 3;
        a.events_per_day = 8;
        a.events.push(MockEvent {
            channel: 2,
            kind: "vehicle".into(),
            start: d(9, 7, 42, 5),
            end: d(9, 7, 42, 25),
        });
    });
    let s = session(&nvr, |_| {});

    let days = s
        .day_records(
            d(1, 0, 0, 0),
            d(31, 23, 59, 59),
            &[
                DayRecordChannel {
                    channel: 0,
                    uid: None,
                },
                DayRecordChannel {
                    channel: 2,
                    uid: None,
                },
            ],
        )
        .await
        .unwrap();
    assert_eq!(days.len(), 2);
    assert_eq!(
        days[1]
            .days
            .iter()
            .map(|t| t.day_of_month())
            .collect::<Vec<_>>(),
        vec![8, 9]
    );

    let files = s
        .search_files(&FileSearch {
            channel: 2,
            uid: None,
            stream: StreamTier::Main,
            start: d(9, 0, 0, 0),
            end: d(9, 23, 59, 59),
        })
        .await
        .unwrap();
    assert_eq!(files.len(), 24);
    assert!(files.windows(2).all(|w| w[0].end < w[1].start));
    // Every search was closed.
    assert_eq!(nvr.command_count(16), 1);

    let events = s
        .search_events(&EventSearch {
            channel: 2,
            uid: None,
            stream: StreamTier::Main,
            start: d(9, 0, 0, 0),
            end: d(9, 23, 59, 59),
        })
        .await
        .unwrap();
    assert_eq!(events.len(), 9);
    assert_eq!(nvr.command_count(273), 3);
    assert_eq!(nvr.command_count(274), 1);
    let v = events
        .iter()
        .find(|e| e.kind == "vehicle" && e.start == d(9, 7, 42, 5))
        .unwrap();
    assert!(files.iter().any(|f| f.id == v.file_id));

    let cover = s
        .cover_image(&CoverRequest {
            channel: 1,
            uid: None,
            stream: StreamTier::Main,
            start: d(9, 7, 0, 0),
            end: d(9, 8, 0, 0),
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!((cover.width, cover.height), (320, 240));
    assert!(!cover.keyframe.is_empty());
    let none = s
        .cover_image(&CoverRequest {
            channel: 1,
            uid: None,
            stream: StreamTier::Main,
            start: d(10, 7, 0, 0),
            end: d(10, 8, 0, 0),
        })
        .await
        .unwrap();
    assert!(none.is_none());
    s.close().await;
}

#[tokio::test]
async fn download_runs_beside_live_view_without_interrupting_it() {
    // Throttled so the download lasts about two seconds.
    let nvr = spawn_mock(|a| a.download_rate = Some(250_000));
    let s = session(&nvr, |_| {});
    let mut live = s.subscribe(0, StreamTier::Sub).await.unwrap();
    // Wait for live video first.
    loop {
        if let Some(Item::Media(Record::Video { .. })) = live.recv().await {
            break;
        }
    }
    let mut dl = s
        .download(&request(1, d(9, 7, 42, 0), d(9, 7, 42, 59)))
        .await
        .unwrap();
    let live_task = tokio::spawn(async move {
        let mut frames = 0;
        let mut worst = Duration::ZERO;
        let mut last = Instant::now();
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            if let Ok(Some(Item::Media(Record::Video { .. }))) =
                tokio::time::timeout(Duration::from_secs(1), live.recv()).await
            {
                frames += 1;
                worst = worst.max(last.elapsed());
                last = Instant::now();
            } else {
                worst = worst.max(last.elapsed());
            }
        }
        (frames, worst, live)
    });
    let started = Instant::now();
    let (est, video, _) = drain(&mut dl).await.unwrap();
    let took = started.elapsed();
    assert!(est.unwrap() > 0);
    assert_eq!(video, 600);
    let (frames, worst, live) = live_task.await.unwrap();
    // 25 fps live during the download, never a gap of more than a few frames.
    assert!(frames >= 40, "live frames during download: {frames}");
    assert!(
        worst < Duration::from_millis(300),
        "worst live gap {worst:?}"
    );
    assert!(took > Duration::from_millis(500), "download was throttled");
    assert_eq!(live.dropped(), 0);
    drop(live);
    s.close().await;
}

#[tokio::test]
async fn a_stalled_download_is_detected_and_cancel_sends_144() {
    let nvr = spawn_mock(|_| {});
    let s = session(&nvr, |_| {});
    nvr.inject_download_fault(DownloadFault {
        channel: Some(2),
        after_bytes: 30_000,
        kind: FaultKind::Stall,
    });
    let mut dl = s
        .download(&request(2, d(9, 1, 0, 0), d(9, 1, 0, 59)))
        .await
        .unwrap();
    let started = Instant::now();
    let err = drain(&mut dl).await.unwrap_err();
    assert_eq!(err, DownloadError::Stalled);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(dl.message_id().is_some());
    assert!(dl.cancel(Duration::from_secs(2)).await);
    // The mock counts the stop.
    let deadline = Instant::now() + Duration::from_secs(2);
    while nvr.command_count(144) == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(nvr.command_count(144), 1);

    // The permit was released and the session still downloads.
    let mut dl = s
        .download(&request(2, d(9, 1, 1, 0), d(9, 1, 1, 9)))
        .await
        .unwrap();
    assert_eq!(drain(&mut dl).await.unwrap().1, 100);
    // A finished download needs no stop.
    assert!(!dl.cancel(Duration::from_millis(200)).await);
    assert_eq!(nvr.command_count(144), 1);
    s.close().await;
}

#[tokio::test]
async fn dropping_a_running_download_stops_it() {
    let nvr = spawn_mock(|a| a.download_rate = Some(100_000));
    let s = session(&nvr, |_| {});
    let mut dl = s
        .download(&request(0, d(9, 3, 0, 0), d(9, 3, 0, 59)))
        .await
        .unwrap();
    for _ in 0..5 {
        dl.next().await.unwrap();
    }
    drop(dl);
    let deadline = Instant::now() + Duration::from_secs(2);
    while nvr.command_count(144) == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(nvr.command_count(144), 1);
    s.close().await;
}

#[tokio::test]
async fn downloads_are_one_at_a_time_per_device_by_default() {
    let nvr = spawn_mock(|a| a.download_rate = Some(100_000));
    let s = session(&nvr, |_| {});
    let first = s
        .download(&request(0, d(9, 3, 0, 0), d(9, 3, 0, 59)))
        .await
        .unwrap();
    let second = tokio::time::timeout(
        Duration::from_millis(300),
        s.download(&request(1, d(9, 3, 0, 0), d(9, 3, 0, 59))),
    )
    .await;
    assert!(second.is_err(), "second download waited for a permit");
    assert!(first.cancel(Duration::from_secs(2)).await);
    let second = tokio::time::timeout(
        Duration::from_secs(2),
        s.download(&request(1, d(9, 3, 0, 0), d(9, 3, 0, 9))),
    )
    .await
    .unwrap()
    .unwrap();
    drop(second);

    let s2 = session(&nvr, |c| c.max_downloads = 2);
    let a = s2
        .download(&request(0, d(9, 3, 0, 0), d(9, 3, 0, 59)))
        .await
        .unwrap();
    let b = tokio::time::timeout(
        Duration::from_millis(500),
        s2.download(&request(1, d(9, 3, 0, 0), d(9, 3, 0, 59))),
    )
    .await;
    assert!(b.is_ok(), "two downloads allowed");
    drop((a, b));
    s2.close().await;
    s.close().await;
}

#[tokio::test]
async fn a_disconnect_fails_the_download_and_the_session_recovers() {
    let nvr = spawn_mock(|_| {});
    let s = session(&nvr, |c| {
        c.backoff.initial = Duration::from_millis(50);
        c.backoff.jitter = 0.0;
    });
    nvr.inject_download_fault(DownloadFault {
        channel: None,
        after_bytes: 20_000,
        kind: FaultKind::Disconnect,
    });
    let mut dl = s
        .download(&request(1, d(8, 22, 0, 0), d(8, 22, 0, 59)))
        .await
        .unwrap();
    assert_eq!(
        drain(&mut dl).await.unwrap_err(),
        DownloadError::ConnectionLost
    );
    drop(dl);
    let mut dl = s
        .download(&request(1, d(8, 22, 0, 0), d(8, 22, 0, 59)))
        .await
        .unwrap();
    assert_eq!(drain(&mut dl).await.unwrap().1, 600);
    s.close().await;
    assert_eq!(nvr.active_logins(), 0);
}
