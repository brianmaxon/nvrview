//! End to end against two in-process mock NVRs with fake recording stores:
//! index fill, search by type, incident grouping across cameras and
//! devices, and incident downloads that survive stalls, disconnects, and a
//! restart without fetching a completed chunk twice. The devices run on the
//! test's runtime and paused clock, reached through in-memory pipes, so
//! stall timeouts and backoffs run in simulated time.

use bc_protocol::archive::DateTime;
use bc_protocol::media::Record;
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, FnConnector, Item, SessionConfig, TcpConnector};
use mock_nvr::archive::{ArchiveConfig, DownloadFault, DownloadLog, FaultKind, MockEvent};
use mock_nvr::fixture::Fixture;
use recordings::chunk;
use recordings::incident::{CameraEvent, CameraId, GroupingConfig, Incident, group};
use recordings::index::{DeviceIndex, EventQuery, IndexConfig};
use recordings::indexer::{IndexedChannel, Indexer, IndexerConfig, run_pass};
use recordings::scheduler::{
    CameraSource, CameraState, IncidentDownload, IncidentProgress, PARTS_DIR, Scheduler,
    SchedulerConfig,
};
use recordings::{Clock, DeviceCamera, Secs, secs};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn ev(ch: u8, kind: &str, a: DateTime, b: DateTime) -> MockEvent {
    MockEvent {
        channel: ch,
        kind: kind.into(),
        start: a,
        end: b,
    }
}

/// Device A: a vehicle seen on channels 0 then 1, and unrelated motion.
fn events_a() -> Vec<MockEvent> {
    vec![
        ev(0, "vehicle", d(10, 7, 42, 5), d(10, 7, 42, 25)),
        ev(1, "vehicle", d(10, 7, 42, 20), d(10, 7, 42, 40)),
        ev(0, "md", d(10, 3, 0, 0), d(10, 3, 0, 12)),
        ev(1, "people", d(9, 18, 30, 0), d(9, 18, 30, 15)),
    ]
}

/// Device B: the same vehicle on its channel 0, a little later.
fn events_b() -> Vec<MockEvent> {
    vec![ev(0, "vehicle", d(10, 7, 42, 45), d(10, 7, 43, 10))]
}

/// Where a test's devices and sessions run.
#[derive(Clone, Copy, PartialEq)]
enum Timing {
    /// On the test's runtime and its paused clock, through in-memory pipes.
    Simulated,
    /// On the devices' own threads in real time, over TCP.
    Real,
}

fn spawn_mock(events: Vec<MockEvent>, rate: Option<u64>) -> mock_nvr::MockNvr {
    spawn_mock_on(Timing::Simulated, events, rate)
}

fn spawn_mock_on(clock: Timing, events: Vec<MockEvent>, rate: Option<u64>) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 2;
    let mut a = ArchiveConfig::new(vec![d(8, 0, 0, 0), d(9, 0, 0, 0), d(10, 0, 0, 0)]);
    a.events_per_day = 0;
    a.events = events;
    a.download_rate = rate;
    cfg.archive = Some(a);
    match clock {
        Timing::Simulated => mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current()),
        Timing::Real => mock_nvr::spawn(cfg).unwrap(),
    }
}

fn session(nvr: &mock_nvr::MockNvr) -> Arc<DeviceSession> {
    session_on(Timing::Simulated, nvr)
}

fn session_on(clock: Timing, nvr: &mock_nvr::MockNvr) -> Arc<DeviceSession> {
    let mut cfg = SessionConfig::new("example-user", "example-password");
    cfg.query_timeout = Duration::from_secs(5);
    cfg.download_stall_timeout = Duration::from_millis(700);
    cfg.backoff.initial = Duration::from_millis(50);
    cfg.backoff.jitter = 0.0;
    let addr = nvr.addr();
    match clock {
        Timing::Simulated => {
            let connector = FnConnector(move || mock_nvr::connect(addr));
            Arc::new(DeviceSession::spawn(connector, cfg))
        }
        Timing::Real => Arc::new(DeviceSession::spawn(TcpConnector::new(addr), cfg)),
    }
}

fn fixed_clock(t: Arc<AtomicI64>) -> Clock {
    Arc::new(move || t.load(Ordering::SeqCst))
}

fn channels() -> Vec<IndexedChannel> {
    (0..2).map(IndexedChannel::first).collect()
}

fn cam(channel: u8) -> DeviceCamera {
    DeviceCamera::first(channel)
}

/// Indexes a device and returns its events as camera events.
async fn index_device(
    s: &DeviceSession,
    dir: &Path,
    device: &str,
    clock: Clock,
) -> Vec<CameraEvent> {
    let idx = Mutex::new(DeviceIndex::open(dir, IndexConfig::default()).unwrap());
    let mut cfg = IndexerConfig::new(channels(), clock);
    cfg.days_back = 3;
    run_pass(s, &idx, &cfg).await;
    let idx = idx.lock().unwrap();
    idx.events(&EventQuery {
        start: 0,
        end: Secs::MAX / 2,
        ..EventQuery::default()
    })
    .into_iter()
    .map(|e| CameraEvent {
        start: e.start,
        end: e.end,
        camera: CameraId::new(device, e.channel),
        kind: e.kind,
    })
    .collect()
}

#[tokio::test(start_paused = true)]
async fn index_fills_newest_first_refreshes_today_and_searches_by_type() {
    let nvr = spawn_mock(events_a(), None);
    let s = session(&nvr);
    let dir = tempfile::tempdir().unwrap();
    let now = Arc::new(AtomicI64::new(secs(d(10, 12, 0, 0))));
    let idx = Mutex::new(DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap());
    let mut cfg = IndexerConfig::new(channels(), fixed_clock(now.clone()));
    cfg.days_back = 5;

    let r = run_pass(&s, &idx, &cfg).await;
    assert_eq!(r.errors, 0);
    assert_eq!(r.months, 1);
    let day = |n: u8| secs(d(n, 0, 0, 0));
    assert_eq!(
        r.days,
        vec![
            (cam(0), day(10)),
            (cam(1), day(10)),
            (cam(0), day(9)),
            (cam(1), day(9)),
            (cam(0), day(8)),
            (cam(1), day(8)),
        ]
    );
    {
        let i = idx.lock().unwrap();
        assert_eq!(i.recorded_days(cam(1)), vec![day(8), day(9), day(10)]);
        assert_eq!(i.files(cam(0), day(10), day(10) + 86_399).len(), 24);
        let vehicles = i.events(&EventQuery {
            kinds: vec!["vehicle".into()],
            start: day(10),
            end: day(10) + 86_399,
            ..EventQuery::default()
        });
        assert_eq!(vehicles.len(), 2);
        assert!(vehicles.iter().all(|e| !e.file_id.is_empty()));
        let people = i.events(&EventQuery {
            kinds: vec!["people".into()],
            cameras: vec![cam(1)],
            start: day(8),
            end: day(10) + 86_399,
        });
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].start, secs(d(9, 18, 30, 0)));
    }

    // Nothing is stale yet.
    let searches = nvr.command_count(14);
    let r = run_pass(&s, &idx, &cfg).await;
    assert!(r.days.is_empty() && r.months == 0);
    assert_eq!(nvr.command_count(14), searches);

    // Two minutes later only today (and the current month) is refreshed.
    now.fetch_add(120, Ordering::SeqCst);
    let r = run_pass(&s, &idx, &cfg).await;
    assert_eq!(r.days, vec![(cam(0), day(10)), (cam(1), day(10))]);
    assert_eq!(r.months, 1);

    // The index survives a restart.
    drop(idx);
    let i = DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap();
    assert_eq!(i.days(), vec![day(8), day(9), day(10)]);

    // The background indexer runs passes until stopped.
    now.fetch_add(120, Ordering::SeqCst);
    let shared = Arc::new(Mutex::new(i));
    let indexer = Indexer::spawn(s.clone(), shared.clone(), cfg.clone());
    let mut reports = indexer.reports();
    let report = tokio::time::timeout(Duration::from_secs(10), reports.wait_for(Option::is_some))
        .await
        .unwrap()
        .unwrap()
        .clone()
        .unwrap();
    assert_eq!(report.days, vec![(cam(0), day(10)), (cam(1), day(10))]);
    indexer.stop().await;
    s.close_shared().await;
}

#[tokio::test(start_paused = true)]
async fn events_from_two_devices_group_into_one_three_camera_incident() {
    let (a, b) = (spawn_mock(events_a(), None), spawn_mock(events_b(), None));
    let (sa, sb) = (session(&a), session(&b));
    let dir = tempfile::tempdir().unwrap();
    let clock = fixed_clock(Arc::new(AtomicI64::new(secs(d(10, 12, 0, 0)))));
    let mut events = index_device(&sa, &dir.path().join("a"), "nvr-a", clock.clone()).await;
    events.extend(index_device(&sb, &dir.path().join("b"), "nvr-b", clock).await);
    let incidents = group(&events, &GroupingConfig::default());
    // Newest first: the vehicle, the night motion, yesterday's person.
    assert_eq!(incidents.len(), 3);
    let v = &incidents[0];
    assert_eq!(v.cameras().len(), 3);
    assert_eq!(v.kinds().into_iter().collect::<Vec<_>>(), vec!["vehicle"]);
    assert_eq!(
        (v.start(), v.end()),
        (secs(d(10, 7, 42, 5)), secs(d(10, 7, 43, 10)))
    );
    let r = v.download_ranges();
    assert_eq!(
        r[&CameraId::new("nvr-b", 0)],
        (secs(d(10, 7, 42, 40)), secs(d(10, 7, 43, 15)))
    );
    sa.close_shared().await;
    sb.close_shared().await;
}

fn vehicle_incident() -> Incident {
    let e = |dev: &str, ch: u8, a: DateTime, b: DateTime| CameraEvent {
        start: secs(a),
        end: secs(b),
        camera: CameraId::new(dev, ch),
        kind: "vehicle".into(),
    };
    let events = vec![
        e("nvr-a", 0, d(10, 7, 42, 5), d(10, 7, 42, 25)),
        e("nvr-a", 1, d(10, 7, 42, 20), d(10, 7, 42, 40)),
        e("nvr-b", 0, d(10, 7, 42, 45), d(10, 7, 43, 10)),
    ];
    let incidents = group(&events, &GroupingConfig::default());
    assert_eq!(incidents.len(), 1);
    incidents.into_iter().next().unwrap()
}

fn sources() -> Vec<CameraSource> {
    [
        ("nvr-a", 0, "Front"),
        ("nvr-a", 1, "Side"),
        ("nvr-b", 0, "Gate"),
    ]
    .into_iter()
    .map(|(dev, ch, name)| CameraSource {
        camera: CameraId::new(dev, ch),
        name: name.into(),
        uid: None,
        stream: StreamTier::Main,
    })
    .collect()
}

fn fast(root: &Path) -> SchedulerConfig {
    let mut c = SchedulerConfig::new(root);
    c.chunk_secs = 10;
    c.retry_delay = Duration::from_millis(50);
    c.max_retry_delay = Duration::from_millis(200);
    c
}

fn settled(p: &[IncidentProgress]) -> bool {
    !p.is_empty() && p.iter().all(IncidentProgress::is_settled)
}

async fn wait(s: &Scheduler, limit: Duration, f: impl Fn(&[IncidentProgress]) -> bool) {
    tokio::time::timeout(limit, s.wait_for(f))
        .await
        .expect("scheduler did not get there in time");
}

/// Times each window was requested, per channel.
fn requests(log: &[DownloadLog]) -> HashMap<(u8, Secs), usize> {
    let mut m = HashMap::new();
    for l in log {
        *m.entry((l.channel, secs(l.start))).or_insert(0) += 1;
    }
    m
}

fn check_chunks(folder: &Path, parts: &str, n: usize) {
    let dir = folder.join(PARTS_DIR).join(parts);
    for i in 0..n {
        let idx = chunk::read_index(&dir, i as u32).unwrap().unwrap();
        assert!(chunk::is_complete(&dir, i as u32));
        assert!(idx.video[0].key, "chunk {i} starts at a keyframe");
        assert_eq!(idx.codec.as_deref(), Some("H264"));
        // The cut starts at the keyframe at or before the window start
        // (keyframes every 2 s in the mock), and covers the window.
        let first = (idx.video[0].pts_us / 1_000_000) as Secs;
        assert!(first <= idx.start && first > idx.start - 2, "chunk {i}");
        let last = (idx.video.last().unwrap().pts_us / 1_000_000) as Secs;
        assert!(last >= idx.end, "chunk {i}");
    }
}

#[tokio::test(start_paused = true)]
async fn three_camera_download_survives_a_stall_and_a_disconnect() {
    three_camera_download(Timing::Simulated).await;
}

/// The same download, with live view throughout, in real time over TCP on
/// a multi-threaded runtime while every core of the machine is kept busy:
/// the live-gap limit (400 ms) under real load, which simulated time cannot
/// show (there, disk work and CPU contention take no time). Opt-in, since
/// it depends on the machine: `cargo test -p recordings --test e2e --
/// --ignored`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real-time stress test; run with --ignored"]
async fn three_camera_download_keeps_live_view_under_real_load() {
    let _load = CpuLoad::start();
    three_camera_download(Timing::Real).await;
}

/// Busy threads, one per core, until dropped.
struct CpuLoad {
    stop: Arc<std::sync::atomic::AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl CpuLoad {
    fn start() -> CpuLoad {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let n = std::thread::available_parallelism().map_or(4, |n| n.get());
        let threads = (0..n)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut x = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        x = std::hint::black_box(x.wrapping_mul(6_364_136_223_846_793_005) + 1);
                    }
                })
            })
            .collect();
        CpuLoad { stop, threads }
    }
}

impl Drop for CpuLoad {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

async fn three_camera_download(clock: Timing) {
    let (a, b) = (
        spawn_mock_on(clock, vec![], None),
        spawn_mock_on(clock, vec![], None),
    );
    // Side (A ch 1) stalls in its second chunk; Gate (B ch 0) loses its
    // connection in its third.
    a.inject_download_fault(DownloadFault {
        channel: Some(1),
        after_bytes: 30_000,
        kind: FaultKind::Stall,
        skip: 1,
    });
    b.inject_download_fault(DownloadFault {
        channel: None,
        after_bytes: 30_000,
        kind: FaultKind::Disconnect,
        skip: 2,
    });
    let (sa, sb) = (session_on(clock, &a), session_on(clock, &b));
    // Live view on device A runs throughout. The downloads start once its
    // first frame is in, and the worst gap between frames is measured from
    // then on: time to the first frame is stream startup, not a stall.
    let mut live = sa.subscribe(0, StreamTier::Sub).await.unwrap();
    let (first_tx, first_rx) = tokio::sync::oneshot::channel();
    let live_task = tokio::spawn(async move {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), live.recv()).await {
                Ok(Some(Item::Media(Record::Video { .. }))) => break,
                Ok(Some(_)) => {}
                other => panic!("no live video: {other:?}"),
            }
        }
        let _ = first_tx.send(());
        let mut worst = Duration::ZERO;
        let mut frames = 0u32;
        let mut last = Instant::now();
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            if let Ok(Some(Item::Media(Record::Video { .. }))) =
                tokio::time::timeout(Duration::from_secs(1), live.recv()).await
            {
                frames += 1;
                worst = worst.max(last.elapsed());
                last = Instant::now();
            }
        }
        (frames, worst)
    });
    first_rx.await.expect("live view started");

    let root = tempfile::tempdir().unwrap();
    let sched = Scheduler::new(fast(root.path()), None);
    sched.add_device("nvr-a", sa.clone());
    sched.add_device("nvr-b", sb.clone());
    let folder = sched
        .enqueue(&IncidentDownload {
            incident: vehicle_incident(),
            cameras: sources(),
        })
        .unwrap();
    assert_eq!(
        folder.file_name().unwrap().to_str().unwrap(),
        "2026-10-10 07.42 vehicle \u{2014} Front, Side, Gate"
    );
    // Queuing it again changes nothing.
    assert_eq!(
        sched
            .enqueue(&IncidentDownload {
                incident: vehicle_incident(),
                cameras: sources(),
            })
            .unwrap(),
        folder
    );
    wait(&sched, Duration::from_secs(30), settled).await;

    let p = sched.progress().borrow().clone();
    let cams = &p[0].cameras;
    assert_eq!(cams.len(), 3);
    for c in cams {
        assert_eq!(c.state, CameraState::ChunksComplete, "{}", c.name);
        assert_eq!(c.chunks_done, c.chunks_total);
        assert!(c.bytes > 0);
    }
    // Ranges with margins, in 10 s chunks: Front from 7.42.00 to 7.42.30
    // (3 chunks), Side 7.42.15 to 7.42.45 (3), Gate 7.42.40 to 7.43.15 (4).
    let totals: Vec<usize> = cams.iter().map(|c| c.chunks_total).collect();
    assert_eq!(totals, vec![3, 3, 4]);
    let names: Vec<String> = cams
        .iter()
        .map(|c| c.output.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec![
            "2026-10-10 07.42.00 Front.mp4",
            "2026-10-10 07.42.15 Side.mp4",
            "2026-10-10 07.42.40 Gate.mp4"
        ]
    );
    check_chunks(&folder, "nvr-a-ch00", 3);
    check_chunks(&folder, "nvr-a-ch01", 3);
    check_chunks(&folder, "nvr-b-ch00", 4);

    // Every window was fetched once, except the two that failed: once more
    // each. Nothing completed was fetched again.
    let ra = requests(&a.download_log());
    let rb = requests(&b.download_log());
    assert_eq!(ra[&(1, secs(d(10, 7, 42, 25)))], 2, "stalled chunk retried");
    assert_eq!(ra.values().filter(|n| **n > 1).count(), 1);
    assert_eq!(ra.values().sum::<usize>(), 3 + 3 + 1);
    assert_eq!(
        rb[&(0, secs(d(10, 7, 43, 0)))],
        2,
        "disconnected chunk retried"
    );
    assert_eq!(rb.values().sum::<usize>(), 4 + 1);
    // The stalled download was stopped with 144.
    assert!(a.command_count(144) >= 1);

    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(folder.join("incident.json")).unwrap()).unwrap();
    assert_eq!(json["types"][0], "vehicle");
    assert_eq!(json["cameras"].as_array().unwrap().len(), 3);
    assert_eq!(json["start"], "2026-10-10 07:42:05");

    let (frames, worst) = live_task.await.unwrap();
    // The mock sends 25 fps, so about 75 frames in the 3 s window; the
    // count only proves the stream kept flowing. The worst gap is the
    // guarantee that downloads never stall live view. (Chunk writes and job
    // saves used to sync to disk on async workers; on a small loaded
    // machine that held the session's reader up for most of a second.)
    assert!(frames >= 30, "live frames during the downloads: {frames}");
    assert!(
        worst < Duration::from_millis(400),
        "worst live gap {worst:?}"
    );

    // A finished download is not resumed after a restart, and can be
    // forgotten.
    let again = Scheduler::new(fast(root.path()), None);
    assert_eq!(again.resume_all().unwrap(), 0);
    assert!(again.progress().borrow().is_empty());
    let id = p[0].id.clone();
    assert!(sched.forget(&id));
    assert!(sched.progress().borrow().is_empty());

    sched.shutdown().await;
    drop(sched);
    sa.close_shared().await;
    sb.close_shared().await;
}

/// Closes a shared session once the scheduler has released it.
trait CloseShared {
    async fn close_shared(self);
}

impl CloseShared for Arc<DeviceSession> {
    async fn close_shared(self) {
        match Arc::try_unwrap(self) {
            Ok(s) => s.close().await,
            Err(s) => drop(s),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_restart_resumes_from_the_first_missing_chunk() {
    // Throttled so a chunk (about 100 kB here) takes a while.
    let a = spawn_mock(vec![], Some(150_000));
    let b = spawn_mock(vec![], Some(150_000));
    let root = tempfile::tempdir().unwrap();
    let incident = vehicle_incident();

    let (sa, sb) = (session(&a), session(&b));
    let first = Scheduler::new(fast(root.path()), None);
    first.add_device("nvr-a", sa.clone());
    first.add_device("nvr-b", sb.clone());
    let folder = first
        .enqueue(&IncidentDownload {
            incident: incident.clone(),
            cameras: sources(),
        })
        .unwrap();
    // Stop once some chunks are done, but not all.
    wait(&first, Duration::from_secs(30), |p| {
        p.first()
            .is_some_and(|i| i.cameras.iter().map(|c| c.chunks_done).sum::<usize>() >= 2)
    })
    .await;
    first.shutdown().await;
    let before = first.progress().borrow().clone();
    let done_before: usize = before[0].cameras.iter().map(|c| c.chunks_done).sum();
    assert!((2..10).contains(&done_before), "{done_before}");
    // Every download still running at shutdown was stopped with 144 (the
    // mock counts requests as it reads them, a moment later).
    let counts = || {
        let started = a.download_log().len() + b.download_log().len();
        let stops = (a.command_count(144) + b.command_count(144)) as usize;
        (started - done_before, stops)
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while counts().0 != counts().1 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (in_flight, stops) = counts();
    assert_eq!(stops, in_flight);
    drop(first);
    let log_before = (a.download_log().len(), b.download_log().len());
    // Windows complete on disk before the restart, per device.
    let complete = |parts: &str, ch: u8| -> Vec<(u8, Secs)> {
        let dir = folder.join(PARTS_DIR).join(parts);
        (0..4u32)
            .filter(|i| chunk::is_complete(&dir, *i))
            .map(|i| (ch, chunk::read_index(&dir, i).unwrap().unwrap().start))
            .collect()
    };
    let mut done_a = complete("nvr-a-ch00", 0);
    done_a.extend(complete("nvr-a-ch01", 1));
    let done_b = complete("nvr-b-ch00", 0);
    assert_eq!(done_a.len() + done_b.len(), done_before);

    // A new scheduler (as after an app restart) picks the job up from disk.
    let second = Scheduler::new(fast(root.path()), None);
    assert_eq!(second.resume_all().unwrap(), 1);
    second.add_device("nvr-a", sa.clone());
    second.add_device("nvr-b", sb.clone());
    wait(&second, Duration::from_secs(30), settled).await;
    let after = second.progress().borrow().clone();
    assert_eq!(after[0].folder, folder);
    assert!(
        after[0]
            .cameras
            .iter()
            .all(|c| c.state == CameraState::ChunksComplete)
    );

    // After the restart, only missing chunks were requested: windows
    // completed before were not fetched again.
    let new_a: Vec<(u8, Secs)> = a.download_log()[log_before.0..]
        .iter()
        .map(|l| (l.channel, secs(l.start)))
        .collect();
    let new_b: Vec<(u8, Secs)> = b.download_log()[log_before.1..]
        .iter()
        .map(|l| (l.channel, secs(l.start)))
        .collect();
    assert!(
        new_a.iter().all(|w| !done_a.contains(w)),
        "{new_a:?} {done_a:?}"
    );
    assert!(
        new_b.iter().all(|w| !done_b.contains(w)),
        "{new_b:?} {done_b:?}"
    );
    // Exactly the missing chunks, each once.
    assert_eq!(new_a.len() + new_b.len(), 10 - done_before);
    check_chunks(&folder, "nvr-a-ch00", 3);
    check_chunks(&folder, "nvr-b-ch00", 4);

    second.shutdown().await;
    drop(second);
    sa.close_shared().await;
    sb.close_shared().await;
}

#[tokio::test(start_paused = true)]
async fn pausing_stops_the_running_download_and_resuming_finishes() {
    let a = spawn_mock(vec![], Some(100_000));
    let sa = session(&a);
    let root = tempfile::tempdir().unwrap();
    let sched = Scheduler::new(fast(root.path()), None);
    sched.add_device("nvr-a", sa.clone());
    let mut inc = vehicle_incident();
    assert!(inc.remove_camera(&CameraId::new("nvr-b", 0)));
    let id = inc.id.clone();
    sched
        .enqueue(&IncidentDownload {
            incident: inc,
            cameras: sources(),
        })
        .unwrap();
    // Wait until a download is running on the device, then pause.
    let deadline = Instant::now() + Duration::from_secs(10);
    while a.download_log().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(sched.set_paused(&id, true).unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    while a.command_count(144) == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(a.command_count(144), 1, "pause stopped the download");
    let p = sched.progress().borrow().clone();
    assert!(p[0].paused);
    assert!(p[0].cameras.iter().all(|c| c.chunks_done < c.chunks_total));

    assert!(sched.set_paused(&id, false).unwrap());
    wait(&sched, Duration::from_secs(30), settled).await;
    let p = sched.progress().borrow().clone();
    assert!(
        p[0].cameras
            .iter()
            .all(|c| c.state == CameraState::ChunksComplete),
        "{p:#?}"
    );
    sched.shutdown().await;
    drop(sched);
    sa.close_shared().await;
}
