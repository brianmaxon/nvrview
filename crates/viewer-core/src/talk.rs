//! Talk-back: the app's microphone or text-to-speech through a camera's or
//! doorbell's speaker (docs/FFI.md, "Talk-back").
//!
//! Camera motion control (docs/ARCHITECTURE.md, "Device commands"): a talk
//! starts only on a channel the app has allowed with
//! [`crate::Core::set_talk_permission`] (off by default) and only when the
//! app asks ([`crate::Core::talk_start`]); the device session adds its rate
//! limit and logs every start and stop. The app pushes mono float samples
//! at its own rate; the core resamples them to the device's 16 kHz, and the
//! session encodes and paces them.

use crate::events::{EventSink, Priority};
use crate::json;
use audio_pipeline::resample::Resampler;
use bc_session::{DeviceSession, Refusal, StopReason, Talk, TalkState};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::runtime::Handle;
use tracing::debug;

/// The device's talk-back rate (docs/protocol.md, "Talk-back (motion)").
pub const TALK_RATE: u32 = bc_protocol::talk::SAMPLE_RATE;
/// Lowest and highest input rates [`crate::Core::talk_push`] accepts.
pub const PUSH_RATES: std::ops::RangeInclusive<u32> = 8_000..=192_000;
/// Audio held while a talk is starting (about 1 s at 16 kHz); more is
/// dropped and counted.
const PENDING_MAX: usize = TALK_RATE as usize;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// How the app asked a talk to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// At once.
    Stop,
    /// After the audio already pushed has been sent.
    Drain,
}

struct Inner {
    talk: Option<Talk>,
    /// 16 kHz samples pushed while starting.
    pending: Vec<i16>,
    /// Asked to end while starting.
    ending: Option<Ending>,
    resampler: Option<(u32, Resampler, i64)>,
    /// 16 kHz samples dropped (pending overflow and the session's buffer).
    dropped: u64,
    scratch: Vec<f32>,
}

/// One tile's talk.
struct Slot {
    tile: u64,
    device: u64,
    channel: u8,
    inner: Mutex<Inner>,
    /// Set once the final event (`refused` or `stopped`) was sent.
    reported: AtomicBool,
}

impl Slot {
    fn event(&self, state: &str) -> json::Object {
        json::Object::event("talk")
            .u64("tile", self.tile)
            .u64("device", self.device)
            .u64("channel", u64::from(self.channel))
            .str("state", state)
    }
}

/// Every tile's talk and the app's permissions.
pub(crate) struct TalkHub {
    allowed: Mutex<HashSet<(u64, u8)>>,
    slots: Arc<Mutex<HashMap<u64, Arc<Slot>>>>,
    events: EventSink,
    runtime: Handle,
}

/// Why [`TalkHub::start`] refused at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartError {
    NotPermitted,
}

impl TalkHub {
    pub(crate) fn new(events: EventSink, runtime: Handle) -> TalkHub {
        TalkHub {
            allowed: Mutex::new(HashSet::new()),
            slots: Arc::new(Mutex::new(HashMap::new())),
            events,
            runtime,
        }
    }

    /// Records the app's permission and passes it to the device session
    /// (which stops a running talk when it is withdrawn).
    pub(crate) fn set_permission(
        &self,
        device: u64,
        channel: u8,
        allowed: bool,
        session: Option<&DeviceSession>,
    ) {
        {
            let mut a = lock(&self.allowed);
            if allowed {
                a.insert((device, channel));
            } else {
                a.remove(&(device, channel));
            }
        }
        if let Some(s) = session {
            s.set_motion_allowed(channel, allowed);
        }
    }

    pub(crate) fn is_permitted(&self, device: u64, channel: u8) -> bool {
        lock(&self.allowed).contains(&(device, channel))
    }

    /// The permissions of `device`, for a session that starts later.
    pub(crate) fn forget_device(&self, device: u64) {
        lock(&self.allowed).retain(|(d, _)| *d != device);
    }

    /// Starts a talk for `tile` (progress as `talk` events). Starting one
    /// already starting or active changes nothing.
    pub(crate) fn start(
        &self,
        tile: u64,
        device: u64,
        channel: u8,
        session: Option<Arc<DeviceSession>>,
    ) -> Result<(), StartError> {
        let slot = Arc::new(Slot {
            tile,
            device,
            channel,
            inner: Mutex::new(Inner {
                talk: None,
                pending: Vec::new(),
                ending: None,
                resampler: None,
                dropped: 0,
                scratch: Vec::new(),
            }),
            reported: AtomicBool::new(false),
        });
        if !self.is_permitted(device, channel) {
            self.final_event(
                &slot,
                slot.event("refused")
                    .str("reason", Refusal::NotPermitted.name()),
            );
            return Err(StartError::NotPermitted);
        }
        {
            let mut slots = lock(&self.slots);
            if slots.contains_key(&tile) {
                return Ok(());
            }
            slots.insert(tile, slot.clone());
        }
        self.events
            .send(slot.event("starting").finish(), Priority::State);
        let Some(session) = session else {
            self.refused(&slot, Refusal::SessionClosed);
            return Ok(());
        };
        let hub = self.hub_ref();
        self.runtime.spawn(async move {
            match session.talk(channel).await {
                Ok(talk) => hub.started(slot, talk).await,
                Err(r) => hub.refused(&slot, r),
            }
        });
        Ok(())
    }

    fn hub_ref(&self) -> HubRef {
        HubRef {
            slots: self.slots.clone(),
            events: self.events.clone(),
        }
    }

    fn refused(&self, slot: &Arc<Slot>, r: Refusal) {
        self.hub_ref().refused(slot, r);
    }

    fn final_event(&self, slot: &Slot, obj: json::Object) {
        if !slot.reported.swap(true, Ordering::AcqRel) {
            self.events.send(obj.finish(), Priority::State);
        }
    }

    /// Queues the app's samples (mono, `rate` Hz) for `tile`'s talk.
    /// Returns false when the tile has no talk starting or active.
    pub(crate) fn push(&self, tile: u64, samples: &[f32], rate: u32) -> bool {
        let Some(slot) = lock(&self.slots).get(&tile).cloned() else {
            return false;
        };
        let mut g = lock(&slot.inner);
        let inner = &mut *g;
        match inner.ending {
            Some(Ending::Stop) => return true,
            // More audio takes back a draining stop asked for while the
            // talk was starting (as it does on a running talk).
            Some(Ending::Drain) if !samples.is_empty() => inner.ending = None,
            _ => {}
        }
        if inner.resampler.as_ref().is_none_or(|(r, _, _)| *r != rate) {
            inner.resampler = (rate != TALK_RATE).then(|| {
                let mut r = Resampler::new(1, rate, TALK_RATE);
                r.reset(0.0);
                (rate, r, 0)
            });
        }
        let mut pcm: Vec<i16> = Vec::new();
        match inner.resampler.as_mut() {
            None => pcm.extend(samples.iter().map(|s| to_i16(*s))),
            Some((r, res, next)) => {
                res.push(*next, samples);
                *next += samples.len() as i64;
                inner.scratch.clear();
                let step = f64::from(*r) / f64::from(TALK_RATE);
                res.produce(step, usize::MAX, &mut inner.scratch);
                pcm.extend(inner.scratch.iter().map(|s| to_i16(*s)));
            }
        }
        match &inner.talk {
            Some(t) => {
                let n = t.push(&pcm);
                inner.dropped += (pcm.len() - n) as u64;
            }
            None => {
                let room = PENDING_MAX.saturating_sub(inner.pending.len());
                let n = room.min(pcm.len());
                inner.pending.extend_from_slice(&pcm[..n]);
                inner.dropped += (pcm.len() - n) as u64;
            }
        }
        true
    }

    /// Ends `tile`'s talk: at once, or after the audio already pushed and
    /// the session's silent tail (`drain`; a later push continues it).
    /// Returns false when it has none.
    pub(crate) fn stop(&self, tile: u64, drain: bool) -> bool {
        let Some(slot) = lock(&self.slots).get(&tile).cloned() else {
            return false;
        };
        let mut inner = lock(&slot.inner);
        let ending = if drain { Ending::Drain } else { Ending::Stop };
        match &inner.talk {
            Some(t) if drain => t.finish(),
            Some(t) => t.stop(),
            None => {
                inner.ending.get_or_insert(ending);
            }
        }
        true
    }

    /// The tile is closing: its talk stops now (reason `tile_closed`).
    pub(crate) fn tile_closed(&self, tile: u64) {
        let Some(slot) = lock(&self.slots).remove(&tile) else {
            return;
        };
        let mut inner = lock(&slot.inner);
        inner.ending = Some(Ending::Stop);
        if let Some(t) = inner.talk.take() {
            let stats = t.stats();
            drop(t);
            self.final_event(
                &slot,
                slot.event("stopped")
                    .str("reason", "tile_closed")
                    .u64("sent_ms", (stats.records_sent - stats.tail_records) * 64)
                    .u64("tail_ms", stats.tail_records * 64)
                    .u64("dropped_ms", (inner.dropped + stats.samples_dropped) / 16),
            );
        } else {
            self.final_event(&slot, slot.event("stopped").str("reason", "tile_closed"));
        }
    }

    /// Stops every talk (shutdown); events are not needed any more.
    pub(crate) fn shutdown(&self) {
        let slots: Vec<Arc<Slot>> = lock(&self.slots).drain().map(|(_, s)| s).collect();
        for s in slots {
            let mut inner = lock(&s.inner);
            inner.ending = Some(Ending::Stop);
            inner.talk.take();
        }
    }
}

fn to_i16(s: f32) -> i16 {
    if s.is_finite() {
        (s.clamp(-1.0, 1.0) * 32767.0).round() as i16
    } else {
        0
    }
}

/// What the start task needs of the hub.
struct HubRef {
    slots: Arc<Mutex<HashMap<u64, Arc<Slot>>>>,
    events: EventSink,
}

impl HubRef {
    fn refused(&self, slot: &Arc<Slot>, r: Refusal) {
        let mut obj = slot.event("refused").str("reason", r.name());
        match r {
            Refusal::RateLimited { retry_after } => {
                obj = obj.u64("retry_ms", retry_after.as_millis() as u64);
            }
            Refusal::DeviceRefused(status) => obj = obj.u64("status", u64::from(status)),
            _ => {}
        }
        if !slot.reported.swap(true, Ordering::AcqRel) {
            self.events.send(obj.finish(), Priority::State);
        }
        let mut slots = lock(&self.slots);
        if slots.get(&slot.tile).is_some_and(|s| Arc::ptr_eq(s, slot)) {
            slots.remove(&slot.tile);
        }
    }

    async fn started(self, slot: Arc<Slot>, talk: Talk) {
        let mut state = talk.watch_state();
        {
            let mut inner = lock(&slot.inner);
            if slot.reported.load(Ordering::Acquire) {
                // The tile closed meanwhile; dropping the talk closes it.
                return;
            }
            let pending = std::mem::take(&mut inner.pending);
            let n = talk.push(&pending);
            inner.dropped += (pending.len() - n) as u64;
            match inner.ending {
                Some(Ending::Stop) => talk.stop(),
                Some(Ending::Drain) => talk.finish(),
                None => {}
            }
            inner.talk = Some(talk);
        }
        self.events
            .send(slot.event("active").finish(), Priority::State);
        let reason = loop {
            let s = *state.borrow_and_update();
            match s {
                TalkState::Stopped(r) => break Some(r),
                TalkState::Refused(_) => break None,
                _ => {}
            }
            if state.changed().await.is_err() {
                break None;
            }
        };
        let (sent, tail, dropped) = {
            let mut inner = lock(&slot.inner);
            let t = inner.talk.take();
            let stats = t.as_ref().map(Talk::stats).unwrap_or_default();
            (
                stats.records_sent - stats.tail_records,
                stats.tail_records,
                inner.dropped + stats.samples_dropped,
            )
        };
        let mut obj = slot
            .event("stopped")
            .str("reason", reason.map_or("session_closed", StopReason::name));
        if let Some(StopReason::DeviceError(status)) = reason {
            obj = obj.u64("status", u64::from(status));
        }
        obj = obj
            .u64("sent_ms", sent * 64)
            .u64("tail_ms", tail * 64)
            .u64("dropped_ms", dropped / 16);
        if !slot.reported.swap(true, Ordering::AcqRel) {
            self.events.send(obj.finish(), Priority::State);
        }
        debug!(tile = slot.tile, "talk slot ended");
        let mut slots = lock(&self.slots);
        if slots.get(&slot.tile).is_some_and(|s| Arc::ptr_eq(s, &slot)) {
            slots.remove(&slot.tile);
        }
    }
}
