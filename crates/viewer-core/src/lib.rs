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
//! - **Logging.** With a log directory, the core installs `viewer-log`
//!   (rotation capped by file count and total size, rate-limited). Passwords
//!   are never logged; addresses only at debug level.

pub mod audio;
mod device;
mod events;
mod json;
mod tile;

pub use bc_protocol::stream::StreamTier;
pub use bc_session::{BackoffConfig, Connector};
pub use video_pipeline::{
    Action, ActionKind, Codec, ColorMatrix, DecodeFailureKind, PipelineConfig, TileStatus,
};

/// Grid layout, re-exported for the C interface's `nv_grid_layout`.
pub mod layout {
    pub use video_pipeline::layout::*;
    pub use video_pipeline::{Rect, Size};
}

use device::{DeviceHandle, HostConnector};
use events::{EventThread, Priority};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tile::{CloseReason, DecoderResult, ResultSink, TileContext, TileHandle};
use tracing::{debug, info, warn};

/// What the app receives. Both methods run on threads the core owns and must
/// return quickly.
pub trait Callbacks: Send + Sync + 'static {
    /// One action for a tile's decoder or view, serialized per tile. Data in
    /// `action` is borrowed for the duration of the call.
    fn on_action(&self, tile: u64, action: &Action<'_>);
    /// One JSON object, serialized across the core.
    fn on_event(&self, json: &str);
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
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CoreError::InvalidArgument => "invalid argument",
            CoreError::UnknownId => "unknown device or tile",
            CoreError::Closed => "the core is shutting down",
            CoreError::Limit => "too many devices or tiles",
            CoreError::Internal => "internal error",
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
}

impl Default for CoreConfig {
    fn default() -> CoreConfig {
        CoreConfig {
            log: None,
            pipeline: PipelineConfig::default(),
            backoff: BackoffConfig::default(),
            queue_frames: 64,
            max_devices: 64,
            max_tiles: 256,
            tick_interval: Duration::from_millis(100),
            stats_interval: Duration::from_secs(1),
        }
    }
}

/// Monotonic microseconds since the core started; the pipelines' clock.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Clock(Instant);

impl Clock {
    pub(crate) fn now_us(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_micros()).unwrap_or(u64::MAX)
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

#[derive(Default)]
struct Registry {
    devices: HashMap<u64, Device>,
    tiles: HashMap<u64, TileHandle>,
}

/// The running core. See the crate documentation for its guarantees.
pub struct Core {
    runtime: Mutex<Option<tokio::runtime::Runtime>>,
    handle: tokio::runtime::Handle,
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
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("nvrview-core")
            .on_thread_start(in_callback::mark_core_thread)
            .enable_all()
            .build()
            .map_err(|_| CoreError::Internal)?;
        let handle = runtime.handle().clone();
        let events = EventThread::spawn(callbacks.clone()).map_err(|_| CoreError::Internal)?;
        let event_sink = events.sink();
        let tile_ctx = TileContext {
            runtime: handle.clone(),
            callbacks,
            events: event_sink.clone(),
            pipeline: config.pipeline,
            clock: Clock(Instant::now()),
            tick_interval: config.tick_interval.max(Duration::from_millis(1)),
            stats_interval: config.stats_interval.max(Duration::from_millis(10)),
        };
        info!("core started");
        Ok(Core {
            runtime: Mutex::new(Some(runtime)),
            handle,
            registry: Mutex::new(Registry::default()),
            sinks: Mutex::new(HashMap::new()),
            events: Mutex::new(Some(events)),
            event_sink,
            tile_ctx,
            config,
            closing: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            log_guard: Mutex::new(log_guard),
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
        let connector = HostConnector {
            host: host.to_owned(),
            port,
        };
        let p = self.add_device_with_deferred(connector, user, password)?;
        debug!(device = p.id(), host, port, "device address");
        Ok(p.publish())
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
        let connector = HostConnector {
            host: host.to_owned(),
            port,
        };
        let p = self.add_device_with_deferred(connector, user, password)?;
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
        let mut cfg = bc_session::SessionConfig::new(user, password);
        cfg.backoff = self.config.backoff;
        cfg.queue_frames = self.config.queue_frames.max(1);
        let events = self.event_sink.clone();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
        let handle = {
            let _rt = self.handle.enter();
            let session = bc_session::DeviceSession::spawn(connector, cfg);
            DeviceHandle::start(id, session, events, gate_rx)
        };
        reg.devices.insert(id, Device { handle });
        info!(device = id, "device added");
        Ok(Pending {
            id,
            gate: Some(gate_tx),
        })
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
        self.end_tiles(tiles, CloseReason::DeviceRemoved);
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
        let session = reg
            .devices
            .get(&device)
            .ok_or(CoreError::UnknownId)?
            .handle
            .session();
        if reg.tiles.len() >= self.config.max_tiles {
            return Err(CoreError::Limit);
        }
        // A session that has ended (refused login) yields a tile that ends at
        // once with `login_failed`, once the app has its ID.
        let sub = match session {
            Some(s) => self.handle.block_on(s.subscribe(channel, tier)).ok(),
            None => None,
        };
        let id = self.new_id();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
        let (tile, sink) = tile::spawn(id, device, sub, self.tile_ctx.clone(), gate_rx)
            .map_err(|_| CoreError::Internal)?;
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

    /// Closes a tile. No `on_action` for it runs after this returns.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`], [`CoreError::Closed`],
    /// [`CoreError::InvalidArgument`] from inside a callback.
    pub fn close_tile(&self, tile: u64) -> Result<(), CoreError> {
        self.check_api()?;
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
        for (_, mut t) in tiles {
            t.end(reason);
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

    /// The platform decoded the picture with this timestamp. Any thread;
    /// never waits for the tile.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`] for a closed tile, [`CoreError::Closed`].
    pub fn decoded(&self, tile: u64, pts_us: u64) -> Result<(), CoreError> {
        self.send_result(tile, DecoderResult::Decoded(pts_us))
    }

    /// The platform could not carry out a `Configure` or a `Decode`. Any
    /// thread; never waits for the tile.
    ///
    /// # Errors
    /// [`CoreError::UnknownId`] for a closed tile, [`CoreError::Closed`].
    pub fn decode_failed(&self, tile: u64, kind: DecodeFailureKind) -> Result<(), CoreError> {
        self.send_result(tile, DecoderResult::Failed(kind))
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
        // Their tile_closed events are delivered before this returns.
        self.end_tiles(tiles, CloseReason::CoreShutdown);
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
