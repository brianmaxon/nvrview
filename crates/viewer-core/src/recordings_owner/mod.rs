//! The recordings owner: one per core (docs/FFI-RECORDINGS.md, sections 1
//! to 5 and 8; docs/MILESTONE-P.md, slice C11).
//!
//! It holds everything `crates/recordings` provides, for every device, and
//! is what the C interface's recordings functions wrap (slice F1):
//!
//! - per device key: the [`DeviceIndex`] (opened when the app enables the
//!   device, so queries answer from it before the device is even added) and,
//!   while the device is added, its search lane ([`DeviceLane`]) on the
//!   device's existing session;
//! - one [`ThumbnailCache`], one [`EditLog`], one [`GroupStore`], and the
//!   incidents derived from every index (regrouped when the index, the
//!   groups, the settings, or the edits change), and the locked incidents'
//!   store, matched to every regrouping (`locks`);
//! - with a `downloads_dir`, one [`Scheduler`] with the MP4 finalizer;
//! - the app's today holds, by hold number.
//!
//! **Devices.** The core tells the owner when a device is added
//! ([`RecordingsHub::device_added`]) and removed
//! ([`RecordingsHub::device_removed`]). Searches, thumbnails, and downloads
//! ride that device's one session; nothing here logs in. A refused login (or
//! a session that closed) ends the lane: every waiting request fails at once
//! and nothing is retried, downloads wait as `waiting_device`, and a
//! background fill stops asking, until the app adds the device again with
//! new credentials (a new session, so a new lane).
//!
//! **Threads.** Every call answers from memory under short locks; device
//! work runs on the core's runtime and reports by event. Lane events arrive
//! on the lane's task: thumbnails and failed days are sent at once; index
//! changes and lane states are coalesced by one pump task (at most one
//! `recordings_changed` a second per device while filling, `recordings_index`
//! on every state change and at most once a second while busy). Download
//! progress is coalesced by a second task per scheduler (at most four events
//! a second per download, plus every state change).
//!
//! **Locks.** `state` is taken first; `reports` and `trashing` may be taken
//! while holding it, never the other way round. Lane sinks take only
//! `reports` (a lane may call its sink from a request made under `state`).
//! An index is locked briefly under `state` (the lane locks it alone).
//!
//! **Playback.** The playback engine (`crate::playback`, slice C10) asks
//! per device key for the index, the device's offset from UTC, and its
//! session ([`RecordingsHub::playback_device`]); live view keeps running
//! beside it.

mod config;
mod day_audio;
mod downloads;
mod edit;
mod locks;
mod query;
mod sound;
mod sound_work;

pub use sound::{MAX_LABELS_PER_DAY, PATTERN_ANALYZER, SOUND_KINDS};
pub use sound_work::{AudioChunk, DayAudioSource, SourceError};

use crate::clock::HostClock;
use crate::events::{EventSink, Priority};
use bc_session::{ConnState, DeviceSession};
use config::{Channels, Enable, Settings};
use recordings::edits::{Edit, EditError, EditLog, Edited};
use recordings::groups::{CameraGroups, GroupStore, GroupsError};
use recordings::incident::{CameraEvent, CameraId, Incident, group};
use recordings::index::{DeviceIndex, EventQuery};
use recordings::indexer::IndexedChannel;
use recordings::lane::{DeviceLane, LaneConfig, LaneEvent, LaneHandle, LaneState, TodayHold};
use recordings::scheduler::Scheduler;
use recordings::thumbs::{ThumbKey, ThumbnailCache};
use recordings::{DeviceCamera, Secs, day_start};
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{info, warn};

pub use recordings::thumbs::{ThumbKind, Thumbnail};

/// Why a recordings call failed. The values map onto `NvStatus` in the C
/// interface (minor 5): the first five as for [`crate::CoreError`], then
/// `NV_ERR_NOT_CONFIGURED` (-9) and `NV_ERR_UNSUPPORTED` (-8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecError {
    /// An argument is invalid, or the call was made from inside a callback.
    InvalidArgument,
    /// No such device, download, camera, or incident key.
    UnknownId,
    /// The core is shutting down.
    Closed,
    /// Beyond a limit (docs/FFI-RECORDINGS.md, "Limits").
    Limit,
    /// Something unexpected failed (a file could not be written).
    Internal,
    /// `recordings_configure` was not called, or there is no
    /// `downloads_dir` (for downloads).
    NotConfigured,
    /// The device's backend cannot do this.
    Unsupported,
}

impl fmt::Display for RecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RecError::InvalidArgument => "invalid argument",
            RecError::UnknownId => "unknown device, download, camera, or incident",
            RecError::Closed => "the core is shutting down",
            RecError::Limit => "beyond a limit",
            RecError::Internal => "internal error",
            RecError::NotConfigured => "recordings are not configured",
            RecError::Unsupported => "not supported by the device",
        })
    }
}

impl std::error::Error for RecError {}

/// What [`crate::Core::thumbnail_get`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThumbnailAnswer {
    /// Cached.
    Ready(Thumbnail),
    /// Not cached: a fetch is queued (or already was), and a `thumbnail`
    /// event follows (`NV_PENDING`).
    Pending,
}

/// Camera-days that may wait for a device that is not added yet (the
/// lane's own limit once it runs).
const MAX_PARKED_DAYS: usize = 4096;
/// Least time between two coalesced index events of one device.
const INDEX_EVENT_INTERVAL: Duration = Duration::from_secs(1);
/// Largest answer of a query, in bytes of JSON.
const MAX_ANSWER_BYTES: usize = 16 * 1024 * 1024;
/// Most distinct days in one want.
const MAX_WANT_DAYS: usize = recordings::lane::MAX_REQUEST_DAYS;
/// Most distinct months in one want.
const MAX_WANT_MONTHS: usize = recordings::lane::MAX_REQUEST_MONTHS;
/// Most incidents in one merge.
const MAX_MERGE: usize = 1000;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The JSON camera object.
fn camera_json(device: &str, c: DeviceCamera) -> Value {
    json!({"device": device, "channel": c.channel, "lens": c.lens})
}

/// The C interface's kind name for a device kind (`md` is `motion`).
fn kind_out(kind: &str) -> &str {
    if kind == "md" { "motion" } else { kind }
}

/// The device's kind name for one the app filters by.
fn kind_in(kind: &str) -> &str {
    if kind == "motion" { "md" } else { kind }
}

/// A directory name for a device key (keys are opaque to the core too).
fn dir_name(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(96)
        .collect()
}

/// What a device reported about one of its channels: the camera's name and
/// how many lenses it has (1 for a single-lens camera).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReportedChannel {
    pub(crate) name: String,
    pub(crate) lenses: u8,
}

/// Every device's reported channels, by device key.
pub(crate) type ReportedNames = HashMap<String, HashMap<u8, ReportedChannel>>;

/// Tells the owner a device's reported channels (the device's watch task
/// calls it whenever they change).
pub(crate) type ChannelNamesHook = Arc<dyn Fn(HashMap<u8, ReportedChannel>) + Send + Sync>;

/// A camera's name: the user's override; else the name the device reports
/// for the channel, with the lens ("Front · Lens 2") for a multi-lens
/// camera; else "Camera N" (before the device has reported names).
fn camera_name(groups: &GroupStore, reported: &ReportedNames, camera: &CameraId) -> String {
    if let Some(n) = groups.name_of(camera) {
        return n.to_owned();
    }
    if let Some(r) = reported
        .get(&camera.device)
        .and_then(|d| d.get(&camera.channel))
    {
        return if r.lenses >= 2 || camera.lens > 0 {
            format!("{} · Lens {}", r.name, u32::from(camera.lens) + 1)
        } else {
            r.name.clone()
        };
    }
    if camera.lens == 0 {
        format!("Camera {}", u32::from(camera.channel) + 1)
    } else {
        format!(
            "Camera {} lens {}",
            u32::from(camera.channel) + 1,
            u32::from(camera.lens) + 1
        )
    }
}

/// A scratch directory for the index and thumbnails of a core without a
/// data directory (they live for the run), removed when dropped.
#[derive(Debug)]
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Scratch> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nvrview-recordings-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What exists once `recordings_configure` was called.
struct Stores {
    settings: Settings,
    /// `data_dir/recordings`, or the scratch directory.
    root: PathBuf,
    _scratch: Option<Scratch>,
    thumbs: Arc<Mutex<ThumbnailCache>>,
    edits: EditLog,
    groups: GroupStore,
    locks: recordings::locks::LockStore,
    downloads: Option<downloads::Downloads>,
}

/// A device the app added to the core.
struct Added {
    id: u64,
    session: Arc<DeviceSession>,
}

/// Wanted days (and months asked for alone) waiting for a device that is
/// not added.
struct Parked {
    cameras: Vec<IndexedChannel>,
    days: Vec<Secs>,
    months: Vec<Secs>,
    force: bool,
}

/// A device the app enabled for recordings.
struct Dev {
    enable: Enable,
    index: Arc<Mutex<DeviceIndex>>,
    lane: Option<DeviceLane>,
    /// Today holds on the lane, by the app's hold number.
    lane_holds: HashMap<u64, TodayHold>,
    parked: Vec<Parked>,
    background: Option<JoinHandle<()>>,
}

impl Dev {
    fn parked_days(&self) -> usize {
        self.parked
            .iter()
            .map(|p| p.cameras.len() * (p.days.len() + p.months.len()))
            .sum()
    }

    fn pending_days(&self) -> usize {
        self.lane.as_ref().map_or(0, |l| l.pending().1) + self.parked_days()
    }

    /// Whether the app allowed this camera (for `all`: a first lens of a
    /// connected channel, or of any channel while the list is unknown).
    fn allows(&self, cam: DeviceCamera, session: Option<&Arc<DeviceSession>>) -> bool {
        match &self.enable.channels {
            Channels::List(v) => v.contains(&cam),
            Channels::All => {
                cam.lens == 0
                    && session
                        .and_then(|s| s.watch_channels().borrow().clone())
                        .is_none_or(|list| {
                            list.iter().any(|c| c.channel == cam.channel && c.connected)
                        })
            }
        }
    }

    /// The cameras a background fill searches.
    fn background_cameras(&self, session: &DeviceSession) -> Vec<IndexedChannel> {
        match &self.enable.channels {
            Channels::List(v) => v
                .iter()
                .map(|c| IndexedChannel {
                    camera: *c,
                    uid: None,
                })
                .collect(),
            Channels::All => session
                .watch_channels()
                .borrow()
                .iter()
                .flatten()
                .filter(|c| c.connected)
                .map(|c| IndexedChannel::first(c.channel))
                .collect(),
        }
    }

    fn stop_tasks(&mut self) -> Option<DeviceLane> {
        if let Some(b) = self.background.take() {
            b.abort();
        }
        self.lane_holds.clear();
        self.lane.take()
    }
}

/// Incidents derived from every index, cached until something changes.
#[derive(Default)]
struct Incidents {
    dirty: bool,
    /// Grouping without the edits (what edits are pushed onto).
    base: Vec<Incident>,
    edited: Edited,
    revision: u64,
    /// Events found by the computer's sound analysis (camera, kind, start,
    /// end), to tell them from device events in incident JSON.
    local: HashSet<(CameraId, String, Secs, Secs)>,
    locks: locks::Matched,
    /// Each device's index revision when last grouped (sorted by key), so a
    /// query regroups when an index changed since, without waiting for the
    /// rate-limited `recordings_changed` that marks them dirty.
    grouped: Vec<(String, u64)>,
}

#[derive(Default)]
struct State {
    stores: Option<Stores>,
    devices: HashMap<String, Dev>,
    sessions: HashMap<String, Added>,
    /// The app's today holds: hold number to cameras.
    holds: HashMap<u64, Vec<(String, DeviceCamera)>>,
    incidents: Incidents,
    pump: Option<JoinHandle<()>>,
    next_lane: u64,
}

/// What the pump still has to report for one device.
struct Report {
    /// The lane whose states count (an older lane's are ignored).
    lane: u64,
    state: &'static str,
    state_dirty: bool,
    progress_dirty: bool,
    last_index: Option<Instant>,
    changed: Option<(Secs, Secs)>,
    last_changed: Option<Instant>,
}

impl Report {
    fn new(lane: u64, state: &'static str) -> Report {
        Report {
            lane,
            state,
            state_dirty: false,
            progress_dirty: false,
            last_index: None,
            changed: None,
            last_changed: None,
        }
    }
}

fn state_name(s: LaneState) -> &'static str {
    match s {
        LaneState::Idle => "idle",
        LaneState::Busy => "busy",
        LaneState::WaitingDevice => "waiting_device",
        LaneState::Ended => "ended",
    }
}

pub(crate) struct Hub {
    rt: Handle,
    events: EventSink,
    clock: HostClock,
    data_dir: Option<PathBuf>,
    state: Mutex<State>,
    reports: Mutex<HashMap<String, Report>>,
    /// Downloads being trashed: ID to folder (hidden from lists and events
    /// until their removal is reported).
    trashing: Mutex<HashMap<String, PathBuf>>,
    wake: Notify,
    /// Sound for recordings (`sound.rs`).
    sound: sound::SoundOwner,
    /// The channel names devices reported (a leaf lock: nothing else is
    /// taken while it is held).
    reported: Mutex<Arc<ReportedNames>>,
    /// Grows with every change to any index.
    revision: AtomicU64,
    closing: AtomicBool,
}

/// The core's recordings owner.
pub(crate) struct RecordingsHub {
    hub: Arc<Hub>,
}

impl RecordingsHub {
    pub(crate) fn new(
        rt: Handle,
        events: EventSink,
        clock: HostClock,
        data_dir: Option<PathBuf>,
    ) -> RecordingsHub {
        RecordingsHub {
            hub: Arc::new(Hub {
                rt,
                events,
                clock,
                data_dir,
                state: Mutex::new(State::default()),
                reports: Mutex::new(HashMap::new()),
                trashing: Mutex::new(HashMap::new()),
                wake: Notify::new(),
                sound: sound::SoundOwner::default(),
                reported: Mutex::new(Arc::new(HashMap::new())),
                revision: AtomicU64::new(0),
                closing: AtomicBool::new(false),
            }),
        }
    }

    /// What the device with this key reports about its channels goes to the
    /// owner through this hook (camera names in answers and downloads).
    pub(crate) fn names_hook(&self, key: &str) -> ChannelNamesHook {
        let hub = Arc::downgrade(&self.hub);
        let key = key.to_owned();
        Arc::new(move |channels| {
            if let Some(h) = hub.upgrade() {
                let mut r = lock(&h.reported);
                let mut all = (**r).clone();
                all.insert(key.clone(), channels);
                *r = Arc::new(all);
            }
        })
    }

    /// A device was added to the core (`key` is its stable key). Called
    /// with the runtime entered.
    pub(crate) fn device_added(&self, key: &str, id: u64, session: Arc<DeviceSession>) {
        let h = &self.hub;
        let old = {
            let mut st = lock(&h.state);
            st.sessions.insert(
                key.to_owned(),
                Added {
                    id,
                    session: session.clone(),
                },
            );
            if let Some(d) = st.stores.as_ref().and_then(|s| s.downloads.as_ref()) {
                d.scheduler.add_device(key, session);
            }
            h.start_lane(&mut st, key)
        };
        // The previous session's lane (if the device was added twice with
        // the same key) ends by itself once its search completes.
        drop(old);
        h.sound_device_added(key);
    }

    /// A device is being removed (before its session closes): its lane is
    /// stopped and waited for, so it no longer holds the session; downloads
    /// wait for it; its cameras leave every today hold. Blocks briefly
    /// (until a search in flight completes).
    pub(crate) fn device_removed(&self, key: &str, id: u64) {
        let h = &self.hub;
        h.sound_device_removed(key, id);
        let lane = {
            let mut st = lock(&h.state);
            if st.sessions.get(key).is_none_or(|a| a.id != id) {
                return;
            }
            st.sessions.remove(key);
            if let Some(d) = st.stores.as_ref().and_then(|s| s.downloads.as_ref()) {
                d.scheduler.remove_device(key);
            }
            for cams in st.holds.values_mut() {
                cams.retain(|(d, _)| d != key);
            }
            st.holds.retain(|_, c| !c.is_empty());
            let lane = st.devices.get_mut(key).and_then(|d| {
                d.parked.clear();
                d.stop_tasks()
            });
            if st.devices.contains_key(key) {
                let n = h.next_lane(&mut st);
                h.set_report(key, n, Some("waiting_device"));
            }
            lane
        };
        if let Some(l) = lane {
            h.rt.block_on(l.stop());
        }
    }

    /// Stops everything (before the core closes its sessions): background
    /// fills, lanes (waited for), the scheduler (running downloads stopped;
    /// job files keep the progress), and the pumps. Holds end.
    pub(crate) fn shutdown(&self) {
        let h = &self.hub;
        if h.closing.swap(true, Ordering::SeqCst) {
            return;
        }
        h.sound_shutdown();
        let (lanes, downloads, pump) = {
            let mut st = lock(&h.state);
            st.holds.clear();
            // The core closes the sessions next: hold none of them.
            st.sessions.clear();
            let lanes: Vec<DeviceLane> = st
                .devices
                .values_mut()
                .filter_map(Dev::stop_tasks)
                .collect();
            let downloads = st.stores.as_mut().and_then(|s| s.downloads.take());
            (lanes, downloads, st.pump.take())
        };
        if let Some(p) = pump {
            p.abort();
        }
        h.rt.block_on(async move {
            for l in lanes {
                l.stop().await;
            }
            if let Some(d) = downloads {
                d.stop().await;
            }
        });
        info!("recordings stopped");
    }

    pub(crate) fn hub(&self) -> &Arc<Hub> {
        &self.hub
    }

    /// What a playback (slice C10) needs of a camera's device: the index
    /// that says which files hold which times, the device's offset from
    /// UTC, and its session and core ID while it is added.
    ///
    /// # Errors
    /// [`RecError::NotConfigured`] before `recordings_configure`,
    /// [`RecError::InvalidArgument`] for a camera of a device that is not
    /// enabled or not allowed by its channel list.
    pub(crate) fn playback_device(
        &self,
        key: &str,
        cam: DeviceCamera,
    ) -> Result<PlaybackDevice, RecError> {
        let st = lock(&self.hub.state);
        sound::check_camera(&st, key, cam)?;
        let dev = st.devices.get(key).ok_or(RecError::InvalidArgument)?;
        let added = st.sessions.get(key);
        Ok(PlaybackDevice {
            index: dev.index.clone(),
            utc_offset_s: dev.enable.utc_offset_s,
            session: added.map(|a| a.session.clone()),
            id: added.map(|a| a.id),
        })
    }
}

/// A device as a playback sees it ([`RecordingsHub::playback_device`]).
pub(crate) struct PlaybackDevice {
    pub(crate) index: Arc<Mutex<DeviceIndex>>,
    pub(crate) utc_offset_s: i64,
    pub(crate) session: Option<Arc<DeviceSession>>,
    pub(crate) id: Option<u64>,
}

impl Hub {
    fn st(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn check(&self) -> Result<(), RecError> {
        if crate::in_callback::on_core_thread() {
            return Err(RecError::InvalidArgument);
        }
        if self.closing.load(Ordering::SeqCst) {
            return Err(RecError::Closed);
        }
        Ok(())
    }

    fn send(&self, v: Value) {
        self.events.send(v.to_string(), Priority::State);
    }

    /// Makes `lane` the one whose states count for `key`, and reports
    /// `state` (if any) as the device's.
    fn set_report(&self, key: &str, lane: u64, state: Option<&'static str>) {
        let mut reports = lock(&self.reports);
        let r = reports
            .entry(key.to_owned())
            .or_insert_with(|| Report::new(lane, "idle"));
        r.lane = lane;
        if let Some(s) = state {
            r.state = s;
            r.state_dirty = true;
        }
        drop(reports);
        self.wake.notify_one();
    }

    fn next_lane(&self, st: &mut State) -> u64 {
        st.next_lane += 1;
        st.next_lane
    }

    /// The device's local time: UTC on the core's clock plus its offset.
    fn device_clock(&self, offset: i64) -> recordings::Clock {
        let clock = self.clock.clone();
        Arc::new(move || clock.wall_now_us().div_euclid(1_000_000) + offset)
    }

    fn device_now(&self, offset: i64) -> Secs {
        self.clock.wall_now_us().div_euclid(1_000_000) + offset
    }

    /// Starts the lane of an enabled, added device (replacing an earlier
    /// one, which is returned to be dropped outside the lock).
    fn start_lane(self: &Arc<Self>, st: &mut State, key: &str) -> Option<DeviceLane> {
        let (thumbs, refresh) = {
            let s = st.stores.as_ref()?;
            (s.thumbs.clone(), s.settings.refresh_today_s)
        };
        let session = st.sessions.get(key)?.session.clone();
        let n = self.next_lane(st);
        let holds: Vec<(u64, Vec<IndexedChannel>)> = st
            .holds
            .iter()
            .map(|(h, cams)| {
                let mine = cams
                    .iter()
                    .filter(|(d, _)| d == key)
                    .map(|(_, c)| IndexedChannel {
                        camera: *c,
                        uid: None,
                    })
                    .collect::<Vec<_>>();
                (*h, mine)
            })
            .filter(|(_, c)| !c.is_empty())
            .collect();
        let dev = st.devices.get_mut(key)?;
        let old = dev.stop_tasks();
        // The new lane reports its own state; what the old one changed is
        // still reported.
        self.set_report(key, n, None);
        let mut cfg = LaneConfig::new(self.device_clock(dev.enable.utc_offset_s));
        cfg.refresh_today = refresh;
        let sink = lane_sink(Arc::downgrade(self), key.to_owned(), n);
        let lane = {
            let _rt = self.rt.enter();
            DeviceLane::spawn(session, dev.index.clone(), thumbs, cfg, sink)
        };
        for (h, cams) in holds {
            dev.lane_holds.insert(h, lane.hold_today(&cams));
        }
        for p in dev.parked.drain(..) {
            if !p.days.is_empty() {
                let _ = lane.request_days(&p.cameras, &p.days, p.force);
            }
            if !p.months.is_empty() {
                let _ = lane.request_months(&p.cameras, &p.months, p.force);
            }
        }
        if let Some(bg) = dev.enable.background {
            dev.background = Some(self.rt.spawn(background(
                Arc::downgrade(self),
                key.to_owned(),
                bg,
            )));
        }
        dev.lane = Some(lane);
        self.wake.notify_one();
        old
    }

    /// The channel names devices reported, as of now.
    fn reported(&self) -> Arc<ReportedNames> {
        lock(&self.reported).clone()
    }

    // ---- lane events ----------------------------------------------------

    fn on_lane_event(&self, key: &str, lane: u64, e: LaneEvent) {
        match e {
            LaneEvent::Thumbnail { key: k, state } => {
                use recordings::lane::ThumbState;
                let (name, reason) = match state {
                    ThumbState::Ready => ("ready", None),
                    ThumbState::Empty => ("empty", None),
                    ThumbState::Failed(r) => ("failed", Some(r)),
                    ThumbState::Dropped => ("dropped", None),
                };
                self.send_thumbnail(&k, name, reason.as_deref());
            }
            LaneEvent::DayFailed {
                camera,
                day,
                reason,
            } => {
                self.send(json!({
                    "type": "recordings_day_failed", "device": key,
                    "camera": camera_json(key, camera), "day": day, "reason": reason,
                }));
                if let Some(r) = lock(&self.reports).get_mut(key) {
                    r.progress_dirty = true;
                }
                self.wake.notify_one();
            }
            LaneEvent::MonthIndexed { month, .. } => {
                self.revision.fetch_add(1, Ordering::SeqCst);
                let mut reports = lock(&self.reports);
                if let Some(r) = reports.get_mut(key) {
                    let (s, e) = recordings::month_span(month);
                    r.changed = Some(r.changed.map_or((s, e), |(a, b)| (a.min(s), b.max(e))));
                    r.progress_dirty = true;
                }
                drop(reports);
                self.wake.notify_one();
            }
            LaneEvent::MonthFailed {
                cameras,
                month,
                reason,
            } => {
                let cameras: Vec<Value> = cameras.iter().map(|c| camera_json(key, *c)).collect();
                self.send(json!({
                    "type": "recordings_month_failed", "device": key,
                    "cameras": cameras, "month": month, "reason": reason,
                }));
                if let Some(r) = lock(&self.reports).get_mut(key) {
                    r.progress_dirty = true;
                }
                self.wake.notify_one();
            }
            LaneEvent::DayIndexed { day, .. } => {
                self.revision.fetch_add(1, Ordering::SeqCst);
                let mut reports = lock(&self.reports);
                if let Some(r) = reports.get_mut(key) {
                    let (s, e) = (day_start(day), day_start(day) + 86_399);
                    r.changed = Some(r.changed.map_or((s, e), |(a, b)| (a.min(s), b.max(e))));
                    r.progress_dirty = true;
                }
                drop(reports);
                self.wake.notify_one();
            }
            LaneEvent::State(s) => {
                let mut reports = lock(&self.reports);
                if let Some(r) = reports.get_mut(key).filter(|r| r.lane == lane) {
                    r.state = state_name(s);
                    r.state_dirty = true;
                }
                drop(reports);
                self.wake.notify_one();
            }
        }
    }

    fn send_thumbnail(&self, k: &ThumbKey, state: &str, reason: Option<&str>) {
        let (lens, tier) = recordings::lens_and_tier(k.stream);
        let mut v = json!({
            "type": "thumbnail",
            "key": {
                "camera": camera_json(&k.device, DeviceCamera::new(k.channel, lens)),
                "tier": tier_name(tier), "start": k.start, "end": k.end,
            },
            "state": state,
        });
        if let Some(r) = reason {
            v["reason"] = Value::from(r);
        }
        self.send(v);
    }

    /// Sends what is due; returns when to look again.
    fn flush_reports(&self, now: Instant) -> Option<Instant> {
        let mut changed: Vec<(String, Secs, Secs)> = Vec::new();
        let mut index: Vec<(String, &'static str)> = Vec::new();
        let mut next: Option<Instant> = None;
        let mut at = |t: Instant| next = Some(next.map_or(t, |n: Instant| n.min(t)));
        {
            let mut reports = lock(&self.reports);
            for (key, r) in reports.iter_mut() {
                if let Some((s, e)) = r.changed {
                    // While filling, at most once a second; the end of a
                    // fill (any state but busy) is reported at once.
                    let due = r
                        .last_changed
                        .is_none_or(|t| now >= t + INDEX_EVENT_INTERVAL)
                        || (r.state_dirty && r.state != "busy");
                    if due {
                        changed.push((key.clone(), s, e));
                        r.changed = None;
                        r.last_changed = Some(now);
                    } else if let Some(t) = r.last_changed {
                        at(t + INDEX_EVENT_INTERVAL);
                    }
                }
                if r.state_dirty {
                    index.push((key.clone(), r.state));
                    r.state_dirty = false;
                    r.progress_dirty = false;
                    r.last_index = Some(now);
                } else if r.progress_dirty {
                    if r.state != "busy" {
                        r.progress_dirty = false;
                    } else if r.last_index.is_none_or(|t| now >= t + INDEX_EVENT_INTERVAL) {
                        index.push((key.clone(), r.state));
                        r.progress_dirty = false;
                        r.last_index = Some(now);
                    } else if let Some(t) = r.last_index {
                        at(t + INDEX_EVENT_INTERVAL);
                    }
                }
            }
        }
        if let Some(t) = self.settle_timer(now) {
            at(t);
        }
        if changed.is_empty() && index.is_empty() && !self.st().incidents.dirty {
            return next;
        }
        let revision = self.revision.load(Ordering::SeqCst);
        for (key, s, e) in &changed {
            self.send(json!({
                "type": "recordings_changed", "device": key, "revision": revision,
                "start": s, "end": e,
            }));
        }
        let mut st = self.st();
        // A lane prunes its index as a fill ends (before it reports idle).
        if !changed.is_empty() || index.iter().any(|(_, s)| *s == "idle") {
            st.incidents.dirty = true;
        }
        if st.incidents.dirty {
            self.regroup(&mut st);
        }
        for (key, state) in index {
            let pending = st.devices.get(&key).map_or(0, Dev::pending_days);
            self.send(json!({
                "type": "recordings_index", "device": key, "state": state,
                "pending_days": pending, "revision": revision,
            }));
        }
        next
    }

    /// Groups every index's events, replays the edits, and sends
    /// `incidents_changed` when the result differs.
    fn regroup(&self, st: &mut State) {
        st.incidents.dirty = false;
        let Some(stores) = st.stores.as_mut() else {
            return;
        };
        let all = EventQuery {
            cameras: Vec::new(),
            kinds: Vec::new(),
            start: 0,
            end: 253_402_300_799,
        };
        let mut events: Vec<CameraEvent> = Vec::new();
        let mut local = HashSet::new();
        let mut grouped = Vec::new();
        let mut keys: Vec<&String> = st.devices.keys().collect();
        keys.sort();
        for key in keys {
            let idx = lock(&st.devices[key].index);
            grouped.push((key.clone(), idx.revision()));
            events.extend(idx.events(&all).into_iter().map(|e| {
                let camera = CameraId::with_lens(key.clone(), e.channel, e.lens);
                if e.source == recordings::index::EventSource::LocalAudio {
                    local.insert((camera.clone(), e.kind.clone(), e.start, e.end));
                }
                CameraEvent {
                    start: e.start,
                    end: e.end,
                    camera,
                    kind: e.kind,
                }
            }));
        }
        st.incidents.local = local;
        st.incidents.grouped = grouped;
        // Edits older than the index keeps refer to nothing any more.
        let oldest =
            self.device_now(0) - (i64::from(stores.settings.index.max_age_days) + 1) * 86_400;
        locks::expire_edits(stores, oldest);
        let cfg = stores
            .groups
            .grouping(stores.settings.gap_s, stores.settings.margin_s);
        let base = group(&events, &cfg);
        let edited = stores.edits.apply(&base);
        st.incidents.base = base;
        let locks_changed = self.follow_locks(st, &edited);
        if edited != st.incidents.edited || locks_changed {
            st.incidents.edited = edited;
            st.incidents.revision += 1;
            self.send(json!({"type": "incidents_changed", "revision": st.incidents.revision}));
        }
    }

    /// Regroups if anything marked the incidents dirty or any index changed
    /// since they were grouped, so they always cover what the indexes return.
    fn fresh_incidents(&self, st: &mut State) {
        if st.incidents.dirty || index_changed(st) {
            self.regroup(st);
        }
    }

    fn mark_incidents(&self, st: &mut State) {
        st.incidents.dirty = true;
        self.wake.notify_one();
    }
}

/// Whether the set of indexes, or any index's revision, differs from what
/// the incidents were grouped from.
fn index_changed(st: &State) -> bool {
    let grouped = &st.incidents.grouped;
    grouped.len() != st.devices.len()
        || grouped.iter().any(|(key, revision)| {
            st.devices
                .get(key)
                .is_none_or(|d| lock(&d.index).revision() != *revision)
        })
}

fn tier_name(t: recordings::Tier) -> &'static str {
    match t {
        recordings::Tier::Main => "main",
        recordings::Tier::Sub => "sub",
    }
}

/// The sink a lane reports to: never blocks, takes only the reports lock.
fn lane_sink(hub: Weak<Hub>, key: String, lane: u64) -> recordings::lane::LaneSink {
    Arc::new(move |e: LaneEvent| {
        if let Some(h) = hub.upgrade() {
            h.on_lane_event(&key, lane, e);
        }
    })
}

/// Coalesces index events (see the module documentation).
async fn pump(hub: Weak<Hub>) {
    loop {
        let Some(h) = hub.upgrade() else {
            return;
        };
        let notified = h.wake.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if h.closing.load(Ordering::SeqCst) {
            return;
        }
        // `h` lives until the wait ends; shutdown aborts this task.
        let next = h.flush_reports(Instant::now());
        match next {
            Some(t) => {
                tokio::select! {
                    _ = &mut notified => {}
                    _ = tokio::time::sleep_until(t) => {}
                }
            }
            None => notified.await,
        }
    }
}

/// The optional background fill of one device: every interval, while the
/// session is connected and the lane has nothing waiting, ask for the last
/// `days_back` days (newest first; days already fresh are not searched
/// again). It goes through the lane, so thumbnails still come first, and
/// a refused login stops it asking (the session is never connected again).
async fn background(hub: Weak<Hub>, key: String, bg: config::Background) {
    loop {
        {
            let Some(h) = hub.upgrade() else {
                return;
            };
            let st = h.st();
            let (Some(dev), Some(added)) = (st.devices.get(&key), st.sessions.get(&key)) else {
                return;
            };
            let lane: Option<LaneHandle> = dev.lane.as_ref().map(|l| l.handle());
            if let Some(lane) =
                lane.filter(|l| added.session.state() == ConnState::Connected && l.pending().1 == 0)
            {
                let cams = dev.background_cameras(&added.session);
                let today = day_start(h.device_now(dev.enable.utc_offset_s));
                let days: Vec<Secs> = (0..i64::from(bg.days_back))
                    .map(|n| today - n * 86_400)
                    .collect();
                for chunk in days.chunks(MAX_WANT_DAYS) {
                    if lane.request_days(&cams, chunk, false).is_err() {
                        break;
                    }
                }
            }
        }
        tokio::time::sleep(bg.pass_interval).await;
    }
}

// ---- the Rust API that the C interface wraps -----------------------------

impl crate::Core {
    fn rec(&self) -> &Arc<Hub> {
        self.recordings.hub()
    }

    /// `nv_recordings_configure`: sets the recordings configuration
    /// (docs/FFI-RECORDINGS.md, section 1). The first call opens the stores
    /// under `data_dir/recordings` (in a scratch directory for the run
    /// without a data directory); later calls change the settings. Index and
    /// thumbnail limits apply to devices enabled afterwards. With a
    /// `downloads_dir`, unfinished downloads of an earlier run are resumed
    /// in the background (they wait for their devices); changing the
    /// downloads settings restarts the scheduler.
    ///
    /// # Errors
    /// [`RecError::InvalidArgument`] for invalid JSON or values,
    /// [`RecError::Internal`] if the stores cannot be created,
    /// [`RecError::Closed`].
    pub fn recordings_configure(&self, json: &str) -> Result<(), RecError> {
        let h = self.rec();
        h.check()?;
        let settings = config::parse_settings(json)?;
        let mut st = h.st();
        if st.stores.is_none() {
            let (root, scratch) = match &h.data_dir {
                Some(d) => (d.join("recordings"), None),
                None => {
                    let s = Scratch::new().map_err(|_| RecError::Internal)?;
                    (s.0.clone(), Some(s))
                }
            };
            std::fs::create_dir_all(&root).map_err(|_| RecError::Internal)?;
            let thumbs = ThumbnailCache::open(root.join("thumbnails"), settings.thumbnails)
                .map_err(|_| RecError::Internal)?;
            let persistent = h.data_dir.is_some();
            let edits = if persistent {
                EditLog::open(&root.join("incident-edits.json"))
            } else {
                EditLog::in_memory()
            };
            let groups = if persistent {
                GroupStore::open(&root.join("groups.json"))
            } else {
                GroupStore::in_memory()
            };
            st.stores = Some(Stores {
                settings: settings.clone(),
                _scratch: scratch,
                thumbs: Arc::new(Mutex::new(thumbs)),
                edits,
                groups,
                locks: locks::open(persistent.then(|| root.join("incident-locks.json"))),
                root,
                downloads: None,
            });
            st.pump = Some(h.rt.spawn(pump(Arc::downgrade(h))));
        }
        let stores = st.stores.as_mut().expect("just set");
        stores.settings = settings.clone();
        let wanted = settings
            .downloads_dir
            .clone()
            .map(|d| (d, settings.downloads.clone()));
        let current = stores.downloads.as_ref().map(downloads::Downloads::key);
        if wanted != current {
            let old = stores.downloads.take();
            if let Some((dir, dl)) = wanted {
                let sessions: Vec<(String, Arc<DeviceSession>)> = st
                    .sessions
                    .iter()
                    .map(|(k, a)| (k.clone(), a.session.clone()))
                    .collect();
                let d = downloads::Downloads::start(h, dir, dl, &sessions, old);
                st.stores.as_mut().expect("set").downloads = Some(d);
            } else if let Some(old) = old {
                h.rt.spawn(old.stop());
            }
        }
        h.mark_incidents(&mut st);
        Ok(())
    }

    /// `nv_recordings_enable`: allows (`json` some) or stops (`None`)
    /// indexing one device by its stable key (docs/FFI-RECORDINGS.md,
    /// section 1). The device need not be added yet: its index opens now,
    /// and its lane starts when it is added. Nothing is searched until a
    /// view asks, unless the `background` block is set.
    ///
    /// # Errors
    /// [`RecError::NotConfigured`] before [`crate::Core::recordings_configure`],
    /// [`RecError::InvalidArgument`], [`RecError::Internal`] if the index
    /// cannot be opened, [`RecError::Closed`].
    pub fn recordings_enable(&self, device_key: &str, json: Option<&str>) -> Result<(), RecError> {
        let h = self.rec();
        h.check()?;
        if device_key.is_empty() || device_key.len() > 256 {
            return Err(RecError::InvalidArgument);
        }
        let enable = json.map(config::parse_enable).transpose()?;
        let mut st = h.st();
        let (root, index_cfg) = {
            let stores = st.stores.as_ref().ok_or(RecError::NotConfigured)?;
            (stores.root.clone(), stores.settings.index)
        };
        let old_lane = match enable {
            None => {
                let lane = st
                    .devices
                    .remove(device_key)
                    .and_then(|mut d| d.stop_tasks());
                for cams in st.holds.values_mut() {
                    cams.retain(|(d, _)| d != device_key);
                }
                st.holds.retain(|_, c| !c.is_empty());
                lock(&h.reports).remove(device_key);
                lane
            }
            Some(enable) => {
                if let Some(dev) = st.devices.get_mut(device_key) {
                    let restart = dev.enable != enable;
                    dev.enable = enable;
                    if restart {
                        h.start_lane(&mut st, device_key)
                    } else {
                        None
                    }
                } else {
                    let dir = root.join("index").join(dir_name(device_key));
                    let index = DeviceIndex::open(&dir, index_cfg).map_err(|e| {
                        warn!(error = %e, "cannot open a recordings index");
                        RecError::Internal
                    })?;
                    st.devices.insert(
                        device_key.to_owned(),
                        Dev {
                            enable,
                            index: Arc::new(Mutex::new(index)),
                            lane: None,
                            lane_holds: HashMap::new(),
                            parked: Vec::new(),
                            background: None,
                        },
                    );
                    locks::keep_days(&st, device_key);
                    let n = h.next_lane(&mut st);
                    h.set_report(device_key, n, Some("waiting_device"));
                    h.start_lane(&mut st, device_key)
                }
            }
        };
        h.mark_incidents(&mut st);
        drop(st);
        drop(old_lane);
        Ok(())
    }

    /// Groups request cameras by device, checking each is enabled and
    /// allowed.
    fn rec_cameras(
        st: &State,
        list: Vec<(String, DeviceCamera)>,
    ) -> Result<Vec<(String, Vec<DeviceCamera>)>, RecError> {
        let mut by: Vec<(String, Vec<DeviceCamera>)> = Vec::new();
        for (key, cam) in list {
            let dev = st.devices.get(&key).ok_or(RecError::InvalidArgument)?;
            let session = st.sessions.get(&key).map(|a| &a.session);
            if !dev.allows(cam, session) {
                return Err(RecError::InvalidArgument);
            }
            match by.iter_mut().find(|(k, _)| *k == key) {
                Some((_, v)) => {
                    if !v.contains(&cam) {
                        v.push(cam);
                    }
                }
                None => by.push((key, vec![cam])),
            }
        }
        Ok(by)
    }

    /// `nv_recordings_want`: asks for these cameras' days to be in the
    /// index (`{"cameras": [...], "days": [...], "months": [...], "force":
    /// false}`); returns at once, and `recordings_changed` follows as days
    /// arrive. `months` (minor 7; any second within each month) asks for
    /// only those months' calendars, after the days; either list may be
    /// left out, not both. A device that is not added keeps the request
    /// until it is (and reports `waiting_device`).
    ///
    /// # Errors
    /// [`RecError::Limit`] for more than 31 days or 12 months, or when a
    /// device's queue (4096 camera-days) would overflow (nothing is queued
    /// then),
    /// [`RecError::InvalidArgument`] for a camera not enabled,
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn recordings_want(&self, json: &str) -> Result<(), RecError> {
        let h = self.rec();
        h.check()?;
        let m = config::request(json)?;
        let cams = config::cameras(&m)?;
        let times =
            |name: &str, start: fn(Secs) -> Secs| -> Result<Option<BTreeSet<Secs>>, RecError> {
                match m.get(name) {
                    None => Ok(None),
                    Some(Value::Array(a)) => a
                        .iter()
                        .map(|v| {
                            v.as_i64()
                                .filter(|s| (0..=253_402_300_799).contains(s))
                                .map(start)
                                .ok_or(RecError::InvalidArgument)
                        })
                        .collect::<Result<_, _>>()
                        .map(Some),
                    Some(_) => Err(RecError::InvalidArgument),
                }
            };
        let (days, months) = match (
            times("days", day_start)?,
            times("months", |s| recordings::month_span(s).0)?,
        ) {
            (None, None) => return Err(RecError::InvalidArgument),
            (d, mo) => (d.unwrap_or_default(), mo.unwrap_or_default()),
        };
        if days.len() > MAX_WANT_DAYS || months.len() > MAX_WANT_MONTHS {
            return Err(RecError::Limit);
        }
        let force = config::flag(&m, "force")?;
        let days: Vec<Secs> = days.into_iter().rev().collect();
        let months: Vec<Secs> = months.into_iter().rev().collect();
        let mut st = h.st();
        if st.stores.is_none() {
            return Err(RecError::NotConfigured);
        }
        let by = Self::rec_cameras(&st, cams)?;
        // Checked for every device first, so a refusal queues nothing.
        for (key, cams) in &by {
            let dev = &st.devices[key];
            if dev.pending_days() + cams.len() * (days.len() + months.len()) > MAX_PARKED_DAYS {
                return Err(RecError::Limit);
            }
        }
        for (key, cams) in by {
            let chs: Vec<IndexedChannel> = cams
                .iter()
                .map(|c| IndexedChannel {
                    camera: *c,
                    uid: None,
                })
                .collect();
            let dev = st.devices.get_mut(&key).expect("checked");
            match &dev.lane {
                Some(l) => {
                    if !days.is_empty() {
                        l.request_days(&chs, &days, force)
                            .map_err(|_| RecError::Limit)?;
                    }
                    if !months.is_empty() {
                        l.request_months(&chs, &months, force)
                            .map_err(|_| RecError::Limit)?;
                    }
                }
                None => {
                    dev.parked.push(Parked {
                        cameras: chs,
                        days: days.clone(),
                        months: months.clone(),
                        force,
                    });
                    if let Some(r) = lock(&h.reports).get_mut(&key) {
                        r.state = "waiting_device";
                        r.state_dirty = true;
                    }
                    h.wake.notify_one();
                }
            }
        }
        Ok(())
    }

    /// `nv_recordings_hold_today`: keeps today fresh for these cameras while
    /// the hold lasts (`hold` is any nonzero number the app chooses; `json`
    /// `{"cameras": [...]}` sets or replaces its cameras, `None` releases
    /// it).
    ///
    /// # Errors
    /// [`RecError::InvalidArgument`] for hold 0 or a camera not enabled,
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn recordings_hold_today(&self, hold: u64, json: Option<&str>) -> Result<(), RecError> {
        let h = self.rec();
        h.check()?;
        if hold == 0 {
            return Err(RecError::InvalidArgument);
        }
        let cams = match json {
            Some(j) => Some(config::cameras(&config::request(j)?)?),
            None => None,
        };
        let mut st = h.st();
        if st.stores.is_none() {
            return Err(RecError::NotConfigured);
        }
        let mut released: Vec<TodayHold> = Vec::new();
        for dev in st.devices.values_mut() {
            released.extend(dev.lane_holds.remove(&hold));
        }
        match cams {
            None => {
                st.holds.remove(&hold);
            }
            Some(cams) => {
                let by = Self::rec_cameras(&st, cams)?;
                let flat: Vec<(String, DeviceCamera)> = by
                    .iter()
                    .flat_map(|(k, v)| v.iter().map(move |c| (k.clone(), *c)))
                    .collect();
                if flat.is_empty() {
                    st.holds.remove(&hold);
                } else {
                    st.holds.insert(hold, flat);
                }
                for (key, cams) in by {
                    let dev = st.devices.get_mut(&key).expect("checked");
                    if let Some(lane) = &dev.lane {
                        let chs: Vec<IndexedChannel> = cams
                            .iter()
                            .map(|c| IndexedChannel {
                                camera: *c,
                                uid: None,
                            })
                            .collect();
                        let t = lane.hold_today(&chs);
                        dev.lane_holds.insert(hold, t);
                    }
                }
            }
        }
        drop(st);
        drop(released);
        Ok(())
    }

    /// `nv_recordings_status`: the index state of every enabled device,
    /// `{"revision": N, "devices": [{"device", "state", "pending_days",
    /// "revision"}]}`.
    ///
    /// # Errors
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn recordings_status(&self) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let st = h.st();
        if st.stores.is_none() {
            return Err(RecError::NotConfigured);
        }
        let revision = h.revision.load(Ordering::SeqCst);
        let reports = lock(&h.reports);
        let mut keys: Vec<&String> = st.devices.keys().collect();
        keys.sort();
        let devices: Vec<Value> = keys
            .into_iter()
            .map(|k| {
                let state = reports.get(k).map_or("idle", |r| r.state);
                json!({"device": k, "state": state,
                       "pending_days": st.devices[k].pending_days(), "revision": revision})
            })
            .collect();
        Ok(json!({"revision": revision, "devices": devices}).to_string())
    }

    /// `nv_recordings_calendar`: days with recordings per camera in the
    /// query's range (docs/FFI-RECORDINGS.md, section 2). From memory.
    ///
    /// # Errors
    /// [`RecError::InvalidArgument`], [`RecError::NotConfigured`],
    /// [`RecError::Closed`].
    pub fn recordings_calendar(&self, query: &str) -> Result<String, RecError> {
        self.rec_query(query, query::calendar)
    }

    /// `nv_recordings_spans`: recording files per camera (timeline lanes).
    ///
    /// # Errors
    /// As [`crate::Core::recordings_calendar`].
    pub fn recordings_spans(&self, query: &str) -> Result<String, RecError> {
        self.rec_query(query, query::spans)
    }

    /// `nv_recordings_events`: events matching the query, by start.
    ///
    /// # Errors
    /// As [`crate::Core::recordings_calendar`].
    pub fn recordings_events(&self, query: &str) -> Result<String, RecError> {
        self.rec_query(query, query::events)
    }

    fn rec_query(
        &self,
        query: &str,
        f: fn(&query::Ctx<'_>, &query::Query) -> Value,
    ) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let q = query::Query::parse(query)?;
        let st = h.st();
        if st.stores.is_none() {
            return Err(RecError::NotConfigured);
        }
        let ctx = query::Ctx {
            devices: &st.devices,
            revision: h.revision.load(Ordering::SeqCst),
        };
        Ok(f(&ctx, &q).to_string())
    }

    /// `nv_incidents_get`: incidents overlapping the query's range, newest
    /// first (section 3).
    ///
    /// # Errors
    /// As [`crate::Core::recordings_calendar`].
    pub fn incidents_get(&self, query: &str) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let q = query::Query::parse(query)?;
        let mut st = h.st();
        if st.stores.is_none() {
            return Err(RecError::NotConfigured);
        }
        h.fresh_incidents(&mut st);
        Ok(query::incidents(h, &st, &q).to_string())
    }

    /// `nv_incident_get`: one incident by key, as
    /// `{"revision": N, "incident": {...}}`.
    ///
    /// # Errors
    /// [`RecError::UnknownId`] if no incident has the key now,
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn incident_get(&self, key: &str) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let mut st = h.st();
        if st.stores.is_none() {
            return Err(RecError::NotConfigured);
        }
        h.fresh_incidents(&mut st);
        let inc = query::incident_by_key(h, &st, key).ok_or(RecError::UnknownId)?;
        Ok(json!({"revision": st.incidents.revision, "incident": inc}).to_string())
    }

    /// `nv_incident_edit`: applies one edit (section 3), stores it, sends
    /// `incidents_changed`, and returns `{"revision": N, "keys": [...]}`.
    ///
    /// # Errors
    /// [`RecError::UnknownId`] for an unknown key,
    /// [`RecError::InvalidArgument`] for an edit that cannot apply,
    /// [`RecError::Limit`] beyond 10000 edits or 1000 incidents in a merge,
    /// [`RecError::Internal`] if it cannot be stored,
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn incident_edit(&self, edit: &str) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let edit: Edit = serde_json::from_str(edit).map_err(|_| RecError::InvalidArgument)?;
        if let Edit::Merge { keys } = &edit
            && keys.len() > MAX_MERGE
        {
            return Err(RecError::Limit);
        }
        if let Edit::Rename { name: Some(n), .. } = &edit
            && n.len() > recordings::edits::MAX_NAME_BYTES
        {
            return Err(RecError::InvalidArgument);
        }
        let mut st = h.st();
        if st.stores.is_none() {
            return Err(RecError::NotConfigured);
        }
        h.fresh_incidents(&mut st);
        locks::check_merge(&st, &edit)?;
        let base = std::mem::take(&mut st.incidents.base);
        let stores = st.stores.as_mut().expect("checked");
        let r = stores.edits.push(&base, edit);
        st.incidents.base = base;
        let keys = r.map_err(|e| match e {
            EditError::UnknownKey(_) => RecError::UnknownId,
            EditError::Invalid(_) => RecError::InvalidArgument,
            EditError::Full => RecError::Limit,
            EditError::Io(e) => {
                warn!(error = %e, "cannot store an incident edit");
                RecError::Internal
            }
        })?;
        h.regroup(&mut st);
        Ok(json!({"revision": st.incidents.revision, "keys": keys}).to_string())
    }

    /// `nv_camera_groups_get`: `{"revision": N, "groups": [...],
    /// "camera_names": [...]}`.
    ///
    /// # Errors
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn camera_groups_get(&self) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let st = h.st();
        let stores = st.stores.as_ref().ok_or(RecError::NotConfigured)?;
        let mut v = serde_json::to_value(stores.groups.get()).map_err(|_| RecError::Internal)?;
        v["revision"] = Value::from(stores.groups.revision());
        Ok(v.to_string())
    }

    /// `nv_camera_groups_put`: replaces the camera groups and names,
    /// regroups, and sends `camera_groups_changed` (and `incidents_changed`
    /// when incidents changed).
    ///
    /// # Errors
    /// [`RecError::Limit`] beyond 64 groups of 64 cameras, 4096 names, or
    /// 256-byte names (nothing stored), [`RecError::InvalidArgument`] for
    /// malformed JSON, empty or duplicate names, [`RecError::Internal`],
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn camera_groups_put(&self, json: &str) -> Result<(), RecError> {
        use recordings::groups::{MAX_CAMERA_NAMES, MAX_GROUP_CAMERAS, MAX_GROUPS, MAX_NAME_BYTES};
        let h = self.rec();
        h.check()?;
        let value: CameraGroups =
            serde_json::from_str(json).map_err(|_| RecError::InvalidArgument)?;
        if value.groups.len() > MAX_GROUPS
            || value
                .groups
                .iter()
                .any(|g| g.cameras.len() > MAX_GROUP_CAMERAS || g.name.len() > MAX_NAME_BYTES)
            || value.camera_names.len() > MAX_CAMERA_NAMES
            || value
                .camera_names
                .iter()
                .any(|n| n.name.len() > MAX_NAME_BYTES)
        {
            return Err(RecError::Limit);
        }
        let mut st = h.st();
        let stores = st.stores.as_mut().ok_or(RecError::NotConfigured)?;
        let before = stores
            .groups
            .grouping(stores.settings.gap_s, stores.settings.margin_s);
        let revision = stores.groups.put(value).map_err(|e| match e {
            GroupsError::Invalid(_) => RecError::InvalidArgument,
            GroupsError::Io(e) => {
                warn!(error = %e, "cannot store the camera groups");
                RecError::Internal
            }
        })?;
        locks::rekey(stores, &before);
        h.send(json!({"type": "camera_groups_changed", "revision": revision}));
        h.regroup(&mut st);
        Ok(())
    }

    /// Maps a thumbnail key (section 4) onto the cache's.
    fn thumb_key(json: &str) -> Result<ThumbKey, RecError> {
        let m = config::request(json)?;
        let (device, cam) = config::camera(m.get("camera").ok_or(RecError::InvalidArgument)?)?;
        let tier = config::tier(&m, recordings::Tier::Sub)?;
        let (start, end) = (config::secs(&m, "start")?, config::secs(&m, "end")?);
        if end < start {
            return Err(RecError::InvalidArgument);
        }
        Ok(ThumbKey {
            device,
            channel: cam.channel,
            stream: cam.stream(tier),
            start,
            end,
        })
    }

    /// `nv_thumbnail_get`: a cached cover thumbnail, or
    /// [`ThumbnailAnswer::Pending`] with a fetch queued (a `thumbnail` event
    /// follows). Never waits on the device. A device that is not enabled
    /// and added gets a `thumbnail` event `failed`.
    ///
    /// # Errors
    /// [`RecError::InvalidArgument`], [`RecError::NotConfigured`],
    /// [`RecError::Closed`].
    pub fn thumbnail_get(&self, key: &str) -> Result<ThumbnailAnswer, RecError> {
        let h = self.rec();
        h.check()?;
        let k = Self::thumb_key(key)?;
        let st = h.st();
        let stores = st.stores.as_ref().ok_or(RecError::NotConfigured)?;
        if let Some(t) = lock(&stores.thumbs).get(&k) {
            return Ok(ThumbnailAnswer::Ready(t));
        }
        match st.devices.get(&k.device).and_then(|d| d.lane.as_ref()) {
            Some(lane) => lane.request_thumbnail(k, None),
            None => h.send_thumbnail(&k, "failed", Some("the device is not enabled and added")),
        }
        Ok(ThumbnailAnswer::Pending)
    }

    /// `nv_thumbnail_put`: replaces the cached entry (usually with the
    /// app's small decoded image).
    ///
    /// # Errors
    /// [`RecError::Limit`] if larger than the cache's byte limit,
    /// [`RecError::InvalidArgument`], [`RecError::Internal`],
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn thumbnail_put(&self, key: &str, thumb: &Thumbnail) -> Result<(), RecError> {
        let h = self.rec();
        h.check()?;
        let k = Self::thumb_key(key)?;
        let st = h.st();
        let stores = st.stores.as_ref().ok_or(RecError::NotConfigured)?;
        match lock(&stores.thumbs).put(&k, thumb) {
            Ok(true) => Ok(()),
            Ok(false) => Err(RecError::Limit),
            Err(e) => {
                warn!(error = %e, "cannot store a thumbnail");
                Err(RecError::Internal)
            }
        }
    }

    /// `nv_thumbnail_cancel`: drops queued fetches of one key, or every
    /// queued fetch (`None`). A fetch already sent completes and is cached.
    ///
    /// # Errors
    /// [`RecError::InvalidArgument`], [`RecError::NotConfigured`],
    /// [`RecError::Closed`].
    pub fn thumbnail_cancel(&self, key: Option<&str>) -> Result<(), RecError> {
        let h = self.rec();
        h.check()?;
        let k = key.map(Self::thumb_key).transpose()?;
        let st = h.st();
        if st.stores.is_none() {
            return Err(RecError::NotConfigured);
        }
        match &k {
            Some(k) => {
                if let Some(l) = st.devices.get(&k.device).and_then(|d| d.lane.as_ref()) {
                    l.cancel_thumbnails(Some(k));
                }
            }
            None => {
                for l in st.devices.values().filter_map(|d| d.lane.as_ref()) {
                    l.cancel_thumbnails(None);
                }
            }
        }
        Ok(())
    }
}

impl Hub {
    /// The current incidents' downloads by incident key (IDs).
    fn downloads_by_key(&self, st: &State) -> HashMap<String, Vec<String>> {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        if let Some(d) = st.stores.as_ref().and_then(|s| s.downloads.as_ref()) {
            let trashing = lock(&self.trashing);
            for p in d.scheduler.progress().borrow().iter() {
                if !trashing.contains_key(&p.id) {
                    m.entry(p.key.clone()).or_default().push(p.id.clone());
                }
            }
        }
        m
    }

    fn scheduler(&self, st: &State) -> Result<Scheduler, RecError> {
        let stores = st.stores.as_ref().ok_or(RecError::NotConfigured)?;
        stores
            .downloads
            .as_ref()
            .map(|d| d.scheduler.clone())
            .ok_or(RecError::NotConfigured)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_names_prefer_the_override_then_the_reported_name_with_its_lens() {
        let dir = tempfile::tempdir().unwrap();
        let groups = GroupStore::open(&dir.path().join("groups.json"));
        let mut reported = ReportedNames::new();
        reported.insert(
            "d-1".into(),
            HashMap::from([
                (
                    0,
                    ReportedChannel {
                        name: "Front".into(),
                        lenses: 1,
                    },
                ),
                (
                    1,
                    ReportedChannel {
                        name: "Yard".into(),
                        lenses: 2,
                    },
                ),
            ]),
        );
        let name = |c: CameraId| camera_name(&groups, &reported, &c);
        assert_eq!(name(CameraId::new("d-1", 0)), "Front");
        assert_eq!(name(CameraId::with_lens("d-1", 1, 0)), "Yard · Lens 1");
        assert_eq!(name(CameraId::with_lens("d-1", 1, 1)), "Yard · Lens 2");
        // Not reported (yet): the numbered name.
        assert_eq!(name(CameraId::new("d-1", 4)), "Camera 5");
        assert_eq!(name(CameraId::new("d-2", 0)), "Camera 1");
    }
}
