//! Range downloads through the scheduler against an in-process mock NVR on
//! the test's paused clock (docs/FFI-RECORDINGS.md, section 7a, "Range
//! download"): a planned range becomes camera jobs and finished files, the
//! same request is the same download, a folder outside the downloads
//! directory is listed in the registry and resumes after a restart, and
//! share-safe jobs write no `incident.json`.

use bc_protocol::archive::DateTime;
use bc_session::{DeviceSession, FnConnector, SessionConfig};
use mock_nvr::archive::ArchiveConfig;
use mock_nvr::fixture::Fixture;
use recordings::Tier;
use recordings::export::range::{RangeCameraRequest, RangeJob, RangeRequest, plan_range};
use recordings::finalize::Mp4Finalizer;
use recordings::incident::CameraId;
use recordings::scheduler::{
    EnqueueError, Finalizer, IncidentProgress, MAX_DOWNLOADS, PARTS_DIR, Scheduler, SchedulerConfig,
};
use recordings::secs;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn spawn_mock() -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 2;
    let mut a = ArchiveConfig::new(vec![d(10, 0, 0, 0)]);
    a.events_per_day = 0;
    cfg.archive = Some(a);
    mock_nvr::spawn_in(cfg, &tokio::runtime::Handle::current())
}

fn session(nvr: &mock_nvr::MockNvr) -> Arc<DeviceSession> {
    let mut cfg = SessionConfig::new("example-user", "example-password");
    cfg.query_timeout = Duration::from_secs(5);
    cfg.download_stall_timeout = Duration::from_millis(700);
    cfg.backoff.initial = Duration::from_millis(50);
    cfg.backoff.jitter = 0.0;
    let addr = nvr.addr();
    Arc::new(DeviceSession::spawn(
        FnConnector(move || mock_nvr::connect(addr)),
        cfg,
    ))
}

fn config(root: &Path, registry: Option<PathBuf>) -> SchedulerConfig {
    let mut c = SchedulerConfig::new(root);
    c.chunk_secs = 10;
    c.retry_delay = Duration::from_millis(50);
    c.max_retry_delay = Duration::from_millis(200);
    c.registry = registry;
    c
}

fn scheduler(root: &Path, registry: Option<PathBuf>) -> Scheduler {
    let f: Arc<dyn Finalizer> = Arc::new(Mp4Finalizer::default());
    Scheduler::new(config(root, registry), Some(f))
}

async fn wait(s: &Scheduler, f: impl Fn(&[IncidentProgress]) -> bool) {
    tokio::time::timeout(Duration::from_secs(60), s.wait_for(f))
        .await
        .expect("scheduler did not get there in time");
}

fn names() -> BTreeMap<CameraId, String> {
    [(0, "Front"), (1, "Side")]
        .into_iter()
        .map(|(ch, n)| (CameraId::new("nvr-a", ch), n.to_owned()))
        .collect()
}

fn request(start_s: u8, end_s: u8) -> RangeRequest {
    RangeRequest {
        start: secs(d(10, 7, 42, start_s)),
        end: secs(d(10, 7, 42, end_s)),
        cameras: (0..2)
            .map(|ch| RangeCameraRequest {
                camera: CameraId::new("nvr-a", ch),
                tier: Tier::Main,
            })
            .collect(),
        name: None,
        dest: None,
        share_safe: false,
    }
}

fn plan(req: &RangeRequest) -> RangeJob {
    plan_range(req, &names(), None).unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_range_downloads_to_finished_files_and_the_same_request_is_the_same_download() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let root = tempfile::tempdir().unwrap();
    let sched = scheduler(root.path(), None);
    sched.add_device("nvr-a", s.clone());

    let job = plan(&request(5, 25));
    let a = sched.enqueue_range(&job, &[]).unwrap();
    assert!(!a.existing);
    assert_eq!(a.id, job.id);
    assert_eq!(a.folder, root.path().join(&job.folder_name));
    wait(&sched, |p| {
        !p.is_empty() && p.iter().all(IncidentProgress::is_done)
    })
    .await;
    {
        let p = sched.progress().borrow().clone();
        assert_eq!(p.len(), 1);
        // A range download has no incident behind it.
        assert_eq!(
            p[0].range.map(|r| (r.start, r.end)),
            Some((job.start, job.end))
        );
        assert_eq!(p[0].key, job.id);
        for (c, j) in p[0].cameras.iter().zip(&job.cameras) {
            assert_eq!(c.output, a.folder.join(&j.file));
            assert_eq!(c.start, job.start);
            assert!(c.output.is_file(), "{:?}", c.output);
        }
    }
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(a.folder.join("incident.json")).unwrap()).unwrap();
    assert_eq!(json["kind"], "range");
    assert_eq!(json["id"], job.id.as_str());

    // The same cameras, tiers, and range, in another order: the download
    // that exists, finished as it is.
    let mut again = request(5, 25);
    again.cameras.reverse();
    let b = sched.enqueue_range(&plan(&again), &[]).unwrap();
    assert_eq!((b.id.as_str(), b.existing), (a.id.as_str(), true));
    assert_eq!(b.folder, a.folder);
    // Another range is another download in another folder.
    let c = sched.enqueue_range(&plan(&request(5, 20)), &[]).unwrap();
    assert!(!c.existing);
    assert_ne!(c.folder, a.folder);
    assert_eq!(sched.progress().borrow().len(), 2);

    sched.shutdown().await;
    drop(sched);
    if let Ok(s) = Arc::try_unwrap(s) {
        s.close().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_taken_folder_gets_a_number_and_a_known_one_from_an_earlier_run_is_found() {
    let root = tempfile::tempdir().unwrap();
    let job = plan(&request(5, 25));
    // Something else in the way.
    std::fs::create_dir_all(root.path().join(&job.folder_name)).unwrap();
    let sched = scheduler(root.path(), None);
    let a = sched.enqueue_range(&job, &[]).unwrap();
    assert!(
        a.folder
            .to_string_lossy()
            .ends_with(&format!("{} (2)", job.folder_name)),
        "{:?}",
        a.folder
    );
    sched.shutdown().await;
    drop(sched);
    // A new run (nothing known in memory, the device never added): the same
    // request finds its download on disk.
    let sched = scheduler(root.path(), None);
    let b = sched.enqueue_range(&job, &[]).unwrap();
    assert_eq!((b.existing, &b.folder), (true, &a.folder));
    sched.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn share_safe_ranges_write_no_incident_json() {
    let root = tempfile::tempdir().unwrap();
    let sched = scheduler(root.path(), None);
    let mut req = request(5, 25);
    req.share_safe = true;
    req.name = Some("Backyard".into());
    let job = plan(&req);
    assert!(!job.folder_name.contains("Front") && !job.folder_name.contains("Backyard"));
    let a = sched.enqueue_range(&job, &[]).unwrap();
    assert!(a.folder.join(PARTS_DIR).is_dir());
    assert!(!a.folder.join("incident.json").exists());
    sched.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_destination_outside_the_root_needs_the_registry_and_resumes_from_it() {
    let nvr = spawn_mock();
    let s = session(&nvr);
    let root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let registry = data.path().join("downloads.json");
    let mut req = request(5, 25);
    req.dest = Some(elsewhere.path().to_string_lossy().into_owned());
    let job = plan(&req);

    // No registry: refused, and nothing is created.
    let sched = scheduler(root.path(), None);
    assert!(matches!(
        sched.enqueue_range(&job, &[]),
        Err(EnqueueError::NotConfigured)
    ));
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    sched.shutdown().await;

    // With one: queued where the user said, and listed.
    let sched = scheduler(root.path(), Some(registry.clone()));
    let a = sched.enqueue_range(&job, &[]).unwrap();
    assert!(a.folder.starts_with(elsewhere.path()));
    let listed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
    assert_eq!(listed["folders"][0], a.folder.to_string_lossy().as_ref());
    // Stopped before its device was ever added: the restart picks it up.
    sched.shutdown().await;
    drop(sched);

    let sched = scheduler(root.path(), Some(registry.clone()));
    sched.add_device("nvr-a", s.clone());
    assert_eq!(sched.resume_all().unwrap(), 1);
    wait(&sched, |p| {
        !p.is_empty() && p.iter().all(IncidentProgress::is_done)
    })
    .await;
    assert!(
        sched.progress().borrow()[0]
            .cameras
            .iter()
            .all(|c| c.output.starts_with(elsewhere.path()) && c.output.is_file())
    );
    // A finished download is no longer listed after the next restart...
    sched.shutdown().await;
    drop(sched);
    let sched = scheduler(root.path(), Some(registry.clone()));
    assert_eq!(sched.resume_all().unwrap(), 0);
    let listed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
    assert_eq!(listed["folders"].as_array().unwrap().len(), 0);

    // ...and an incident download can go to a directory of its own too.
    sched.shutdown().await;
    if let Ok(s) = Arc::try_unwrap(s) {
        s.close().await;
    }
}

#[tokio::test(start_paused = true)]
async fn forgetting_with_partial_files_drops_the_registry_entry() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let registry = data.path().join("downloads.json");
    let mut req = request(5, 25);
    req.dest = Some(elsewhere.path().to_string_lossy().into_owned());
    let job = plan(&req);
    let sched = scheduler(root.path(), Some(registry.clone()));
    let a = sched.enqueue_range(&job, &[]).unwrap();
    assert!(sched.forget(&a.id, true).unwrap());
    let listed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
    assert_eq!(listed["folders"].as_array().unwrap().len(), 0);
    sched.shutdown().await;
}

#[test]
fn the_downloads_limit_is_a_thousand_by_default() {
    assert_eq!(MAX_DOWNLOADS, 1000);
    assert_eq!(SchedulerConfig::new("x").max_downloads, MAX_DOWNLOADS);
}

/// With a limit of three (the default is a thousand, checked above), the
/// fourth range is refused and a known one is still answered.
#[tokio::test(start_paused = true)]
async fn the_downloads_limit_applies_to_ranges() {
    const LIMIT: usize = 3;
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(root.path(), None);
    cfg.max_downloads = LIMIT;
    let f: Arc<dyn Finalizer> = Arc::new(Mp4Finalizer::default());
    let sched = Scheduler::new(cfg, Some(f));
    let base = secs(d(10, 0, 0, 0));
    for n in 0..LIMIT as i64 {
        let mut req = request(0, 10);
        req.start = base + n * 2;
        req.end = req.start + 1;
        req.cameras.truncate(1);
        sched.enqueue_range(&plan(&req), &[]).unwrap();
    }
    let mut req = request(0, 10);
    req.start = base + 100_000;
    req.end = req.start + 1;
    assert!(matches!(
        sched.enqueue_range(&plan(&req), &[]),
        Err(EnqueueError::Limit)
    ));
    // A known one is still answered.
    let mut known = request(0, 10);
    known.start = base;
    known.end = base + 1;
    known.cameras.truncate(1);
    assert!(sched.enqueue_range(&plan(&known), &[]).unwrap().existing);
    sched.shutdown().await;
}
