//! Download control against in-process mock NVRs on the test's paused
//! clock: one camera paused while the others finish, a device removed and
//! added back (its jobs wait, nothing completed is fetched again), a range
//! with no recording reported as such, and forgetting a download with its
//! partial files.

use bc_protocol::archive::DateTime;
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, FnConnector, SessionConfig};
use mock_nvr::archive::ArchiveConfig;
use mock_nvr::fixture::Fixture;
use recordings::finalize::Mp4Finalizer;
use recordings::incident::{CameraEvent, CameraId, GroupingConfig, Incident, group};
use recordings::scheduler::{
    CameraProgress, CameraSource, CameraState, FailReason, Finalizer, IncidentDownload,
    IncidentProgress, PARTS_DIR, Scheduler, SchedulerConfig,
};
use recordings::secs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn spawn_mock(rate: Option<u64>) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(Fixture::load(&path).unwrap());
    cfg.channels = 2;
    let mut a = ArchiveConfig::new(vec![d(10, 0, 0, 0)]);
    a.events_per_day = 0;
    a.download_rate = rate;
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

/// Two cameras of one device, both seeing the same 20 s on `day`.
fn incident(day: u8) -> Incident {
    let e = |ch: u8| CameraEvent {
        start: secs(d(day, 7, 42, 5)),
        end: secs(d(day, 7, 42, 25)),
        camera: CameraId::new("nvr-a", ch),
        kind: "vehicle".into(),
    };
    group(&[e(0), e(1)], &GroupingConfig::default()).remove(0)
}

fn sources() -> Vec<CameraSource> {
    [(0, "Front"), (1, "Side")]
        .into_iter()
        .map(|(ch, name)| CameraSource {
            camera: CameraId::new("nvr-a", ch),
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

async fn wait(s: &Scheduler, f: impl Fn(&[IncidentProgress]) -> bool) {
    tokio::time::timeout(Duration::from_secs(60), s.wait_for(f))
        .await
        .expect("scheduler did not get there in time");
}

fn cam(p: &[IncidentProgress], ch: u8) -> &CameraProgress {
    p[0].cameras
        .iter()
        .find(|c| c.camera.channel == ch)
        .unwrap()
}

/// Closes a shared session once the scheduler has released it.
async fn close(s: Arc<DeviceSession>) {
    if let Ok(s) = Arc::try_unwrap(s) {
        s.close().await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_paused_camera_and_a_removed_device_wait_and_resume() {
    // Throttled so each 10 s chunk takes a while.
    let nvr = spawn_mock(Some(150_000));
    let s = session(&nvr);
    let root = tempfile::tempdir().unwrap();
    let sched = Scheduler::new(fast(root.path()), None);
    let side = CameraId::new("nvr-a", 1);
    let folder = sched
        .enqueue(&IncidentDownload {
            incident: incident(10),
            cameras: sources(),
        })
        .unwrap();
    let id = sched.progress().borrow()[0].id.clone();
    // Side is paused before anything runs: Front alone downloads.
    assert!(sched.set_camera_paused(&id, &side, true).unwrap());
    assert!(!sched.set_camera_paused("nope", &side, true).unwrap());
    // Not added yet: Front waits for its device.
    assert!(cam(&sched.progress().borrow(), 0).waiting_device);
    sched.add_device("nvr-a", s.clone());
    wait(&sched, |p| cam(p, 0).state == CameraState::ChunksComplete).await;
    {
        let p = sched.progress().borrow().clone();
        let side_p = cam(&p, 1);
        assert!(side_p.paused);
        assert_eq!(
            (side_p.state.clone(), side_p.chunks_done),
            (CameraState::Queued, 0)
        );
        assert!(!cam(&p, 0).waiting_device);
    }

    // Side resumes; partway, its device is removed: it stops and waits.
    sched.set_camera_paused(&id, &side, false).unwrap();
    wait(&sched, |p| cam(p, 1).chunks_done >= 1).await;
    sched.remove_device("nvr-a");
    wait(&sched, |p| {
        let c = cam(p, 1);
        c.waiting_device && c.state == CameraState::Queued
    })
    .await;
    let done_before = cam(&sched.progress().borrow(), 1).chunks_done;
    let requests = nvr.download_log().len();
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(nvr.download_log().len(), requests, "nothing while removed");

    // Added back (same session): it finishes, without fetching a completed
    // chunk again.
    sched.add_device("nvr-a", s.clone());
    wait(&sched, |p| cam(p, 1).state == CameraState::ChunksComplete).await;
    let side_requests: Vec<_> = nvr
        .download_log()
        .into_iter()
        .filter(|l| l.channel == 1)
        .map(|l| secs(l.start))
        .collect();
    let mut unique = side_requests.clone();
    unique.dedup();
    // At most the one chunk that was running at removal is asked twice.
    assert!(side_requests.len() <= unique.len() + 1, "{side_requests:?}");
    assert!(done_before >= 1);

    // Forgotten with its partial files: the parts folder goes, what the
    // user keeps (incident.json) stays.
    assert!(sched.forget(&id, true).unwrap());
    assert!(!folder.join(PARTS_DIR).exists());
    assert!(folder.join("incident.json").exists());
    sched.shutdown().await;
    drop(sched);
    close(s).await;
}

#[tokio::test(start_paused = true)]
async fn a_range_without_recording_fails_as_no_recording() {
    let nvr = spawn_mock(None);
    let s = session(&nvr);
    let root = tempfile::tempdir().unwrap();
    let sched = Scheduler::new(fast(root.path()), None);
    sched.add_device("nvr-a", s.clone());
    // The mock records only on the 10th.
    sched
        .enqueue(&IncidentDownload {
            incident: incident(9),
            cameras: sources(),
        })
        .unwrap();
    wait(&sched, |p| {
        !p.is_empty() && p.iter().all(IncidentProgress::is_settled)
    })
    .await;
    for c in &sched.progress().borrow()[0].cameras {
        match &c.state {
            CameraState::Failed(f) => assert_eq!(f.reason, FailReason::NoRecording, "{f}"),
            other => panic!("{other:?}"),
        }
    }
    // Each window was asked once: no retries for an empty range.
    assert_eq!(nvr.download_log().len(), 2 * 3);
    // Retrying a camera queues it again.
    let id = sched.progress().borrow()[0].id.clone();
    assert!(sched.retry_camera(&id, &CameraId::new("nvr-a", 0)).unwrap());
    assert!(!sched.retry_camera(&id, &CameraId::new("nvr-a", 5)).unwrap());
    sched.shutdown().await;
    drop(sched);
    close(s).await;
}

/// [`incident`] with the cameras' events running until `end_s` past 7:42.
fn incident_until(day: u8, end_s: u8) -> Incident {
    let e = |ch: u8| CameraEvent {
        start: secs(d(day, 7, 42, 5)),
        end: secs(d(day, 7, 42, end_s)),
        camera: CameraId::new("nvr-a", ch),
        kind: "vehicle".into(),
    };
    group(&[e(0), e(1)], &GroupingConfig::default()).remove(0)
}

fn request(inc: &Incident) -> IncidentDownload {
    IncidentDownload {
        incident: inc.clone(),
        cameras: sources(),
    }
}

#[tokio::test(start_paused = true)]
async fn downloads_are_deduplicated_by_key_while_unfinished_and_trashed_whole() {
    let nvr = spawn_mock(None);
    let s = session(&nvr);
    let root = tempfile::tempdir().unwrap();
    let finalizer = || Some(Arc::new(Mp4Finalizer::default()) as Arc<dyn Finalizer>);
    let sched = Scheduler::new(fast(root.path()), finalizer());
    let first = incident_until(10, 25);
    let grown = incident_until(10, 40);
    assert_eq!(first.key(), grown.key());
    assert_ne!(first.id, grown.id);

    // Unfinished (its device is not added yet): the grown incident gets
    // the same download, with the range it was queued with.
    let a = sched.enqueue_incident(&request(&first)).unwrap();
    assert!(!a.existing);
    assert_eq!(a.id, first.id);
    let b = sched.enqueue_incident(&request(&grown)).unwrap();
    assert_eq!((b.id.as_str(), b.existing), (a.id.as_str(), true));
    assert_eq!(sched.progress().borrow().len(), 1);
    assert_eq!(sched.progress().borrow()[0].key, first.key());

    sched.add_device("nvr-a", s.clone());
    wait(&sched, |p| p.iter().all(IncidentProgress::is_done)).await;
    // Finished and unchanged: the same download.
    let c = sched.enqueue_incident(&request(&first)).unwrap();
    assert_eq!((c.id.as_str(), c.existing), (a.id.as_str(), true));
    // Finished, and the incident grew since: a new folder.
    let g = sched.enqueue_incident(&request(&grown)).unwrap();
    assert!(!g.existing);
    assert_eq!(g.id, grown.id);
    assert_eq!(
        g.folder.file_name().unwrap().to_string_lossy(),
        format!("{} (2)", a.folder.file_name().unwrap().to_string_lossy())
    );

    // Trash: stopped, forgotten, every file left in place.
    assert_eq!(sched.trash("nope").await.unwrap(), None);
    let folder = sched.trash(&g.id).await.unwrap().unwrap();
    assert_eq!(folder, g.folder);
    assert!(sched.progress().borrow().iter().all(|p| p.id != g.id));
    assert!(folder.join("incident.json").is_file());
    sched.shutdown().await;
    drop(sched);

    // After a restart, a finished download is not resumed, but queuing the
    // unchanged incident again finds it on disk.
    let sched = Scheduler::new(fast(root.path()), finalizer());
    sched.resume_all().unwrap();
    assert!(
        sched.progress().borrow().is_empty(),
        "finished and trashed downloads are not resumed"
    );
    let again = sched.enqueue_incident(&request(&first)).unwrap();
    assert_eq!((again.folder, again.existing), (a.folder, true));
    assert!(sched.progress().borrow()[0].is_done());
    assert!(nvr.download_log().len() >= 2);
    sched.shutdown().await;
    drop(sched);
    close(s).await;
}

/// A device going away clears the size estimate in the same progress list
/// that says the camera waits for it, so one change is one event (the
/// estimate is not reset in a later, second change).
#[tokio::test(start_paused = true)]
async fn removing_a_device_clears_the_estimate_in_the_same_update() {
    let nvr = spawn_mock(Some(150_000));
    let s = session(&nvr);
    let root = tempfile::tempdir().unwrap();
    let sched = Scheduler::new(fast(root.path()), None);
    sched.add_device("nvr-a", s.clone());
    sched
        .enqueue(&IncidentDownload {
            incident: incident(10),
            cameras: sources(),
        })
        .unwrap();
    wait(&sched, |p| {
        cam(p, 1).bytes_estimate.is_some() && !cam(p, 1).waiting_device
    })
    .await;
    // The single-threaded test runtime cannot run the workers between these
    // two statements, so this is the list the removal itself published.
    sched.remove_device("nvr-a");
    let p = sched.progress().borrow().clone();
    assert!(cam(&p, 1).waiting_device);
    assert_eq!(cam(&p, 1).bytes_estimate, None);
    sched.shutdown().await;
    drop(sched);
    close(s).await;
}
