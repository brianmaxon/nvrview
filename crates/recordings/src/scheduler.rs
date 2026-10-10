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
//!   failed and can be retried with [`Scheduler::retry`].
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
use bc_session::{DeviceSession, Download, DownloadError, DownloadEvent, SearchError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
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

/// State of one camera job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CameraState {
    /// Waiting for a worker.
    Queued,
    /// Downloading.
    Running,
    /// Every chunk is on disk; the final file needs the [`Finalizer`].
    ChunksComplete,
    /// The final file is written.
    Done,
    /// Gave up; the reason. [`Scheduler::retry`] queues it again.
    Failed(String),
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JobFile {
    version: u32,
    incident: Incident,
    paused: bool,
    cameras: Vec<CameraJob>,
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
    /// State.
    pub state: CameraState,
    /// The final file's path.
    pub output: PathBuf,
}

/// Progress of one incident download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncidentProgress {
    /// Incident ID.
    pub id: String,
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

struct Job {
    seq: u64,
    folder: PathBuf,
    file: JobFile,
    running: Vec<bool>,
    /// Bumped on every change to `file`; see [`JobSave`].
    version: u64,
    /// The version last written to disk.
    saved: Arc<Mutex<u64>>,
}

impl Job {
    fn new(seq: u64, folder: PathBuf, file: JobFile) -> Job {
        Job {
            seq,
            folder,
            running: vec![false; file.cameras.len()],
            file,
            version: 0,
            saved: Arc::new(Mutex::new(0)),
        }
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
    saved: Arc<Mutex<u64>>,
}

impl JobSave {
    /// Writes it; blocking.
    fn write(self) -> io::Result<()> {
        let mut saved = lock(&self.saved);
        if self.version <= *saved {
            return Ok(());
        }
        write_atomic(&self.path, &self.bytes)?;
        *saved = self.version;
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
}

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

fn slug(c: &CameraId) -> String {
    naming::sanitize(&format!("{}-ch{:02}", c.device, c.channel)).replace(' ', "_")
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
            }),
        }
    }

    /// Makes a device's session available and starts its workers. Jobs of
    /// devices that were never added wait.
    pub fn add_device(&self, device: &str, session: Arc<DeviceSession>) {
        let new = lock(&self.inner.devices)
            .insert(device.to_owned(), session)
            .is_none();
        if new {
            let mut w = lock(&self.inner.workers);
            for _ in 0..self.inner.cfg.per_device.max(1) {
                let inner = self.inner.clone();
                let dev = device.to_owned();
                w.push(tokio::spawn(device_worker(inner, dev)));
            }
        }
        self.inner.changed.notify_waiters();
    }

    /// Progress of every known incident download, updated on every change.
    pub fn progress(&self) -> watch::Receiver<Vec<IncidentProgress>> {
        self.inner.progress.subscribe()
    }

    /// Queues an incident: creates its folder, `incident.json`, and job
    /// file. Queuing the same incident again returns the existing folder.
    pub fn enqueue(&self, req: &IncidentDownload) -> io::Result<PathBuf> {
        let inc = &req.incident;
        let ranges: BTreeMap<CameraId, (Secs, Secs)> = inc
            .download_ranges()
            .into_iter()
            .filter(|(c, _)| req.cameras.iter().any(|s| &s.camera == c))
            .collect();
        if ranges.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no camera of the incident has a source",
            ));
        }
        {
            let st = lock(&self.inner.state);
            if let Some(j) = st.jobs.get(&inc.id) {
                return Ok(j.folder.clone());
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
                Ok(Some(j)) if j.incident.id == inc.id => break,
                Ok(None) if !folder.exists() => break,
                _ => {
                    folder = self.inner.cfg.root.join(format!("{base} ({n})"));
                    n += 1;
                }
            }
        }
        fs::create_dir_all(folder.join(PARTS_DIR))?;
        let json = naming::incident_json(inc, &ranges, &names, pattern);
        write_json(&folder.join("incident.json"), &json)?;
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
                        })
                        .collect(),
                    state: CameraState::Queued,
                })
            })
            .collect();
        let file = JobFile {
            version: JOB_VERSION,
            incident: inc.clone(),
            paused: false,
            cameras,
        };
        self.insert(folder.clone(), file)?;
        info!(cameras = ranges.len(), "incident download queued");
        Ok(folder)
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
                    Ok(Some(j)) if j.version == JOB_VERSION => found.push((folder, j)),
                    Ok(_) => {}
                    Err(e) => warn!(error = %e, "unreadable download job skipped"),
                }
            }
        }
        found.sort_by_key(|(_, j)| j.incident.start());
        let mut n = 0;
        for (folder, mut j) in found {
            if lock(&self.inner.state).jobs.contains_key(&j.incident.id) {
                continue;
            }
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

    /// Queues an incident's failed camera jobs again with fresh attempts.
    pub fn retry(&self, incident: &str) -> io::Result<bool> {
        let save = {
            let mut st = lock(&self.inner.state);
            match st.jobs.get_mut(incident) {
                Some(j) => {
                    for c in &mut j.file.cameras {
                        if matches!(c.state, CameraState::Failed(_)) {
                            c.state = CameraState::Queued;
                            for ch in &mut c.chunks {
                                ch.attempts = 0;
                            }
                        }
                    }
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

    /// Forgets a download that has no camera job running (its files stay
    /// on disk). Apps call it for finished incidents the user dismissed, so
    /// the in-memory list stays bounded.
    pub fn forget(&self, incident: &str) -> bool {
        let removed = {
            let mut st = lock(&self.inner.state);
            match st.jobs.get(incident) {
                Some(j) if !j.running.iter().any(|r| *r) => st.jobs.remove(incident).is_some(),
                _ => false,
            }
        };
        if removed {
            self.inner.publish();
        }
        removed
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
        let list = {
            let st = lock(&self.state);
            let mut jobs: Vec<&Job> = st.jobs.values().collect();
            jobs.sort_by_key(|j| j.seq);
            jobs.iter()
                .map(|j| IncidentProgress {
                    id: j.file.incident.id.clone(),
                    folder: j.folder.clone(),
                    paused: j.file.paused,
                    cameras: j
                        .file
                        .cameras
                        .iter()
                        .map(|c| CameraProgress {
                            camera: c.camera.clone(),
                            name: c.name.clone(),
                            chunks_done: c.chunks.iter().filter(|x| x.done).count(),
                            chunks_total: c.chunks.len(),
                            bytes: c.chunks.iter().map(|x| x.bytes).sum(),
                            state: c.state.clone(),
                            output: j.folder.join(&c.file),
                        })
                        .collect(),
                })
                .collect()
        };
        self.progress.send_replace(list);
    }

    fn interrupted(&self, incident: &str) -> bool {
        let st = lock(&self.state);
        st.shutdown || st.jobs.get(incident).is_none_or(|j| j.file.paused)
    }

    /// Updates a camera job and saves the job file (on a blocking thread).
    async fn update(&self, incident: &str, cam: usize, f: impl FnOnce(&mut CameraJob, &mut bool)) {
        let save = {
            let mut st = lock(&self.state);
            st.jobs.get_mut(incident).map(|j| {
                if let (Some(c), Some(r)) = (j.file.cameras.get_mut(cam), j.running.get_mut(cam)) {
                    f(c, r);
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
}

/// A camera job picked by a worker.
struct Picked {
    incident: String,
    cam: usize,
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
            if !pickable || j.running[i] || c.camera.device != device {
                continue;
            }
            j.running[i] = true;
            if c.state == CameraState::Queued {
                c.state = CameraState::Running;
            }
            return Some(Picked {
                incident: j.file.incident.id.clone(),
                cam: i,
                job: c.clone(),
                parts_dir: j.folder.join(PARTS_DIR).join(&c.parts),
                output: j.folder.join(&c.file),
            });
        }
    }
    None
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
        let picked = session.as_ref().and_then(|_| pick(&inner, &device));
        match (session, picked) {
            (Some(session), Some(p)) => {
                inner.publish();
                run_camera(&inner, &session, p).await;
            }
            _ => notified.await,
        }
    }
}

/// Sleeps `d`, returning early (true) when the job is interrupted.
async fn sleep_or_interrupt(inner: &Inner, incident: &str, d: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + d;
    loop {
        let notified = inner.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if inner.interrupted(incident) {
            return true;
        }
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => return false,
            _ = notified => {}
        }
    }
}

async fn run_camera(inner: &Arc<Inner>, session: &DeviceSession, p: Picked) {
    let stream = StreamTier::from_name(&p.job.stream).unwrap_or(StreamTier::Main);
    let mut job = p.job.clone();
    for i in 0..job.chunks.len() {
        if job.chunks[i].done {
            continue;
        }
        let mut delay = inner.cfg.retry_delay;
        loop {
            if inner.interrupted(&p.incident) {
                inner
                    .update(&p.incident, p.cam, |c, r| {
                        *r = false;
                        if c.state == CameraState::Running {
                            c.state = CameraState::Queued;
                        }
                    })
                    .await;
                return;
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
            match fetch_chunk(
                inner,
                session,
                &p.incident,
                &req,
                &p.parts_dir,
                i as u32,
                &ch,
            )
            .await
            {
                Ok(idx) => {
                    job.chunks[i].done = true;
                    job.chunks[i].bytes = idx.bytes;
                    let (bytes, attempts) = (idx.bytes, job.chunks[i].attempts);
                    inner
                        .update(&p.incident, p.cam, |c, _| {
                            if let Some(x) = c.chunks.get_mut(i) {
                                x.done = true;
                                x.bytes = bytes;
                                x.attempts = attempts;
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
                        .update(&p.incident, p.cam, |c, _| {
                            if let Some(x) = c.chunks.get_mut(i) {
                                x.attempts = attempts;
                            }
                        })
                        .await;
                    if attempts >= inner.cfg.max_attempts {
                        warn!(chunk = i, attempts, error = %e, "camera download failed");
                        let reason = format!("chunk {i}: {e}");
                        inner
                            .update(&p.incident, p.cam, |c, r| {
                                *r = false;
                                c.state = CameraState::Failed(reason);
                            })
                            .await;
                        return;
                    }
                    sleep_or_interrupt(inner, &p.incident, delay).await;
                    delay = (delay * 2).min(inner.cfg.max_retry_delay);
                }
            }
        }
    }
    inner
        .update(&p.incident, p.cam, |c, _| {
            c.state = CameraState::ChunksComplete
        })
        .await;
    // The final MP4 step, when a finalizer is installed.
    let Some(fin) = inner.finalizer.clone() else {
        inner.update(&p.incident, p.cam, |_, r| *r = false).await;
        return;
    };
    let (dir, out, n) = (
        p.parts_dir.clone(),
        p.output.clone(),
        job.chunks.len() as u32,
    );
    let result = tokio::task::spawn_blocking(move || fin.finalize(&dir, n, &out)).await;
    inner
        .update(&p.incident, p.cam, |c, r| {
            *r = false;
            c.state = match result {
                Ok(Ok(())) => CameraState::Done,
                Ok(Err(e)) => CameraState::Failed(format!("finalize: {e}")),
                Err(e) => CameraState::Failed(format!("finalize: {e}")),
            };
        })
        .await;
}

/// Downloads one chunk into its files. Stops the download (144) when it
/// stalls or the job is interrupted.
#[allow(clippy::too_many_arguments)]
async fn fetch_chunk(
    inner: &Inner,
    session: &DeviceSession,
    incident: &str,
    req: &DownloadRequest,
    dir: &Path,
    n: u32,
    ch: &ChunkState,
) -> Result<ChunkIndex, ChunkError> {
    let dl = tokio::select! {
        r = session.download(req) => r.map_err(ChunkError::Start)?,
        _ = wait_interrupted(inner, incident) => return Err(ChunkError::Interrupted),
    };
    let mut w = BlockingChunk::start(dir, n, ch.start, ch.end);
    match receive(inner, incident, dl, &mut w).await {
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
    incident: &str,
    mut dl: Download,
    w: &mut BlockingChunk,
) -> Result<(), ChunkError> {
    loop {
        let next = tokio::select! {
            r = dl.next() => r,
            _ = wait_interrupted(inner, incident) => {
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
            Ok(Some(DownloadEvent::Estimate(_))) => {}
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
async fn wait_interrupted(inner: &Inner, incident: &str) {
    loop {
        let notified = inner.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if inner.interrupted(incident) {
            return;
        }
        notified.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
