//! Recorded playback (docs/FFI-RECORDINGS.md, section 6; docs/MILESTONE-P.md,
//! slice C10): playbacks of up to four cameras, of one device or several,
//! on one shared clock, each camera shown by a playback tile.
//!
//! **Tiles.** A playback tile is a tile like a live one (IDs from the same
//! space, the same `on_action` actions and answering rules, generations,
//! keyframe gating, audio, `tile_stats`, `tile_closed`); only its media
//! comes from the playback engine instead of a live stream
//! (`crate::tile::Source::Feed`). Generation 0 is an empty placeholder;
//! every seek is a new generation of every tile, shown once its picture at
//! the target was decoded, when the old generation gets its final `Reset`.
//! [`crate::Core::close_tile`] refuses a playback tile
//! ([`crate::CoreError::InvalidArgument`]): remove it from its playback.
//! [`crate::Core::set_tile_tier`] on a playback tile restarts its device's
//! plays at the position in the new tier.
//!
//! **The engine** ([`engine`]) runs one task per playback: the clock,
//! the plays (started per device as a group at one seek time, as the
//! device needs), pacing, synchronization, buffering, ends, and the
//! events. Live view keeps running beside it on the same sessions.
//!
//! **Calls.** The Rust calls on [`crate::Core`] mirror section 6 and answer
//! with [`RecError`] like the other recordings calls; the C functions are
//! slice F2. Events: `playback_state`, `playback_camera`, `playback_seek`,
//! `playback_closed`.
//!
//! **Not done here.** Stepping (`nv_playback_step`) and stills from a
//! playback (section 7a) are not implemented; a playback needs the
//! cameras' days in the recordings index (`nv_recordings_want`): a camera
//! whose day is not indexed shows `no_recording`.

mod buffer;
mod clock;
mod engine;
pub(crate) mod feed;

use crate::events::Priority;
use crate::recordings_owner::RecError;
use crate::tile::{self, CloseReason, Source, TileHandle, TileStart};
use crate::{Core, CoreError, StreamTier, in_callback, lock};
use engine::{CamSetup, Cmd, DevSetup, Engine, EngineSetup, RATES, Tuning};
use recordings::{DeviceCamera, Tier};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// Playbacks open at once.
pub const MAX_PLAYBACKS: usize = 8;

/// Cameras of one playback.
pub const MAX_CAMERAS: usize = 4;

/// Longest range of one playback.
pub const MAX_RANGE_MS: i64 = 24 * 3600 * 1000;

/// Requests waiting for a playback's engine.
const COMMANDS_CAPACITY: usize = 64;

/// Longest a call waits for the engine (closing, removing a camera).
const CALL_WAIT: Duration = Duration::from_secs(10);

/// How a seek shows its target (`NvSeekMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeekMode {
    /// Show the keyframe at or before the position first (fast; scrubbing).
    Keyframe = 0,
    /// Decode from that keyframe and show the picture at the position.
    Exact = 1,
}

/// A playback's state (`NvPlaybackState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    /// Playing, but the clock waits for a camera.
    Buffering = 0,
    Playing = 1,
    Paused = 2,
    /// At the end of the range or of the recordings.
    Ended = 3,
    /// Every camera failed.
    Failed = 4,
}

/// Whether the clock waits for late cameras.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyncMode {
    Wait,
    Free,
}

pub(crate) fn state_code(name: &str) -> u8 {
    match name {
        "buffering" => 0,
        "playing" => 1,
        "ended" => 3,
        "failed" => 4,
        _ => 2,
    }
}

fn state_of(code: u8) -> PlaybackState {
    match code {
        0 => PlaybackState::Buffering,
        1 => PlaybackState::Playing,
        3 => PlaybackState::Ended,
        4 => PlaybackState::Failed,
        _ => PlaybackState::Paused,
    }
}

/// What the engine publishes for lock-free reads.
pub(crate) struct PbShared {
    position_ms: AtomicI64,
    state: AtomicU8,
}

impl PbShared {
    pub(crate) fn publish(&self, position_ms: i64, state: u8) {
        self.position_ms.store(position_ms, Ordering::Relaxed);
        self.state.store(state, Ordering::Relaxed);
    }
}

struct PbTile {
    id: u64,
    device: String,
    camera: DeviceCamera,
    handle: TileHandle,
}

struct Pb {
    cmd: mpsc::Sender<Cmd>,
    task: Option<JoinHandle<()>>,
    shared: Arc<PbShared>,
    tiles: Vec<PbTile>,
}

/// What the engines share: which playback owns each device, the
/// per-device start locks, and every playback.
pub(crate) struct HubCore {
    owners: Mutex<HashMap<String, u64>>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    playbacks: Mutex<HashMap<u64, Pb>>,
    closing: AtomicBool,
}

impl HubCore {
    /// The lock that serializes starts of plays on a device.
    pub(crate) fn device_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.locks).entry(key.to_owned()).or_default().clone()
    }

    /// Makes `playback` the device's owner; an older owner's cameras of
    /// that device stop (it is waited for).
    pub(crate) async fn claim(&self, key: &str, playback: u64) {
        let prev = lock(&self.owners).insert(key.to_owned(), playback);
        let Some(q) = prev.filter(|q| *q != playback) else {
            return;
        };
        let tx = lock(&self.playbacks).get(&q).map(|p| p.cmd.clone());
        if let Some(tx) = tx {
            let (a, r) = oneshot::channel();
            if tx.send(Cmd::Evict(key.to_owned(), a)).await.is_ok() {
                let _ = r.await;
            }
        }
    }

    /// Gives the device up if `playback` owns it.
    pub(crate) fn release(&self, key: &str, playback: u64) {
        let mut o = lock(&self.owners);
        if o.get(key) == Some(&playback) {
            o.remove(key);
        }
    }
}

/// The core's playbacks.
pub(crate) struct PlaybackHub {
    core: Arc<HubCore>,
}

impl PlaybackHub {
    pub(crate) fn new() -> PlaybackHub {
        PlaybackHub {
            core: Arc::new(HubCore {
                owners: Mutex::new(HashMap::new()),
                locks: Mutex::new(HashMap::new()),
                playbacks: Mutex::new(HashMap::new()),
                closing: AtomicBool::new(false),
            }),
        }
    }

    fn find_tile(&self, tile: u64) -> Option<(u64, mpsc::Sender<Cmd>)> {
        lock(&self.core.playbacks)
            .iter()
            .find(|(_, p)| p.tiles.iter().any(|t| t.id == tile))
            .map(|(id, p)| (*id, p.cmd.clone()))
    }

    /// Whether `tile` is a playback tile.
    pub(crate) fn has_tile(&self, tile: u64) -> bool {
        self.find_tile(tile).is_some()
    }

    fn tile_count(&self) -> usize {
        lock(&self.core.playbacks)
            .values()
            .map(|p| p.tiles.len())
            .sum()
    }
}

/// Held back IDs of a playback opened with
/// [`Core::playback_open_deferred`]: nothing naming the playback or its
/// tiles is delivered until [`PlaybackPending::publish`] (or until it is
/// dropped).
#[derive(Debug)]
pub struct PlaybackPending {
    gates: Vec<oneshot::Sender<()>>,
}

impl PlaybackPending {
    /// Lets the playback's tiles and events go.
    pub fn publish(self) {
        for g in self.gates {
            let _ = g.send(());
        }
    }
}

/// One camera of a request.
struct CamReq {
    device: String,
    camera: DeviceCamera,
    tier: Tier,
    offset_ms: i64,
}

/// A parsed `nv_playback_open` request.
struct OpenReq {
    start_ms: i64,
    end_ms: i64,
    position_ms: i64,
    rate: f64,
    sync: SyncMode,
    cameras: Vec<CamReq>,
}

fn int(m: &Map<String, Value>, key: &str) -> Result<Option<i64>, RecError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_i64().map(Some).ok_or(RecError::InvalidArgument),
    }
}

fn tier_of(m: &Map<String, Value>, default: Tier) -> Result<Tier, RecError> {
    match m.get("tier") {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(s)) if s == "sub" => Ok(Tier::Sub),
        Some(Value::String(s)) if s == "main" => Ok(Tier::Main),
        Some(_) => Err(RecError::InvalidArgument),
    }
}

fn camera_of(v: &Value) -> Result<(String, DeviceCamera), RecError> {
    let o = v.as_object().ok_or(RecError::InvalidArgument)?;
    let device = match o.get("device") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err(RecError::InvalidArgument),
    };
    let channel = o
        .get("channel")
        .and_then(Value::as_u64)
        .filter(|c| *c <= u64::from(u8::MAX))
        .ok_or(RecError::InvalidArgument)? as u8;
    let lens = match o.get("lens") {
        None | Some(Value::Null) => 0,
        Some(v) => v
            .as_u64()
            .filter(|l| *l <= 1)
            .ok_or(RecError::InvalidArgument)? as u8,
    };
    Ok((device, DeviceCamera::new(channel, lens)))
}

/// A camera entry: `{"camera": {...}, "tier": "sub", "offset_ms": 0}`.
fn cam_req(v: &Value, default_tier: Tier) -> Result<CamReq, RecError> {
    let o = v.as_object().ok_or(RecError::InvalidArgument)?;
    let (device, camera) = camera_of(o.get("camera").ok_or(RecError::InvalidArgument)?)?;
    Ok(CamReq {
        device,
        camera,
        tier: tier_of(o, default_tier)?,
        offset_ms: int(o, "offset_ms")?.unwrap_or(0),
    })
}

fn object(json: &str) -> Result<Map<String, Value>, RecError> {
    match serde_json::from_str(json) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(RecError::InvalidArgument),
    }
}

fn check_rate(rate: f64) -> Result<f64, RecError> {
    RATES
        .iter()
        .copied()
        .find(|r| (r - rate).abs() < 1e-9)
        .ok_or(RecError::InvalidArgument)
}

fn closed_event(id: u64, reason: &str) -> String {
    json!({"type": "playback_closed", "playback": id, "reason": reason}).to_string()
}

// ---- the Rust API that the C interface (F2) wraps ------------------------

impl Core {
    fn pb_check(&self) -> Result<(), RecError> {
        if in_callback::on_core_thread() {
            return Err(RecError::InvalidArgument);
        }
        if self.closing.load(Ordering::SeqCst) || self.playback.core.closing.load(Ordering::SeqCst)
        {
            return Err(RecError::Closed);
        }
        Ok(())
    }

    fn parse_open(&self, json: &str) -> Result<OpenReq, RecError> {
        let m = object(json)?;
        let default_tier = tier_of(&m, Tier::Sub)?;
        let (start_ms, end_ms, cameras) = if let Some(k) = m.get("incident") {
            let key = k.as_str().ok_or(RecError::InvalidArgument)?;
            let inc: Value =
                serde_json::from_str(&self.incident_get(key)?).map_err(|_| RecError::Internal)?;
            let inc = &inc["incident"];
            let margin = inc["margin_s"].as_i64().unwrap_or(0);
            let start = inc["start"].as_i64().ok_or(RecError::Internal)?;
            let end = inc["end"].as_i64().ok_or(RecError::Internal)?;
            let cams = inc["cameras"]
                .as_array()
                .ok_or(RecError::Internal)?
                .iter()
                .map(|c| {
                    let (device, camera) = camera_of(&c["camera"])?;
                    Ok(CamReq {
                        device,
                        camera,
                        tier: default_tier,
                        offset_ms: 0,
                    })
                })
                .collect::<Result<Vec<_>, RecError>>()?;
            ((start - margin) * 1000, (end + margin + 1) * 1000, cams)
        } else {
            let start = int(&m, "start_ms")?.ok_or(RecError::InvalidArgument)?;
            let end = int(&m, "end_ms")?.ok_or(RecError::InvalidArgument)?;
            let cams = m
                .get("cameras")
                .and_then(Value::as_array)
                .ok_or(RecError::InvalidArgument)?
                .iter()
                .map(|v| cam_req(v, default_tier))
                .collect::<Result<Vec<_>, RecError>>()?;
            (start, end, cams)
        };
        if start_ms < 0 || end_ms <= start_ms || cameras.is_empty() {
            return Err(RecError::InvalidArgument);
        }
        if end_ms - start_ms > MAX_RANGE_MS || cameras.len() > MAX_CAMERAS {
            return Err(RecError::Limit);
        }
        for (i, c) in cameras.iter().enumerate() {
            if cameras[..i]
                .iter()
                .any(|o| o.device == c.device && o.camera == c.camera)
            {
                return Err(RecError::InvalidArgument);
            }
        }
        let position_ms = int(&m, "position_ms")?
            .unwrap_or(start_ms)
            .clamp(start_ms, end_ms);
        let rate = match m.get("rate") {
            None | Some(Value::Null) => 1.0,
            Some(v) => check_rate(v.as_f64().ok_or(RecError::InvalidArgument)?)?,
        };
        let sync = match m.get("sync") {
            None | Some(Value::Null) => SyncMode::Wait,
            Some(Value::String(s)) if s == "wait" => SyncMode::Wait,
            Some(Value::String(s)) if s == "free" => SyncMode::Free,
            Some(_) => return Err(RecError::InvalidArgument),
        };
        Ok(OpenReq {
            start_ms,
            end_ms,
            position_ms,
            rate,
            sync,
            cameras,
        })
    }

    /// The device of a camera, for the engine, and the core's ID for it.
    fn pb_device(&self, c: &CamReq) -> Result<(DevSetup, u64), RecError> {
        let d = self.recordings.playback_device(&c.device)?;
        Ok((
            DevSetup {
                key: c.device.clone(),
                session: d.session,
                utc_offset_s: d.utc_offset_s,
                index: d.index,
            },
            d.id.unwrap_or(0),
        ))
    }

    /// Starts a playback tile (held back by its gate).
    fn pb_tile(
        &self,
        device: u64,
        c: &CamReq,
    ) -> Result<(u64, TileHandle, oneshot::Sender<()>), RecError> {
        let id = self.new_id();
        let (gate_tx, gate_rx) = oneshot::channel();
        let control = self.tile_ctx.audio.add_tile(id);
        let start = TileStart {
            id,
            device,
            channel: c.camera.channel,
            tier: c.camera.stream(c.tier),
            playback: true,
        };
        match tile::spawn(
            start,
            Some(Source::Placeholder),
            self.tile_ctx.clone(),
            control,
            gate_rx,
        ) {
            Ok((handle, sink)) => {
                lock(&self.sinks).insert(id, sink);
                Ok((id, handle, gate_tx))
            }
            Err(_) => {
                self.tile_ctx.audio.remove_tile(id);
                Err(RecError::Internal)
            }
        }
    }

    fn pb_tiles_fit(&self, n: usize) -> Result<(), RecError> {
        let live = lock(&self.registry).tiles.len();
        if live + self.playback.tile_count() + n > self.config.max_tiles {
            Err(RecError::Limit)
        } else {
            Ok(())
        }
    }

    /// `nv_playback_open`: opens a playback (docs/FFI-RECORDINGS.md,
    /// section 6), paused at `position_ms` with an exact seek. JSON in:
    /// `{"start_ms", "end_ms", "position_ms", "rate", "sync", "cameras":
    /// [{"camera": {"device", "channel", "lens"}, "tier": "sub"|"main",
    /// "offset_ms"}]}`, or `{"incident": key, "tier"}` for an incident's
    /// cameras and range with its margins. Times are device-local
    /// milliseconds. Answers `{"playback": N, "tiles": [...]}`, one tile per
    /// camera in request order. The cameras' devices must be enabled for
    /// recordings (their index says which file holds which time).
    ///
    /// # Errors
    /// [`RecError::InvalidArgument`] for invalid JSON, a rate other than
    /// 0.25, 0.5, 1, 2, 4, 8, or 16, a camera twice, or a camera of a
    /// device that is not enabled; [`RecError::Limit`] beyond
    /// [`MAX_PLAYBACKS`], [`MAX_CAMERAS`], [`MAX_RANGE_MS`], or the core's
    /// tile limit; [`RecError::NotConfigured`] before
    /// `recordings_configure`; [`RecError::UnknownId`] for an unknown
    /// incident; [`RecError::Closed`].
    pub fn playback_open(&self, json: &str) -> Result<String, RecError> {
        let (answer, pending) = self.playback_open_deferred(json)?;
        pending.publish();
        Ok(answer)
    }

    /// [`Core::playback_open`], holding back the playback's tiles and
    /// events until the returned [`PlaybackPending`] is published.
    ///
    /// # Errors
    /// As [`Core::playback_open`].
    pub fn playback_open_deferred(
        &self,
        json: &str,
    ) -> Result<(String, PlaybackPending), RecError> {
        self.pb_check()?;
        // Refusals are logged (user-paced, one per open) so a refused open
        // says why.
        let req = self
            .parse_open(json)
            .inspect_err(|e| info!(error = ?e, "playback refused: the request"))?;
        if lock(&self.playback.core.playbacks).len() >= MAX_PLAYBACKS {
            info!("playback refused: too many playbacks");
            return Err(RecError::Limit);
        }
        self.pb_tiles_fit(req.cameras.len())
            .inspect_err(|_| info!("playback refused: the tile limit"))?;
        let mut devices: Vec<DevSetup> = Vec::new();
        let mut ids = Vec::new();
        for c in &req.cameras {
            let (d, id) = self
                .pb_device(c)
                .inspect_err(|e| info!(error = ?e, "playback refused: a camera's device"))?;
            if !devices.iter().any(|o| o.key == d.key) {
                devices.push(d);
            }
            ids.push(id);
        }
        let id = self.new_id();
        let mut gates = Vec::new();
        let mut tiles = Vec::new();
        let mut cams = Vec::new();
        for (c, device) in req.cameras.iter().zip(ids) {
            let (tile, handle, gate) = match self.pb_tile(device, c) {
                Ok(t) => t,
                Err(e) => {
                    let opened: Vec<(u64, TileHandle)> = tiles
                        .into_iter()
                        .map(|t: PbTile| (t.id, t.handle))
                        .collect();
                    drop(gates);
                    self.end_tiles(opened, CloseReason::Closed);
                    return Err(e);
                }
            };
            cams.push(CamSetup {
                tile,
                device: c.device.clone(),
                camera: c.camera,
                tier: c.tier,
                offset_ms: c.offset_ms,
                commands: handle.commands(),
            });
            tiles.push(PbTile {
                id: tile,
                device: c.device.clone(),
                camera: c.camera,
                handle,
            });
            gates.push(gate);
        }
        let shared = Arc::new(PbShared {
            position_ms: AtomicI64::new(req.position_ms),
            state: AtomicU8::new(state_code("paused")),
        });
        let engine = Engine::new(EngineSetup {
            id,
            hub: self.playback.core.clone(),
            events: self.event_sink.clone(),
            clock: self.clock.clone(),
            tuning: Tuning::default(),
            shared: shared.clone(),
            start_ms: req.start_ms,
            end_ms: req.end_ms,
            position_ms: req.position_ms,
            rate: req.rate,
            sync: req.sync,
            cams,
            devices,
        });
        let (cmd_tx, cmd_rx) = mpsc::channel(COMMANDS_CAPACITY);
        let (gate_tx, gate_rx) = oneshot::channel();
        let task = self.handle.spawn(engine.run(cmd_rx, gate_rx));
        gates.push(gate_tx);
        let tile_ids: Vec<u64> = tiles.iter().map(|t| t.id).collect();
        lock(&self.playback.core.playbacks).insert(
            id,
            Pb {
                cmd: cmd_tx,
                task: Some(task),
                shared,
                tiles,
            },
        );
        info!(playback = id, tiles = ?tile_ids, "playback opening");
        Ok((
            json!({"playback": id, "tiles": tile_ids}).to_string(),
            PlaybackPending { gates },
        ))
    }

    fn pb_cmd(&self, playback: u64) -> Result<mpsc::Sender<Cmd>, RecError> {
        self.pb_check()?;
        lock(&self.playback.core.playbacks)
            .get(&playback)
            .map(|p| p.cmd.clone())
            .ok_or(RecError::UnknownId)
    }

    fn pb_send(&self, playback: u64, c: Cmd) -> Result<(), RecError> {
        self.pb_cmd(playback)?
            .try_send(c)
            .map_err(|_| RecError::Limit)
    }

    /// Waits (bounded) for an engine's answer.
    fn pb_wait<T>(&self, r: oneshot::Receiver<T>) -> Option<T> {
        self.handle
            .block_on(async { tokio::time::timeout(CALL_WAIT, r).await.ok()?.ok() })
    }

    /// `nv_playback_add_camera`: adds a camera (`{"camera": {...}, "tier":
    /// "sub", "offset_ms": 0}`); it joins at the current position (its
    /// device's plays start again together). Returns its tile.
    ///
    /// # Errors
    /// [`RecError::UnknownId`], [`RecError::InvalidArgument`] (also for a
    /// camera already in the playback), [`RecError::Limit`] beyond
    /// [`MAX_CAMERAS`], [`RecError::Closed`].
    pub fn playback_add_camera(&self, playback: u64, json: &str) -> Result<u64, RecError> {
        let (tile, pending) = self.playback_add_camera_deferred(playback, json)?;
        pending.publish();
        Ok(tile)
    }

    /// [`Core::playback_add_camera`], holding back the new tile's actions
    /// until the returned [`PlaybackPending`] is published (the C function
    /// writes the tile's ID first).
    ///
    /// # Errors
    /// As [`Core::playback_add_camera`].
    pub fn playback_add_camera_deferred(
        &self,
        playback: u64,
        json: &str,
    ) -> Result<(u64, PlaybackPending), RecError> {
        let cmd = self.pb_cmd(playback)?;
        let c = cam_req(&Value::Object(object(json)?), Tier::Sub)?;
        {
            let pbs = lock(&self.playback.core.playbacks);
            let p = pbs.get(&playback).ok_or(RecError::UnknownId)?;
            if p.tiles
                .iter()
                .any(|t| t.device == c.device && t.camera == c.camera)
            {
                return Err(RecError::InvalidArgument);
            }
            if p.tiles.len() >= MAX_CAMERAS {
                return Err(RecError::Limit);
            }
        }
        self.pb_tiles_fit(1)?;
        let (dev, device) = self.pb_device(&c)?;
        let (tile, handle, gate) = self.pb_tile(device, &c)?;
        let setup = CamSetup {
            tile,
            device: c.device.clone(),
            camera: c.camera,
            tier: c.tier,
            offset_ms: c.offset_ms,
            commands: handle.commands(),
        };
        let mut pbs = lock(&self.playback.core.playbacks);
        let Some(p) = pbs.get_mut(&playback) else {
            drop(pbs);
            self.end_tiles(vec![(tile, handle)], CloseReason::Closed);
            return Err(RecError::UnknownId);
        };
        if p.tiles
            .iter()
            .any(|t| t.device == c.device && t.camera == c.camera)
        {
            // Added by another call meanwhile.
            drop(pbs);
            self.end_tiles(vec![(tile, handle)], CloseReason::Closed);
            return Err(RecError::InvalidArgument);
        }
        p.tiles.push(PbTile {
            id: tile,
            device: c.device,
            camera: c.camera,
            handle,
        });
        drop(pbs);
        if cmd.try_send(Cmd::Add(setup, Some(dev))).is_err() {
            warn!(playback, "playback busy: camera not added");
        }
        Ok((tile, PlaybackPending { gates: vec![gate] }))
    }

    /// `nv_playback_remove_camera`: removes one camera; its tile closes
    /// (`tile_closed`, reason `closed`) and its play stops.
    ///
    /// # Errors
    /// [`RecError::UnknownId`], [`RecError::Closed`],
    /// [`RecError::InvalidArgument`] from inside a callback.
    pub fn playback_remove_camera(&self, playback: u64, tile: u64) -> Result<(), RecError> {
        let cmd = self.pb_cmd(playback)?;
        let t = {
            let mut pbs = lock(&self.playback.core.playbacks);
            let p = pbs.get_mut(&playback).ok_or(RecError::UnknownId)?;
            let i = p
                .tiles
                .iter()
                .position(|t| t.id == tile)
                .ok_or(RecError::UnknownId)?;
            p.tiles.remove(i)
        };
        let (a, r) = oneshot::channel();
        if self.handle.block_on(cmd.send(Cmd::Remove(tile, a))).is_ok() {
            self.pb_wait(r);
        }
        self.end_tiles(vec![(t.id, t.handle)], CloseReason::Closed);
        Ok(())
    }

    /// `nv_playback_play`: starts the clock (from `start_ms` after the
    /// end).
    ///
    /// # Errors
    /// [`RecError::UnknownId`], [`RecError::Limit`] when too many requests
    /// wait, [`RecError::Closed`].
    pub fn playback_play(&self, playback: u64) -> Result<(), RecError> {
        self.pb_send(playback, Cmd::Play)
    }

    /// `nv_playback_pause`: holds the clock (the plays fill their buffers,
    /// then stop until it runs again).
    ///
    /// # Errors
    /// As [`Core::playback_play`].
    pub fn playback_pause(&self, playback: u64) -> Result<(), RecError> {
        self.pb_send(playback, Cmd::Pause)
    }

    /// `nv_playback_seek`: moves the shared clock to `position_ms` (clamped
    /// to the range). Returns at once; every tile starts a new generation,
    /// and a newer seek replaces one still starting.
    ///
    /// # Errors
    /// As [`Core::playback_play`].
    pub fn playback_seek(
        &self,
        playback: u64,
        position_ms: i64,
        mode: SeekMode,
    ) -> Result<(), RecError> {
        self.pb_send(playback, Cmd::Seek(position_ms, mode))
    }

    /// `nv_playback_set_rate`: 0.25, 0.5, 1, 2, 4, 8, or 16. The plays
    /// restart at the position with the device's `playSpeed` (1 below 1x).
    ///
    /// # Errors
    /// [`RecError::InvalidArgument`] for another rate; as
    /// [`Core::playback_play`].
    pub fn playback_set_rate(&self, playback: u64, rate: f64) -> Result<(), RecError> {
        let rate = check_rate(rate)?;
        self.pb_send(playback, Cmd::Rate(rate))
    }

    /// `nv_playback_set_offset`: this camera shows position + `offset_ms`
    /// (its device's plays restart).
    ///
    /// # Errors
    /// [`RecError::UnknownId`] for a tile not in the playback; as
    /// [`Core::playback_play`].
    pub fn playback_set_offset(
        &self,
        playback: u64,
        tile: u64,
        offset_ms: i64,
    ) -> Result<(), RecError> {
        let known = lock(&self.playback.core.playbacks)
            .get(&playback)
            .is_some_and(|p| p.tiles.iter().any(|t| t.id == tile));
        if !known {
            return Err(RecError::UnknownId);
        }
        if offset_ms.abs() > MAX_RANGE_MS {
            return Err(RecError::InvalidArgument);
        }
        self.pb_send(playback, Cmd::Offset(tile, offset_ms))
    }

    /// `nv_playback_position`: the position (device-local ms) and state,
    /// as of the engine's last step (every 10 ms). Takes one brief lock;
    /// callable from any thread, including callbacks.
    ///
    /// # Errors
    /// [`RecError::UnknownId`].
    pub fn playback_position(&self, playback: u64) -> Result<(i64, PlaybackState), RecError> {
        let pbs = lock(&self.playback.core.playbacks);
        let p = pbs.get(&playback).ok_or(RecError::UnknownId)?;
        Ok((
            p.shared.position_ms.load(Ordering::Relaxed),
            state_of(p.shared.state.load(Ordering::Relaxed)),
        ))
    }

    /// The playback's details: `{"playback", "state", "position_ms",
    /// "rate", "sync", "start_ms", "end_ms", "cameras": [{"tile", "camera",
    /// "tier", "offset_ms", "state", "reason"?, "generation"}]}`.
    ///
    /// # Errors
    /// [`RecError::UnknownId`], [`RecError::Closed`],
    /// [`RecError::InvalidArgument`] from inside a callback.
    pub fn playback_status(&self, playback: u64) -> Result<String, RecError> {
        let cmd = self.pb_cmd(playback)?;
        let (a, r) = oneshot::channel();
        cmd.try_send(Cmd::Status(a)).map_err(|_| RecError::Limit)?;
        self.pb_wait(r)
            .map(|v| v.to_string())
            .ok_or(RecError::Internal)
    }

    /// `nv_playback_close`: stops every play, closes every tile of it
    /// (`tile_closed` for each), then `playback_closed` (`closed`).
    ///
    /// # Errors
    /// [`RecError::UnknownId`], [`RecError::Closed`],
    /// [`RecError::InvalidArgument`] from inside a callback.
    pub fn playback_close(&self, playback: u64) -> Result<(), RecError> {
        self.pb_check()?;
        let p = lock(&self.playback.core.playbacks)
            .remove(&playback)
            .ok_or(RecError::UnknownId)?;
        self.pb_finish(playback, p, CloseReason::Closed, "closed");
        Ok(())
    }

    /// Stops a playback's engine (every play stopped), closes its tiles,
    /// and reports it closed.
    fn pb_finish(&self, id: u64, mut p: Pb, reason: CloseReason, name: &str) {
        let (a, r) = oneshot::channel();
        if self.handle.block_on(p.cmd.send(Cmd::Close(a))).is_ok() {
            self.pb_wait(r);
        }
        if let Some(t) = p.task.take() {
            let _ = self
                .handle
                .block_on(async { tokio::time::timeout(CALL_WAIT, t).await });
        }
        let tiles: Vec<(u64, TileHandle)> = p.tiles.into_iter().map(|t| (t.id, t.handle)).collect();
        self.end_tiles(tiles, reason);
        info!(playback = id, reason = name, "playback closed");
        self.event_sink
            .send(closed_event(id, name), Priority::State);
    }

    /// A tier switch of a playback tile, if `tile` is one.
    pub(crate) fn playback_tile_tier(
        &self,
        tile: u64,
        tier: StreamTier,
    ) -> Option<Result<(), CoreError>> {
        let (_, cmd) = self.playback.find_tile(tile)?;
        let (_, t) = recordings::lens_and_tier(tier);
        Some(
            cmd.try_send(Cmd::Tier(tile, t))
                .map_err(|_| CoreError::Limit),
        )
    }

    /// A device is being removed (before its session closes): its cameras
    /// leave their playbacks (`tile_closed` `device_removed`); a playback
    /// of that device alone closes (`playback_closed` `device_removed`).
    pub(crate) fn playback_device_removed(&self, key: &str) {
        let ids: Vec<u64> = lock(&self.playback.core.playbacks)
            .iter()
            .filter(|(_, p)| p.tiles.iter().any(|t| t.device == key))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let mut pbs = lock(&self.playback.core.playbacks);
            let Some(p) = pbs.get_mut(&id) else { continue };
            if p.tiles.iter().all(|t| t.device == key) {
                let p = pbs.remove(&id).expect("present");
                drop(pbs);
                self.pb_finish(id, p, CloseReason::DeviceRemoved, "device_removed");
                continue;
            }
            let cmd = p.cmd.clone();
            let (gone, kept): (Vec<PbTile>, Vec<PbTile>) = std::mem::take(&mut p.tiles)
                .into_iter()
                .partition(|t| t.device == key);
            p.tiles = kept;
            drop(pbs);
            let (a, r) = oneshot::channel();
            if self
                .handle
                .block_on(cmd.send(Cmd::DeviceGone(key.to_owned(), a)))
                .is_ok()
            {
                self.pb_wait(r);
            }
            let tiles = gone.into_iter().map(|t| (t.id, t.handle)).collect();
            self.end_tiles(tiles, CloseReason::DeviceRemoved);
        }
    }

    /// Closes every playback (`core_shutdown`) before the sessions close.
    pub(crate) fn playback_shutdown(&self) {
        self.playback.core.closing.store(true, Ordering::SeqCst);
        let all: Vec<(u64, Pb)> = lock(&self.playback.core.playbacks).drain().collect();
        for (id, p) in all {
            self.pb_finish(id, p, CloseReason::CoreShutdown, "core_shutdown");
        }
    }
}
