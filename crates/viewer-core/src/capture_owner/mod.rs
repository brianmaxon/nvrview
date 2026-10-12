//! The capture owner: live recordings and call recordings of the core
//! (docs/CAPTURE.md, "Live recording" and "Recording calls"; the
//! interface in docs/FFI.md, "Capture").
//!
//! Each recording is a recorder (one OS thread, or a task on a test
//! runtime) with its own subscription to the camera's stream at a fixed
//! tier, so the tiles showing the camera are never touched. The pure
//! decisions (timeline, segment policy, mixer, caps, disk guard, partials,
//! names) come from the `capture` crate; this module drives them with the
//! session, the clock, the file system, and the app's AAC encoder.
//!
//! Choices made where the contract is silent are listed in
//! docs/CAPTURE.md, "Implementation status (K4)".

mod disk;
mod recorder;
mod segment;

pub use disk::{FreeSpace, SystemFreeSpace};

use crate::clock::HostClock;
use crate::events::{EventSink, Priority};
use crate::json::Object;
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, Subscription, TalkObserver};
use capture::caps::{CountLimits, DiskConfig, Limits, RecordingKind, Running, StartRequest, admit};
use capture::naming;
use capture::partials::{PartialEntry, RecoveryAction, Registry, plan_recovery};
use capture::{LimitKind, Reason};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tracing::{info, warn};

/// How long shutdown, device removal, and the app's quit wait for
/// recordings to finalize (docs/CAPTURE.md, "Lifetime"); what is left is
/// recovered at the next configure.
pub const FINALIZE_WAIT: Duration = Duration::from_secs(5);

/// Encoded units waiting to be placed, per recording (about 10 s of three
/// tracks); beyond that units are dropped and counted.
const INBOX_UNITS: usize = 3 * capture::mixer::TEN_SECONDS_OF_FRAMES;

/// Sent talk records the observer keeps (10 s of 64 ms records).
const OBSERVER_RECORDS: usize = 160;

/// The app's AAC encoder (docs/FFI.md, "Capture", `NvAudioEncoder`).
///
/// `track` is the index of the audio track in the file (0 for a live
/// recording's track; 0 Conversation, 1 Visitor, 2 You for a call). The
/// core calls `open` once per track that needs encoding when a file
/// starts, `encode` with 1024 mono samples at a time on the recorder's own
/// thread, and `close` at the end of the file. The app answers each
/// encoded frame with [`crate::Core::record_audio_encoded`]: one raw AAC
/// access unit, in order, from any thread, until `close` returns.
pub trait AudioEncoder: Send + Sync + 'static {
    /// Opens an AAC-LC encoder for one track; `None` if it cannot.
    fn open(
        &self,
        recording: u64,
        track: u32,
        sample_rate: u32,
        channels: u32,
        bit_rate: u32,
    ) -> Option<EncoderOpened>;
    /// One frame of 1024 mono samples.
    fn encode(&self, recording: u64, track: u32, pcm: &[f32]);
    /// Ends the track; with `flush`, the encoder's remaining output first.
    fn close(&self, recording: u64, track: u32, flush: bool);
}

/// What an opened encoder reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderOpened {
    /// The AudioSpecificConfig (two bytes for AAC-LC).
    pub audio_specific_config: Vec<u8>,
    /// The encoder's delay in frames (samples), written as an edit list.
    pub priming_frames: u32,
}

/// Why a capture call failed. The C interface maps these to `NvStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureError {
    /// A bad request, or a call from inside a callback.
    InvalidArgument,
    /// No such tile or recording.
    UnknownId,
    /// The core is shutting down.
    Closed,
    /// A count or disk limit; the answer names it.
    Limit(LimitKind),
    /// No `live_dir` configured, or a call recording without an encoder.
    NotConfigured,
    /// Something unexpected failed (a folder could not be created).
    Internal,
}

impl CaptureError {
    /// The answer that goes with [`CaptureError::Limit`]:
    /// `{"limit":"live"|"calls"|"main_streams"|"disk_low"}`.
    pub fn limit_answer(&self) -> Option<String> {
        match self {
            CaptureError::Limit(l) => Some(Object::new().str("limit", l.as_str()).finish()),
            _ => None,
        }
    }
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::InvalidArgument => f.write_str("invalid argument"),
            CaptureError::UnknownId => f.write_str("unknown tile or recording"),
            CaptureError::Closed => f.write_str("the core is shutting down"),
            CaptureError::Limit(l) => write!(f, "limit reached: {}", l.as_str()),
            CaptureError::NotConfigured => f.write_str("capture is not configured"),
            CaptureError::Internal => f.write_str("internal error"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// The settings of `nv_capture_configure`.
#[derive(Clone, Debug)]
pub(crate) struct Settings {
    live_dir: PathBuf,
    limits: Limits,
    disk: DiskConfig,
    counts: CountLimits,
    main: bool,
    share_safe: bool,
}

fn as_u64(v: &serde_json::Value, key: &str) -> Result<Option<u64>, CaptureError> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(x) => x.as_u64().map(Some).ok_or(CaptureError::InvalidArgument),
    }
}

fn as_bool(v: &serde_json::Value, key: &str) -> Result<Option<bool>, CaptureError> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(x) => x.as_bool().map(Some).ok_or(CaptureError::InvalidArgument),
    }
}

fn as_str<'a>(v: &'a serde_json::Value, key: &str) -> Result<Option<&'a str>, CaptureError> {
    match v.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(x) => x.as_str().map(Some).ok_or(CaptureError::InvalidArgument),
    }
}

fn quality(s: Option<&str>) -> Result<Option<bool>, CaptureError> {
    match s {
        None => Ok(None),
        Some("main") => Ok(Some(true)),
        Some("sub") => Ok(Some(false)),
        Some(_) => Err(CaptureError::InvalidArgument),
    }
}

fn parse_object(json: &str) -> Result<serde_json::Value, CaptureError> {
    if json.len() > 64 * 1024 {
        return Err(CaptureError::InvalidArgument);
    }
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|_| CaptureError::InvalidArgument)?;
    if v.is_object() {
        Ok(v)
    } else {
        Err(CaptureError::InvalidArgument)
    }
}

impl Settings {
    fn parse(json: &str) -> Result<Settings, CaptureError> {
        let v = parse_object(json)?;
        let dir = as_str(&v, "live_dir")?.ok_or(CaptureError::InvalidArgument)?;
        let live_dir = PathBuf::from(dir);
        if dir.is_empty() || !live_dir.is_absolute() || dir.chars().any(char::is_control) {
            return Err(CaptureError::InvalidArgument);
        }
        let disk_default = DiskConfig::default();
        let counts_default = CountLimits::default();
        let secs = as_u64(&v, "max_duration_s")?.unwrap_or(3600);
        let max_duration_us = i64::try_from(secs.saturating_mul(1_000_000)).unwrap_or(i64::MAX);
        let max_bytes = as_u64(&v, "max_bytes")?.unwrap_or(4 * capture::caps::GB);
        let count = |k: &str, d: u32| -> Result<u32, CaptureError> {
            Ok(as_u64(&v, k)?.map_or(d, |n| u32::try_from(n).unwrap_or(u32::MAX)))
        };
        Ok(Settings {
            live_dir,
            limits: Limits::live(max_duration_us, max_bytes),
            disk: DiskConfig {
                min_free_bytes: as_u64(&v, "min_free_bytes")?
                    .unwrap_or(disk_default.min_free_bytes),
                stop_free_bytes: as_u64(&v, "stop_free_bytes")?
                    .unwrap_or(disk_default.stop_free_bytes),
                ..disk_default
            },
            counts: CountLimits {
                max_live: count("max_live", counts_default.max_live)?,
                max_calls: count("max_calls", counts_default.max_calls)?,
                ..counts_default
            },
            main: quality(as_str(&v, "quality")?)?.unwrap_or(true),
            share_safe: as_bool(&v, "share_safe")?.unwrap_or(false),
        })
    }
}

/// A parsed `nv_record_start` request.
#[derive(Clone, Debug)]
pub(crate) struct StartParams {
    pub(crate) kind: RecordingKind,
    pub(crate) tile: u64,
    quality: Option<bool>,
    name: Option<String>,
    share_safe: Option<bool>,
    file: Option<String>,
    utc_offset_s: i64,
}

impl StartParams {
    pub(crate) fn parse(json: &str) -> Result<StartParams, CaptureError> {
        let v = parse_object(json)?;
        let kind = match as_str(&v, "kind")? {
            Some("live") => RecordingKind::Live,
            Some("call") => RecordingKind::Call,
            _ => return Err(CaptureError::InvalidArgument),
        };
        let tile = as_u64(&v, "tile")?.ok_or(CaptureError::InvalidArgument)?;
        let utc_offset_s = match v.get("utc_offset_s") {
            None | Some(serde_json::Value::Null) => 0,
            Some(x) => x
                .as_i64()
                .filter(|s| s.abs() <= 18 * 3600)
                .ok_or(CaptureError::InvalidArgument)?,
        };
        let p = StartParams {
            kind,
            tile,
            quality: quality(as_str(&v, "quality")?)?,
            name: as_str(&v, "name")?.map(str::to_owned),
            share_safe: as_bool(&v, "share_safe")?,
            file: as_str(&v, "file")?.map(str::to_owned),
            utc_offset_s,
        };
        if kind == RecordingKind::Call && p.file.is_none() {
            return Err(CaptureError::InvalidArgument);
        }
        Ok(p)
    }
}

/// Where the core found the tile to record.
pub(crate) struct TileSource {
    pub(crate) device: u64,
    pub(crate) channel: u8,
    /// The tier the tile shows (only its lens matters).
    pub(crate) tile_tier: StreamTier,
    pub(crate) session: Option<Arc<DeviceSession>>,
}

/// The recorder's phase, as the hub counts it.
const ACTIVE: u8 = 0;
const FINALIZING: u8 = 1;
const DONE: u8 = 2;

/// Shared between the hub, a recorder, and the encoder's answers.
pub(crate) struct Control {
    stop: Mutex<Option<Reason>>,
    wake: tokio::sync::Notify,
    inbox: Mutex<VecDeque<(u32, Vec<u8>)>>,
    inbox_dropped: AtomicU64,
    /// The latest `recording` event, for `nv_record_status`.
    snapshot: Mutex<String>,
    phase: AtomicU8,
    done: (Mutex<bool>, Condvar),
}

impl Control {
    fn new() -> Control {
        Control {
            stop: Mutex::new(None),
            wake: tokio::sync::Notify::new(),
            inbox: Mutex::new(VecDeque::new()),
            inbox_dropped: AtomicU64::new(0),
            snapshot: Mutex::new(String::new()),
            phase: AtomicU8::new(ACTIVE),
            done: (Mutex::new(false), Condvar::new()),
        }
    }

    /// Asks the recorder to stop for `reason` (the first reason wins).
    pub(crate) fn request_stop(&self, reason: Reason) {
        lock(&self.stop).get_or_insert(reason);
        self.wake.notify_one();
    }

    pub(crate) fn stop_reason(&self) -> Option<Reason> {
        *lock(&self.stop)
    }

    pub(crate) fn take_units(&self) -> Vec<(u32, Vec<u8>)> {
        lock(&self.inbox).drain(..).collect()
    }

    pub(crate) fn inbox_dropped(&self) -> u64 {
        self.inbox_dropped.load(Ordering::Relaxed)
    }

    pub(crate) fn set_snapshot(&self, json: &str) {
        json.clone_into(&mut lock(&self.snapshot));
    }

    pub(crate) fn set_finalizing(&self) {
        self.phase.store(FINALIZING, Ordering::SeqCst);
    }

    pub(crate) fn mark_done(&self) {
        self.phase.store(DONE, Ordering::SeqCst);
        *lock(&self.done.0) = true;
        self.done.1.notify_all();
    }

    fn is_active(&self) -> bool {
        self.phase.load(Ordering::SeqCst) == ACTIVE
    }

    fn is_done(&self) -> bool {
        self.phase.load(Ordering::SeqCst) == DONE
    }

    /// Waits until done or `deadline`; true if done.
    fn wait_done(&self, deadline: Instant) -> bool {
        let mut g = lock(&self.done.0);
        while !*g {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            g = self
                .done
                .1
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }
}

/// What the hub and every recorder share.
pub(crate) struct Shared {
    pub(crate) events: EventSink,
    pub(crate) clock: HostClock,
    registry_path: Option<PathBuf>,
    registry: Mutex<Registry>,
    /// Final and partial paths in use by running recordings.
    reserved: Mutex<HashSet<PathBuf>>,
}

impl Shared {
    /// Lists a partial file before it is created; false if the registry
    /// refused it (the recording goes on without crash recovery).
    pub(crate) fn register(&self, id: u64, kind: RecordingKind, target: &Path) -> bool {
        let Some(e) = PartialEntry::for_target(id, kind, &target.to_string_lossy()) else {
            return false;
        };
        let mut r = lock(&self.registry);
        if r.add(e).is_err() {
            return false;
        }
        self.save(&r);
        true
    }

    /// Drops a recording's entry once its final file is in place.
    pub(crate) fn unregister(&self, id: u64) {
        let mut r = lock(&self.registry);
        if r.remove(id) {
            self.save(&r);
        }
    }

    fn save(&self, r: &Registry) {
        let Some(p) = &self.registry_path else {
            return;
        };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = r.save(p) {
            warn!(error = %e, "the capture registry could not be saved");
        }
    }

    pub(crate) fn reserve(&self, path: PathBuf) {
        lock(&self.reserved).insert(path);
    }

    pub(crate) fn release(&self, paths: &[PathBuf]) {
        let mut r = lock(&self.reserved);
        for p in paths {
            r.remove(p);
        }
    }

    pub(crate) fn taken(&self, path: &Path) -> bool {
        path.exists() || lock(&self.reserved).contains(path)
    }
}

enum Runner {
    Thread(std::thread::JoinHandle<()>),
    Task(tokio::task::JoinHandle<()>, Handle),
}

struct Entry {
    kind: RecordingKind,
    device: u64,
    channel: u8,
    tile: u64,
    main: bool,
    control: Arc<Control>,
    runner: Option<Runner>,
}

struct State {
    settings: Option<Settings>,
    recovered: bool,
    encoder: Option<Arc<dyn AudioEncoder>>,
    free: Arc<dyn FreeSpace>,
    recs: HashMap<u64, Entry>,
}

/// The capture owner of a core.
pub(crate) struct CaptureHub {
    shared: Arc<Shared>,
    runtime: Handle,
    threads: bool,
    state: Mutex<State>,
    /// Recordings by ID, for the encoder's answers (held only for a lookup).
    controls: Mutex<HashMap<u64, Arc<Control>>>,
    closing: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl CaptureHub {
    pub(crate) fn new(
        runtime: Handle,
        events: EventSink,
        clock: HostClock,
        data_dir: Option<PathBuf>,
        threads: bool,
    ) -> CaptureHub {
        let registry_path = data_dir.map(|d| d.join("capture").join("partials.json"));
        let loaded = registry_path
            .as_deref()
            .map(Registry::load)
            .map(|l| {
                if l.corrupt {
                    warn!("the capture registry was unreadable in part; ignored what could not be used");
                }
                l.registry
            })
            .unwrap_or_default();
        CaptureHub {
            shared: Arc::new(Shared {
                events,
                clock,
                registry_path,
                registry: Mutex::new(loaded),
                reserved: Mutex::new(HashSet::new()),
            }),
            runtime,
            threads,
            state: Mutex::new(State {
                settings: None,
                recovered: false,
                encoder: None,
                free: Arc::new(SystemFreeSpace),
                recs: HashMap::new(),
            }),
            controls: Mutex::new(HashMap::new()),
            closing: AtomicBool::new(false),
        }
    }

    fn check_open(&self) -> Result<(), CaptureError> {
        if self.closing.load(Ordering::SeqCst) {
            Err(CaptureError::Closed)
        } else {
            Ok(())
        }
    }

    /// `nv_capture_configure`: settings for later recordings; the first
    /// call also recovers what a crash left.
    pub(crate) fn configure(&self, json: &str) -> Result<(), CaptureError> {
        self.check_open()?;
        let s = Settings::parse(json)?;
        let first = {
            let mut st = lock(&self.state);
            st.settings = Some(s);
            !std::mem::replace(&mut st.recovered, true)
        };
        if first {
            self.recover();
        }
        Ok(())
    }

    /// Executes the recovery plan for every listed partial file.
    fn recover(&self) {
        let actions = {
            let r = lock(&self.shared.registry);
            plan_recovery(&r, |p| std::fs::metadata(p).ok().map(|m| m.len()))
        };
        for a in actions {
            match a {
                RecoveryAction::Forget { id } => {
                    info!(recording = id, "a listed partial recording is gone");
                    self.shared.unregister(id);
                }
                RecoveryAction::Delete { id, partial, why } => {
                    info!(recording = id, ?why, "deleting a partial recording");
                    if let Err(e) = std::fs::remove_file(&partial) {
                        warn!(recording = id, error = %e, "a partial recording could not be deleted");
                    }
                    self.shared.unregister(id);
                }
                RecoveryAction::Recover {
                    id,
                    kind,
                    partial,
                    target,
                } => {
                    match recordings::finalize::recover_fragmented(
                        Path::new(&partial),
                        Path::new(&target),
                        true,
                    ) {
                        Ok(_) => {
                            info!(recording = id, "recovered a recording");
                            self.shared.events.send(
                                Object::event("recording")
                                    .u64("recording", id)
                                    .str("kind", kind_name(kind))
                                    .str("state", "recovered")
                                    .str("file", &target)
                                    .finish(),
                                Priority::State,
                            );
                        }
                        // The partial file stays as it is (it plays up to
                        // its last complete fragment); it is no longer
                        // listed, so it is never touched again.
                        Err(e) => {
                            warn!(recording = id, error = %e, "a partial recording could not be recovered")
                        }
                    }
                    self.shared.unregister(id);
                }
            }
        }
    }

    pub(crate) fn set_encoder(
        &self,
        encoder: Option<Arc<dyn AudioEncoder>>,
    ) -> Result<(), CaptureError> {
        self.check_open()?;
        lock(&self.state).encoder = encoder;
        Ok(())
    }

    pub(crate) fn set_free_space(&self, free: Arc<dyn FreeSpace>) -> Result<(), CaptureError> {
        self.check_open()?;
        lock(&self.state).free = free;
        Ok(())
    }

    /// An encoded access unit from the app. Allowed from any thread,
    /// callbacks included, and while the recording finalizes.
    pub(crate) fn audio_encoded(
        &self,
        recording: u64,
        track: u32,
        data: &[u8],
    ) -> Result<(), CaptureError> {
        if data.is_empty() || data.len() > 8192 {
            return Err(CaptureError::InvalidArgument);
        }
        let c = lock(&self.controls)
            .get(&recording)
            .cloned()
            .ok_or(CaptureError::UnknownId)?;
        {
            let mut q = lock(&c.inbox);
            if q.len() >= INBOX_UNITS {
                c.inbox_dropped.fetch_add(1, Ordering::Relaxed);
                return Ok(());
            }
            q.push_back((track, data.to_vec()));
        }
        c.wake.notify_one();
        Ok(())
    }

    /// Removes recordings that have ended (their threads are done).
    fn reap(&self, st: &mut State) {
        let done: Vec<u64> = st
            .recs
            .iter()
            .filter(|(_, e)| e.control.is_done())
            .map(|(id, _)| *id)
            .collect();
        for id in done {
            if let Some(e) = st.recs.remove(&id) {
                lock(&self.controls).remove(&id);
                if let Some(Runner::Thread(t)) = e.runner {
                    let _ = t.join();
                }
            }
        }
    }

    /// `nv_record_start`. `id` is the new recording's ID.
    pub(crate) fn start(
        &self,
        id: u64,
        p: &StartParams,
        src: TileSource,
    ) -> Result<String, CaptureError> {
        self.check_open()?;
        let mut st = lock(&self.state);
        self.reap(&mut st);
        let settings = st.settings.clone().ok_or(CaptureError::NotConfigured)?;
        let encoder = st.encoder.clone();
        if p.kind == RecordingKind::Call && encoder.is_none() {
            return Err(CaptureError::NotConfigured);
        }
        let main = match p.kind {
            RecordingKind::Live => p.quality.unwrap_or(settings.main),
            RecordingKind::Call => settings.main,
        };
        let second_lens = matches!(src.tile_tier, StreamTier::Extern | StreamTier::Mobile);
        let tier = match (second_lens, main) {
            (false, true) => StreamTier::Main,
            (false, false) => StreamTier::Sub,
            (true, true) => StreamTier::Extern,
            (true, false) => StreamTier::Mobile,
        };
        // Where the file goes.
        let (dir, call_target) = match p.kind {
            RecordingKind::Live => (settings.live_dir.clone(), None),
            RecordingKind::Call => {
                let f = p.file.as_deref().unwrap_or_default();
                let valid = PartialEntry::for_target(0, RecordingKind::Call, f).is_some();
                let path = PathBuf::from(f);
                let dir = path.parent().map(Path::to_path_buf);
                match (valid, dir) {
                    (true, Some(d)) if path.is_absolute() => (d, Some(path)),
                    _ => return Err(CaptureError::InvalidArgument),
                }
            }
        };
        let free = st.free.free_bytes(&dir).unwrap_or(u64::MAX);
        let active = st.recs.values().filter(|e| e.control.is_active());
        let mut running = Running::default();
        let mut same_call = false;
        for e in active {
            match e.kind {
                RecordingKind::Live => running.live += 1,
                RecordingKind::Call => running.calls += 1,
            }
            if e.device == src.device && e.main {
                running.main_on_device += 1;
            }
            same_call |= e.kind == RecordingKind::Call
                && p.kind == RecordingKind::Call
                && e.device == src.device
                && e.channel == src.channel;
        }
        admit(
            &settings.counts,
            &settings.disk,
            &running,
            &StartRequest {
                kind: p.kind,
                main_stream: main,
                free_bytes: free,
            },
        )
        .map_err(CaptureError::Limit)?;
        // One call recording per camera: the talk observer is per channel.
        if same_call {
            return Err(CaptureError::Limit(LimitKind::Calls));
        }
        if std::fs::create_dir_all(&dir).is_err() {
            return Err(CaptureError::Internal);
        }
        let stem = match call_target {
            None => {
                let start_s = self.shared.clock.wall_now_us() / 1_000_000 + p.utc_offset_s;
                let share_safe = p.share_safe.unwrap_or(settings.share_safe);
                let camera = p
                    .name
                    .clone()
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or_else(|| format!("Camera {}", u32::from(src.channel) + 1));
                let base = naming::live_base_stem(start_s, &camera, share_safe);
                naming::unique_stem(&base, |n| self.shared.taken(&dir.join(n)))
                    .map_err(|_| CaptureError::Internal)?
            }
            Some(t) => {
                let file = t.file_name().map(|n| n.to_string_lossy().into_owned());
                let stem = file
                    .as_deref()
                    .and_then(naming::stem_of_final)
                    .ok_or(CaptureError::InvalidArgument)?
                    .to_owned();
                // Starting again in the same call writes `(part 2)` and so on.
                let free_stem = |s: &str| {
                    !self.shared.taken(&dir.join(naming::final_file_name(s)))
                        && !self.shared.taken(&dir.join(naming::partial_file_name(s)))
                };
                (1..=naming::MAX_NAME_TRIES)
                    .map(|n| naming::part_stem(&stem, n))
                    .find(|s| free_stem(s))
                    .ok_or(CaptureError::Internal)?
            }
        };
        let target = dir.join(naming::final_file_name(&stem));
        let partial = dir.join(naming::partial_file_name(&stem));
        self.shared.reserve(target.clone());
        self.shared.reserve(partial.clone());
        let control = Arc::new(Control::new());
        st.recs.insert(
            id,
            Entry {
                kind: p.kind,
                device: src.device,
                channel: src.channel,
                tile: p.tile,
                main,
                control: control.clone(),
                runner: None,
            },
        );
        lock(&self.controls).insert(id, control.clone());
        let free_space = st.free.clone();
        drop(st);

        let sub: Option<Subscription> = src
            .session
            .as_ref()
            .and_then(|s| self.runtime.block_on(s.subscribe(src.channel, tier)).ok());
        let observer = match (p.kind, &src.session) {
            (RecordingKind::Call, Some(s)) => {
                let o = TalkObserver::new(OBSERVER_RECORDS);
                s.attach_talk_observer(src.channel, &o);
                Some(o)
            }
            _ => None,
        };
        let rec = recorder::Recorder::new(recorder::Init {
            id,
            kind: p.kind,
            device: src.device,
            channel: src.channel,
            lens: u8::from(second_lens),
            main,
            dir,
            stem,
            limits: match p.kind {
                RecordingKind::Live => settings.limits,
                RecordingKind::Call => Limits::call(),
            },
            disk: settings.disk,
            encoder,
            free: free_space,
            sub,
            session: src.session,
            observer,
            control: control.clone(),
            shared: self.shared.clone(),
        });
        let answer = Object::new()
            .u64("recording", id)
            .str("file", &target.to_string_lossy())
            .str("state", "waiting_keyframe")
            .finish();
        let runner = if self.threads {
            match std::thread::Builder::new()
                .name(format!("nvrview-rec-{id}"))
                .spawn({
                    let runtime = self.runtime.clone();
                    move || rec.run_thread(&runtime)
                }) {
                Ok(t) => Runner::Thread(t),
                Err(_) => {
                    // The recorder (and its subscription) was dropped with
                    // the closure; nothing was written.
                    lock(&self.state).recs.remove(&id);
                    lock(&self.controls).remove(&id);
                    self.shared.release(&[target.clone(), partial.clone()]);
                    return Err(CaptureError::Internal);
                }
            }
        } else {
            Runner::Task(self.runtime.spawn(rec.run_task()), self.runtime.clone())
        };
        if let Some(e) = lock(&self.state).recs.get_mut(&id) {
            e.runner = Some(runner);
        }
        info!(
            recording = id,
            kind = kind_name(p.kind),
            device = src.device,
            channel = src.channel,
            main,
            "recording started"
        );
        Ok(answer)
    }

    /// `nv_record_stop`: stops and finalizes (returns at once; the events
    /// report `finalizing`, then `finished`).
    pub(crate) fn stop(&self, recording: u64) -> Result<(), CaptureError> {
        self.check_open()?;
        let st = lock(&self.state);
        let e = st.recs.get(&recording).ok_or(CaptureError::UnknownId)?;
        e.control.request_stop(Reason::Stopped);
        Ok(())
    }

    /// `nv_record_status`: `{"recordings":[...]}`, each the latest event.
    pub(crate) fn status(&self) -> String {
        let mut st = lock(&self.state);
        self.reap(&mut st);
        let mut ids: Vec<&u64> = st.recs.keys().collect();
        ids.sort_unstable();
        let items: Vec<String> = ids
            .into_iter()
            .filter_map(|id| {
                let s = lock(&st.recs[id].control.snapshot).clone();
                (!s.is_empty()).then_some(s)
            })
            .collect();
        format!("{{\"recordings\":[{}]}}", items.join(","))
    }

    /// The call on `tile` ended (the conversation closed): its call
    /// recordings stop.
    pub(crate) fn call_ended(&self, tile: u64) {
        self.stop_where(
            |e| e.kind == RecordingKind::Call && e.tile == tile,
            Reason::Stopped,
        );
    }

    /// A tile closed: call recordings bound to it stop; live recordings go
    /// on (docs/CAPTURE.md, "Lifetime").
    pub(crate) fn tile_closed(&self, tile: u64) {
        self.call_ended(tile);
    }

    fn stop_where(&self, pred: impl Fn(&Entry) -> bool, reason: Reason) -> Vec<u64> {
        let st = lock(&self.state);
        st.recs
            .iter()
            .filter(|(_, e)| pred(e))
            .map(|(id, e)| {
                e.control.request_stop(reason);
                *id
            })
            .collect()
    }

    /// The device is being removed: its recordings stop (`device_removed`)
    /// and let go of the session before it closes; waits up to
    /// [`FINALIZE_WAIT`] for them to finalize.
    pub(crate) fn device_removed(&self, device: u64) {
        let ids = self.stop_where(|e| e.device == device, Reason::DeviceRemoved);
        self.wait(&ids);
    }

    /// Stops every recording (`core_shutdown`) and waits up to
    /// [`FINALIZE_WAIT`]; anything left is recovered at the next launch.
    pub(crate) fn shutdown(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let ids = self.stop_where(|_| true, Reason::CoreShutdown);
        self.wait(&ids);
    }

    fn wait(&self, ids: &[u64]) {
        let deadline = Instant::now() + FINALIZE_WAIT;
        let waits: Vec<(u64, Arc<Control>, Option<Runner>)> = {
            let mut st = lock(&self.state);
            ids.iter()
                .filter_map(|id| {
                    st.recs
                        .get_mut(id)
                        .map(|e| (*id, e.control.clone(), e.runner.take()))
                })
                .collect()
        };
        for (id, control, runner) in waits {
            match runner {
                Some(Runner::Thread(t)) => {
                    if control.wait_done(deadline) {
                        let _ = t.join();
                    } else {
                        // It finishes on its own thread (it needs nothing of
                        // the core); if the process ends first, the next
                        // configure recovers the file.
                        warn!(
                            recording = id,
                            "a recording is still finalizing; not waiting longer"
                        );
                    }
                }
                Some(Runner::Task(t, rt)) if Handle::try_current().is_err() => {
                    let _ = rt.block_on(t);
                }
                Some(Runner::Task(..)) | None => {}
            }
        }
        let mut st = lock(&self.state);
        self.reap(&mut st);
    }
}

pub(crate) fn kind_name(k: RecordingKind) -> &'static str {
    match k {
        RecordingKind::Live => "live",
        RecordingKind::Call => "call",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_parse_with_defaults_and_clamps() {
        let s = Settings::parse(r#"{"live_dir":"/tmp/x"}"#);
        // Absolute on Unix; Windows wants a drive letter.
        if cfg!(unix) {
            let s = s.unwrap();
            assert_eq!(s.limits, Limits::live_default());
            assert_eq!(s.disk, DiskConfig::default());
            assert_eq!(s.counts, CountLimits::default());
            assert!(s.main);
            assert!(!s.share_safe);
            let s = Settings::parse(
                r#"{"live_dir":"/tmp/x","max_duration_s":1,"max_bytes":5,"quality":"sub","share_safe":true,"max_live":1}"#,
            )
            .unwrap();
            assert_eq!(s.limits, Limits::live(60_000_000, 100 * capture::caps::MB));
            assert!(!s.main);
            assert!(s.share_safe);
            assert_eq!(s.counts.max_live, 1);
        }
        for bad in [
            "{}",
            "[]",
            r#"{"live_dir":"relative"}"#,
            r#"{"live_dir":""}"#,
            r#"{"live_dir":"/tmp/x","quality":"best"}"#,
            r#"{"live_dir":"/tmp/x","max_bytes":-1}"#,
            "not json",
        ] {
            assert_eq!(
                Settings::parse(bad).err(),
                Some(CaptureError::InvalidArgument),
                "{bad}"
            );
        }
    }

    #[test]
    fn start_requests_parse() {
        let p = StartParams::parse(r#"{"kind":"live","tile":7,"quality":"sub","name":"Porch"}"#)
            .unwrap();
        assert_eq!(p.kind, RecordingKind::Live);
        assert_eq!(p.tile, 7);
        assert_eq!(p.quality, Some(false));
        let p = StartParams::parse(r#"{"kind":"call","tile":7,"file":"/x/a.mp4"}"#).unwrap();
        assert_eq!(p.kind, RecordingKind::Call);
        for bad in [
            r#"{"kind":"call","tile":7}"#,
            r#"{"kind":"photo","tile":7}"#,
            r#"{"kind":"live"}"#,
            r#"{"kind":"live","tile":7,"utc_offset_s":999999}"#,
        ] {
            assert!(StartParams::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn limit_answers_name_the_limit() {
        assert_eq!(
            CaptureError::Limit(LimitKind::DiskLow)
                .limit_answer()
                .as_deref(),
            Some(r#"{"limit":"disk_low"}"#)
        );
        assert_eq!(CaptureError::Internal.limit_answer(), None);
    }
}
