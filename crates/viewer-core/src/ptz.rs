//! Camera motion control from tiles: pan and tilt, preset recall, and lens
//! zoom and focus (docs/FFI.md, "Camera motion (PTZ)").
//!
//! The core hands each call to the tile's device session in call order
//! ([`bc_session::DeviceSession::ptz_send`]), so a release is never
//! overtaken by its move, and reports the outcome as a `ptz` event. Every
//! safety rule (origins and grants, rate limits, the dead-man timer and
//! maximum hold, the stop on every path, logging) lives in the session;
//! this module adds the tile's side: a tile that started a move and closes
//! (alone, with its window, its device, or the core) sends a stop, and the
//! session's own stops (timers, reconnects, refusals) are reported on the
//! tile that started the move.

use crate::events::{EventSink, Priority};
use crate::json::Object;
use bc_protocol::ptz::{Cam, LensRange, MAX_SPEED, MIN_SPEED};
use bc_session::{
    DeviceSession, MotionOrigin, PtzCommand, PtzInfo, PtzOutcome, PtzRefusal, StopCause,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use tokio::runtime::Handle;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tracing::debug;

/// The move speeds the core accepts (the official client's slider).
pub const SPEEDS: std::ops::RangeInclusive<u8> = MIN_SPEED..=MAX_SPEED;

/// Longest automation name ([`crate::Core::set_motion_grant`]).
pub const MAX_CLIENT_NAME: usize = 64;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The tile that last moved a camera, while its move may be running.
struct Mover {
    tile: u64,
    session: Weak<DeviceSession>,
}

#[derive(Default)]
struct State {
    /// By (device, camera).
    movers: HashMap<(u64, Cam), Mover>,
    /// One task per device reporting the session's own stops.
    watchers: HashMap<u64, JoinHandle<()>>,
}

/// Every tile's camera motion.
pub(crate) struct PtzHub {
    state: Arc<Mutex<State>>,
    events: EventSink,
    runtime: Handle,
}

fn event(tile: u64, action: &str, state: &str) -> Object {
    Object::event("ptz")
        .u64("tile", tile)
        .str("action", action)
        .str("state", state)
}

fn outcome_event(tile: u64, action: &str, r: Result<PtzOutcome, PtzRefusal>) -> Option<Object> {
    Some(match r {
        Ok(PtzOutcome::Renewed) => return None,
        Ok(PtzOutcome::Lens(pos)) => event(tile, action, "sent").i64("position", i64::from(pos)),
        Ok(PtzOutcome::Stopped) => event(tile, action, "sent").str("reason", "released"),
        Ok(_) => event(tile, action, "sent"),
        Err(e) if e.is_refusal() => {
            let o = event(tile, action, "refused").str("reason", e.name());
            match e {
                PtzRefusal::RateLimited { retry_after } => {
                    o.u64("retry_after_ms", retry_after.as_millis() as u64)
                }
                _ => o,
            }
        }
        Err(e) => {
            let o = event(tile, action, "failed").str("reason", e.name());
            match e {
                PtzRefusal::DeviceRefused(status) => o.u64("status", u64::from(status)),
                _ => o,
            }
        }
    })
}

fn range(r: Option<LensRange>) -> Option<Object> {
    r.map(|r| {
        Object::new()
            .i64("min", i64::from(r.min))
            .i64("max", i64::from(r.max))
            .i64("cur", i64::from(r.cur))
    })
}

/// The `nv_ptz_info` JSON for one camera.
pub(crate) fn info_json(i: &PtzInfo) -> String {
    let o = Object::new().bool("pan_tilt", i.caps.pan_tilt);
    let o = match range(i.zoom) {
        Some(z) => o.object("zoom", z),
        None => o.null("zoom"),
    };
    let o = match range(i.focus) {
        Some(f) => o.object("focus", f),
        None => o.null("focus"),
    };
    o.objects(
        "presets",
        i.presets.iter().map(|p| {
            Object::new()
                .u64("id", u64::from(p.id))
                .str("name", &p.name)
        }),
    )
    .u64s("speeds", [u64::from(MIN_SPEED), u64::from(MAX_SPEED)])
    .finish()
}

impl PtzHub {
    pub(crate) fn new(events: EventSink, runtime: Handle) -> PtzHub {
        PtzHub {
            state: Arc::new(Mutex::new(State::default())),
            events,
            runtime,
        }
    }

    /// Reports the session's own stops for `device` on the tile that
    /// started the move (started once per device).
    fn watch(&self, device: u64, session: &Arc<DeviceSession>) {
        let mut st = lock(&self.state);
        if st.watchers.get(&device).is_some_and(|j| !j.is_finished()) {
            return;
        }
        let mut rx = session.ptz_notices();
        let state = Arc::downgrade(&self.state);
        let events = self.events.clone();
        let task = self.runtime.spawn(async move {
            loop {
                let n = match rx.recv().await {
                    Ok(n) => n,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return,
                };
                // The caller's own release is reported by its call.
                if n.cause == StopCause::Released {
                    continue;
                }
                let Some(state) = state.upgrade() else {
                    return;
                };
                let tile = lock(&state).movers.remove(&(device, n.cam)).map(|m| m.tile);
                if let Some(tile) = tile {
                    events.send(
                        event(tile, "stop", "sent")
                            .str("reason", n.cause.name())
                            .finish(),
                        Priority::State,
                    );
                }
            }
        });
        st.watchers.insert(device, task);
    }

    /// Hands one command for `tile`'s camera to its session and reports
    /// the outcome as a `ptz` event.
    pub(crate) fn send(
        &self,
        tile: u64,
        device: u64,
        cam: Cam,
        session: &Arc<DeviceSession>,
        command: PtzCommand,
        origin: MotionOrigin,
    ) {
        self.watch(device, session);
        {
            let mut st = lock(&self.state);
            match command {
                PtzCommand::Move { .. } => {
                    st.movers.insert(
                        (device, cam),
                        Mover {
                            tile,
                            session: Arc::downgrade(session),
                        },
                    );
                }
                PtzCommand::Stop => {
                    st.movers.remove(&(device, cam));
                }
                PtzCommand::Preset { .. } | PtzCommand::Lens { .. } => {}
            }
        }
        let pending = {
            let _rt = self.runtime.enter();
            session.ptz_send(cam, command, origin)
        };
        let events = self.events.clone();
        let action = command.action();
        self.runtime.spawn(async move {
            if let Some(o) = outcome_event(tile, action, pending.outcome().await) {
                events.send(o.finish(), Priority::State);
            }
        });
    }

    /// A tile closed: a move it started is stopped.
    pub(crate) fn tile_closed(&self, tile: u64) {
        let stops: Vec<(Cam, Weak<DeviceSession>)> = {
            let mut st = lock(&self.state);
            let keys: Vec<(u64, Cam)> = st
                .movers
                .iter()
                .filter(|(_, m)| m.tile == tile)
                .map(|(k, _)| *k)
                .collect();
            keys.into_iter()
                .filter_map(|k| st.movers.remove(&k).map(|m| (k.1, m.session)))
                .collect()
        };
        for (cam, session) in stops {
            if let Some(s) = session.upgrade() {
                debug!(
                    tile,
                    channel = cam.channel,
                    lens = cam.lens,
                    "tile closed: stopping its camera"
                );
                let _rt = self.runtime.enter();
                s.ptz_stop_now(cam, MotionOrigin::Manual);
            }
        }
    }

    /// A device was removed: its watcher ends (its session sends the stops
    /// as it closes).
    pub(crate) fn forget_device(&self, device: u64) {
        let mut st = lock(&self.state);
        st.movers.retain(|(d, _), _| *d != device);
        if let Some(j) = st.watchers.remove(&device) {
            j.abort();
        }
    }

    /// The core is shutting down.
    pub(crate) fn shutdown(&self) {
        let mut st = lock(&self.state);
        st.movers.clear();
        for (_, j) in st.watchers.drain() {
            j.abort();
        }
    }
}
