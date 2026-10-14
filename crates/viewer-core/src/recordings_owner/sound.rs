//! Sound for recordings (docs/FFI-RECORDINGS.md, section 2a; slice C12):
//! the loudness envelope store, sound labels as events of source
//! `local_audio`, and the owner calls around them. Listening and analysis
//! of recorded days are in [`super::sound_work`].
//!
//! - **Envelope.** The live analysis of every tile with audio
//!   ([`crate::sound`]) reports each second of loudness here
//!   ([`SoundTap`]); the second goes into the device's
//!   [`recordings::sound::EnvelopeStore`] (`data_dir/recordings/sound/<device>/`,
//!   or memory without a data directory) as a device-local naive second
//!   (UTC plus the offset given to `recordings_enable`) for the camera's
//!   first lens (a microphone belongs to the camera, not a lens). Only
//!   devices enabled for recordings are recorded: their offset and limits
//!   are known. The store's limits are the index's. A task writes changed
//!   days every [`FLUSH_INTERVAL`] and at shutdown, so a crash loses at
//!   most that much.
//! - **Labels.** The core's alarm detector (`pattern-v1`) writes
//!   `smoke_alarm` and `co_alarm` into the device's index the moment an
//!   episode starts (a label of zero length) and extends it when the
//!   episode ends; labels from the app arrive with
//!   [`crate::Core::sound_labels_put`]. Both are stored with the index, so
//!   `recordings_events` and incidents show them like device events.
//! - **Envelope revision.** `recordings_loudness` answers with the sum of
//!   the revisions of the stores it read, which grows with every second
//!   stored. It is not the index revision, which labels do raise.

use super::sound_work::{self, DayAudioSource};
use super::{Hub, RecError, State, camera_json, config, dir_name, lock};
use crate::sound::{SoundHub, SoundSettings, SoundTap};
use recordings::index::{EventSource, IndexedEvent};
use recordings::lane::LaneEvent;
use recordings::sound::{EnvelopeConfig, EnvelopeStore, MAX_BUCKETS};
use recordings::{DeviceCamera, Secs};
use serde_json::{Value, json};
use sound_events::{AlarmKind, SecondLevels};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::warn;

/// How often changed loudness days are written.
pub(super) const FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// The analyzer name of the core's own alarm detector.
pub const PATTERN_ANALYZER: &str = "pattern-v1";

/// Most labels one day of one camera keeps (section 2a).
pub const MAX_LABELS_PER_DAY: usize = 2000;

/// Most labels in one `nv_sound_labels_put`.
const MAX_LABELS_PER_PUT: usize = 20_000;

/// Longest range one `nv_sound_labels_put` may cover, in days.
const MAX_PUT_DAYS: i64 = 31;

/// The ten kinds the apps map their classifiers' labels onto (section 2a).
/// Others pass through.
pub const SOUND_KINDS: [&str; 10] = [
    "smoke_alarm",
    "co_alarm",
    "siren",
    "car_alarm",
    "glass_break",
    "dog_bark",
    "baby_cry",
    "shout",
    "knock",
    "doorbell",
];

/// Largest answer of `recordings_loudness`, in values (two per bucket).
const MAX_ANSWER_VALUES: usize = 2_000_000;

/// One device's store and the offset its seconds were converted with.
struct Env {
    store: EnvelopeStore,
    offset: i64,
}

/// The sound state of the recordings owner.
#[derive(Default)]
pub(super) struct SoundOwner {
    envelopes: Mutex<HashMap<String, Env>>,
    /// Alarm episodes in progress: (device, channel, kind) to the Unix
    /// second the episode started.
    open: Mutex<HashMap<(u64, u8, AlarmKind), i64>>,
    flusher: Mutex<Option<JoinHandle<()>>>,
    hub: OnceLock<Arc<SoundHub>>,
    next_listener: AtomicU64,
    pub(super) work: sound_work::Work,
}

impl SoundOwner {
    pub(super) fn sound_hub(&self) -> Option<&Arc<SoundHub>> {
        self.hub.get()
    }

    /// A tile id no tile has, for a headless listener.
    pub(super) fn listener_id(&self) -> u64 {
        (1 << 62) + self.next_listener.fetch_add(1, Ordering::Relaxed)
    }
}

impl crate::recordings_owner::RecordingsHub {
    /// Connects the owner to the core's sound analysis: the headless
    /// listeners use its hub, and its seconds and alarms come here.
    pub(crate) fn attach_sound(&self, sound: Arc<SoundHub>) {
        let h = &self.hub;
        let _ = h.sound.hub.set(sound.clone());
        sound.set_tap(Arc::new(Tap(Arc::downgrade(h))));
    }
}

/// What the tiles' analysis calls; holds the owner weakly (it is held by
/// the core's sound hub, which the owner's tasks also hold).
struct Tap(Weak<Hub>);

impl SoundTap for Tap {
    fn second(&self, device: u64, channel: u8, levels: &SecondLevels) {
        if let Some(h) = self.0.upgrade() {
            h.sound_second(device, channel, levels);
        }
    }

    fn alarm(&self, device: u64, channel: u8, kind: AlarmKind, start: bool, at_ns: u64) {
        if let Some(h) = self.0.upgrade() {
            h.sound_alarm(device, channel, kind, start, at_ns);
        }
    }
}

/// What the tap needs to know about a device.
pub(super) struct Target {
    key: String,
    offset: i64,
    dir: Option<PathBuf>,
    cfg: EnvelopeConfig,
}

impl Target {
    pub(super) fn new(
        key: String,
        offset: i64,
        dir: Option<PathBuf>,
        cfg: EnvelopeConfig,
    ) -> Target {
        Target {
            key,
            offset,
            dir,
            cfg,
        }
    }
}

impl Hub {
    fn sound_target(&self, device: u64) -> Option<Target> {
        let st = lock(&self.state);
        let stores = st.stores.as_ref()?;
        let key = st
            .sessions
            .iter()
            .find(|(_, a)| a.id == device)
            .map(|(k, _)| k.clone())?;
        let dev = st.devices.get(&key)?;
        Some(Target {
            offset: dev.enable.utc_offset_s,
            dir: self
                .data_dir
                .is_some()
                .then(|| stores.root.join("sound").join(dir_name(&key))),
            cfg: stores.settings.index.into(),
            key,
        })
    }

    /// Runs `f` on the device's envelope store, opening it first.
    pub(super) fn with_envelope<R>(
        self: &Arc<Self>,
        t: &Target,
        f: impl FnOnce(&mut EnvelopeStore) -> R,
    ) -> R {
        let mut envs = lock(&self.sound.envelopes);
        if !envs.contains_key(&t.key) {
            let store = match &t.dir {
                Some(d) => EnvelopeStore::open(d, t.cfg).unwrap_or_else(|e| {
                    warn!(error = %e, "cannot open the loudness store; keeping it in memory");
                    EnvelopeStore::in_memory(t.cfg)
                }),
                None => EnvelopeStore::in_memory(t.cfg),
            };
            envs.insert(
                t.key.clone(),
                Env {
                    store,
                    offset: t.offset,
                },
            );
            drop(envs);
            self.ensure_flusher();
            envs = lock(&self.sound.envelopes);
        }
        let env = envs.get_mut(&t.key).expect("inserted");
        env.offset = t.offset;
        if env.store.config() != t.cfg {
            env.store.set_config(t.cfg);
        }
        f(&mut env.store)
    }

    fn ensure_flusher(self: &Arc<Self>) {
        let mut f = lock(&self.sound.flusher);
        if f.is_some() || self.closing.load(Ordering::SeqCst) {
            return;
        }
        let weak = Arc::downgrade(self);
        *f = Some(self.rt.spawn(async move {
            loop {
                tokio::time::sleep(FLUSH_INTERVAL).await;
                let Some(h) = weak.upgrade() else {
                    return;
                };
                h.sound_flush();
            }
        }));
    }

    /// Writes changed loudness days and applies the limits.
    pub(super) fn sound_flush(&self) {
        let wall = self.clock.wall_now_us().div_euclid(1_000_000);
        for env in lock(&self.sound.envelopes).values_mut() {
            if let Err(e) = env.store.flush(wall + env.offset) {
                warn!(error = %e, "cannot write the loudness store");
            }
        }
    }

    /// One second of a camera's loudness from a live tile.
    fn sound_second(self: &Arc<Self>, device: u64, channel: u8, s: &SecondLevels) {
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        let Some(t) = self.sound_target(device) else {
            return;
        };
        let second = s.second as i64 + t.offset;
        let (peak, rms) = (s.peak, s.rms);
        lock(&self.sound.work.no_audio).camera_heard(&t.key, channel);
        self.with_envelope(&t, |store| {
            store.put(DeviceCamera::first(channel), second, peak, rms);
        });
    }

    /// An alarm episode of a live channel started or ended.
    fn sound_alarm(
        self: &Arc<Self>,
        device: u64,
        channel: u8,
        kind: AlarmKind,
        start: bool,
        at_ns: u64,
    ) {
        if self.closing.load(Ordering::SeqCst) {
            return;
        }
        let ago_us = (self.clock.now_ns().saturating_sub(at_ns) / 1000) as i64;
        let unix = (self.clock.wall_now_us() - ago_us).div_euclid(1_000_000);
        let key = (device, channel, kind);
        let from = {
            let mut open = lock(&self.sound.open);
            if start {
                open.insert(key, unix);
                unix
            } else {
                open.remove(&key).unwrap_or(unix)
            }
        };
        let Some(t) = self.sound_target(device) else {
            return;
        };
        let label = IndexedEvent {
            kind: kind.as_str().to_owned(),
            start: from + t.offset,
            end: unix.max(from) + t.offset,
            ..IndexedEvent::default()
        };
        let range = (label.start, label.start);
        // A device without an index (not enabled) keeps no labels.
        let _ = self.put_labels(
            &t.key,
            DeviceCamera::first(channel),
            PATTERN_ANALYZER,
            range,
            vec![label],
        );
    }

    /// Stores labels in the device's index (an index must exist) and
    /// reports the days that changed like a fill does.
    pub(super) fn put_labels(
        &self,
        key: &str,
        camera: DeviceCamera,
        analyzer: &str,
        range: (Secs, Secs),
        labels: Vec<IndexedEvent>,
    ) -> Result<usize, RecError> {
        let index = {
            let st = lock(&self.state);
            st.devices
                .get(key)
                .map(|d| d.index.clone())
                .ok_or(RecError::InvalidArgument)?
        };
        let (days, revision) = {
            let mut idx = lock(&index);
            let days = idx
                .put_labels(
                    camera,
                    analyzer,
                    range.0,
                    range.1,
                    labels,
                    MAX_LABELS_PER_DAY,
                )
                .map_err(|e| {
                    warn!(error = %e, "cannot store sound labels");
                    RecError::Internal
                })?;
            (days, idx.revision())
        };
        for day in &days {
            self.on_lane_event(
                key,
                0,
                LaneEvent::DayIndexed {
                    camera,
                    day: *day,
                    revision,
                },
            );
        }
        Ok(days.len())
    }

    /// The device was added: starts its listeners that were waiting.
    pub(super) fn sound_device_added(self: &Arc<Self>, key: &str) {
        sound_work::device_added(self, key);
    }

    /// The device is going away: stops its listeners and writes what is
    /// held.
    pub(super) fn sound_device_removed(&self, key: &str, id: u64) {
        sound_work::device_removed(self, key, id);
        if let Some(env) = lock(&self.sound.envelopes).get_mut(key) {
            let wall = self.clock.wall_now_us().div_euclid(1_000_000);
            if let Err(e) = env.store.flush(wall + env.offset) {
                warn!(error = %e, "cannot write the loudness store");
            }
        }
    }

    /// The core is closing: listeners and analyses stop, loudness is
    /// written.
    pub(super) fn sound_shutdown(&self) {
        sound_work::shutdown(self);
        if let Some(f) = lock(&self.sound.flusher).take() {
            f.abort();
        }
        self.sound_flush();
    }
}

/// A kind name: lowercase ASCII letters, digits, and underscores.
fn kind_name(v: &Value) -> Result<String, RecError> {
    let k = v.as_str().ok_or(RecError::InvalidArgument)?.to_lowercase();
    let ok = !k.is_empty()
        && k.len() <= 64
        && k.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(k)
    } else {
        Err(RecError::InvalidArgument)
    }
}

fn opt_text(
    m: &serde_json::Map<String, Value>,
    key: &str,
    max: usize,
) -> Result<Option<String>, RecError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() && s.chars().count() <= max => Ok(Some(s.clone())),
        Some(_) => Err(RecError::InvalidArgument),
    }
}

impl crate::Core {
    /// `nv_sound_configure`: live sound settings, JSON (unknown members
    /// ignored): `{"alarms": true, "loudness_history_s": 600}`. Members
    /// left out keep their values. Applies to tiles opened afterwards
    /// (and listeners started afterwards). With `loudness_history_s` 0 no
    /// loudness is measured, so nothing reaches the envelope store.
    ///
    /// # Errors
    /// [`RecError::InvalidArgument`] for invalid JSON or values (nothing
    /// changes), [`RecError::Closed`].
    pub fn sound_configure(&self, json: &str) -> Result<(), RecError> {
        let h = self.rec();
        h.check()?;
        let m = config::request(json)?;
        let alarms = match m.get("alarms") {
            None | Some(Value::Null) => None,
            Some(Value::Bool(b)) => Some(*b),
            Some(_) => return Err(RecError::InvalidArgument),
        };
        let history = match m.get("loudness_history_s") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .filter(|n| *n <= u64::from(crate::sound::MAX_LOUDNESS_HISTORY_S))
                    .ok_or(RecError::InvalidArgument)? as u32,
            ),
        };
        self.tile_ctx.sound.configure(|s: &mut SoundSettings| {
            if let Some(a) = alarms {
                s.alarms = a;
            }
            if let Some(n) = history {
                s.loudness_history_s = n;
            }
        });
        Ok(())
    }

    /// `nv_recordings_loudness`: loudness lanes for the timeline, answered
    /// from memory (a day not held is read from its file):
    /// `{"start": S, "end": E, "bucket_s": 60, "cameras": [{"device", "channel"}]}`
    /// gives `{"revision": N, "lanes": [{"camera", "bucket_s", "start",
    /// "peak_db": [...], "rms_db": [...]}]}`, whole dBFS, `null` where no
    /// audio was analyzed. `start` and `end` are the device's local
    /// seconds like the other queries; `bucket_s` is 1 to 3600 (default
    /// 60); `cameras` left out means every camera with loudness. A camera
    /// of a device that is not enabled gets a lane of `null`s.
    ///
    /// With `"gaps": true` each lane of an enabled device also has
    /// `"gaps": [{"start", "end", "reason", "analyzable"}]`: the runs of
    /// empty buckets (inclusive device-local seconds, clipped to the
    /// query), each with what the core knows about why. `reason` is
    /// `not_recorded` (the device has no recording of the camera there),
    /// `no_audio` (the camera's live stream or the downloaded video of
    /// that span was read and has no sound), or `not_analyzed` (recorded,
    /// sound not measured yet); `analyzable` is true when a finished
    /// download covers the gap, so `sound_analyze_day` with its `start` and
    /// `end` can measure it now. See [`split_gap`].
    ///
    /// # Errors
    /// [`RecError::InvalidArgument`], [`RecError::Limit`] for more than
    /// 100000 buckets in a lane or an answer over 2 million values,
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn recordings_loudness(&self, query: &str) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let m = config::request(query)?;
        let (start, end) = (config::secs(&m, "start")?, config::secs(&m, "end")?);
        let bucket_s = match m.get("bucket_s") {
            None | Some(Value::Null) => 60,
            Some(v) => v
                .as_u64()
                .filter(|b| (1..=3600).contains(b))
                .ok_or(RecError::InvalidArgument)? as u32,
        };
        if end < start {
            return Err(RecError::InvalidArgument);
        }
        let buckets = ((end - start) / i64::from(bucket_s) + 1) as usize;
        if buckets > MAX_BUCKETS {
            return Err(RecError::Limit);
        }
        let cams = config::cameras(&m)?;
        let want_gaps = match m.get("gaps") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => return Err(RecError::InvalidArgument),
        };
        let st = lock(&h.state);
        let stores = st.stores.as_ref().ok_or(RecError::NotConfigured)?;
        let cfg: EnvelopeConfig = stores.settings.index.into();
        let persistent = h.data_dir.is_some();
        let mut envs = lock(&h.sound.envelopes);
        // Which cameras: those asked for, else every camera with loudness.
        let mut keys: Vec<&String> = st.devices.keys().collect();
        keys.sort();
        let mut wanted: Vec<(String, DeviceCamera)> = cams;
        if wanted.is_empty() {
            for key in &keys {
                open_envelope(&mut envs, &st, key, persistent, cfg);
                if let Some(e) = envs.get(*key) {
                    wanted.extend(e.store.cameras().into_iter().map(|c| ((*key).clone(), c)));
                }
            }
        }
        if wanted.len() * buckets * 2 > MAX_ANSWER_VALUES {
            return Err(RecError::Limit);
        }
        let mut revision = 0u64;
        let mut raw = Vec::new();
        for (key, cam) in wanted {
            open_envelope(&mut envs, &st, &key, persistent, cfg);
            let lane = match envs.get_mut(&key) {
                Some(e) => {
                    revision += e.store.revision();
                    e.store.lane(cam, start, end, bucket_s)
                }
                None => EnvelopeStore::in_memory(cfg).lane(cam, start, end, bucket_s),
            }
            .ok_or(RecError::Limit)?;
            raw.push((key, cam, lane));
        }
        drop(envs);
        drop(st);
        let mut lanes = Vec::new();
        for (key, cam, lane) in raw {
            let mut v = json!({
                "camera": camera_json(&key, cam), "bucket_s": lane.bucket_s,
                "start": lane.start, "peak_db": lane.peak_db, "rms_db": lane.rms_db,
            });
            if want_gaps && let Some(g) = lane_gaps(h, &key, cam, &lane, end) {
                v["gaps"] = g;
            }
            lanes.push(v);
        }
        Ok(json!({"revision": revision, "lanes": lanes}).to_string())
    }

    /// `nv_sound_labels_put`: stores the labels an app's classifier found
    /// for one camera and a time range, as events of source `local_audio`:
    /// `{"camera", "analyzer", "start", "end", "labels": [{"kind", "start",
    /// "end", "confidence", "label"}]}`. The analyzer's earlier labels
    /// starting within the range are replaced, so analyzing again does not
    /// duplicate; labels starting outside it are ignored; each camera-day
    /// keeps at most 2000 (the lowest confidence goes first). `kind` is
    /// lowercase letters, digits, and underscores (unknown kinds pass
    /// through; [`SOUND_KINDS`] are the ones the apps map onto), `label` is
    /// the classifier's own name, `confidence` 0 to 1. The analyzer name
    /// `pattern-v1` is the core's. The camera's device must be enabled for
    /// recordings. Returns how many days of the index changed;
    /// `recordings_changed` and `incidents_changed` follow like for a fill.
    ///
    /// # Errors
    /// [`RecError::InvalidArgument`] (also for a device not enabled),
    /// [`RecError::Limit`] for more than 20000 labels or a range over 31
    /// days, [`RecError::NotConfigured`], [`RecError::Internal`],
    /// [`RecError::Closed`].
    pub fn sound_labels_put(&self, json: &str) -> Result<usize, RecError> {
        let h = self.rec();
        h.check()?;
        let m = config::request(json)?;
        let (key, camera) = config::camera(m.get("camera").ok_or(RecError::InvalidArgument)?)?;
        let analyzer = opt_text(&m, "analyzer", 64)?.ok_or(RecError::InvalidArgument)?;
        if analyzer == PATTERN_ANALYZER {
            return Err(RecError::InvalidArgument);
        }
        let (start, end) = (config::secs(&m, "start")?, config::secs(&m, "end")?);
        if end < start {
            return Err(RecError::InvalidArgument);
        }
        if end - start >= MAX_PUT_DAYS * 86_400 {
            return Err(RecError::Limit);
        }
        let list = match m.get("labels") {
            Some(Value::Array(a)) => a,
            _ => return Err(RecError::InvalidArgument),
        };
        if list.len() > MAX_LABELS_PER_PUT {
            return Err(RecError::Limit);
        }
        let mut labels = Vec::with_capacity(list.len());
        for l in list {
            let o = l.as_object().ok_or(RecError::InvalidArgument)?;
            let kind = kind_name(o.get("kind").ok_or(RecError::InvalidArgument)?)?;
            let (ls, le) = (config::secs(o, "start")?, config::secs(o, "end")?);
            let confidence_pm = match o.get("confidence") {
                None | Some(Value::Null) => None,
                Some(v) => {
                    let c = v
                        .as_f64()
                        .filter(|c| (0.0..=1.0).contains(c))
                        .ok_or(RecError::InvalidArgument)?;
                    Some((c * 1000.0).round() as u16)
                }
            };
            labels.push(IndexedEvent {
                kind,
                start: ls,
                end: le.max(ls),
                source: EventSource::LocalAudio,
                label: opt_text(o, "label", 128)?,
                confidence_pm,
                ..IndexedEvent::default()
            });
        }
        {
            let st = lock(&h.state);
            if st.stores.is_none() {
                return Err(RecError::NotConfigured);
            }
            if !st.devices.contains_key(&key) {
                return Err(RecError::InvalidArgument);
            }
        }
        h.put_labels(&key, camera, &analyzer, (start, end), labels)
    }

    /// `nv_sound_watch`: adds a camera to (or removes it from) the "always
    /// listen for sound" list (Decision 12). `camera` is
    /// `{"device","channel","lens"}`. While the camera is not on screen, the
    /// core keeps its sub-stream open without a tile or video decoding and
    /// analyzes its audio (envelope, alarms) like a tile's; while a tile
    /// shows the channel the tile's audio does the work and the listener
    /// stands by. The device may be added later (the listener then
    /// starts). `sound_watch` events report `watching`, `stopped`, or
    /// `refused` (`reason`: `no_audio` after 30 s of video without audio,
    /// `device_closed` when the session ended, `sound_off` when the sound
    /// settings switch the analysis off). At most 64 cameras.
    ///
    /// # Errors
    /// [`RecError::InvalidArgument`], [`RecError::Limit`],
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn sound_watch(&self, camera: &str, enabled: bool) -> Result<(), RecError> {
        let h = self.rec();
        h.check()?;
        let (key, cam) = config::camera_text(camera)?;
        if lock(&h.state).stores.is_none() {
            return Err(RecError::NotConfigured);
        }
        sound_work::watch(h, &key, cam, enabled)
    }

    /// Asks for the sound of one camera's day to be analyzed (Decision 13:
    /// only on request, never in the background): `{"camera": {...},
    /// "day": S}` (any second of the day; device-local), with optional
    /// `"start"` and `"end"` (both, inclusive, inside the day) to analyze
    /// only that span (a loudness gap): the source is read no further than
    /// `end`, and only that span's seconds and alarm labels are stored. Returns
    /// `{"request": N, "state": "queued"}`, or the existing request for the
    /// same camera-day. At most one analysis runs per device at a time (one
    /// stream per device); `sound_analyze` events report `running`, `done`
    /// (`seconds`, `labels`), `failed` (`reason`), or `canceled`. The audio
    /// comes from the source set with [`crate::Core::set_sound_day_source`] or,
    /// when none is set, from the camera's finished downloads covering the
    /// day (`no_audio` when none does, `no_source` when no downloads folder
    /// is configured). The result is the
    /// day's loudness in the envelope store and the core's alarm labels
    /// (`pattern-v1`) for the audio analyzed; labels from the app's
    /// classifier come with [`crate::Core::sound_labels_put`]. At most 32
    /// requests are held.
    ///
    /// # Errors
    /// [`RecError::InvalidArgument`] (also for a camera not enabled),
    /// [`RecError::Limit`], [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn sound_analyze_day(&self, json: &str) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let m = config::request(json)?;
        let (key, cam) = config::camera(m.get("camera").ok_or(RecError::InvalidArgument)?)?;
        let day = recordings::day_start(config::secs(&m, "day")?);
        let span = match (m.get("start"), m.get("end")) {
            (None | Some(Value::Null), None | Some(Value::Null)) => None,
            (Some(_), Some(_)) => {
                let (s, e) = (config::secs(&m, "start")?, config::secs(&m, "end")?);
                if e < s || s < day || e >= day + 86_400 {
                    return Err(RecError::InvalidArgument);
                }
                Some((s, e))
            }
            _ => return Err(RecError::InvalidArgument),
        };
        {
            let st = lock(&h.state);
            check_camera(&st, &key, cam)?;
        }
        sound_work::analyze_day(h, &key, cam, day, span)
    }

    /// Cancels an analysis request that is queued or running; a running
    /// one stops at its next chunk and stores nothing. Unknown or finished
    /// requests are [`RecError::UnknownId`].
    ///
    /// # Errors
    /// [`RecError::UnknownId`], [`RecError::Closed`].
    pub fn sound_analyze_cancel(&self, request: u64) -> Result<(), RecError> {
        let h = self.rec();
        h.check()?;
        sound_work::cancel(h, request)
    }

    /// Sets (or clears) where recorded days' audio comes from for
    /// [`crate::Core::sound_analyze_day`], replacing the default (finished
    /// downloads). This is the seam for other sources: see [`DayAudioSource`].
    ///
    /// # Errors
    /// [`RecError::Closed`].
    pub fn set_sound_day_source(
        &self,
        source: Option<Arc<dyn DayAudioSource>>,
    ) -> Result<(), RecError> {
        let h = self.rec();
        h.check()?;
        *lock(&h.sound.work.source) = source;
        Ok(())
    }
}

/// Why a stretch of a loudness lane is empty (`recordings_loudness` with
/// `gaps`). Inclusive device-local seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Gap {
    pub start: Secs,
    pub end: Secs,
    pub reason: &'static str,
    pub analyzable: bool,
}

/// `a` split by the spans `cut`: the parts inside them, and the parts left.
type Spans = Vec<(Secs, Secs)>;

fn partition(a: (Secs, Secs), cut: &[(Secs, Secs)]) -> (Spans, Spans) {
    let (mut inside, mut outside) = (Vec::new(), Vec::new());
    let mut from = a.0;
    let mut spans: Vec<(Secs, Secs)> = cut.to_vec();
    spans.sort_unstable();
    for (s, e) in spans {
        if e < from || s > a.1 {
            continue;
        }
        if s > from {
            outside.push((from, s - 1));
        }
        let (is, ie) = (s.max(from), e.min(a.1));
        inside.push((is, ie));
        from = ie + 1;
        if from > a.1 {
            break;
        }
    }
    if from <= a.1 {
        outside.push((from, a.1));
    }
    (inside, outside)
}

/// Splits an empty stretch `a` of a loudness lane by what is known about
/// it: outside every recording it is `not_recorded`; inside one it is
/// `no_audio` when the camera, or measurement of that span, says it has no
/// sound, else `not_analyzed` (`analyzable` where a finished download
/// covers it). Results are in time order.
pub(super) fn split_gap(
    a: (Secs, Secs),
    recorded: &[(Secs, Secs)],
    camera_no_audio: bool,
    no_audio: &[(Secs, Secs)],
    downloaded: &[(Secs, Secs)],
) -> Vec<Gap> {
    let mut out: Vec<Gap> = Vec::new();
    let mut push = |(start, end): (Secs, Secs), reason: &'static str, analyzable: bool| {
        out.push(Gap {
            start,
            end,
            reason,
            analyzable,
        });
    };
    let (rec, unrec) = partition(a, recorded);
    for p in unrec {
        push(p, "not_recorded", false);
    }
    for r in rec {
        let (none, rest) = if camera_no_audio {
            (vec![r], Vec::new())
        } else {
            partition(r, no_audio)
        };
        for p in none {
            push(p, "no_audio", false);
        }
        for p in rest {
            let (have, lack) = partition(p, downloaded);
            for q in have {
                push(q, "not_analyzed", true);
            }
            for q in lack {
                push(q, "not_analyzed", false);
            }
        }
    }
    out.sort_by_key(|g| g.start);
    // Neighbors with the same meaning are one gap.
    let mut merged: Vec<Gap> = Vec::new();
    for g in out {
        match merged.last_mut() {
            Some(l)
                if l.reason == g.reason && l.analyzable == g.analyzable && g.start <= l.end + 1 =>
            {
                l.end = l.end.max(g.end);
            }
            _ => merged.push(g),
        }
    }
    merged
}

/// The gaps of one lane as JSON; `None` for a device that is not enabled
/// (nothing is known about its recordings).
fn lane_gaps(
    h: &Arc<Hub>,
    key: &str,
    cam: DeviceCamera,
    lane: &recordings::sound::Lane,
    end: Secs,
) -> Option<Value> {
    let start = lane.start;
    let recorded: Vec<(Secs, Secs)> = {
        let st = lock(&h.state);
        let dev = st.devices.get(key)?;
        let idx = lock(&dev.index);
        idx.files(cam, start, end)
            .iter()
            .map(|f| (f.start, f.end))
            .collect()
    };
    let (camera_no_audio, no_audio) = lock(&h.sound.work.no_audio).of(key, cam.channel);
    // Downloaded video, by day (at most a month of days is looked at).
    let mut downloaded: Vec<(Secs, Secs)> = Vec::new();
    let mut day = recordings::day_start(start);
    let mut days = 0;
    while day <= end && days < 31 {
        for f in super::day_audio::files_for_day(h, key, cam, day).unwrap_or_default() {
            downloaded.push((f.start_ms.div_euclid(1000), f.end_ms.div_euclid(1000)));
        }
        day += 86_400;
        days += 1;
    }
    let b = i64::from(lane.bucket_s);
    let mut gaps: Vec<Gap> = Vec::new();
    let mut i = 0;
    while i < lane.peak_db.len() {
        if lane.peak_db[i].is_some() {
            i += 1;
            continue;
        }
        let from = i;
        while i < lane.peak_db.len() && lane.peak_db[i].is_none() {
            i += 1;
        }
        let a = (start + from as i64 * b, (start + i as i64 * b - 1).min(end));
        gaps.extend(split_gap(
            a,
            &recorded,
            camera_no_audio,
            &no_audio,
            &downloaded,
        ));
    }
    Some(Value::Array(
        gaps.iter()
            .map(|g| {
                json!({"start": g.start, "end": g.end, "reason": g.reason,
                       "analyzable": g.analyzable})
            })
            .collect(),
    ))
}

/// Checks a camera is on an enabled device and allowed by its channel
/// list.
pub(super) fn check_camera(st: &State, key: &str, cam: DeviceCamera) -> Result<(), RecError> {
    if st.stores.is_none() {
        return Err(RecError::NotConfigured);
    }
    let dev = st.devices.get(key).ok_or(RecError::InvalidArgument)?;
    let session = st.sessions.get(key).map(|a| &a.session);
    if dev.allows(cam, session) {
        Ok(())
    } else {
        Err(RecError::InvalidArgument)
    }
}

/// Makes sure `key`'s envelope store is loaded if its device is enabled
/// (for queries; the live path opens it itself).
fn open_envelope(
    envs: &mut HashMap<String, Env>,
    st: &State,
    key: &str,
    persistent: bool,
    cfg: EnvelopeConfig,
) {
    if envs.contains_key(key) {
        return;
    }
    let Some(dev) = st.devices.get(key) else {
        return;
    };
    let Some(stores) = st.stores.as_ref() else {
        return;
    };
    let store = if persistent {
        EnvelopeStore::open(&stores.root.join("sound").join(dir_name(key)), cfg).ok()
    } else {
        None
    }
    .unwrap_or_else(|| EnvelopeStore::in_memory(cfg));
    envs.insert(
        key.to_owned(),
        Env {
            store,
            offset: dev.enable.utc_offset_s,
        },
    );
}

#[cfg(test)]
mod gap_tests {
    use super::*;

    fn g(start: Secs, end: Secs, reason: &'static str, analyzable: bool) -> Gap {
        Gap {
            start,
            end,
            reason,
            analyzable,
        }
    }

    #[test]
    fn a_gap_is_split_by_what_is_known() {
        // Recorded 100..=299 and 400..=499; a download covers 150..=199;
        // 250..=299 is known to have no sound.
        let got = split_gap(
            (50, 549),
            &[(100, 299), (400, 499)],
            false,
            &[(250, 299)],
            &[(150, 199)],
        );
        assert_eq!(
            got,
            vec![
                g(50, 99, "not_recorded", false),
                g(100, 149, "not_analyzed", false),
                g(150, 199, "not_analyzed", true),
                g(200, 249, "not_analyzed", false),
                g(250, 299, "no_audio", false),
                g(300, 399, "not_recorded", false),
                g(400, 499, "not_analyzed", false),
                g(500, 549, "not_recorded", false),
            ]
        );
    }

    #[test]
    fn a_camera_without_sound_is_no_audio_wherever_it_recorded() {
        let got = split_gap((0, 99), &[(10, 59)], true, &[], &[(10, 59)]);
        assert_eq!(
            got,
            vec![
                g(0, 9, "not_recorded", false),
                g(10, 59, "no_audio", false),
                g(60, 99, "not_recorded", false),
            ]
        );
    }

    #[test]
    fn no_recordings_at_all_is_one_not_recorded_gap() {
        assert_eq!(
            split_gap((0, 9), &[], false, &[], &[]),
            vec![g(0, 9, "not_recorded", false)]
        );
    }

    #[test]
    fn touching_files_make_one_gap() {
        let got = split_gap((0, 99), &[(0, 49), (50, 99)], false, &[], &[]);
        assert_eq!(got, vec![g(0, 99, "not_analyzed", false)]);
    }
}
