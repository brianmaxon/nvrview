//! Background downloads of incidents (docs/PLAYBACK.md, "Download").
//!
//! An incident becomes one **camera job** per camera: that camera's range
//! (its events plus the margin), cut into windows of
//! [`SchedulerConfig::chunk_secs`] (60 s by default), each fetched with one
//! download (command 143) and written as a raw chunk ([`crate::chunk`]).
//!
//! - **Retries.** A chunk that stalls (nothing for the session's stall
//!   timeout; the download is stopped with 144), loses its connection, or
//!   fails otherwise is retried after a growing delay, up to
//!   [`SchedulerConfig::max_attempts`] times; then the camera job is marked
//!   failed with a [`FailReason`] and can be retried with
//!   [`Scheduler::retry`] or [`Scheduler::retry_camera`]. While the device
//!   is not connected, or not added ([`Scheduler::remove_device`]), or its
//!   login was refused, camera jobs wait (`waiting_device` in the progress)
//!   without spending attempts.
//! - **Resume.** Chunk completion is recorded in the job file after every
//!   chunk, so work always resumes at the first missing chunk; completed
//!   chunks are never fetched again, after a failure, a pause, or an app
//!   restart ([`Scheduler::resume_all`]).
//! - **Scheduling.** Camera jobs of an incident are queued together, in
//!   enqueue order. Each device has [`SchedulerConfig::per_device`] workers
//!   (default 1, and the session's own download limit applies too); devices
//!   work in parallel.
//! - **Output.** One folder per incident ([`crate::naming`]) with
//!   `incident.json`; the chunks and the job file live in its `.parts`
//!   subfolder. Joining a camera's chunks into its MP4 file is the
//!   [`Finalizer`] hook, left to the muxer; without one, a camera job stops
//!   at [`CameraState::ChunksComplete`].
//! - **Live view first.** Downloads share the device's session with live
//!   view, and usually its async runtime. File writes, `fsync`s, and job
//!   file saves run on blocking threads, never on an async worker: a worker
//!   stuck in a slow disk flush also holds up the tasks queued behind it,
//!   which can be the session's reader and with it every live frame.

use crate::chunk::{self, ChunkIndex, ChunkWriter};
use crate::incident::{CameraId, Incident};
use crate::naming::{self, NamePattern};
use crate::store::{read_json, write_atomic, write_json};
use crate::{Secs, date_time};
use bc_protocol::archive::DownloadRequest;
use bc_protocol::media::Record;
use bc_protocol::stream::StreamTier;
use bc_session::{
    ConnState, DeviceSession, Download, DownloadError, DownloadEvent, QueryError, SearchError,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

const JOB_VERSION: u32 = 1;
/// Name of the hidden folder inside an incident folder.
pub const PARTS_DIR: &str = ".parts";
const JOB_FILE: &str = "job.json";
/// Records queued for a chunk's writer while it writes the previous batch.
/// When the disk falls behind, the download waits here, and its session
/// buffer (bounded) takes the rest; live view never waits.
const WRITE_QUEUE: usize = 256;

/// The final step of a camera job: join its complete chunks into one file.
///
/// The implementation reads chunks `0..chunks` from `parts_dir` (see
/// [`crate::chunk`]) and writes `output`; [`crate::finalize::Mp4Finalizer`]
/// is the one the apps use. It runs on a blocking thread and must leave
/// `output` absent or complete, never partial. Without a finalizer, camera
/// jobs end at [`CameraState::ChunksComplete`].
pub trait Finalizer: Send + Sync {
    /// Joins the chunks into `output`.
    fn finalize(&self, parts_dir: &Path, chunks: u32, output: &Path) -> io::Result<()>;
}

/// Scheduler settings.
#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    /// Where incident folders are created.
    pub root: PathBuf,
    /// Window of one chunk in seconds (default 60).
    pub chunk_secs: Secs,
    /// Attempts per chunk before the camera job fails (default 8).
    pub max_attempts: u32,
    /// Delay before the first retry; doubles per attempt (default 1 s).
    pub retry_delay: Duration,
    /// Longest retry delay (default 30 s).
    pub max_retry_delay: Duration,
    /// Camera jobs downloading at once per device (default 1).
    pub per_device: usize,
    /// How long a cancelled download may take to send its stop.
    pub cancel_timeout: Duration,
    /// Output names.
    pub pattern: NamePattern,
}

impl SchedulerConfig {
    /// Defaults with output under `root`.
    pub fn new(root: impl Into<PathBuf>) -> SchedulerConfig {
        SchedulerConfig {
            root: root.into(),
            chunk_secs: 60,
            max_attempts: 8,
            retry_delay: Duration::from_secs(1),
            max_retry_delay: Duration::from_secs(30),
            per_device: 1,
            cancel_timeout: Duration::from_secs(2),
            pattern: NamePattern::default(),
        }
    }
}

/// Where a camera's video comes from, and what to call it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraSource {
    /// The camera.
    pub camera: CameraId,
    /// The user's name for it (used in file names).
    pub name: String,
    /// Its device ID (needed for a second lens).
    pub uid: Option<String>,
    /// Stream to download (lens and tier).
    pub stream: StreamTier,
}

/// Why a camera job gave up (docs/FFI-RECORDINGS.md, "Downloads").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailReason {
    /// Nothing arrived for the stall timeout, attempt after attempt.
    Stalled,
    /// The connection or the session ended during the download.
    SessionClosed,
    /// The device answered with an error.
    Refused,
    /// The range held no video.
    NoRecording,
    /// The chunks could not be written (disk full or not writable).
    WriteFailed,
    /// The chunks could not be joined into the output file (for example a
    /// codec change inside the range).
    JoinFailed,
    /// The device refused the login.
    LoginFailed,
    /// Anything else (and failures recorded before reasons were kept).
    Other,
}

impl FailReason {
    /// The reason's name in the C interface.
    pub const fn name(self) -> &'static str {
        match self {
            FailReason::Stalled => "stalled",
            FailReason::SessionClosed => "session_closed",
            FailReason::Refused => "refused",
            FailReason::NoRecording => "no_recording",
            FailReason::WriteFailed => "write_failed",
            FailReason::JoinFailed => "join_failed",
            FailReason::LoginFailed => "login_failed",
            FailReason::Other => "other",
        }
    }
}

/// A camera job's failure: the reason, and English detail for logs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "FailureRepr")]
pub struct Failure {
    /// Why.
    pub reason: FailReason,
    /// English detail, for logs and diagnostics, not for parsing.
    pub detail: String,
}

impl Failure {
    fn new(reason: FailReason, detail: impl Into<String>) -> Failure {
        Failure {
            reason,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason.name(), self.detail)
    }
}

/// How a failure is read back: job files written before reasons were kept
/// hold only the text.
#[derive(Deserialize)]
#[serde(untagged)]
enum FailureRepr {
    Full { reason: FailReason, detail: String },
    Text(String),
}

impl From<FailureRepr> for Failure {
    fn from(r: FailureRepr) -> Failure {
        match r {
            FailureRepr::Full { reason, detail } => Failure { reason, detail },
            FailureRepr::Text(detail) => Failure {
                reason: FailReason::Other,
                detail,
            },
        }
    }
}

/// State of one camera job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CameraState {
    /// Waiting for a worker (or for its device).
    Queued,
    /// Downloading.
    Running,
    /// Every chunk is on disk; the final file needs the [`Finalizer`].
    ChunksComplete,
    /// The final file is written.
    Done,
    /// Gave up. [`Scheduler::retry`] queues it again.
    Failed(Failure),
}

/// One chunk of a camera job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkState {
    /// Window start (naive seconds).
    pub start: Secs,
    /// Window end.
    pub end: Secs,
    /// Complete on disk.
    pub done: bool,
    /// Payload bytes written.
    pub bytes: u64,
    /// Attempts made.
    pub attempts: u32,
    /// Video frames in the complete chunk (`None` when not recorded, as in
    /// job files written before this was kept).
    #[serde(default)]
    pub video_frames: Option<u32>,
}

/// One camera's part of an incident download, as persisted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraJob {
    /// The camera.
    pub camera: CameraId,
    /// Its name.
    pub name: String,
    /// Its device ID.
    pub uid: Option<String>,
    /// Stream name (`mainStream`, ...).
    pub stream: String,
    /// Range start.
    pub start: Secs,
    /// Range end.
    pub end: Secs,
    /// Output file name in the incident folder.
    pub file: String,
    /// Parts folder name inside `.parts`.
    pub parts: String,
    /// Chunks in order.
    pub chunks: Vec<ChunkState>,
    /// State.
    pub state: CameraState,
    /// Paused by the user (this camera only).
    #[serde(default)]
    pub paused: bool,
    /// When its first picture was taken, in ms on the keyframes' wall
    /// clock ([`crate::recorded_time`]; whether that clock is local or UTC
    /// is still open), once every chunk is on disk.
    #[serde(default)]
    pub first_picture_wall_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JobFile {
    version: u32,
    incident: Incident,
    paused: bool,
    cameras: Vec<CameraJob>,
    /// Handed to the app for the Trash ([`Scheduler::trash`]): never
    /// resumed or returned again, even if the folder stays.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    trashed: bool,
}

/// Progress of one camera.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraProgress {
    /// The camera.
    pub camera: CameraId,
    /// Its name.
    pub name: String,
    /// Chunks complete.
    pub chunks_done: usize,
    /// Chunks in all.
    pub chunks_total: usize,
    /// Payload bytes written.
    pub bytes: u64,
    /// `bytes` plus the device's size estimate of the chunk being
    /// downloaded, when the device sent one.
    pub bytes_estimate: Option<u64>,
    /// State.
    pub state: CameraState,
    /// Paused by the user (this camera only; see also the incident's).
    pub paused: bool,
    /// Queued or running, but its device is not added or not connected.
    pub waiting_device: bool,
    /// A worker has the camera job right now (downloading, waiting for its
    /// device, or joining its chunks): something may still write into the
    /// download's folder.
    pub working: bool,
    /// See [`CameraJob::first_picture_wall_ms`].
    pub first_picture_wall_ms: Option<i64>,
    /// The final file's path.
    pub output: PathBuf,
}

/// Progress of one incident download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncidentProgress {
    /// Incident ID (the download's ID).
    pub id: String,
    /// The incident's stable key ([`Incident::key`]) as queued.
    pub key: String,
    /// Its folder.
    pub folder: PathBuf,
    /// Paused by the user.
    pub paused: bool,
    /// Per camera.
    pub cameras: Vec<CameraProgress>,
}

impl IncidentProgress {
    /// Whether no camera job is queued or running.
    pub fn is_settled(&self) -> bool {
        self.cameras
            .iter()
            .all(|c| !matches!(c.state, CameraState::Queued | CameraState::Running))
    }

    /// Whether every camera job is done (the download is finished).
    pub fn is_done(&self) -> bool {
        self.cameras.iter().all(|c| c.state == CameraState::Done)
    }

    /// Whether a worker has any of its camera jobs.
    pub fn is_working(&self) -> bool {
        self.cameras.iter().any(|c| c.working)
    }
}

/// What [`Scheduler::enqueue_incident`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Enqueued {
    /// The download's ID (the incident's [`Incident::id`] when it was
    /// first queued).
    pub id: String,
    /// Its folder.
    pub folder: PathBuf,
    /// An existing download was returned instead of queuing a new one.
    pub existing: bool,
}

/// Why [`Scheduler::enqueue_incident`] queued nothing.
#[derive(Debug)]
pub enum EnqueueError {
    /// [`MAX_DOWNLOADS`] downloads are known already.
    Limit,
    /// No camera of the incident has a source.
    NoSource,
    /// The folder or job file could not be written.
    Io(io::Error),
}

impl fmt::Display for EnqueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnqueueError::Limit => f.write_str("too many downloads known; forget finished ones"),
            EnqueueError::NoSource => f.write_str("no camera of the incident has a source"),
            EnqueueError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for EnqueueError {}

impl From<EnqueueError> for io::Error {
    fn from(e: EnqueueError) -> io::Error {
        match e {
            EnqueueError::Io(e) => e,
            EnqueueError::NoSource => io::Error::new(io::ErrorKind::InvalidInput, e.to_string()),
            EnqueueError::Limit => io::Error::other(e.to_string()),
        }
    }
}

/// A request to download an incident.
#[derive(Clone, Debug)]
pub struct IncidentDownload {
    /// The incident.
    pub incident: Incident,
    /// One source per camera to download; cameras of the incident without a
    /// source are skipped.
    pub cameras: Vec<CameraSource>,
}

/// Why a chunk attempt failed.
#[derive(Debug)]
enum ChunkError {
    Download(DownloadError),
    Start(SearchError),
    Io(io::Error),
    /// Paused, cancelled, or shutting down: not a failure.
    Interrupted,
}

impl fmt::Display for ChunkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChunkError::Download(e) => write!(f, "{e}"),
            ChunkError::Start(e) => write!(f, "{e}"),
            ChunkError::Io(e) => write!(f, "write failed: {e}"),
            ChunkError::Interrupted => write!(f, "interrupted"),
        }
    }
}

impl ChunkError {
    /// The reason a camera job reports when this error ends it.
    fn reason(&self) -> FailReason {
        match self {
            ChunkError::Download(DownloadError::Stalled) => FailReason::Stalled,
            ChunkError::Download(
                DownloadError::ConnectionLost | DownloadError::Closed | DownloadError::Cancelled,
            ) => FailReason::SessionClosed,
            ChunkError::Download(DownloadError::Refused(_) | DownloadError::NotAllowed) => {
                FailReason::Refused
            }
            // The chunk writer fell behind the device.
            ChunkError::Download(DownloadError::Overflow) => FailReason::WriteFailed,
            ChunkError::Download(DownloadError::BadMedia | DownloadError::Busy) => {
                FailReason::Other
            }
            ChunkError::Start(SearchError::Query(q)) => match q {
                QueryError::Closed | QueryError::ConnectionLost => FailReason::SessionClosed,
                QueryError::TimedOut => FailReason::Stalled,
                QueryError::NotAllowed => FailReason::Refused,
                QueryError::Busy => FailReason::Other,
            },
            ChunkError::Start(SearchError::Refused(_)) => FailReason::Refused,
            ChunkError::Start(_) => FailReason::Other,
            ChunkError::Io(_) => FailReason::WriteFailed,
            ChunkError::Interrupted => FailReason::Other,
        }
    }
}

/// What a camera job is doing right now (not persisted).
#[derive(Clone, Copy, Debug, Default)]
struct Live {
    /// A worker has it.
    running: bool,
    /// Its worker waits for the device to connect.
    waiting: bool,
    /// The device's size estimate of the chunk being downloaded.
    estimate: Option<u64>,
}

struct Job {
    seq: u64,
    /// The incident's [`Incident::key`] (derived from `file`, kept).
    key: String,
    folder: PathBuf,
    file: JobFile,
    live: Vec<Live>,
    /// Bumped on every change to `file`; see [`JobSave`].
    version: u64,
    /// What was written to disk; held for the whole of every write.
    saved: Arc<Mutex<Saved>>,
}

/// The job file's state on disk, shared by a job's saves.
#[derive(Debug, Default)]
struct Saved {
    /// The version last written.
    version: u64,
    /// The job was forgotten: nothing may be written for it any more (its
    /// parts folder may be gone).
    closed: bool,
}

impl Job {
    fn new(seq: u64, folder: PathBuf, file: JobFile) -> Job {
        Job {
            seq,
            key: file.incident.key(),
            folder,
            live: vec![Live::default(); file.cameras.len()],
            file,
            version: 0,
            saved: Arc::new(Mutex::new(Saved::default())),
        }
    }

    fn is_done(&self) -> bool {
        self.file
            .cameras
            .iter()
            .all(|c| c.state == CameraState::Done)
    }

    fn any_running(&self) -> bool {
        self.live.iter().any(|l| l.running)
    }

    fn parts_root(&self) -> PathBuf {
        self.folder.join(PARTS_DIR)
    }

    /// A snapshot of the job file to write once the state lock is released.
    fn snapshot(&mut self) -> io::Result<JobSave> {
        self.version += 1;
        Ok(JobSave {
            path: self.parts_root().join(JOB_FILE),
            bytes: serde_json::to_vec_pretty(&self.file).map_err(io::Error::other)?,
            version: self.version,
            saved: self.saved.clone(),
        })
    }
}

/// A job file write taken under the state lock and done outside it (it
/// syncs to disk, which can take a long time). Saves of one job are
/// serialized, and one older than what is already on disk is skipped, so
/// the file never goes back to an earlier state.
struct JobSave {
    path: PathBuf,
    bytes: Vec<u8>,
    version: u64,
    saved: Arc<Mutex<Saved>>,
}

impl JobSave {
    /// Writes it; blocking. Skipped once the job was forgotten, so a save
    /// taken before [`Scheduler::forget`] never recreates its folder.
    fn write(self) -> io::Result<()> {
        let mut saved = lock(&self.saved);
        if saved.closed || self.version <= saved.version {
            return Ok(());
        }
        write_atomic(&self.path, &self.bytes)?;
        saved.version = self.version;
        Ok(())
    }

    /// Writes it on a blocking thread.
    async fn write_off_thread(self) -> io::Result<()> {
        tokio::task::spawn_blocking(move || self.write())
            .await
            .map_err(io::Error::other)?
    }
}

#[derive(Default)]
struct State {
    jobs: HashMap<String, Job>,
    next_seq: u64,
    shutdown: bool,
}

struct Inner {
    cfg: SchedulerConfig,
    finalizer: Option<Arc<dyn Finalizer>>,
    state: Mutex<State>,
    devices: Mutex<HashMap<String, Arc<DeviceSession>>>,
    changed: Notify,
    progress: watch::Sender<Vec<IncidentProgress>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    /// Devices whose workers run (they stay, idle, after the device is
    /// removed, and serve it again when it is added back).
    staffed: Mutex<HashSet<String>>,
}

/// Most downloads the scheduler knows at once ([`Scheduler::forget`]
/// finished ones).
pub const MAX_DOWNLOADS: usize = 1000;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The download scheduler. Cheap to clone; all clones share one queue.
#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

impl fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scheduler")
            .field("root", &self.inner.cfg.root)
            .finish_non_exhaustive()
    }
}

/// Windows of `chunk` seconds covering `start..=end`. Each window ends where
/// the next begins, so no second is missed whether the device treats the
/// end as inclusive or not (the overlap is removed by timestamp).
pub fn chunk_windows(start: Secs, end: Secs, chunk: Secs) -> Vec<(Secs, Secs)> {
    let chunk = chunk.max(1);
    let end = end.max(start);
    let mut out = Vec::new();
    let mut s = start;
    loop {
        let e = (s + chunk).min(end);
        out.push((s, e));
        if e >= end {
            break;
        }
        s = e;
    }
    out
}

/// Queues a failed camera job again with fresh attempts; true if it had
/// failed.
fn retry_failed(c: &mut CameraJob) -> bool {
    if !matches!(c.state, CameraState::Failed(_)) {
        return false;
    }
    c.state = CameraState::Queued;
    for ch in &mut c.chunks {
        ch.attempts = 0;
    }
    true
}

/// Brings a job file read back from disk up to date with its folder:
/// chunks marked complete whose files are missing are fetched again, and a
/// job that was running is queued.
fn normalize(folder: &Path, j: &mut JobFile) {
    for cam in &mut j.cameras {
        let dir = folder.join(PARTS_DIR).join(&cam.parts);
        for (i, c) in cam.chunks.iter_mut().enumerate() {
            if c.done && !chunk::is_complete(&dir, i as u32) {
                c.done = false;
                c.bytes = 0;
            }
        }
        match cam.state {
            CameraState::Running => cam.state = CameraState::Queued,
            CameraState::Done | CameraState::ChunksComplete
                if cam.chunks.iter().any(|c| !c.done) =>
            {
                cam.state = CameraState::Queued;
            }
            _ => {}
        }
    }
}

fn slug(c: &CameraId) -> String {
    let base = if c.lens == 0 {
        format!("{}-ch{:02}", c.device, c.channel)
    } else {
        format!("{}-ch{:02}-lens{}", c.device, c.channel, c.lens)
    };
    naming::sanitize(&base).replace(' ', "_")
}

impl Scheduler {
    /// A scheduler writing under `cfg.root`; `finalizer` is the MP4 hook.
    /// Call [`Scheduler::resume_all`] to pick up work from an earlier run,
    /// and [`Scheduler::add_device`] for every device.
    pub fn new(cfg: SchedulerConfig, finalizer: Option<Arc<dyn Finalizer>>) -> Scheduler {
        let (progress, _) = watch::channel(Vec::new());
        Scheduler {
            inner: Arc::new(Inner {
                cfg,
                finalizer,
                state: Mutex::new(State::default()),
                devices: Mutex::new(HashMap::new()),
                changed: Notify::new(),
                progress,
                workers: Mutex::new(Vec::new()),
                staffed: Mutex::new(HashSet::new()),
            }),
        }
    }

    /// Makes a device's session available (or replaces it, for example
    /// after a refused login, with one that has new credentials) and starts
    /// its workers. Jobs of devices that are not added wait
    /// (`waiting_device`).
    pub fn add_device(&self, device: &str, session: Arc<DeviceSession>) {
        lock(&self.inner.devices).insert(device.to_owned(), session);
        let new = lock(&self.inner.staffed).insert(device.to_owned());
        if new {
            let mut w = lock(&self.inner.workers);
            for _ in 0..self.inner.cfg.per_device.max(1) {
                let inner = self.inner.clone();
                let dev = device.to_owned();
                w.push(tokio::spawn(device_worker(inner, dev)));
            }
        }
        self.inner.publish();
        self.inner.changed.notify_waiters();
    }

    /// Takes a device's session away (the device was removed). Its running
    /// chunks are stopped (144) and its camera jobs wait until the device
    /// is added again; completed chunks are kept. The scheduler holds no
    /// reference to the session once its running chunks have stopped.
    pub fn remove_device(&self, device: &str) {
        let removed = lock(&self.inner.devices).remove(device);
        self.inner.publish();
        self.inner.changed.notify_waiters();
        drop(removed);
    }

    /// Progress of every known incident download, updated on every change.
    pub fn progress(&self) -> watch::Receiver<Vec<IncidentProgress>> {
        self.inner.progress.subscribe()
    }

    /// Queues an incident: creates its folder, `incident.json`, and job
    /// file. Queuing the same incident again returns the existing folder;
    /// see [`Scheduler::enqueue_incident`] for the rules.
    pub fn enqueue(&self, req: &IncidentDownload) -> io::Result<PathBuf> {
        self.enqueue_incident(req)
            .map(|e| e.folder)
            .map_err(io::Error::from)
    }

    /// Queues an incident's download, unless one already covers it
    /// (docs/FFI-RECORDINGS.md, "Download IDs"):
    ///
    /// 1. an **unfinished** download (any state but done, failed and paused
    ///    included) of the same incident [`Incident::key`] is returned, even
    ///    if the incident has grown since (its range stays as queued);
    /// 2. otherwise a download with the same [`Incident::id`] (the incident
    ///    has not changed) is returned, also a finished one from an earlier
    ///    run whose folder is still there (it is known again from then on);
    /// 3. otherwise a new download is queued, in a new folder: ` (2)`,
    ///    ` (3)`, ... is appended when the name is taken.
    pub fn enqueue_incident(&self, req: &IncidentDownload) -> Result<Enqueued, EnqueueError> {
        let inc = &req.incident;
        let ranges: BTreeMap<CameraId, (Secs, Secs)> = inc
            .download_ranges()
            .into_iter()
            .filter(|(c, _)| req.cameras.iter().any(|s| &s.camera == c))
            .collect();
        if ranges.is_empty() {
            return Err(EnqueueError::NoSource);
        }
        let key = inc.key();
        {
            let st = lock(&self.inner.state);
            let unfinished = st
                .jobs
                .values()
                .filter(|j| j.key == key && !j.is_done())
                .min_by_key(|j| j.seq);
            if let Some(j) = unfinished.or_else(|| st.jobs.get(&inc.id)) {
                return Ok(Enqueued {
                    id: j.file.incident.id.clone(),
                    folder: j.folder.clone(),
                    existing: true,
                });
            }
            if st.jobs.len() >= MAX_DOWNLOADS {
                return Err(EnqueueError::Limit);
            }
        }
        let source = |c: &CameraId| req.cameras.iter().find(|s| &s.camera == c);
        let names: BTreeMap<CameraId, String> = ranges
            .keys()
            .filter_map(|c| Some((c.clone(), source(c)?.name.clone())))
            .collect();
        let ordered: Vec<String> = {
            // Cameras in the order they first saw the incident.
            let mut v: Vec<(&CameraId, &(Secs, Secs))> = ranges.iter().collect();
            v.sort_by_key(|(c, r)| (r.0, (*c).clone()));
            v.iter().map(|(c, _)| names[*c].clone()).collect()
        };
        let pattern = &self.inner.cfg.pattern;
        let base = naming::folder_name(inc, &ordered, pattern);
        let mut folder = self.inner.cfg.root.join(&base);
        let mut n = 2;
        loop {
            match read_json::<JobFile>(&folder.join(PARTS_DIR).join(JOB_FILE)) {
                Ok(Some(mut j))
                    if j.incident.id == inc.id && j.version == JOB_VERSION && !j.trashed =>
                {
                    // Queued in an earlier run (finished, or beyond the
                    // resume cap): known again, as it is on disk.
                    normalize(&folder, &mut j);
                    self.insert(folder.clone(), j).map_err(EnqueueError::Io)?;
                    return Ok(Enqueued {
                        id: inc.id.clone(),
                        folder,
                        existing: true,
                    });
                }
                Ok(None) if !folder.exists() => break,
                _ => {
                    folder = self.inner.cfg.root.join(format!("{base} ({n})"));
                    n += 1;
                }
            }
        }
        let io = EnqueueError::Io;
        fs::create_dir_all(folder.join(PARTS_DIR)).map_err(io)?;
        let json = naming::incident_json(inc, &ranges, &names, pattern);
        write_json(&folder.join("incident.json"), &json).map_err(io)?;
        let cameras: Vec<CameraJob> = ranges
            .iter()
            .filter_map(|(c, (s, e))| {
                let src = source(c)?;
                Some(CameraJob {
                    camera: c.clone(),
                    name: src.name.clone(),
                    uid: src.uid.clone(),
                    stream: src.stream.name().to_owned(),
                    start: *s,
                    end: *e,
                    file: naming::camera_file_name(*s, &src.name, pattern),
                    parts: slug(c),
                    chunks: chunk_windows(*s, *e, self.inner.cfg.chunk_secs)
                        .into_iter()
                        .map(|(start, end)| ChunkState {
                            start,
                            end,
                            done: false,
                            bytes: 0,
                            attempts: 0,
                            video_frames: None,
                        })
                        .collect(),
                    state: CameraState::Queued,
                    paused: false,
                    first_picture_wall_ms: None,
                })
            })
            .collect();
        let file = JobFile {
            version: JOB_VERSION,
            incident: inc.clone(),
            paused: false,
            cameras,
            trashed: false,
        };
        self.insert(folder.clone(), file).map_err(io)?;
        info!(cameras = ranges.len(), "incident download queued");
        Ok(Enqueued {
            id: inc.id.clone(),
            folder,
            existing: false,
        })
    }

    fn insert(&self, folder: PathBuf, file: JobFile) -> io::Result<()> {
        let seq = {
            let mut st = lock(&self.inner.state);
            st.next_seq += 1;
            st.next_seq - 1
        };
        // Saved before it is shared, so without holding the state lock.
        let mut job = Job::new(seq, folder, file);
        job.snapshot()?.write()?;
        lock(&self.inner.state)
            .jobs
            .insert(job.file.incident.id.clone(), job);
        self.inner.publish();
        self.inner.changed.notify_waiters();
        Ok(())
    }

    /// Picks up every unfinished incident download under the root, from
    /// its job file. Chunks marked complete whose files are missing are
    /// fetched again. Returns how many incidents were resumed.
    pub fn resume_all(&self) -> io::Result<usize> {
        let mut found = Vec::new();
        if self.inner.cfg.root.is_dir() {
            for entry in fs::read_dir(&self.inner.cfg.root)? {
                let folder = entry?.path();
                let path = folder.join(PARTS_DIR).join(JOB_FILE);
                match read_json::<JobFile>(&path) {
                    Ok(Some(j)) if j.version == JOB_VERSION && !j.trashed => {
                        found.push((folder, j));
                    }
                    Ok(_) => {}
                    Err(e) => warn!(error = %e, "unreadable download job skipped"),
                }
            }
        }
        found.sort_by_key(|(_, j)| j.incident.start());
        let mut n = 0;
        for (folder, mut j) in found {
            {
                let st = lock(&self.inner.state);
                if st.jobs.contains_key(&j.incident.id) {
                    continue;
                }
                if st.jobs.len() >= MAX_DOWNLOADS {
                    warn!("too many unfinished downloads; the oldest are resumed first");
                    break;
                }
            }
            normalize(&folder, &mut j);
            let unfinished = j.cameras.iter().any(|c| {
                matches!(c.state, CameraState::Queued)
                    || (c.state == CameraState::ChunksComplete && self.inner.finalizer.is_some())
            });
            let failed = j
                .cameras
                .iter()
                .any(|c| matches!(c.state, CameraState::Failed(_)));
            // Finished downloads stay on disk but not in memory, so memory
            // does not grow with history. Failed ones are kept for retry.
            if unfinished {
                n += 1;
            } else if !failed {
                continue;
            }
            self.insert(folder, j)?;
        }
        Ok(n)
    }

    /// Pauses or resumes an incident download. A running chunk is stopped
    /// (144) and fetched again on resume; completed chunks are kept.
    pub fn set_paused(&self, incident: &str, paused: bool) -> io::Result<bool> {
        let save = {
            let mut st = lock(&self.inner.state);
            match st.jobs.get_mut(incident) {
                Some(j) => {
                    j.file.paused = paused;
                    Some(j.snapshot()?)
                }
                None => None,
            }
        };
        let found = save.is_some();
        if let Some(save) = save {
            save.write()?;
        }
        self.inner.publish();
        self.inner.changed.notify_waiters();
        Ok(found)
    }

    /// Pauses or resumes one camera of an incident download. A running
    /// chunk is stopped (144) and fetched again on resume. `Ok(false)` if
    /// the incident or the camera is unknown.
    pub fn set_camera_paused(
        &self,
        incident: &str,
        camera: &CameraId,
        paused: bool,
    ) -> io::Result<bool> {
        self.edit_cameras(incident, |c| {
            if &c.camera == camera {
                c.paused = paused;
                true
            } else {
                false
            }
        })
    }

    /// Applies `f` to every camera job of an incident and saves the job;
    /// `Ok(false)` if the incident is unknown or `f` matched no camera.
    fn edit_cameras(
        &self,
        incident: &str,
        mut f: impl FnMut(&mut CameraJob) -> bool,
    ) -> io::Result<bool> {
        let save = {
            let mut st = lock(&self.inner.state);
            match st.jobs.get_mut(incident) {
                Some(j) => {
                    let mut any = false;
                    for c in &mut j.file.cameras {
                        any |= f(c);
                    }
                    if any { Some(j.snapshot()?) } else { None }
                }
                None => None,
            }
        };
        let found = save.is_some();
        if let Some(save) = save {
            save.write()?;
        }
        self.inner.publish();
        self.inner.changed.notify_waiters();
        Ok(found)
    }

    /// Queues an incident's failed camera jobs again with fresh attempts.
    pub fn retry(&self, incident: &str) -> io::Result<bool> {
        self.edit_cameras(incident, retry_failed)?;
        Ok(lock(&self.inner.state).jobs.contains_key(incident))
    }

    /// Queues one failed camera job again with fresh attempts. `Ok(false)`
    /// if the incident or camera is unknown, or the camera had not failed.
    pub fn retry_camera(&self, incident: &str, camera: &CameraId) -> io::Result<bool> {
        self.edit_cameras(incident, |c| &c.camera == camera && retry_failed(c))
    }

    /// Forgets a download that has no camera job running (pause it first);
    /// `Ok(false)` if it is unknown or still running. Its finished files
    /// and `incident.json` are the user's and stay; with `delete_partial`,
    /// its parts folder (raw chunks and the job file) is removed, so it can
    /// no longer resume. Apps call it for downloads the user dismissed, so
    /// the in-memory list stays bounded.
    pub fn forget(&self, incident: &str, delete_partial: bool) -> io::Result<bool> {
        let removed = {
            let mut st = lock(&self.inner.state);
            match st.jobs.get(incident) {
                Some(j) if !j.any_running() => st.jobs.remove(incident),
                _ => None,
            }
        };
        let Some(job) = removed else {
            return Ok(false);
        };
        self.inner.publish();
        // A save taken before the job left the map (the one that cleared
        // its last running flag, for example) may still be writing: wait
        // for it, and make every later one a no-op.
        lock(&job.saved).closed = true;
        if delete_partial {
            match fs::remove_dir_all(job.parts_root()) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    /// Ends a download so its folder can go to the Trash
    /// (docs/FFI-RECORDINGS.md, "Trash"): pauses it (a running chunk is
    /// stopped with 144; a join already running completes), waits until no
    /// worker has any of its camera jobs, then forgets it and keeps every
    /// file. Returns its folder, or `None` if the download is unknown. Once
    /// this returns, nothing writes into the folder.
    pub async fn trash(&self, incident: &str) -> io::Result<Option<PathBuf>> {
        let folder = lock(&self.inner.state)
            .jobs
            .get(incident)
            .map(|j| j.folder.clone());
        let Some(folder) = folder else {
            return Ok(None);
        };
        // The pause saves the job file: a disk write, off the async worker.
        let (me, id) = (self.clone(), incident.to_owned());
        tokio::task::spawn_blocking(move || me.set_paused(&id, true))
            .await
            .map_err(io::Error::other)??;
        let mut rx = self.progress();
        let _ = rx
            .wait_for(|p| {
                p.iter()
                    .find(|x| x.id == incident)
                    .is_none_or(|x| !x.is_working())
            })
            .await;
        // Marked in its job file, so it is not resumed after a restart if
        // the folder stays (moving it to the Trash can fail).
        let save = {
            let mut st = lock(&self.inner.state);
            match st.jobs.get_mut(incident) {
                Some(j) => {
                    j.file.trashed = true;
                    Some(j.snapshot()?)
                }
                None => None,
            }
        };
        if let Some(save) = save {
            save.write_off_thread().await?;
        }
        self.forget(incident, false)?;
        Ok(Some(folder))
    }

    /// Waits until `f` holds for the progress list.
    pub async fn wait_for(&self, f: impl Fn(&[IncidentProgress]) -> bool) {
        let mut rx = self.progress();
        let _ = rx.wait_for(|p| f(p)).await;
    }

    /// Stops every worker (running downloads are stopped with 144) and
    /// waits for them. Job files keep the progress for
    /// [`Scheduler::resume_all`].
    pub async fn shutdown(&self) {
        lock(&self.inner.state).shutdown = true;
        self.inner.changed.notify_waiters();
        let workers: Vec<JoinHandle<()>> = lock(&self.inner.workers).drain(..).collect();
        for w in workers {
            let _ = w.await;
        }
    }
}

impl Inner {
    fn publish(&self) {
        // Which devices can download now (taken before the state lock; the
        // two locks are never held together).
        let ready: HashMap<String, bool> = lock(&self.devices)
            .iter()
            .map(|(d, s)| (d.clone(), s.state() == ConnState::Connected))
            .collect();
        let list = {
            let st = lock(&self.state);
            let mut jobs: Vec<&Job> = st.jobs.values().collect();
            jobs.sort_by_key(|j| j.seq);
            jobs.iter()
                .map(|j| IncidentProgress {
                    id: j.file.incident.id.clone(),
                    key: j.key.clone(),
                    folder: j.folder.clone(),
                    paused: j.file.paused,
                    cameras: j
                        .file
                        .cameras
                        .iter()
                        .zip(&j.live)
                        .map(|(c, live)| {
                            let bytes = c.chunks.iter().map(|x| x.bytes).sum();
                            let active =
                                matches!(c.state, CameraState::Queued | CameraState::Running);
                            let device_ready =
                                ready.get(&c.camera.device).copied().unwrap_or(false);
                            CameraProgress {
                                camera: c.camera.clone(),
                                name: c.name.clone(),
                                chunks_done: c.chunks.iter().filter(|x| x.done).count(),
                                chunks_total: c.chunks.len(),
                                bytes,
                                bytes_estimate: live.estimate.map(|e| bytes + e),
                                state: c.state.clone(),
                                paused: c.paused,
                                waiting_device: active && (!device_ready || live.waiting),
                                working: live.running,
                                first_picture_wall_ms: c.first_picture_wall_ms,
                                output: j.folder.join(&c.file),
                            }
                        })
                        .collect(),
                })
                .collect()
        };
        self.progress.send_replace(list);
    }

    /// Whether a camera job must stop: shutting down, forgotten, paused
    /// (the incident or the camera), or its device removed or given a new
    /// session since the worker took `session`.
    fn interrupted(&self, slot: &JobRef, session: &Arc<DeviceSession>) -> bool {
        let current = lock(&self.devices).get(&slot.device).cloned();
        if !current.is_some_and(|s| Arc::ptr_eq(&s, session)) {
            return true;
        }
        let st = lock(&self.state);
        st.shutdown
            || st.jobs.get(&slot.incident).is_none_or(|j| {
                j.file.paused || j.file.cameras.get(slot.cam).is_none_or(|c| c.paused)
            })
    }

    /// Updates a camera job and saves the job file (on a blocking thread).
    async fn update(&self, slot: &JobRef, f: impl FnOnce(&mut CameraJob, &mut Live)) {
        let save = {
            let mut st = lock(&self.state);
            st.jobs.get_mut(&slot.incident).map(|j| {
                if let (Some(c), Some(l)) =
                    (j.file.cameras.get_mut(slot.cam), j.live.get_mut(slot.cam))
                {
                    f(c, l);
                }
                j.snapshot()
            })
        };
        let result = match save {
            Some(Ok(save)) => save.write_off_thread().await,
            Some(Err(e)) => Err(e),
            None => Ok(()),
        };
        if let Err(e) = result {
            warn!(error = %e, "cannot save the download job");
        }
        // Published once saved, so what the progress shows is on disk.
        self.publish();
    }

    /// Changes what a camera job is doing right now (nothing to save).
    fn set_live(&self, slot: &JobRef, f: impl FnOnce(&mut Live)) {
        {
            let mut st = lock(&self.state);
            if let Some(l) = st
                .jobs
                .get_mut(&slot.incident)
                .and_then(|j| j.live.get_mut(slot.cam))
            {
                f(l);
            }
        }
        self.publish();
    }
}

/// Which camera job a worker has.
struct JobRef {
    incident: String,
    cam: usize,
    device: String,
}

/// A camera job picked by a worker.
struct Picked {
    slot: JobRef,
    job: CameraJob,
    parts_dir: PathBuf,
    output: PathBuf,
}

fn pick(inner: &Inner, device: &str) -> Option<Picked> {
    let mut st = lock(&inner.state);
    if st.shutdown {
        return None;
    }
    let mut jobs: Vec<&mut Job> = st.jobs.values_mut().filter(|j| !j.file.paused).collect();
    jobs.sort_by_key(|j| j.seq);
    for j in jobs {
        for (i, c) in j.file.cameras.iter_mut().enumerate() {
            let pickable = match c.state {
                CameraState::Queued | CameraState::Running => true,
                CameraState::ChunksComplete => inner.finalizer.is_some(),
                _ => false,
            };
            if !pickable || c.paused || j.live[i].running || c.camera.device != device {
                continue;
            }
            j.live[i].running = true;
            if c.state == CameraState::Queued {
                c.state = CameraState::Running;
            }
            return Some(Picked {
                slot: JobRef {
                    incident: j.file.incident.id.clone(),
                    cam: i,
                    device: device.to_owned(),
                },
                job: c.clone(),
                parts_dir: j.folder.join(PARTS_DIR).join(&c.parts),
                output: j.folder.join(&c.file),
            });
        }
    }
    None
}

/// Whether a session can still connect (not refused, not closed).
fn usable(s: &DeviceSession) -> bool {
    !matches!(s.state(), ConnState::LoginFailed { .. } | ConnState::Closed)
}

async fn device_worker(inner: Arc<Inner>, device: String) {
    loop {
        let notified = inner.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if lock(&inner.state).shutdown {
            return;
        }
        let session = lock(&inner.devices).get(&device).cloned();
        let picked = session
            .as_ref()
            .filter(|s| usable(s))
            .and_then(|_| pick(&inner, &device));
        match (session, picked) {
            (Some(session), Some(p)) => {
                inner.publish();
                run_camera(&inner, &session, p).await;
            }
            (Some(session), None) => {
                // Connection changes show in `waiting_device`. (A new
                // receiver would report the last change at once.)
                let mut state = session.watch_state();
                state.borrow_and_update();
                tokio::select! {
                    _ = &mut notified => {}
                    _ = state.changed() => inner.publish(),
                }
            }
            (None, _) => notified.await,
        }
    }
}

/// Sleeps `d`, returning early (true) when the job is interrupted.
async fn sleep_or_interrupt(
    inner: &Inner,
    slot: &JobRef,
    session: &Arc<DeviceSession>,
    d: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + d;
    loop {
        let notified = inner.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if inner.interrupted(slot, session) {
            return true;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return false,
            _ = notified => {}
        }
    }
}

/// How waiting for the device ended.
enum Connection {
    Up,
    Interrupted,
    /// Login refused or session closed: the device must be added again.
    Ended,
}

/// Waits until the session is connected, without spending attempts, so a
/// device that is offline for a while does not fail its downloads.
async fn wait_connected(inner: &Inner, slot: &JobRef, session: &Arc<DeviceSession>) -> Connection {
    let mut state = session.watch_state();
    state.borrow_and_update();
    let mut waiting = false;
    let outcome = loop {
        let notified = inner.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if inner.interrupted(slot, session) {
            break Connection::Interrupted;
        }
        match session.state() {
            ConnState::Connected => break Connection::Up,
            ConnState::LoginFailed { .. } | ConnState::Closed => break Connection::Ended,
            ConnState::Connecting | ConnState::Backoff { .. } => {}
        }
        if !waiting {
            waiting = true;
            inner.set_live(slot, |l| l.waiting = true);
        }
        tokio::select! {
            _ = state.changed() => {}
            _ = notified => {}
        }
    };
    if waiting {
        inner.set_live(slot, |l| l.waiting = false);
    }
    outcome
}

/// Puts a camera job back in the queue (it is picked again when it can
/// run).
async fn requeue(inner: &Inner, slot: &JobRef) {
    inner
        .update(slot, |c, l| {
            *l = Live::default();
            if c.state == CameraState::Running {
                c.state = CameraState::Queued;
            }
        })
        .await;
}

async fn run_camera(inner: &Arc<Inner>, session: &Arc<DeviceSession>, p: Picked) {
    let slot = &p.slot;
    let stream = StreamTier::from_name(&p.job.stream).unwrap_or(StreamTier::Main);
    let mut job = p.job.clone();
    let max = inner.cfg.max_attempts;
    for i in 0..job.chunks.len() {
        if job.chunks[i].done {
            continue;
        }
        let mut delay = inner.cfg.retry_delay;
        loop {
            match wait_connected(inner, slot, session).await {
                Connection::Up => {}
                Connection::Interrupted | Connection::Ended => {
                    requeue(inner, slot).await;
                    return;
                }
            }
            job.chunks[i].attempts += 1;
            let attempts = job.chunks[i].attempts;
            let ch = job.chunks[i].clone();
            let req = DownloadRequest {
                channel: job.camera.channel,
                uid: job.uid.clone(),
                stream,
                start: date_time(ch.start),
                end: date_time(ch.end),
            };
            let result = fetch_chunk(inner, session, slot, &req, &p.parts_dir, i as u32, &ch).await;
            inner.set_live(slot, |l| l.estimate = None);
            match result {
                Ok(idx) => {
                    let frames = u32::try_from(idx.video.len()).unwrap_or(u32::MAX);
                    let c = &mut job.chunks[i];
                    c.done = true;
                    c.bytes = idx.bytes;
                    c.video_frames = Some(frames);
                    let done = c.clone();
                    inner
                        .update(slot, |c, _| {
                            if let Some(x) = c.chunks.get_mut(i) {
                                *x = done;
                            }
                        })
                        .await;
                    break;
                }
                Err(ChunkError::Interrupted) => {
                    job.chunks[i].attempts -= 1;
                    continue;
                }
                Err(e) => {
                    debug!(chunk = i, attempts, error = %e, "chunk failed");
                    inner
                        .update(slot, |c, _| {
                            if let Some(x) = c.chunks.get_mut(i) {
                                x.attempts = attempts;
                            }
                        })
                        .await;
                    if attempts >= max {
                        warn!(chunk = i, attempts, error = %e, "camera download failed");
                        let failure = Failure::new(
                            e.reason(),
                            format!("chunk {i}: {e} (attempt {attempts} of {max})"),
                        );
                        inner
                            .update(slot, |c, l| {
                                *l = Live::default();
                                c.state = CameraState::Failed(failure);
                            })
                            .await;
                        return;
                    }
                    sleep_or_interrupt(inner, slot, session, delay).await;
                    delay = (delay * 2).min(inner.cfg.max_retry_delay);
                }
            }
        }
    }
    // Every window answered with no picture: nothing to join.
    if job.chunks.iter().all(|c| c.video_frames == Some(0)) {
        inner
            .update(slot, |c, l| {
                *l = Live::default();
                c.state = CameraState::Failed(Failure::new(
                    FailReason::NoRecording,
                    "the device sent no video for the range",
                ));
            })
            .await;
        return;
    }
    let (dir, n) = (p.parts_dir.clone(), job.chunks.len() as u32);
    let first =
        tokio::task::spawn_blocking(move || crate::recorded_time::first_picture_wall_ms(&dir, n))
            .await;
    let first = match first {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => {
            warn!(error = %e, "cannot read the chunk indexes");
            None
        }
        Err(_) => None,
    };
    inner
        .update(slot, |c, _| {
            c.state = CameraState::ChunksComplete;
            c.first_picture_wall_ms = first;
        })
        .await;
    // The final MP4 step, when a finalizer is installed.
    let Some(fin) = inner.finalizer.clone() else {
        inner.update(slot, |_, l| *l = Live::default()).await;
        return;
    };
    let (dir, out, n) = (
        p.parts_dir.clone(),
        p.output.clone(),
        job.chunks.len() as u32,
    );
    let result = tokio::task::spawn_blocking(move || fin.finalize(&dir, n, &out)).await;
    inner
        .update(slot, |c, l| {
            *l = Live::default();
            c.state = match result {
                Ok(Ok(())) => CameraState::Done,
                // The muxer reports content it cannot join as InvalidData;
                // anything else is the disk.
                Ok(Err(e)) if e.kind() == io::ErrorKind::InvalidData => CameraState::Failed(
                    Failure::new(FailReason::JoinFailed, format!("finalize: {e}")),
                ),
                Ok(Err(e)) => CameraState::Failed(Failure::new(
                    FailReason::WriteFailed,
                    format!("finalize: {e}"),
                )),
                Err(e) => {
                    CameraState::Failed(Failure::new(FailReason::Other, format!("finalize: {e}")))
                }
            };
        })
        .await;
}

/// Downloads one chunk into its files. Stops the download (144) when it
/// stalls or the job is interrupted.
async fn fetch_chunk(
    inner: &Inner,
    session: &Arc<DeviceSession>,
    slot: &JobRef,
    req: &DownloadRequest,
    dir: &Path,
    n: u32,
    ch: &ChunkState,
) -> Result<ChunkIndex, ChunkError> {
    let dl = tokio::select! {
        r = session.download(req) => r.map_err(ChunkError::Start)?,
        _ = wait_interrupted(inner, slot, session) => return Err(ChunkError::Interrupted),
    };
    let mut w = BlockingChunk::start(dir, n, ch.start, ch.end);
    match receive(inner, slot, session, dl, &mut w).await {
        Ok(()) => w.finish().await.map_err(ChunkError::Io),
        Err(e) => {
            // Stopped (and its files removed) before returning, so the next
            // attempt never races a writer still holding the old files.
            let writer_error = w.discard().await;
            match (e, writer_error) {
                (ChunkError::Io(_), Some(we)) => Err(ChunkError::Io(we)),
                (e, _) => Err(e),
            }
        }
    }
}

/// Feeds a download's records to its chunk writer until the download ends
/// (`Ok`) or fails.
async fn receive(
    inner: &Inner,
    slot: &JobRef,
    session: &Arc<DeviceSession>,
    mut dl: Download,
    w: &mut BlockingChunk,
) -> Result<(), ChunkError> {
    loop {
        let next = tokio::select! {
            r = dl.next() => r,
            _ = wait_interrupted(inner, slot, session) => {
                dl.cancel(inner.cfg.cancel_timeout).await;
                return Err(ChunkError::Interrupted);
            }
        };
        match next {
            Ok(Some(DownloadEvent::Record(r))) => {
                if !w.write(r).await {
                    // The writer failed (its error replaces this one).
                    dl.cancel(inner.cfg.cancel_timeout).await;
                    return Err(ChunkError::Io(io::Error::other("chunk writer stopped")));
                }
            }
            Ok(Some(DownloadEvent::Estimate(n))) => inner.set_live(slot, |l| l.estimate = Some(n)),
            Ok(None) => return Ok(()),
            Err(DownloadError::Stalled) => {
                dl.cancel(inner.cfg.cancel_timeout).await;
                return Err(ChunkError::Download(DownloadError::Stalled));
            }
            Err(e) => return Err(ChunkError::Download(e)),
        }
    }
}

/// A [`ChunkWriter`] whose file work runs on blocking threads, so its
/// writes and its final `fsync` never block an async worker. Records queue
/// here (at most [`WRITE_QUEUE`]) while the previous batch is written; each
/// blocking task lasts one batch, not the whole chunk, so the runtime knows
/// when disk work is in progress (a runtime on a paused test clock does not
/// move its clock during one).
struct BlockingChunk {
    slot: Slot,
    queue: Vec<Record>,
    dir: PathBuf,
    n: u32,
}

/// Where a [`BlockingChunk`]'s writer is.
enum Slot {
    /// Between batches.
    Idle(Box<ChunkWriter>),
    /// Creating the files or writing a batch; the task returns the writer.
    Busy(JoinHandle<io::Result<Box<ChunkWriter>>>),
    /// The writer failed; nothing more is written.
    Failed(io::Error),
    /// Taken for a moment while changing state.
    Empty,
}

impl BlockingChunk {
    fn start(dir: &Path, n: u32, start: Secs, end: Secs) -> BlockingChunk {
        let path = dir.to_owned();
        let create = tokio::task::spawn_blocking(move || {
            ChunkWriter::create(&path, n, start, end).map(Box::new)
        });
        BlockingChunk {
            slot: Slot::Busy(create),
            queue: Vec::new(),
            dir: dir.to_owned(),
            n,
        }
    }

    /// Waits for the batch in progress, if any.
    async fn settle(&mut self) {
        let Slot::Busy(task) = &mut self.slot else {
            return;
        };
        self.slot = match task.await {
            Ok(Ok(w)) => Slot::Idle(w),
            Ok(Err(e)) => Slot::Failed(e),
            Err(e) => Slot::Failed(io::Error::other(e)),
        };
    }

    /// Starts writing the queued records if the writer is free.
    fn hand_off(&mut self) {
        if self.queue.is_empty() || !matches!(self.slot, Slot::Idle(_)) {
            return;
        }
        let Slot::Idle(mut w) = std::mem::replace(&mut self.slot, Slot::Empty) else {
            unreachable!("checked");
        };
        let batch = std::mem::take(&mut self.queue);
        self.slot = Slot::Busy(tokio::task::spawn_blocking(move || {
            for r in &batch {
                w.write(r)?;
            }
            Ok(w)
        }));
    }

    /// Queues a record, waiting while the queue is full; false once the
    /// writer has stopped (it failed).
    async fn write(&mut self, r: Record) -> bool {
        if matches!(self.slot, Slot::Failed(_)) {
            return false;
        }
        self.queue.push(r);
        let finished = matches!(&self.slot, Slot::Busy(t) if t.is_finished());
        if finished || self.queue.len() >= WRITE_QUEUE {
            self.settle().await;
        }
        self.hand_off();
        !matches!(self.slot, Slot::Failed(_))
    }

    /// Completes the chunk and returns its index.
    async fn finish(mut self) -> io::Result<ChunkIndex> {
        self.settle().await;
        self.hand_off();
        self.settle().await;
        match std::mem::replace(&mut self.slot, Slot::Empty) {
            Slot::Idle(w) => tokio::task::spawn_blocking(move || w.finish())
                .await
                .map_err(io::Error::other)?,
            Slot::Failed(e) => Err(e),
            Slot::Busy(_) | Slot::Empty => Err(io::Error::other("chunk writer stopped")),
        }
    }

    /// Stops the writer and removes the chunk's files; the writer's error,
    /// if it failed.
    async fn discard(mut self) -> Option<io::Error> {
        self.queue.clear();
        // Removed after the writer has stopped, so it cannot recreate them.
        self.settle().await;
        let err = match std::mem::replace(&mut self.slot, Slot::Empty) {
            Slot::Failed(e) => Some(e),
            _ => None,
        };
        let (dir, n) = (self.dir, self.n);
        let _ = tokio::task::spawn_blocking(move || chunk::remove_chunk(&dir, n)).await;
        err
    }
}

/// Resolves once the job is paused, removed, or the scheduler shuts down.
async fn wait_interrupted(inner: &Inner, slot: &JobRef, session: &Arc<DeviceSession>) {
    loop {
        let notified = inner.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if inner.interrupted(slot, session) {
            return;
        }
        notified.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worker's last update clears its running flag under the state lock
    /// and writes the job file after releasing it. A forget in between used
    /// to delete the parts folder while (or before) that write ran, which
    /// failed the delete or recreated the folder.
    #[test]
    fn a_save_taken_before_forget_never_writes_after_it() {
        use crate::incident::{CameraEvent, GroupingConfig, group};
        let root = tempfile::tempdir().unwrap();
        let sched = Scheduler::new(SchedulerConfig::new(root.path()), None);
        let camera = CameraId::new("nvr-a", 0);
        let incident = group(
            &[CameraEvent {
                start: 1000,
                end: 1020,
                camera: camera.clone(),
                kind: "md".into(),
            }],
            &GroupingConfig::default(),
        )
        .remove(0);
        let folder = sched
            .enqueue(&IncidentDownload {
                incident: incident.clone(),
                cameras: vec![CameraSource {
                    camera,
                    name: "Front".into(),
                    uid: None,
                    stream: StreamTier::Main,
                }],
            })
            .unwrap();
        // The worker's final update: snapshot taken, write still pending.
        let late = lock(&sched.inner.state)
            .jobs
            .get_mut(&incident.id)
            .unwrap()
            .snapshot()
            .unwrap();
        assert!(sched.forget(&incident.id, true).unwrap());
        assert!(!folder.join(PARTS_DIR).exists());
        late.write().unwrap();
        assert!(
            !folder.join(PARTS_DIR).exists(),
            "a forgotten job was saved"
        );
        assert!(folder.join("incident.json").exists());
    }

    #[test]
    fn failures_round_trip_and_old_job_files_still_load() {
        let old: CameraState =
            serde_json::from_str(r#"{"Failed":"chunk 2: download stalled"}"#).unwrap();
        assert_eq!(
            old,
            CameraState::Failed(Failure::new(FailReason::Other, "chunk 2: download stalled"))
        );
        let new = CameraState::Failed(Failure::new(FailReason::Stalled, "no data"));
        let json = serde_json::to_string(&new).unwrap();
        assert_eq!(
            json,
            r#"{"Failed":{"reason":"stalled","detail":"no data"}}"#
        );
        assert_eq!(serde_json::from_str::<CameraState>(&json).unwrap(), new);
        // A chunk and a camera job written before video_frames and paused.
        let c: ChunkState =
            serde_json::from_str(r#"{"start":0,"end":10,"done":true,"bytes":5,"attempts":1}"#)
                .unwrap();
        assert_eq!(c.video_frames, None);
        assert_eq!(FailReason::NoRecording.name(), "no_recording");
    }

    #[test]
    fn windows_cover_the_range_and_touch() {
        assert_eq!(
            chunk_windows(0, 150, 60),
            vec![(0, 60), (60, 120), (120, 150)]
        );
        assert_eq!(chunk_windows(0, 60, 60), vec![(0, 60)]);
        assert_eq!(chunk_windows(5, 5, 60), vec![(5, 5)]);
        assert_eq!(chunk_windows(10, 0, 60), vec![(10, 10)]);
        let w = chunk_windows(1000, 1000 + 3600, 60);
        assert_eq!(w.len(), 60);
        assert!(w.windows(2).all(|p| p[0].1 == p[1].0));
    }
}
