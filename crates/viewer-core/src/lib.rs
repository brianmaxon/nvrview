//! The nvrview core behind the C interface (`docs/FFI.md`).
//!
//! A [`Core`] owns a tokio runtime, one [`bc_session::DeviceSession`] per
//! device, and one tile per displayed stream. Each tile has a subscription,
//! a [`video_pipeline::TilePipeline`], and its own delivery thread that calls
//! [`Callbacks::on_action`]; control events go to [`Callbacks::on_event`] as
//! small JSON objects from a single event thread.
//!
//! Guarantees (the Rust side of `docs/FFI.md`):
//!
//! - **Threads.** `on_action` is serialized per tile and `on_event` across
//!   the core, all on threads the core owns. A slow `on_action` delays only
//!   its own tile: media reaches each tile through a bounded queue that
//!   drops to the next keyframe, and the session never waits for a tile.
//! - **Results.** [`Core::decoded`] and [`Core::decode_failed`] may be called
//!   from any thread, including inside `on_action`, and never wait for a
//!   tile.
//! - **Ends.** After [`Core::close_tile`] returns, no `on_action` for that
//!   tile is running or will run, and its preview is being stopped. After
//!   [`Core::remove_device`] returns, the device's tiles are closed and its
//!   socket is shut. After [`Core::shutdown`] (or dropping the core) returns,
//!   no callback runs, every session is closed, and every core thread has
//!   been joined.
//! - **IDs first.** No event names a device, and no action or event names a
//!   tile, until its ID is published: at the end of [`Core::add_device`] and
//!   [`Core::open_tile`], or by [`Pending::publish`] for the `*_deferred`
//!   variants the C interface uses (it publishes after writing the ID out).
//!   Opening a tile never waits on any tile's `on_action`.
//! - **`tile_closed`** is sent exactly once for every tile that ends, after
//!   its last `on_action` returned: `closed`, `device_removed`,
//!   `login_failed` (its session ended; the tile ends on its own), or
//!   `core_shutdown` (delivered before [`Core::shutdown`] returns).
//! - **Reentrancy.** From inside a callback only `decoded` and
//!   `decode_failed` are allowed; anything else returns
//!   [`CoreError::InvalidArgument`] instead of deadlocking.
//! - **Talk-back** ([`Core::talk_start`]) is camera motion control: refused
//!   with [`CoreError::NotPermitted`] unless the app enabled the channel
//!   ([`Core::set_talk_permission`], off by default); the device session
//!   rate-limits, paces, logs, and closes it on every exit path.
//! - **Camera motion** ([`Core::ptz_move`] and its siblings) names its
//!   origin: the app's own controls ([`MotionOrigin::Manual`]) need no
//!   enable; automation needs a per-camera grant
//!   ([`Core::set_motion_grant`]). The device session rate-limits, logs,
//!   and stops every move on every path; a tile that started a move stops
//!   it when it closes.
//! - **Logging.** With a log directory, the core installs `viewer-log`
//!   (rotation capped by file count and total size, rate-limited). Passwords
//!   are never logged; addresses only at debug level.

mod alerts_owner;
pub mod audio;
mod capture_owner;
pub mod clock;
mod device;
pub mod discover;
mod events;
mod json;
mod lens;
mod live_pacer;
mod playback;
pub mod ptz;
mod recordings_owner;
pub mod sound;
pub mod store;
pub mod talk;
mod tile;
pub mod update;

pub use alerts_owner::{
    AlertError, AlertFrames, AlertInput, AlertNotice, AlertOutput, DeliveryDone, ExternalTrigger,
    ExternalTriggers, InputContext, InputFactory, NoticeState, OutputContext, OutputFactory,
    RecordingOutput, Secret, TestDone, TestResult, TriggerRejected, home_assistant_factory,
    mqtt_factory,
};
pub use bc_protocol::ptz::{Direction as PtzDirection, LensAxis};
pub use bc_protocol::stream::StreamTier;
pub use bc_session::{BackoffConfig, Connector, MotionOrigin};
pub use capture::LimitKind;
pub use capture_owner::{
    AudioEncoder, CaptureError, EncoderOpened, FINALIZE_WAIT, FreeSpace, SystemFreeSpace,
};
pub use playback::{MAX_CAMERAS, MAX_PLAYBACKS, PlaybackPending, PlaybackState, SeekMode};
pub use recordings_owner::{
    AudioChunk, DayAudioSource, MAX_LABELS_PER_DAY, PATTERN_ANALYZER, RecError, SOUND_KINDS,
    SourceError, ThumbKind, Thumbnail, ThumbnailAnswer,
};
pub use video_pipeline::{
    Action, ActionKind, Codec, ColorMatrix, DecodeFailureKind, PipelineConfig, TileStatus,
};

/// Grid layout, re-exported for the C interface's `nv_grid_layout`.
pub mod layout {
    pub use video_pipeline::layout::*;
    pub use video_pipeline::{Rect, Size};
}

use clock::HostClock;
use device::{DeviceHandle, HostConnector};
use events::{EventThread, Priority};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tile::{
    CloseReason, DecoderResult, ResultKind, ResultSink, TileCommand, TileContext, TileHandle,
    TileStart,
};
use tracing::{debug, info, warn};

/// What the app receives. Both methods run on threads the core owns and must
/// return quickly.
pub trait Callbacks: Send + Sync + 'static {
    /// One action for a tile's decoder or view, serialized per tile. Data in
    /// `action` is borrowed for the duration of the call. `generation`
    /// names the tile's stream it belongs to (0 when opened, one more for
    /// each tier switch, [`Core::set_tile_tier`]); answer with the same
    /// generation.
    fn on_action(&self, tile: u64, generation: u32, action: &Action<'_>);
    /// One JSON object, serialized across the core.
    fn on_event(&self, json: &str);
    /// One block of a tapped tile's audio ([`Core::set_tile_audio_tap`]):
    /// 16 kHz mono with its timestamps. Serialized across the core on the
    /// tap thread; the block is borrowed for the duration of the call.
    fn on_audio_tap(&self, tile: u64, block: &audio::TapBlock) {
        let _ = (tile, block);
    }
}

/// Why a call failed. The values match `NvStatus` in the C interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CoreError {
    /// An argument is invalid, or the call was made from inside a callback.
    InvalidArgument,
    /// The device or tile does not exist (any more).
    UnknownId,
    /// The core is shutting down.
    Closed,
    /// Too many devices or tiles.
    Limit,
    /// Something unexpected failed (a thread could not start).
    Internal,
    /// Camera motion control (talk-back) is not enabled for that camera.
    NotPermitted,
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CoreError::InvalidArgument => "invalid argument",
            CoreError::UnknownId => "unknown device or tile",
            CoreError::Closed => "the core is shutting down",
            CoreError::Limit => "too many devices or tiles",
            CoreError::Internal => "internal error",
            CoreError::NotPermitted => "not enabled for this camera",
        })
    }
}

impl std::error::Error for CoreError {}

/// File logging settings.
#[derive(Clone, Debug)]
pub struct LogSettings {
    /// Directory for rotated log files (created if missing).
    pub dir: PathBuf,
    /// Most files kept (0 = the `viewer-log` default).
    pub max_files: usize,
    /// Most bytes across all files (0 = the `viewer-log` default).
    pub max_total_bytes: u64,
}

/// Core settings. [`Default`] gives the production values.
#[derive(Clone, Debug)]
pub struct CoreConfig {
    /// File logging; `None` logs nothing to files.
    pub log: Option<LogSettings>,
    /// The app's data directory, where the core keeps layouts and device
    /// keys ([`store`]); `None` keeps them in memory only.
    pub data_dir: Option<PathBuf>,
    /// Pipeline tunables for every tile.
    pub pipeline: PipelineConfig,
    /// Reconnect backoff for every device.
    pub backoff: BackoffConfig,
    /// Capacity, in frames, of each tile's media queue.
    pub queue_frames: usize,
    /// Most devices at once.
    pub max_devices: usize,
    /// Most tiles at once, across all devices.
    pub max_tiles: usize,
    /// How often each tile's pipeline clock advances without media.
    pub tick_interval: Duration,
    /// How often each tile reports `tile_stats`.
    pub stats_interval: Duration,
    /// How long a tier switch may take to decode its first picture before
    /// it is abandoned (`tile_tier` `failed`, reason `timeout`).
    pub tier_switch_timeout: Duration,
    /// Longest a tier switch between main and sub waits, after the new
    /// tier's first decoded picture, for a picture that is not older than
    /// the one shown (docs/FFI.md, "Switching a tile's stream tier").
    pub tier_switch_align_cap: Duration,
    /// After a preview start answered with status 400 (a third-party camera
    /// behind an NVR), how long until the session restarts it while no
    /// media has arrived (`tile_waiting`, docs/FFI.md).
    pub preview_retry_interval: Duration,
    /// How long after the first 400 the session keeps waiting before the
    /// tile reports `tile_error` `start_rejected`; zero makes a 400 final.
    pub preview_wait_limit: Duration,
    /// How long a started stream may deliver only a stream-info record of
    /// size 0x0 and no video before the tile reports `tile_error`
    /// `start_rejected` with status 0 (docs/FFI.md). A multi-lens camera's
    /// second lens has a 0x0 record too, but video follows within seconds.
    pub info_only_limit: Duration,
    /// How long a second-lens probe (docs/FFI.md, `lenses`) waits for the
    /// first picture of a channel's `extern` stream before deciding the
    /// channel has one lens.
    pub lens_probe_window: Duration,
    /// Sound analysis of every live tile with audio: smoke and CO alarm
    /// patterns as `detection` events, and per-second loudness
    /// ([`Core::tile_loudness`]). See [`sound`].
    pub sound: sound::SoundSettings,
    /// Test support: run on a test's runtime and clock instead of the
    /// core's own runtime and the host clock. `None` (the default) in apps.
    pub test_runtime: Option<TestRuntime>,
    /// Test support, with [`CoreConfig::test_runtime`] only: the wall clock
    /// (microseconds since the Unix epoch) at which the simulated clock
    /// starts, instead of the real time, so recordings tests know which day
    /// is "today". `None` (the default) in apps.
    pub test_wall_clock_start_us: Option<i64>,
    /// Test support for self-tests against tools/mock-nvr, which needs none
    /// of the pauses a real device does: talk-back records go out this many
    /// times faster (its silent tail too, with the same number of records)
    /// and a play group's pause between its seeks and its plays is this many
    /// times shorter. Their timing on a real device is proven in the
    /// session's tests on a simulated clock. 1 (the default) in apps; the
    /// C interface reads `NVRVIEW_MOCK_SPEEDUP` (docs/FFI.md, "Self-tests").
    pub mock_speedup: u32,
}

/// Test support ([`CoreConfig::test_runtime`]): the core runs every task,
/// tiles included, on `runtime` (a current-thread runtime, usually with a
/// paused clock that the test steps), reads that runtime's clock wherever
/// it would read the host clock ([`Core::clock_now_ns`]), and reaches
/// devices added by host and port through `connect` (in-memory pipes to
/// mock devices on the same runtime). Timeouts, retries, pacing, and audio
/// timing then run in the test's simulated time.
///
/// Tiles run as tasks on the runtime instead of threads of their own, so
/// a slow `on_action` holds up the whole core: tests of the core's
/// threading keep the default.
#[derive(Clone)]
pub struct TestRuntime {
    /// Where the core runs. Must outlive the core.
    pub runtime: tokio::runtime::Handle,
    /// Opens a connection to the device at a host and port.
    #[allow(clippy::type_complexity)]
    pub connect: Arc<dyn Fn(&str, u16) -> std::io::Result<tokio::io::DuplexStream> + Send + Sync>,
}

impl fmt::Debug for TestRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestRuntime").finish_non_exhaustive()
    }
}

impl Default for CoreConfig {
    fn default() -> CoreConfig {
        CoreConfig {
            log: None,
            data_dir: None,
            pipeline: PipelineConfig::default(),
            backoff: BackoffConfig::default(),
            queue_frames: 64,
            max_devices: 64,
            max_tiles: 256,
            tick_interval: Duration::from_millis(100),
            stats_interval: Duration::from_secs(1),
            tier_switch_timeout: Duration::from_secs(15),
            tier_switch_align_cap: Duration::from_millis(300),
            preview_retry_interval: Duration::from_secs(6),
            preview_wait_limit: Duration::from_secs(60),
            info_only_limit: Duration::from_secs(15),
            lens_probe_window: lens::ProbeTiming::default().window,
            sound: sound::SoundSettings::default(),
            test_runtime: None,
            test_wall_clock_start_us: None,
            mock_speedup: 1,
        }
    }
}

/// Marks threads the core owns, so API calls made from inside a callback can
/// be refused instead of waiting on the thread they run on.
pub(crate) mod in_callback {
    use std::cell::Cell;

    thread_local! {
        static CORE_THREAD: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn mark_core_thread() {
        CORE_THREAD.with(|c| c.set(true));
    }

    pub(crate) fn on_core_thread() -> bool {
        CORE_THREAD.with(Cell::get)
    }
}

struct Device {
    handle: DeviceHandle,
    /// The device's stable key ([`store::DeviceKeys`]).
    key: String,
}

/// A device or tile whose ID is known to the caller of a `*_deferred` call
/// but not yet to the app: nothing naming it (device events, tile actions)
/// is delivered until [`Pending::publish`], or until it is dropped. The C
/// interface publishes after writing the ID through its out pointer.
#[derive(Debug)]
pub struct Pending {
    id: u64,
    gate: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Pending {
    /// The new ID.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Lets events and actions for this ID flow; returns the ID.
    pub fn publish(mut self) -> u64 {
        if let Some(g) = self.gate.take() {
            let _ = g.send(());
        }
        self.id
    }
}

/// The least time between two retries of one tile ([`Core::retry_tile`]).
pub const RETRY_FLOOR: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Registry {
    devices: HashMap<u64, Device>,
    tiles: HashMap<u64, TileHandle>,
}

/// The running core. See the crate documentation for its guarantees.
pub struct Core {
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    handle: tokio::runtime::Handle,
    /// The host clock, or a test runtime's ([`TestRuntime`]).
    clock: HostClock,
    /// What exists. Held only to look up, insert, or take out entries (and
    /// across a subscribe request), never while joining a thread or closing
    /// a session, so no API call waits on another tile's `on_action`.
    registry: Mutex<Registry>,
    /// Decoder result channels by tile; held only for a lookup.
    sinks: Mutex<HashMap<u64, ResultSink>>,
    events: Mutex<Option<EventThread>>,
    event_sink: events::EventSink,
    tile_ctx: TileContext,
    config: CoreConfig,
    closing: AtomicBool,
    next_id: AtomicU64,
    log_guard: Mutex<Option<viewer_log::LogGuard>>,
    /// When the core started, on its clock (for `timing` events).
    started_ns: u64,
    /// Set once the first picture of any tile has been reported decoded.
    first_picture: AtomicBool,
    layouts: Mutex<store::Layouts>,
    device_keys: Mutex<store::DeviceKeys>,
    /// Second-lens probe answers, shared by every device.
    lens: lens::LensContext,
    /// Talk-back permissions and every tile's talk.
    talk: talk::TalkHub,
    /// Camera motion from tiles.
    ptz: ptz::PtzHub,
    /// The one LAN device search ([`discover`]).
    discover: discover::DiscoverHub,
    /// The one update check or download ([`update`]).
    update: update::UpdateHub,
    /// The recordings owner (docs/FFI-RECORDINGS.md).
    recordings: recordings_owner::RecordingsHub,
    /// Recorded playbacks (docs/FFI-RECORDINGS.md, section 6).
    playback: playback::PlaybackHub,
    /// Live and call recordings (docs/CAPTURE.md).
    capture: capture_owner::CaptureHub,
    /// Smart alerts (docs/NOTIFICATIONS.md).
    alerts: alerts_owner::AlertHub,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Core {
    /// Starts the core: logging (if configured), the runtime, and the event
    /// thread.
    ///
    /// # Errors
    /// [`CoreError::InvalidArgument`] if the log directory cannot be used,
    /// [`CoreError::Internal`] if a thread cannot start.
    pub fn new(config: CoreConfig, callbacks: Arc<dyn Callbacks>) -> Result<Core, CoreError> {
        let log_guard = match &config.log {
            Some(l) => start_logging(l)?,
            None => None,
        };
        let (runtime, handle, clock) = match &config.test_runtime {
            None => {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("nvrview-core")
                    .on_thread_start(in_callback::mark_core_thread)
                    .enable_all()
                    .build()
                    .map_err(|_| CoreError::Internal)?;
                let handle = runtime.handle().clone();
                (Some(runtime), handle, HostClock::Host)
            }
            Some(t) => (
                None,
                t.runtime.clone(),
                HostClock::runtime(t.runtime.clone(), config.test_wall_clock_start_us),
            ),
        };
        let started_ns = clock.now_ns();
        let events = EventThread::spawn(callbacks.clone()).map_err(|_| CoreError::Internal)?;
        let audio = Arc::new(
            audio::AudioHub::new(callbacks.clone(), config.max_tiles, clock.clone())
                .map_err(|_| CoreError::Internal)?,
        );
        let event_sink = events.sink();
        let sound = Arc::new(sound::SoundHub::new(
            config.sound,
            event_sink.clone(),
            clock.clone(),
        ));
        let tile_ctx = TileContext {
            runtime: handle.clone(),
            callbacks,
            events: event_sink.clone(),
            pipeline: config.pipeline,
            clock: clock.clone(),
            tile_threads: config.test_runtime.is_none(),
            tick_interval: config.tick_interval.max(Duration::from_millis(1)),
            stats_interval: config.stats_interval.max(Duration::from_millis(10)),
            tier_switch_timeout: config.tier_switch_timeout,
            tier_switch_align_cap: config.tier_switch_align_cap,
            info_only_limit: config.info_only_limit,
            audio,
            sound,
        };
        let layouts = store::Layouts::open(config.data_dir.clone());
        let device_keys = store::DeviceKeys::open(config.data_dir.clone());
        let lens = lens::LensContext {
            cache: Arc::new(Mutex::new(lens::LensCache::open(config.data_dir.clone()))),
            timing: lens::ProbeTiming {
                window: config.lens_probe_window,
                ..lens::ProbeTiming::default()
            },
        };
        let talk = talk::TalkHub::new(event_sink.clone(), handle.clone());
        let ptz = ptz::PtzHub::new(event_sink.clone(), handle.clone());
        let discover = discover::DiscoverHub::new(event_sink.clone(), handle.clone());
        let update =
            update::UpdateHub::new(event_sink.clone(), config.data_dir.clone(), clock.clone());
        let recordings = recordings_owner::RecordingsHub::new(
            handle.clone(),
            event_sink.clone(),
            clock.clone(),
            config.data_dir.clone(),
        );
        // Sound analysis of tiles also feeds the recordings (loudness
        // envelope and sound labels), and lets it listen without tiles.
        recordings.attach_sound(tile_ctx.sound.clone());
        let capture = capture_owner::CaptureHub::new(
            handle.clone(),
            event_sink.clone(),
            clock.clone(),
            config.data_dir.clone(),
            config.test_runtime.is_none(),
        );
        let alerts = alerts_owner::AlertHub::new(
            handle.clone(),
            event_sink.clone(),
            clock.clone(),
            config.data_dir.clone(),
        );
        // Detections, motion, and alarms become smart-alert triggers.
        event_sink.set_tap(alerts.tap());
        info!("core started");
        if config.mock_speedup > 1 {
            warn!(
                speedup = config.mock_speedup,
                "mock speedup on: talk-back and play starts are timed for tools/mock-nvr, not a real device"
            );
        }
        Ok(Core {
            runtime: Mutex::new(runtime),
            handle,
            clock,
            registry: Mutex::new(Registry::default()),
            sinks: Mutex::new(HashMap::new()),
            events: Mutex::new(Some(events)),
            event_sink,
            tile_ctx,
            config,
            closing: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            log_guard: Mutex::new(log_guard),
            started_ns,
            first_picture: AtomicBool::new(false),
            layouts: Mutex::new(layouts),
            device_keys: Mutex::new(device_keys),
            lens,
            talk,
            ptz,
            discover,
            update,
            recordings,
            playback: playback::PlaybackHub::new(),
            capture,
            alerts,
        })
    }

    /// Refuses calls from inside callbacks and after shutdown began.
    fn check_api(&self) -> Result<(), CoreError> {
        if in_callback::on_core_thread() {
            return Err(CoreError::InvalidArgument);
        }
        if self.closing.load(Ordering::SeqCst) {
            return Err(CoreError::Closed);
        }
        Ok(())
    }

    fn new_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Adds a device reached by host name or address and port; connecting
    /// starts in the background. The password is kept in memory only.
    ///
    /// # Errors
    /// [`CoreError::InvalidArgument`] for an empty host or user or port 0,
    /// [`CoreError::Limit`], [`CoreError::Closed`].
    pub fn add_device(
        &self,
        host: &str,
        port: u16,
        user: &str,
        password: &str,
    ) -> Result<u64, CoreError> {
        if host.is_empty() || port == 0 {
            return Err(CoreError::InvalidArgument);
        }
        Ok(self
            .add_device_deferred(host, port, user, password)?
            .publish())
    }

    /// [`Core::add_device`], holding back the device's events until the
    /// returned [`Pending`] is published.
    ///
    /// # Errors
    /// As [`Core::add_device`].
    pub fn add_device_deferred(
        &self,
        host: &str,
        port: u16,
        user: &str,
        password: &str,
    ) -> Result<Pending, CoreError> {
        if host.is_empty() || port == 0 {
            return Err(CoreError::InvalidArgument);
        }
        let p = match &self.config.test_runtime {
            None => {
                let connector = HostConnector {
                    host: host.to_owned(),
                    port,
                };
                self.add_device_inner(connector, user, password, Some((host, port)))?
            }
            Some(t) => {
                let (connect, h) = (t.connect.clone(), host.to_owned());
                let connector = bc_session::FnConnector(move || connect(&h, port));
                self.add_device_inner(connector, user, password, Some((host, port)))?
            }
        };
        debug!(device = p.id(), host, port, "device address");
        Ok(p)
    }

    /// Adds a device reached through `connector` (tests use this to observe
    /// the connection).
    ///
    /// # Errors
    /// As [`Core::add_device`].
    pub fn add_device_with<C: Connector>(
        &self,
        connector: C,
        user: &str,
        password: &str,
    ) -> Result<u64, CoreError> {
        Ok(self
            .add_device_with_deferred(connector, user, password)?
            .publish())
    }

    /// [`Core::add_device_with`], holding back events until published.
    ///
    /// # Errors
    /// As [`Core::add_device`].
    pub fn add_device_with_deferred<C: Connector>(
        &self,
        connector: C,
        user: &str,
        password: &str,
    ) -> Result<Pending, CoreError> {
        self.add_device_inner(connector, user, password, None)
    }

    /// Adds a device; `address` (host and port) keys its stable device key.
    /// A connector without an address gets a key for this run only.
    fn add_device_inner<C: Connector>(
        &self,
        connector: C,
        user: &str,
        password: &str,
        address: Option<(&str, u16)>,
    ) -> Result<Pending, CoreError> {
        self.check_api()?;
        if user.is_empty() {
            return Err(CoreError::InvalidArgument);
        }
        let mut reg = lock(&self.registry);
        if self.closing.load(Ordering::SeqCst) {
            return Err(CoreError::Closed);
        }
        if reg.devices.len() >= self.config.max_devices {
            return Err(CoreError::Limit);
        }
        let id = self.new_id();
        let key = match address {
            Some((host, port)) => lock(&self.device_keys).key_for(host, port),
            None => format!("run-{id}"),
        };
        let mut cfg = bc_session::SessionConfig::new(user, password);
        cfg.backoff = self.config.backoff;
        cfg.queue_frames = self.config.queue_frames.max(1);
        cfg.preview_retry_interval = self.config.preview_retry_interval;
        cfg.preview_wait_limit = self.config.preview_wait_limit;
        let speedup = self.config.mock_speedup.max(1);
        cfg.talk_pace /= speedup;
        cfg.talk_tail /= speedup;
        cfg.play_group_delay /= speedup;
        let events = self.event_sink.clone();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
        let handle = {
            let _rt = self.handle.enter();
            let session = bc_session::DeviceSession::spawn(connector, cfg);
            let handle = DeviceHandle::start(
                id,
                key.clone(),
                session,
                events,
                gate_rx,
                device::DeviceContext {
                    lens: self.lens.clone(),
                    clock: self.clock.clone(),
                    names_hook: Some(self.recordings.names_hook(&key)),
                },
            );
            if let Some(s) = handle.session() {
                self.alerts.device_added(id, &key, s.clone());
                self.recordings.device_added(&key, id, s);
            }
            handle
        };
        reg.devices.insert(id, Device { handle, key });
        info!(device = id, "device added");
        Ok(Pending {
            id,
            gate: Some(gate_tx),
        })
    }

    /// The device's stable key: the same for the same address (host and
    /// port) across runs while the data directory is kept. Layouts name
    /// devices by it.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`].
    pub fn device_key(&self, device: u64) -> Result<String, CoreError> {
        lock(&self.registry)
            .devices
            .get(&device)
            .map(|d| d.key.clone())
            .ok_or(CoreError::UnknownId)
    }

    /// Every stored layout: `{"revision": n, "layouts": [...]}` (docs/FFI.md,
    /// "Layouts").
    pub fn layouts_json(&self) -> String {
        lock(&self.layouts).to_json()
    }

    /// Inserts or replaces (by `id`) a layout given as JSON, validates it,
    /// stores it, and returns the version the core gave it. Sends
    /// `layouts_changed`.
    ///
    /// # Errors
    /// [`CoreError::InvalidArgument`] for a layout that is not valid,
    /// [`CoreError::Limit`] when [`store::MAX_LAYOUTS`] are stored,
    /// [`CoreError::Internal`] when it cannot be written (nothing changes),
    /// [`CoreError::Closed`].
    pub fn put_layout(&self, json: &str) -> Result<u64, CoreError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(CoreError::Closed);
        }
        let mut l = lock(&self.layouts);
        let r = l.put(json);
        let revision = l.revision();
        drop(l);
        match r {
            Ok(version) => {
                self.layouts_changed(revision);
                Ok(version)
            }
            Err(e) => Err(layout_error(e)),
        }
    }

    /// Deletes a layout. Sends `layouts_changed`.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`] for an unknown ID, [`CoreError::Internal`]
    /// when the change cannot be written, [`CoreError::Closed`].
    pub fn delete_layout(&self, id: &str) -> Result<(), CoreError> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(CoreError::Closed);
        }
        let mut l = lock(&self.layouts);
        let r = l.delete(id);
        let revision = l.revision();
        drop(l);
        r.map_err(layout_error)?;
        self.layouts_changed(revision);
        Ok(())
    }

    fn layouts_changed(&self, revision: u64) {
        self.event_sink.send(
            json::Object::event("layouts_changed")
                .u64("revision", revision)
                .finish(),
            Priority::State,
        );
    }

    /// Starts a search for devices on the local network ([`discover`],
    /// docs/FFI.md, "Device discovery"). Returns the search's ID at once;
    /// answers arrive as `discovered` events and the end as one
    /// `discovery_done`. A search already running is stopped first (its
    /// `discovery_done` has `stopped` true): one search per core. Build
    /// `options` with [`discover::parse_options`] from the app's JSON.
    ///
    /// # Errors
    /// [`CoreError::Closed`], [`CoreError::InvalidArgument`] from inside a
    /// callback.
    pub fn discover_start(&self, options: discovery::Options) -> Result<u64, CoreError> {
        self.check_api()?;
        Ok(self.discover.start(options))
    }

    /// Stops the running search, if any; its `discovery_done` (`stopped`
    /// true) is queued before this returns. Nothing running is not an
    /// error.
    ///
    /// # Errors
    /// [`CoreError::Closed`], [`CoreError::InvalidArgument`] from inside a
    /// callback.
    pub fn discover_stop(&self) -> Result<(), CoreError> {
        self.check_api()?;
        self.discover.stop();
        Ok(())
    }

    /// Starts an update check, or a download when the request has one
    /// ([`update`], docs/FFI.md, "Updates"). Returns the operation's ID at
    /// once; progress and the outcome arrive as `update` events. Build the
    /// request with [`update::parse_request`] from the app's JSON.
    ///
    /// # Errors
    /// [`CoreError::Limit`] while another update operation runs,
    /// [`CoreError::Closed`], [`CoreError::InvalidArgument`] from inside a
    /// callback.
    pub fn update_start(&self, request: update::Request) -> Result<u64, CoreError> {
        self.check_api()?;
        self.update.start(request)
    }

    /// Cancels the running update operation, if any, waiting briefly for
    /// it to end. Nothing running is not an error.
    ///
    /// # Errors
    /// [`CoreError::Closed`], [`CoreError::InvalidArgument`] from inside a
    /// callback.
    pub fn update_cancel(&self) -> Result<(), CoreError> {
        self.check_api()?;
        self.update.cancel();
        Ok(())
    }

    /// Closes a device's tiles, then its session.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`], [`CoreError::Closed`],
    /// [`CoreError::InvalidArgument`] from inside a callback.
    pub fn remove_device(&self, device: u64) -> Result<(), CoreError> {
        self.check_api()?;
        let mut reg = lock(&self.registry);
        let dev = reg.devices.remove(&device).ok_or(CoreError::UnknownId)?;
        let ids: Vec<u64> = reg
            .tiles
            .iter()
            .filter(|(_, t)| t.device == device)
            .map(|(id, _)| *id)
            .collect();
        let tiles: Vec<(u64, TileHandle)> = ids
            .into_iter()
            .filter_map(|id| reg.tiles.remove(&id).map(|t| (id, t)))
            .collect();
        drop(reg);
        // Its recordings end (and let go of the session) first.
        self.capture.device_removed(device);
        // Cameras woken for smart-alert checks are let go.
        self.alerts.device_removed(device);
        self.end_tiles(tiles, CloseReason::DeviceRemoved);
        // Its plays stop before the session closes.
        self.playback_device_removed(&dev.key);
        self.talk.forget_device(device);
        self.ptz.forget_device(device);
        // Recordings let go of the session before it closes.
        self.recordings.device_removed(&dev.key, device);
        dev.handle.close_blocking(&self.handle);
        self.event_sink
            .send(device::closed_event(device), Priority::State);
        info!(device, "device removed");
        Ok(())
    }

    /// Opens a tile on one channel and tier of a device. Actions start
    /// arriving on `on_action` once the stream reaches a keyframe.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`] for an unknown device, [`CoreError::Limit`],
    /// [`CoreError::Closed`], [`CoreError::Internal`].
    pub fn open_tile(&self, device: u64, channel: u8, tier: StreamTier) -> Result<u64, CoreError> {
        Ok(self.open_tile_deferred(device, channel, tier)?.publish())
    }

    /// [`Core::open_tile`], holding back the tile's actions and events until
    /// the returned [`Pending`] is published.
    ///
    /// # Errors
    /// As [`Core::open_tile`].
    pub fn open_tile_deferred(
        &self,
        device: u64,
        channel: u8,
        tier: StreamTier,
    ) -> Result<Pending, CoreError> {
        self.check_api()?;
        let mut reg = lock(&self.registry);
        let handle = &reg.devices.get(&device).ok_or(CoreError::UnknownId)?.handle;
        let session = handle.session();
        if reg.tiles.len() >= self.config.max_tiles {
            return Err(CoreError::Limit);
        }
        handle.tile_starting(channel);
        // A session that has ended (refused login) yields a tile that ends at
        // once with `login_failed`, once the app has its ID.
        let sub = match session {
            Some(s) => self.handle.block_on(s.subscribe(channel, tier)).ok(),
            None => None,
        };
        let id = self.new_id();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
        let control = self.tile_ctx.audio.add_tile(id);
        let start = TileStart {
            id,
            device,
            channel,
            tier,
            playback: false,
        };
        let sub = sub.map(tile::Source::Live);
        let (tile, sink) = match tile::spawn(start, sub, self.tile_ctx.clone(), control, gate_rx) {
            Ok(t) => t,
            Err(_) => {
                self.tile_ctx.audio.remove_tile(id);
                return Err(CoreError::Internal);
            }
        };
        lock(&self.sinks).insert(id, sink);
        reg.tiles.insert(id, tile);
        info!(
            tile = id,
            device,
            channel,
            tier = device::tier_name(tier),
            "tile opened"
        );
        Ok(Pending {
            id,
            gate: Some(gate_tx),
        })
    }

    /// Asks a tile to show another stream tier (main on focus, sub back in
    /// the grid). Returns at once. The tile keeps showing its current
    /// stream while the new one starts as a new generation (its actions
    /// carry the new generation number); once the new generation's first
    /// picture is reported decoded, the old generation gets a final
    /// `Reset` and its stream stops (unless another tile shows it).
    /// Progress arrives as `tile_tier` events (`starting`, `active`,
    /// `failed`) and a `timing` event (`tier_switch`). Asking for the tier
    /// already requested changes nothing (and reports `active` again if
    /// no switch is in progress).
    ///
    /// # Errors
    /// [`CoreError::UnknownId`], [`CoreError::Limit`] when too many
    /// requests are queued for the tile, [`CoreError::Closed`],
    /// [`CoreError::InvalidArgument`] from inside a callback.
    pub fn set_tile_tier(&self, tile: u64, tier: StreamTier) -> Result<(), CoreError> {
        self.check_api()?;
        let requested_ns = self.clock.now_ns();
        let mut reg = lock(&self.registry);
        if !reg.tiles.contains_key(&tile) {
            drop(reg);
            // A playback tile restarts its play in the new tier.
            return self
                .playback_tile_tier(tile, tier)
                .unwrap_or(Err(CoreError::UnknownId));
        }
        let (device, channel, current) = {
            let t = reg.tiles.get(&tile).ok_or(CoreError::UnknownId)?;
            (t.device, t.channel, t.tier)
        };
        if current == tier {
            let t = reg.tiles.get(&tile).ok_or(CoreError::UnknownId)?;
            return if t.command(TileCommand::Reaffirm) {
                Ok(())
            } else {
                Err(CoreError::Limit)
            };
        }
        // Start the new stream first (shared with any tile already showing
        // it); the old one stops once the switch is done.
        let session = reg.devices.get(&device).and_then(|d| {
            d.handle.tile_starting(channel);
            d.handle.session()
        });
        let sub = match session {
            Some(s) => self.handle.block_on(s.subscribe(channel, tier)).ok(),
            None => None,
        };
        let t = reg.tiles.get_mut(&tile).ok_or(CoreError::UnknownId)?;
        if !t.command(TileCommand::Switch {
            tier,
            sub,
            requested_ns,
        }) {
            return Err(CoreError::Limit);
        }
        t.tier = tier;
        info!(tile, tier = device::tier_name(tier), "tier requested");
        Ok(())
    }

    /// Retries a tile that has no picture (the app's Retry button): its
    /// stream is stopped and started again now, on the same login; when the
    /// device is waiting to reconnect, the next attempt is made now. Asked
    /// again within [`RETRY_FLOOR`] it does nothing, and nothing happens
    /// after the device refused the login (that state is terminal, to
    /// protect the account).
    ///
    /// # Errors
    /// [`CoreError::UnknownId`], [`CoreError::Closed`],
    /// [`CoreError::InvalidArgument`] from inside a callback.
    pub fn retry_tile(&self, tile: u64) -> Result<(), CoreError> {
        self.check_api()?;
        let now = self.clock.now_ns();
        let mut reg = lock(&self.registry);
        let t = reg.tiles.get_mut(&tile).ok_or(CoreError::UnknownId)?;
        if t.last_retry_ns
            .is_some_and(|l| now.saturating_sub(l) < RETRY_FLOOR.as_nanos() as u64)
        {
            return Ok(());
        }
        t.last_retry_ns = Some(now);
        let (device, channel, tier) = (t.device, t.channel, t.tier);
        if let Some(s) = reg.devices.get(&device).and_then(|d| d.handle.session()) {
            s.retry_stream(channel, tier);
            info!(tile, "retry requested");
        }
        Ok(())
    }

    /// Closes a tile. No `on_action` for it runs after this returns.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`], [`CoreError::Closed`],
    /// [`CoreError::InvalidArgument`] from inside a callback, or for a
    /// playback tile (remove it from its playback instead).
    pub fn close_tile(&self, tile: u64) -> Result<(), CoreError> {
        self.check_api()?;
        if self.playback.has_tile(tile) {
            return Err(CoreError::InvalidArgument);
        }
        let mut reg = lock(&self.registry);
        let t = reg.tiles.remove(&tile).ok_or(CoreError::UnknownId)?;
        drop(reg);
        self.end_tiles(vec![(tile, t)], CloseReason::Closed);
        Ok(())
    }

    /// Stops tiles in parallel, then waits for each and reports it closed
    /// (unless it already ended on its own and said so).
    fn end_tiles(&self, mut tiles: Vec<(u64, TileHandle)>, reason: CloseReason) {
        {
            let mut sinks = lock(&self.sinks);
            for (id, t) in &mut tiles {
                sinks.remove(id);
                t.signal_stop();
            }
        }
        for (id, _) in &tiles {
            self.talk.tile_closed(*id);
            self.capture.tile_closed(*id);
            self.ptz.tile_closed(*id);
        }
        for (id, mut t) in tiles {
            t.end(reason);
            self.tile_ctx.audio.remove_tile(id);
        }
    }

    fn send_result(&self, tile: u64, r: DecoderResult) -> Result<(), CoreError> {
        match lock(&self.sinks).get(&tile) {
            Some(s) => {
                s.send(r);
                Ok(())
            }
            None if self.closing.load(Ordering::SeqCst) => Err(CoreError::Closed),
            None => Err(CoreError::UnknownId),
        }
    }

    /// The platform decoded the picture with this timestamp, from the
    /// tile's stream `generation` (the action's). Any thread; never waits
    /// for the tile. The first one for any tile also reports the
    /// `first_picture` timing.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`] for a closed tile, [`CoreError::Closed`].
    pub fn decoded(&self, tile: u64, generation: u32, pts_us: u64) -> Result<(), CoreError> {
        self.send_result(
            tile,
            DecoderResult {
                generation,
                kind: ResultKind::Decoded(pts_us),
            },
        )?;
        if !self.first_picture.swap(true, Ordering::AcqRel) {
            let ns = self.clock.now_ns().saturating_sub(self.started_ns);
            info!(tile, ms = ns / 1_000_000, "first picture");
            self.event_sink.send(
                json::Object::event("timing")
                    .str("what", "first_picture")
                    .u64("tile", tile)
                    .u64("ns", ns)
                    .finish(),
                Priority::State,
            );
        }
        Ok(())
    }

    /// The platform could not carry out a `Configure` or a `Decode` of the
    /// tile's stream `generation`. Any thread; never waits for the tile.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`] for a closed tile, [`CoreError::Closed`].
    pub fn decode_failed(
        &self,
        tile: u64,
        generation: u32,
        kind: DecodeFailureKind,
    ) -> Result<(), CoreError> {
        self.send_result(
            tile,
            DecoderResult {
                generation,
                kind: ResultKind::Failed(kind),
            },
        )
    }

    /// Sets the app's audio output format: `sample_rate` frames per second
    /// and 1 or 2 interleaved channels. Replaces any earlier output (after a
    /// device or sample-rate change); audible tiles move to the new one
    /// within a tick and start over in sync.
    ///
    /// # Errors
    /// [`CoreError::InvalidArgument`] for a rate outside
    /// [`audio::OUTPUT_RATES`], other channel counts, or a call from inside a
    /// callback; [`CoreError::Closed`].
    pub fn set_audio_output(&self, sample_rate: u32, channels: u32) -> Result<(), CoreError> {
        self.check_api()?;
        if !audio::OUTPUT_RATES.contains(&sample_rate) || !(1..=2).contains(&channels) {
            return Err(CoreError::InvalidArgument);
        }
        self.tile_ctx
            .audio
            .set_output(Some(audio_pipeline::OutputFormat {
                sample_rate,
                channels: channels as usize,
            }));
        info!(sample_rate, channels, "audio output set");
        Ok(())
    }

    /// Fills `out` (interleaved, `channels` per frame) with the mix of the
    /// audible tiles; `host_time_ns` is when `out`'s first frame will be
    /// heard, on the host clock ([`clock`]). For the app's real-time audio
    /// callback: it never blocks, takes no lock, and never allocates or
    /// frees.
    ///
    /// # Errors
    /// Writes silence and returns [`CoreError::InvalidArgument`] when
    /// `channels` differs from the output's (as during a format change), or
    /// [`CoreError::Closed`] before an output is set, after shutdown, or
    /// while another thread renders (render from one thread at a time).
    pub fn render_audio(
        &self,
        out: &mut [f32],
        channels: usize,
        host_time_ns: u64,
    ) -> Result<(), CoreError> {
        self.tile_ctx.audio.render(out, channels, host_time_ns)
    }

    /// Makes a tile audible (it claims a playout slot; up to eight play at
    /// once) or silent (it releases its slot), at `volume` (0 to 1; 0 keeps
    /// its slot and timing, a mute).
    ///
    /// # Errors
    /// [`CoreError::UnknownId`], [`CoreError::InvalidArgument`] for a
    /// volume that is negative or not finite, or a call from inside a
    /// callback; [`CoreError::Closed`].
    pub fn set_tile_audio(&self, tile: u64, audible: bool, volume: f32) -> Result<(), CoreError> {
        self.check_api()?;
        if !volume.is_finite() || volume < 0.0 {
            return Err(CoreError::InvalidArgument);
        }
        if self.tile_ctx.audio.set_tile(tile, audible, volume) {
            Ok(())
        } else {
            Err(CoreError::UnknownId)
        }
    }

    /// Starts or stops a tile's audio tap: its decoded audio as 16 kHz mono
    /// blocks to [`Callbacks::on_audio_tap`], whether or not it is audible.
    /// The tap queue holds [`audio::TAP_CAPACITY`] blocks and drops the
    /// oldest when the app falls behind. No block for the tile is delivered
    /// after stopping returns.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`], [`CoreError::Closed`],
    /// [`CoreError::InvalidArgument`] from inside a callback.
    pub fn set_tile_audio_tap(&self, tile: u64, enabled: bool) -> Result<(), CoreError> {
        self.check_api()?;
        if self.tile_ctx.audio.set_tap(tile, enabled) {
            Ok(())
        } else {
            Err(CoreError::UnknownId)
        }
    }

    /// A tile's audio level: peak and RMS, 0 to 1, of its decoded audio
    /// before the tile's volume (peak with a 0.3 s release, RMS over about
    /// 0.1 s), audible or not. For a tile that plays while the app renders,
    /// [`Core::render_audio`] measures it (what is heard); otherwise it is
    /// measured as records are decoded, and falls to 0 within a second when
    /// no audio arrives. 0 for an unknown tile or one without audio.
    /// Lock-free and allocation-free: callable from any thread (and from
    /// callbacks) at display rate.
    pub fn tile_audio_level(&self, tile: u64) -> (f32, f32) {
        if tile == 0 {
            return (0.0, 0.0);
        }
        self.tile_ctx.audio.level(tile)
    }

    /// A live tile's recent loudness: peak and RMS per second, one byte per
    /// value, for the last [`sound::SoundSettings::loudness_history_s`]
    /// seconds at most, measured on its decoded audio whether audible or
    /// not. Empty for a tile without audio so far, or with loudness kept
    /// for no seconds. Takes two brief locks; callable from any thread
    /// outside callbacks.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`], [`CoreError::Closed`],
    /// [`CoreError::InvalidArgument`] from inside a callback.
    pub fn tile_loudness(&self, tile: u64) -> Result<sound::TileLoudness, CoreError> {
        self.check_api()?;
        if !lock(&self.registry).tiles.contains_key(&tile) {
            return Err(CoreError::UnknownId);
        }
        Ok(self.tile_ctx.sound.loudness(tile).unwrap_or_default())
    }

    /// Render calls that found another in progress (and rendered silence),
    /// and renderers leaked rather than freed on the real-time thread.
    pub fn audio_render_counters(&self) -> (u64, u64) {
        self.tile_ctx.audio.render_counters()
    }

    /// Enables or disables talk-back (camera motion control) on one channel
    /// of a device, for this run. Off until enabled; disabling stops a
    /// running talk on it (`talk` `stopped`, `permission_revoked`).
    ///
    /// # Errors
    /// [`CoreError::UnknownId`], [`CoreError::Closed`],
    /// [`CoreError::InvalidArgument`] from inside a callback.
    pub fn set_talk_permission(
        &self,
        device: u64,
        channel: u8,
        allowed: bool,
    ) -> Result<(), CoreError> {
        self.check_api()?;
        let session = lock(&self.registry)
            .devices
            .get(&device)
            .ok_or(CoreError::UnknownId)?
            .handle
            .session();
        self.talk
            .set_permission(device, channel, allowed, session.as_deref());
        info!(device, channel, allowed, "talk-back permission set");
        Ok(())
    }

    /// Starts talk-back to a tile's camera; returns at once, with progress
    /// as `talk` events (docs/FFI.md, "Talk-back"). Starting a talk that is
    /// starting or active changes nothing.
    ///
    /// # Errors
    /// [`CoreError::NotPermitted`] when the app has not enabled talk-back
    /// for the camera (a `talk` `refused` event follows; nothing is sent),
    /// [`CoreError::UnknownId`], [`CoreError::Closed`],
    /// [`CoreError::InvalidArgument`] from inside a callback.
    pub fn talk_start(&self, tile: u64) -> Result<(), CoreError> {
        self.check_api()?;
        let reg = lock(&self.registry);
        let t = reg.tiles.get(&tile).ok_or(CoreError::UnknownId)?;
        let (device, channel) = (t.device, t.channel);
        let session = reg.devices.get(&device).and_then(|d| d.handle.session());
        drop(reg);
        self.talk
            .start(tile, device, channel, session)
            .map_err(|talk::StartError::NotPermitted| CoreError::NotPermitted)
    }

    /// Marks a tile's camera as in a conversation the user has open (a
    /// doorbell call: a ring or Start Conversation) or no longer. While a
    /// conversation is open, talk starts on that camera skip the device
    /// session's per-minute limit; they still need the permission and are
    /// logged. The mark ends with `open` false, when the tile closes, when
    /// the camera's permission is withdrawn, and at shutdown.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`], [`CoreError::Closed`],
    /// [`CoreError::InvalidArgument`] from inside a callback.
    pub fn set_talk_conversation(&self, tile: u64, open: bool) -> Result<(), CoreError> {
        self.check_api()?;
        let reg = lock(&self.registry);
        let t = reg.tiles.get(&tile).ok_or(CoreError::UnknownId)?;
        let (device, channel) = (t.device, t.channel);
        let session = reg.devices.get(&device).and_then(|d| d.handle.session());
        drop(reg);
        self.talk
            .set_conversation(tile, device, channel, open, session.as_ref());
        if !open {
            // A call recording follows its call.
            self.capture.call_ended(tile);
        }
        info!(tile, device, channel, open, "talk-back conversation set");
        Ok(())
    }

    /// Whether a tile is marked as in a conversation
    /// ([`Core::set_talk_conversation`]).
    pub fn talk_conversation(&self, tile: u64) -> bool {
        self.talk.in_conversation(tile)
    }

    /// Queues mono samples (-1 to 1) at `sample_rate` Hz for a tile's talk;
    /// the core resamples them to the device's 16 kHz. Samples for a tile
    /// without a talk are ignored. Takes a short lock and may allocate: not
    /// for a real-time audio thread.
    ///
    /// # Errors
    /// [`CoreError::InvalidArgument`] for a rate outside
    /// [`talk::PUSH_RATES`] or from inside a callback,
    /// [`CoreError::UnknownId`], [`CoreError::Closed`].
    pub fn talk_push(&self, tile: u64, samples: &[f32], sample_rate: u32) -> Result<(), CoreError> {
        self.check_api()?;
        if !talk::PUSH_RATES.contains(&sample_rate) {
            return Err(CoreError::InvalidArgument);
        }
        if !self.talk.push(tile, samples, sample_rate)
            && !lock(&self.registry).tiles.contains_key(&tile)
        {
            return Err(CoreError::UnknownId);
        }
        Ok(())
    }

    /// Ends a tile's talk: at once, or (`drain`) after the audio already
    /// pushed has been sent. Nothing happens for a tile without a talk.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`], [`CoreError::Closed`],
    /// [`CoreError::InvalidArgument`] from inside a callback.
    pub fn talk_stop(&self, tile: u64, drain: bool) -> Result<(), CoreError> {
        self.check_api()?;
        if !self.talk.stop(tile, drain) && !lock(&self.registry).tiles.contains_key(&tile) {
            return Err(CoreError::UnknownId);
        }
        Ok(())
    }

    /// Sets the capture settings (docs/FFI.md, "Capture",
    /// `nv_capture_configure`): the live recordings' folder, caps, disk
    /// thresholds, count limits, the "Record from" tier, and share-safe
    /// names, for recordings started afterwards. The first call also
    /// finalizes or deletes the partial files a crash left (one `recording`
    /// event `recovered` for each finalized file); it runs before the call
    /// returns.
    ///
    /// # Errors
    /// [`CaptureError::InvalidArgument`] for bad JSON or a relative
    /// `live_dir`, or from inside a callback; [`CaptureError::Closed`].
    pub fn capture_configure(&self, json: &str) -> Result<(), CaptureError> {
        self.check_api().map_err(capture_error)?;
        self.capture.configure(json)
    }

    /// Installs the app's AAC encoder, or removes it (`None`). Recordings
    /// already running keep the encoder they started with.
    ///
    /// # Errors
    /// [`CaptureError::InvalidArgument`] from inside a callback,
    /// [`CaptureError::Closed`].
    pub fn capture_set_encoder(
        &self,
        encoder: Option<Arc<dyn AudioEncoder>>,
    ) -> Result<(), CaptureError> {
        self.check_api().map_err(capture_error)?;
        self.capture.set_encoder(encoder)
    }

    /// Replaces how free space is read ([`SystemFreeSpace`] by default) for
    /// recordings started afterwards. Platforms may supply their own; tests
    /// inject a fake.
    ///
    /// # Errors
    /// [`CaptureError::InvalidArgument`] from inside a callback,
    /// [`CaptureError::Closed`].
    pub fn capture_set_free_space(&self, free: Arc<dyn FreeSpace>) -> Result<(), CaptureError> {
        self.check_api().map_err(capture_error)?;
        self.capture.set_free_space(free)
    }

    /// One encoded AAC access unit for `track` of `recording`, in order,
    /// answering [`AudioEncoder::encode`]. Allowed from any thread,
    /// including inside the encoder's own calls, until its `close` returns.
    ///
    /// # Errors
    /// [`CaptureError::UnknownId`] for a recording that ended,
    /// [`CaptureError::InvalidArgument`] for an empty or oversized unit.
    pub fn record_audio_encoded(
        &self,
        recording: u64,
        track: u32,
        data: &[u8],
    ) -> Result<(), CaptureError> {
        self.capture.audio_encoded(recording, track, data)
    }

    /// Starts a live or call recording of a tile's camera (docs/FFI.md,
    /// "Capture", `nv_record_start`); answers
    /// `{"recording":12,"file":"...","state":"waiting_keyframe"}`. The
    /// recording has its own subscription at its own tier and outlives the
    /// tile (a call recording ends with its call: the tile's conversation
    /// closing, or the tile closing).
    ///
    /// # Errors
    /// [`CaptureError::Limit`] (see [`CaptureError::limit_answer`]),
    /// [`CaptureError::NotConfigured`] (no settings, or a call without an
    /// encoder), [`CaptureError::UnknownId`] (no such tile),
    /// [`CaptureError::InvalidArgument`], [`CaptureError::Closed`],
    /// [`CaptureError::Internal`].
    pub fn record_start(&self, request: &str) -> Result<String, CaptureError> {
        self.check_api().map_err(capture_error)?;
        let p = capture_owner::StartParams::parse(request)?;
        let src = {
            let reg = lock(&self.registry);
            let t = reg.tiles.get(&p.tile).ok_or(CaptureError::UnknownId)?;
            capture_owner::TileSource {
                device: t.device,
                channel: t.channel,
                tile_tier: t.tier,
                session: reg.devices.get(&t.device).and_then(|d| d.handle.session()),
            }
        };
        let id = self.new_id();
        self.capture.start(id, &p, src)
    }

    /// Stops a recording and finalizes it; returns at once (`recording`
    /// events `finalizing`, then `finished`).
    ///
    /// # Errors
    /// [`CaptureError::UnknownId`], [`CaptureError::Closed`],
    /// [`CaptureError::InvalidArgument`] from inside a callback.
    pub fn record_stop(&self, recording: u64) -> Result<(), CaptureError> {
        self.check_api().map_err(capture_error)?;
        self.capture.stop(recording)
    }

    /// `{"recordings":[...]}`: the latest `recording` event of each
    /// recording that has not ended.
    pub fn record_status(&self) -> String {
        self.capture.status()
    }

    /// A tile's device, channel and session, for camera motion.
    fn ptz_target(
        &self,
        tile: u64,
    ) -> Result<(u64, bc_protocol::ptz::Cam, Arc<bc_session::DeviceSession>), CoreError> {
        let reg = lock(&self.registry);
        let t = reg.tiles.get(&tile).ok_or(CoreError::UnknownId)?;
        let (device, cam) = (
            t.device,
            bc_protocol::ptz::Cam::with_lens(t.channel, t.tier.lens()),
        );
        let session = reg
            .devices
            .get(&device)
            .and_then(|d| d.handle.session())
            .ok_or(CoreError::UnknownId)?;
        Ok((device, cam, session))
    }

    fn ptz_send(
        &self,
        tile: u64,
        command: bc_session::PtzCommand,
        origin: MotionOrigin,
    ) -> Result<(), CoreError> {
        self.check_api()?;
        if let MotionOrigin::Automation(c) = &origin
            && !valid_client(c)
        {
            return Err(CoreError::InvalidArgument);
        }
        let (device, cam, session) = self.ptz_target(tile)?;
        self.ptz.send(tile, device, cam, &session, command, origin);
        Ok(())
    }

    /// Reads what a tile's camera can do and where its lens is (199, 294,
    /// 190, 340: viewing tier only) and returns the `nv_ptz_info` JSON
    /// (docs/FFI.md, "Camera motion (PTZ)"). Blocks until the device
    /// answers, at most [`PTZ_INFO_TIMEOUT`].
    ///
    /// # Errors
    /// [`CoreError::UnknownId`], [`CoreError::Closed`],
    /// [`CoreError::InvalidArgument`] from inside a callback,
    /// [`CoreError::Internal`] when the device did not answer.
    pub fn ptz_info(&self, tile: u64) -> Result<String, CoreError> {
        self.check_api()?;
        let (_, cam, session) = self.ptz_target(tile)?;
        let read = self.handle.block_on(async move {
            tokio::time::timeout(PTZ_INFO_TIMEOUT, session.ptz_info(cam)).await
        });
        match read {
            Ok(Ok(info)) => Ok(ptz::info_json(&info)),
            Ok(Err(e)) => {
                debug!(tile, reason = e.name(), "camera motion info not read");
                Err(CoreError::Internal)
            }
            Err(_) => Err(CoreError::Internal),
        }
    }

    /// Starts or renews a pan or tilt move of a tile's camera at `speed`
    /// (1 to 64). The app renews it at least every 500 ms while its control
    /// is held (renewals are not sent to the camera); the camera stops 1 s
    /// after the last renewal, after 30 s of holding, on
    /// [`Core::ptz_stop`], when the tile closes, and on every other exit
    /// path. Returns at once; the outcome comes as a `ptz` event.
    ///
    /// # Errors
    /// [`CoreError::InvalidArgument`] for a speed outside 1–64, an empty or
    /// overlong automation name, or a call from inside a callback;
    /// [`CoreError::UnknownId`], [`CoreError::Closed`].
    pub fn ptz_move(
        &self,
        tile: u64,
        direction: PtzDirection,
        speed: u32,
        origin: MotionOrigin,
    ) -> Result<(), CoreError> {
        let speed = u8::try_from(speed)
            .ok()
            .filter(|s| ptz::SPEEDS.contains(s))
            .ok_or(CoreError::InvalidArgument)?;
        self.ptz_send(
            tile,
            bc_session::PtzCommand::Move { direction, speed },
            origin,
        )
    }

    /// Stops a tile's camera at once. Never rate-limited.
    ///
    /// # Errors
    /// As [`Core::ptz_move`].
    pub fn ptz_stop(&self, tile: u64, origin: MotionOrigin) -> Result<(), CoreError> {
        self.ptz_send(tile, bc_session::PtzCommand::Stop, origin)
    }

    /// Moves a tile's lens zoom or focus to `position`, clamped to the
    /// range the camera reported.
    ///
    /// # Errors
    /// As [`Core::ptz_move`].
    pub fn ptz_lens(
        &self,
        tile: u64,
        axis: LensAxis,
        position: i32,
        origin: MotionOrigin,
    ) -> Result<(), CoreError> {
        self.ptz_send(
            tile,
            bc_session::PtzCommand::Lens { axis, position },
            origin,
        )
    }

    /// Recalls preset `id` (from [`Core::ptz_info`]) on a tile's camera.
    /// Saving or deleting presets is not possible.
    ///
    /// # Errors
    /// As [`Core::ptz_move`].
    pub fn ptz_preset(&self, tile: u64, id: u32, origin: MotionOrigin) -> Result<(), CoreError> {
        self.ptz_send(tile, bc_session::PtzCommand::Preset { id }, origin)
    }

    /// Grants or withdraws camera motion control on one channel of a device
    /// for an automation: a local API client by name, or
    /// [`MotionOrigin::AUTO_CORRECTION`]. None is granted until granted,
    /// for this run of the core (the app stores the user's grants and
    /// passes them after adding the device). Withdrawing stops that
    /// automation's move. The app's own controls need no grant.
    ///
    /// # Errors
    /// [`CoreError::InvalidArgument`] for an empty or overlong name or from
    /// inside a callback, [`CoreError::UnknownId`], [`CoreError::Closed`].
    pub fn set_motion_grant(
        &self,
        device: u64,
        channel: u8,
        client: &str,
        granted: bool,
    ) -> Result<(), CoreError> {
        self.check_api()?;
        if !valid_client(client) {
            return Err(CoreError::InvalidArgument);
        }
        let session = lock(&self.registry)
            .devices
            .get(&device)
            .ok_or(CoreError::UnknownId)?
            .handle
            .session()
            .ok_or(CoreError::UnknownId)?;
        session.set_motion_grant(channel, client, granted);
        Ok(())
    }

    /// Now on the core's clock, nanoseconds: the host clock ([`clock`]), or
    /// a test runtime's simulated clock ([`TestRuntime`]). Render host
    /// times ([`Core::render_audio`]) are on this clock.
    pub fn clock_now_ns(&self) -> u64 {
        self.clock.now_ns()
    }

    /// Waits until every event queued so far has been delivered (its
    /// `on_event` returned). For tests; must not be called from a callback
    /// (it returns at once there).
    pub fn flush_events(&self) {
        if in_callback::on_core_thread() {
            return;
        }
        self.event_sink.flush();
    }

    /// True on threads the core owns (where callbacks run). The C interface
    /// uses it to refuse freeing the core from inside a callback.
    pub fn on_core_thread() -> bool {
        in_callback::on_core_thread()
    }

    /// Events dropped because `on_event` did not keep up.
    pub fn dropped_events(&self) -> u64 {
        self.event_sink.dropped()
    }

    /// Stops every preview, closes every session, and joins every core
    /// thread. No callback runs after it returns. Later calls do nothing.
    /// Must not be called from a callback (it returns at once there, doing
    /// nothing).
    pub fn shutdown(&self) {
        if in_callback::on_core_thread() {
            warn!("shutdown called from a callback; ignored");
            return;
        }
        if self.closing.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut reg = lock(&self.registry);
        let tiles: Vec<(u64, TileHandle)> = reg.tiles.drain().collect();
        let devices: Vec<(u64, Device)> = reg.devices.drain().collect();
        drop(reg);
        // Smart alerts stop first: woken streams, inputs, and outputs.
        self.alerts.shutdown();
        // Recordings stop first (`core_shutdown`) and finalize within
        // FINALIZE_WAIT; what is left is recovered at the next configure.
        self.capture.shutdown();
        // Their tile_closed events are delivered before this returns.
        self.end_tiles(tiles, CloseReason::CoreShutdown);
        // Playbacks stop their plays and close their tiles.
        self.playback_shutdown();
        // No on_audio_tap after this; the renderer renders silence.
        self.tile_ctx.audio.shutdown();
        self.talk.shutdown();
        self.ptz.shutdown();
        // A running search ends with its `discovery_done` (stopped).
        self.discover.stop();
        // A running update check or download is canceled (its folder
        // removed); its `canceled` event comes first if it ends in time.
        self.update.cancel();
        // Searches, thumbnail fetches, and downloads stop and let go of
        // the sessions before they close.
        self.recordings.shutdown();
        // Close every session at once; each is bounded by its write timeout.
        let closes: Vec<_> = devices
            .into_iter()
            .map(|(id, d)| (id, self.handle.spawn(d.handle.close())))
            .collect();
        for (id, c) in closes {
            let _ = self.handle.block_on(c);
            self.event_sink
                .send(device::closed_event(id), Priority::State);
        }
        info!("core stopped");
        if let Some(mut e) = lock(&self.events).take() {
            e.stop();
        }
        if let Some(rt) = lock(&self.runtime).take() {
            rt.shutdown_timeout(Duration::from_secs(2));
        }
        if let Some(g) = lock(&self.log_guard).take() {
            g.flush();
        }
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl fmt::Debug for Core {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Core").finish_non_exhaustive()
    }
}

fn capture_error(e: CoreError) -> CaptureError {
    match e {
        CoreError::Closed => CaptureError::Closed,
        _ => CaptureError::InvalidArgument,
    }
}

/// Longest wait of [`Core::ptz_info`].
pub const PTZ_INFO_TIMEOUT: Duration = Duration::from_secs(20);

/// An automation name: 1 to [`ptz::MAX_CLIENT_NAME`] bytes, no control
/// characters.
fn valid_client(c: &str) -> bool {
    !c.is_empty() && c.len() <= ptz::MAX_CLIENT_NAME && !c.chars().any(char::is_control)
}

fn layout_error(e: store::LayoutError) -> CoreError {
    match e {
        store::LayoutError::Invalid(why) => {
            debug!(%why, "layout refused");
            CoreError::InvalidArgument
        }
        store::LayoutError::TooMany => CoreError::Limit,
        store::LayoutError::Unknown => CoreError::UnknownId,
        store::LayoutError::Io => CoreError::Internal,
    }
}

fn start_logging(l: &LogSettings) -> Result<Option<viewer_log::LogGuard>, CoreError> {
    let mut cfg = viewer_log::LogConfig::new(&l.dir, "nvrview-core");
    if l.max_files > 0 {
        cfg.max_files = l.max_files;
    }
    if l.max_total_bytes > 0 {
        cfg.max_total_bytes = l.max_total_bytes;
        cfg.max_file_bytes = cfg.max_file_bytes.min(l.max_total_bytes);
    }
    // Apps keep their own console output; the core writes only its files.
    cfg.stderr = false;
    match viewer_log::init(&cfg) {
        Ok(g) => Ok(Some(g)),
        // One process, one global log: a second core shares the first one's.
        Err(viewer_log::InitError::AlreadyInitialized) => Ok(None),
        Err(viewer_log::InitError::Io(_)) => Err(CoreError::InvalidArgument),
    }
}
