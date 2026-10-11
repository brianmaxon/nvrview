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
use recordings::incident::{CameraEvent, CameraId, GroupingConfig, Incident, group};
use recordings::scheduler::{
    CameraProgress, CameraSource, CameraState, FailReason, IncidentDownload, IncidentProgress,
    PARTS_DIR, Scheduler, SchedulerConfig,
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
