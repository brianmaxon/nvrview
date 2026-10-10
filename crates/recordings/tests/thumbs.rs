//! Cover thumbnails fetched from the mock NVR's archive store into the
//! bounded cache.

use bc_protocol::archive::DateTime;
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, FnConnector, SessionConfig};
use mock_nvr::archive::ArchiveConfig;
use mock_nvr::fixture::Fixture;
use recordings::secs;
use recordings::thumbs::{ThumbCacheConfig, ThumbKey, ThumbKind, ThumbnailCache, cover_thumbnail};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

fn d(h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, 10, h, m, s).unwrap()
}

fn key(device: &str, from: DateTime, to: DateTime) -> ThumbKey {
    ThumbKey {
        device: device.into(),
        channel: 0,
        stream: StreamTier::Main,
        start: secs(from),
        end: secs(to),
    }
}

#[tokio::test(start_paused = true)]
async fn covers_are_fetched_once_then_served_from_the_bounded_cache() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 1;
    let mut a = ArchiveConfig::new(vec![d(0, 0, 0)]);
    a.events_per_day = 0;
    cfg.archive = Some(a);
    let nvr = mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current());
    let mut sc = SessionConfig::new("example-user", "example-password");
    sc.query_timeout = Duration::from_secs(5);
    let session = DeviceSession::spawn(in_memory(&nvr), sc);

    let dir = tempfile::tempdir().unwrap();
    // Room for two covers (a few KB each in the mock) but not three.
    let cache = Mutex::new(
        ThumbnailCache::open(
            dir.path().join("thumbs"),
            ThumbCacheConfig {
                max_entries: 2,
                max_bytes: 10 << 20,
            },
        )
        .unwrap(),
    );
    let k1 = key("nvr-a", d(7, 0, 0), d(7, 1, 0));
    let k2 = key("nvr-a", d(7, 1, 0), d(7, 2, 0));
    let k3 = key("nvr-a", d(7, 2, 0), d(7, 3, 0));

    let first = cover_thumbnail(&session, &cache, &k1, None)
        .await
        .unwrap()
        .expect("a recording exists");
    assert_eq!(first.kind, ThumbKind::H264Keyframe);
    assert!(first.data.len() > 100 && first.data.starts_with(&[0, 0, 0, 1]));
    assert_eq!(nvr.command_count(298), 1);
    // The second call is a cache hit: the device is not asked again.
    let again = cover_thumbnail(&session, &cache, &k1, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again, first);
    assert_eq!(nvr.command_count(298), 1);

    cover_thumbnail(&session, &cache, &k2, None).await.unwrap();
    // Using k1 again makes k2 the least recently used; k3 pushes it out.
    cover_thumbnail(&session, &cache, &k1, None).await.unwrap();
    cover_thumbnail(&session, &cache, &k3, None).await.unwrap();
    assert_eq!(nvr.command_count(298), 3);
    {
        let c = cache.lock().unwrap();
        assert_eq!(c.len(), 2);
        assert!(c.contains(&k1) && c.contains(&k3) && !c.contains(&k2));
    }
    // A range with no recording gives nothing and caches nothing.
    let day_before = |h| DateTime::new(2026, 10, 9, h, 0, 0).unwrap();
    let none = key("nvr-a", day_before(7), day_before(8));
    assert!(
        cover_thumbnail(&session, &cache, &none, None)
            .await
            .unwrap()
            .is_none()
    );
    {
        let c = cache.lock().unwrap();
        assert!(!c.contains(&none) && c.len() == 2);
    }
    drop(cache);
    session.close().await;
}

/// Connects to `nvr` (on the test's runtime) through in-memory pipes.
fn in_memory(nvr: &mock_nvr::MockNvr) -> impl bc_session::Connector + use<> {
    let addr = nvr.addr();
    FnConnector(move || mock_nvr::connect(addr))
}
