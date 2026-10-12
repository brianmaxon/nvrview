//! The per-device search lane against an in-process mock NVR on the test's
//! paused clock: days fill on demand, newest first, once; thumbnails jump
//! the queue, newest first, bounded and cancelable; a held today stays
//! fresh; requests wait for the device and fail when its session ends.

use bc_protocol::archive::DateTime;
use bc_session::{DeviceSession, FnConnector, SessionConfig};
use mock_nvr::archive::{ArchiveConfig, MockEvent};
use mock_nvr::fixture::Fixture;
use recordings::index::{DeviceIndex, EventQuery, IndexConfig};
use recordings::indexer::IndexedChannel;
use recordings::lane::{DeviceLane, LaneConfig, LaneError, LaneEvent, LaneState, ThumbState};
use recordings::thumbs::{ThumbCacheConfig, ThumbKey, ThumbnailCache};
use recordings::{Clock, DeviceCamera, Secs, Tier, secs};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn day(n: u8) -> Secs {
    secs(d(n, 0, 0, 0))
}

fn cam(c: u8) -> DeviceCamera {
    DeviceCamera::first(c)
}

fn spawn_mock() -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 2;
    let mut a = ArchiveConfig::new(vec![d(8, 0, 0, 0), d(9, 0, 0, 0), d(10, 0, 0, 0)]);
    a.events_per_day = 0;
    a.events = vec![MockEvent {
        channel: 1,
        kind: "vehicle".into(),
        start: d(9, 7, 42, 5),
        end: d(9, 7, 42, 25),
    }];
    cfg.archive = Some(a);
    mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current())
}

fn session_cfg(password: &str) -> SessionConfig {
    let mut cfg = SessionConfig::new("example-user", password);
    cfg.query_timeout = Duration::from_secs(5);
    cfg.backoff.initial = Duration::from_millis(200);
    cfg.backoff.jitter = 0.0;
    cfg
}

fn session(nvr: &mock_nvr::MockNvr) -> Arc<DeviceSession> {
    let addr = nvr.addr();
    let connector = FnConnector(move || mock_nvr::connect(addr));
    Arc::new(DeviceSession::spawn(
        connector,
        session_cfg("example-password"),
    ))
}

/// Closes a session once the lane (stopped) has released it.
trait CloseShared {
    async fn close_shared(self);
}

impl CloseShared for Arc<DeviceSession> {
    async fn close_shared(self) {
        match Arc::try_unwrap(self) {
            Ok(s) => s.close().await,
            Err(_) => panic!("the lane still holds the session"),
        }
    }
}

/// Runs on every event, from the lane's task.
type Hook = Box<dyn FnMut(&LaneEvent) + Send>;

/// What a test sees of a lane.
struct Rig {
    lane: DeviceLane,
    index: Arc<Mutex<DeviceIndex>>,
    thumbs: Arc<Mutex<ThumbnailCache>>,
    events: Arc<Mutex<Vec<LaneEvent>>>,
    hook: Arc<Mutex<Option<Hook>>>,
    now: Arc<AtomicI64>,
    _dir: tempfile::TempDir,
}

fn rig(session: Arc<DeviceSession>, tweak: impl FnOnce(&mut LaneConfig)) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let index = Arc::new(Mutex::new(
        DeviceIndex::open(&dir.path().join("index"), IndexConfig::default()).unwrap(),
    ));
    let thumbs = Arc::new(Mutex::new(
        ThumbnailCache::open(dir.path().join("thumbs"), ThumbCacheConfig::default()).unwrap(),
    ));
    let now = Arc::new(AtomicI64::new(secs(d(10, 12, 0, 0))));
    let clock: Clock = {
        let now = now.clone();
        Arc::new(move || now.load(Ordering::SeqCst))
    };
    let mut cfg = LaneConfig::new(clock);
    tweak(&mut cfg);
    let events = Arc::new(Mutex::new(Vec::new()));
    let hook: Arc<Mutex<Option<Hook>>> = Arc::new(Mutex::new(None));
    let sink = {
        let (events, hook) = (events.clone(), hook.clone());
        Arc::new(move |e: LaneEvent| {
            if let Some(h) = hook.lock().unwrap().as_mut() {
                h(&e);
            }
            events.lock().unwrap().push(e);
        })
    };
    let lane = DeviceLane::spawn(session, index.clone(), thumbs.clone(), cfg, sink);
    Rig {
        lane,
        index,
        thumbs,
        events,
        hook,
        now,
        _dir: dir,
    }
}

impl Rig {
    /// Waits (in simulated time) until `f` holds for the events so far.
    async fn wait(&self, f: impl Fn(&[LaneEvent]) -> bool) {
        let reached = tokio::time::timeout(Duration::from_secs(60), async {
            while !f(&self.events.lock().unwrap()) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            reached.is_ok(),
            "the lane did not get there: {:?}",
            self.events.lock().unwrap()
        );
    }

    /// Waits until the lane is idle after the latest event.
    async fn settle(&self) {
        self.wait(|e| matches!(e.last(), Some(LaneEvent::State(LaneState::Idle))))
            .await;
    }

    fn take(&self) -> Vec<LaneEvent> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}

fn indexed(events: &[LaneEvent]) -> Vec<(DeviceCamera, Secs)> {
    events
        .iter()
        .filter_map(|e| match e {
            LaneEvent::DayIndexed { camera, day, .. } => Some((*camera, *day)),
            _ => None,
        })
        .collect()
}

fn thumb_key(from: DateTime, to: DateTime) -> ThumbKey {
    ThumbKey {
        device: "nvr-a".into(),
        channel: 0,
        stream: cam(0).stream(Tier::Sub),
        start: secs(from),
        end: secs(to),
    }
}

fn thumb_events(events: &[LaneEvent]) -> Vec<(ThumbKey, ThumbState)> {
    events
        .iter()
        .filter_map(|e| match e {
            LaneEvent::Thumbnail { key, state } => Some((key.clone(), state.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn requested_days_fill_newest_first_and_only_once() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let r = rig(s.clone(), |_| {});
    let cams = [IndexedChannel::first(0), IndexedChannel::first(1)];

    r.lane
        .request_days(&cams, &[day(8), day(9) + 3600, day(10)], false)
        .unwrap();
    r.settle().await;
    assert_eq!(
        indexed(&r.take()),
        vec![
            (cam(0), day(10)),
            (cam(1), day(10)),
            (cam(0), day(9)),
            (cam(1), day(9)),
            (cam(0), day(8)),
            (cam(1), day(8)),
        ]
    );
    // One calendar request for the month, both channels at once.
    assert_eq!(nvr.command_count(142), 1);
    assert_eq!(nvr.command_count(14), 6);
    {
        let idx = r.index.lock().unwrap();
        let vehicles = idx.events(&EventQuery {
            kinds: vec!["vehicle".into()],
            start: day(8),
            end: day(10) + 86_399,
            ..EventQuery::default()
        });
        assert_eq!(vehicles.len(), 1);
        assert_eq!(vehicles[0].camera(), cam(1));
    }

    // Asking again searches nothing: past days are final, today is fresh.
    r.lane
        .request_days(&cams, &[day(9), day(10)], false)
        .unwrap();
    r.settle().await;
    assert!(indexed(&r.take()).is_empty());
    assert_eq!(nvr.command_count(14), 6);

    // A forced refresh of one day asks its month and that day again.
    r.lane.request_days(&cams[..1], &[day(9)], true).unwrap();
    r.settle().await;
    assert_eq!(indexed(&r.take()), vec![(cam(0), day(9))]);
    assert_eq!(nvr.command_count(142), 2);
    assert_eq!(nvr.command_count(14), 7);

    // More than a month of days at once is refused.
    let many: Vec<Secs> = (0..32).map(|n| day(1) - n * 86_400).collect();
    assert_eq!(
        r.lane.request_days(&cams, &many, false),
        Err(LaneError::Limit)
    );
    r.lane.stop().await;
    s.close_shared().await;
}

#[tokio::test(start_paused = true)]
async fn thumbnails_jump_the_queue_newest_first_bounded_and_cancelable() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let r = rig(s.clone(), |c| c.max_thumbnails = 2);
    let cams = [IndexedChannel::first(0), IndexedChannel::first(1)];
    let k1 = thumb_key(d(10, 7, 0, 0), d(10, 7, 1, 0));
    let k2 = thumb_key(d(10, 7, 1, 0), d(10, 7, 2, 0));
    let k3 = thumb_key(d(10, 7, 2, 0), d(10, 7, 3, 0));
    let k4 = thumb_key(d(10, 7, 3, 0), d(10, 7, 4, 0));
    let none = thumb_key(d(7, 7, 0, 0), d(7, 7, 1, 0));

    // Everything is asked before the lane runs: three days of index work,
    // then four thumbnails into a queue of two, one of them canceled.
    r.lane
        .request_days(&cams, &[day(8), day(9), day(10)], false)
        .unwrap();
    r.lane.request_thumbnail(k1.clone(), None);
    r.lane.request_thumbnail(k2.clone(), None);
    r.lane.request_thumbnail(k3.clone(), None);
    assert_eq!(
        thumb_events(&r.take()),
        vec![(k1.clone(), ThumbState::Dropped)]
    );
    r.lane.request_thumbnail(k4.clone(), None);
    r.lane.cancel_thumbnails(Some(&k4));
    // k2 fell out when k4 came; k4 was canceled: only k3 is left.
    assert_eq!(r.lane.pending().0, 1);
    r.lane.request_thumbnail(k2.clone(), None);
    r.settle().await;
    let events = r.take();
    let first_day = events
        .iter()
        .position(|e| matches!(e, LaneEvent::DayIndexed { .. }))
        .unwrap();
    let thumbs: Vec<(usize, ThumbKey, ThumbState)> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            LaneEvent::Thumbnail { key, state } => Some((i, key.clone(), state.clone())),
            _ => None,
        })
        .collect();
    // k2 was dropped when k4 came (reported then), asked again later; the
    // newest request is fetched first, both before any index work.
    assert_eq!(
        thumbs
            .iter()
            .map(|(_, k, s)| (k.clone(), s.clone()))
            .collect::<Vec<_>>(),
        vec![
            (k2.clone(), ThumbState::Dropped),
            (k2.clone(), ThumbState::Ready),
            (k3.clone(), ThumbState::Ready),
        ]
    );
    assert!(thumbs.iter().all(|(i, _, _)| *i < first_day));
    assert_eq!(nvr.command_count(298), 2);
    assert!(r.thumbs.lock().unwrap().contains(&k2));
    assert!(r.thumbs.lock().unwrap().contains(&k3));
    assert_eq!(indexed(&events).len(), 6);

    // A thumbnail asked in the middle of a fill (right after its first
    // day) is fetched next, before the rest of the fill.
    {
        let (handle, k1) = (r.lane.handle(), k1.clone());
        let mut asked = false;
        *r.hook.lock().unwrap() = Some(Box::new(move |e| {
            if !asked && matches!(e, LaneEvent::DayIndexed { .. }) {
                asked = true;
                handle.request_thumbnail(k1.clone(), None);
            }
        }));
    }
    r.lane.request_days(&cams, &[day(8), day(9)], true).unwrap();
    r.settle().await;
    *r.hook.lock().unwrap() = None;
    let events = r.take();
    let at = events
        .iter()
        .position(|e| matches!(e, LaneEvent::Thumbnail { .. }))
        .unwrap_or_else(|| panic!("no thumbnail in {events:?}"));
    assert_eq!(indexed(&events[..at]).len(), 1, "{events:?}");
    assert_eq!(indexed(&events).len(), 4);

    // Cached: answered without asking the device. No recording: empty.
    r.lane.request_thumbnail(k3.clone(), None);
    r.lane.request_thumbnail(none.clone(), None);
    r.settle().await;
    assert_eq!(
        thumb_events(&r.take()),
        vec![(none, ThumbState::Empty), (k3, ThumbState::Ready)]
    );
    assert_eq!(nvr.command_count(298), 4);
    r.lane.stop().await;
    s.close_shared().await;
}

#[tokio::test(start_paused = true)]
async fn a_held_today_stays_fresh_until_released() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let r = rig(s.clone(), |c| c.hold_poll = Duration::from_secs(5));
    let hold = r.lane.hold_today(&[IndexedChannel::first(0)]);
    r.wait(|e| !indexed(e).is_empty()).await;
    r.settle().await;
    assert_eq!(indexed(&r.take()), vec![(cam(0), day(10))]);
    let (cal, search) = (nvr.command_count(142), nvr.command_count(14));
    assert_eq!((cal, search), (1, 1));

    // Not stale yet: nothing is searched.
    r.now.fetch_add(30, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(12)).await;
    assert_eq!(nvr.command_count(14), 1);

    // A minute old: today's calendar and day are asked again.
    r.now.fetch_add(40, Ordering::SeqCst);
    r.wait(|e| !indexed(e).is_empty()).await;
    r.settle().await;
    assert_eq!(indexed(&r.take()), vec![(cam(0), day(10))]);
    assert_eq!(nvr.command_count(142), 2);
    assert_eq!(nvr.command_count(14), 2);

    // Released: nothing more.
    drop(hold);
    r.now.fetch_add(600, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert_eq!(nvr.command_count(14), 2);
    r.lane.stop().await;
    s.close_shared().await;
}

#[tokio::test(start_paused = true)]
async fn requests_wait_for_the_device_and_fail_once_its_session_ends() {
    let nvr = spawn_mock();
    let online = Arc::new(AtomicBool::new(false));
    let addr = nvr.addr();
    let gate = online.clone();
    let connector = FnConnector(move || {
        if gate.load(Ordering::SeqCst) {
            mock_nvr::connect(addr)
        } else {
            Err(io::Error::new(io::ErrorKind::ConnectionRefused, "offline"))
        }
    });
    let s = Arc::new(DeviceSession::spawn(
        connector,
        session_cfg("example-password"),
    ));
    let r = rig(s.clone(), |_| {});
    let k = thumb_key(d(10, 7, 0, 0), d(10, 7, 1, 0));
    r.lane.request_thumbnail(k.clone(), None);
    r.wait(|e| e.contains(&LaneEvent::State(LaneState::WaitingDevice)))
        .await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(r.lane.pending().0, 1);
    assert_eq!(nvr.command_count(298), 0);

    online.store(true, Ordering::SeqCst);
    r.settle().await;
    assert_eq!(
        thumb_events(&r.take()),
        vec![(k.clone(), ThumbState::Ready)]
    );
    r.lane.stop().await;
    s.close_shared().await;

    // A refused login ends the session: waiting requests fail at once.
    let s = Arc::new(DeviceSession::spawn(
        {
            let addr = nvr.addr();
            FnConnector(move || mock_nvr::connect(addr))
        },
        session_cfg("wrong-password"),
    ));
    let r = rig(s.clone(), |_| {});
    r.lane.request_thumbnail(k.clone(), None);
    r.lane
        .request_days(&[IndexedChannel::first(0)], &[day(10)], false)
        .unwrap();
    r.wait(|e| e.contains(&LaneEvent::State(LaneState::Ended)))
        .await;
    r.wait(|e| e.iter().any(|x| matches!(x, LaneEvent::DayFailed { .. })))
        .await;
    let events = r.take();
    assert!(matches!(
        thumb_events(&events)[..],
        [(_, ThumbState::Failed(_))]
    ));
    r.lane.stop().await;
    s.close_shared().await;
}

fn months_indexed(events: &[LaneEvent]) -> Vec<(Vec<DeviceCamera>, Secs)> {
    events
        .iter()
        .filter_map(|e| match e {
            LaneEvent::MonthIndexed { cameras, month, .. } => Some((cameras.clone(), *month)),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn months_asked_alone_fetch_only_their_calendars_once() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let r = rig(s.clone(), |_| {});
    let cams = [IndexedChannel::first(0), IndexedChannel::first(1)];
    let september = secs(DateTime::new(2026, 9, 1, 0, 0, 0).unwrap());

    // Two months, any second within each: one 142 per month for both
    // cameras, newest month first, and no day searched.
    r.lane
        .request_months(&cams, &[september + 86_400 * 3, day(9) + 3600], false)
        .unwrap();
    r.settle().await;
    let events = r.take();
    assert_eq!(
        months_indexed(&events),
        vec![
            (vec![cam(0), cam(1)], day(1)),
            (vec![cam(0), cam(1)], september)
        ]
    );
    assert!(indexed(&events).is_empty());
    assert_eq!(nvr.command_count(142), 2);
    assert_eq!(nvr.command_count(14), 0);
    {
        let idx = r.index.lock().unwrap();
        assert!(idx.recorded_days(cam(0)).contains(&day(8)));
        assert!(idx.recorded_days(cam(0)).contains(&day(10)));
    }

    // September ended before it was fetched: final, not asked again. The
    // current month is fresh for a minute.
    r.lane
        .request_months(&cams, &[september, day(10)], false)
        .unwrap();
    r.settle().await;
    assert!(months_indexed(&r.take()).is_empty());
    assert_eq!(nvr.command_count(142), 2);

    // Forced: asked again.
    r.lane
        .request_months(&cams[..1], &[september], true)
        .unwrap();
    r.settle().await;
    assert_eq!(months_indexed(&r.take()), vec![(vec![cam(0)], september)]);
    assert_eq!(nvr.command_count(142), 3);

    // Wanted days come before months asked alone.
    r.lane
        .request_months(&cams[..1], &[september], true)
        .unwrap();
    r.lane.request_days(&cams[..1], &[day(9)], true).unwrap();
    r.settle().await;
    let events = r.take();
    let first_day = events
        .iter()
        .position(|e| matches!(e, LaneEvent::DayIndexed { .. }))
        .unwrap();
    let september_at = events
        .iter()
        .position(|e| matches!(e, LaneEvent::MonthIndexed { month, .. } if *month == september))
        .unwrap();
    assert!(first_day < september_at, "{events:?}");

    // More than a year at once is refused.
    let many: Vec<Secs> = (0..13).map(|n| day(1) - n * 32 * 86_400).collect();
    assert_eq!(
        r.lane.request_months(&cams, &many, false),
        Err(LaneError::Limit)
    );
    r.lane.stop().await;
    s.close_shared().await;
}
