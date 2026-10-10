//! One tile: a subscription, a pipeline, and the thread that delivers its
//! actions.
//!
//! Each tile has its own OS thread, so a slow `on_action` delays only that
//! tile. Media waits in the subscription's bounded queue (filled by the
//! session without ever waiting for the tile); when the tile falls behind,
//! that queue drops to the next keyframe and reports a discontinuity, and the
//! pipeline's own backpressure bounds what is handed to the decoder. Decoder
//! results come in through a bounded channel that never blocks the sender.

use crate::audio::{AudioFrame, AudioHub, TileAudio, TileAudioControl, TileAudioLink, TileConfig};
use crate::events::{EventSink, Priority};
use crate::json::Object;
use crate::{Callbacks, Clock, in_callback};
use bc_protocol::media::{Codec as MediaCodec, Record};
use bc_protocol::stream::StreamTier;
use bc_session::{Item, Subscription};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior, interval};
use tracing::{info, warn};
use video_pipeline::{
    AccessUnit, Action, Codec, DecodeFailureKind, PipelineConfig, TilePipeline, TileStatus,
};

/// Decoder results waiting for a tile thread. Every `Decode` gets one result
/// and at most a few are in flight, so this is only reached by an app that
/// reports results that match nothing.
pub(crate) const RESULTS_CAPACITY: usize = 256;

/// Tier requests waiting for a tile thread (each is one app call).
pub(crate) const COMMANDS_CAPACITY: usize = 16;

/// A result reported by the platform decoder for one generation of a tile.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DecoderResult {
    pub(crate) generation: u32,
    pub(crate) kind: ResultKind,
}

/// What the platform decoder reported.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ResultKind {
    Decoded(u64),
    Failed(DecodeFailureKind),
}

/// The sending side of a tile's results channel.
pub(crate) struct ResultSink {
    tx: mpsc::Sender<DecoderResult>,
    overflow: Arc<AtomicU64>,
}

impl ResultSink {
    /// Queues a result without waiting. A full queue drops it (counted).
    pub(crate) fn send(&self, r: DecoderResult) {
        if self.tx.try_send(r).is_err() {
            self.overflow.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// A request to change what a tile shows (`nv_tile_set_tier`).
pub(crate) enum TileCommand {
    /// Show `tier` from the new subscription `sub` (`None` when the
    /// session has ended). `requested_ns` is when the app asked, on the
    /// core's clock.
    Switch {
        tier: StreamTier,
        sub: Option<Subscription>,
        requested_ns: u64,
    },
    /// The app asked again for the tier already requested.
    Reaffirm,
}

/// What a tile thread needs from the core.
#[derive(Clone)]
pub(crate) struct TileContext {
    pub(crate) runtime: Handle,
    pub(crate) callbacks: Arc<dyn Callbacks>,
    pub(crate) events: EventSink,
    pub(crate) pipeline: PipelineConfig,
    pub(crate) clock: Clock,
    pub(crate) tick_interval: Duration,
    pub(crate) stats_interval: Duration,
    pub(crate) tier_switch_timeout: Duration,
    pub(crate) audio: Arc<AudioHub>,
}

/// Why a tile ended, as `tile_closed` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseReason {
    /// The app closed it.
    Closed,
    /// The app removed its device.
    DeviceRemoved,
    /// Its device refused the login; the session is over.
    LoginFailed,
    /// The core is shutting down.
    CoreShutdown,
}

impl CloseReason {
    fn name(self) -> &'static str {
        match self {
            CloseReason::Closed => "closed",
            CloseReason::DeviceRemoved => "device_removed",
            CloseReason::LoginFailed => "login_failed",
            CloseReason::CoreShutdown => "core_shutdown",
        }
    }
}

/// Sends `tile_closed` unless it was already sent for this tile: exactly one
/// per tile, whoever ends it first.
fn report_closed(events: &EventSink, claimed: &AtomicBool, id: u64, reason: CloseReason) {
    if claimed.swap(true, Ordering::SeqCst) {
        return;
    }
    info!(tile = id, reason = reason.name(), "tile closed");
    events.send(
        Object::event("tile_closed")
            .u64("tile", id)
            .str("reason", reason.name())
            .finish(),
        Priority::State,
    );
}

/// The core's handle on a running tile.
pub(crate) struct TileHandle {
    pub(crate) device: u64,
    pub(crate) channel: u8,
    /// The tier most recently requested (opened with, or set since).
    pub(crate) tier: StreamTier,
    id: u64,
    events: EventSink,
    /// Set once `tile_closed` has been sent.
    closed_reported: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    wake: Option<oneshot::Sender<()>>,
    commands: mpsc::Sender<TileCommand>,
    thread: Option<JoinHandle<()>>,
}

impl TileHandle {
    /// Hands a tier request to the tile thread without waiting. False when
    /// too many are queued.
    pub(crate) fn command(&self, c: TileCommand) -> bool {
        self.commands.try_send(c).is_ok()
    }

    /// Asks the thread to stop: no further action is started.
    pub(crate) fn signal_stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(w) = self.wake.take() {
            let _ = w.send(());
        }
    }

    /// Stops and waits for the thread (and an `on_action` in progress) to
    /// end. Its subscriptions are dropped on the way out, which stops the
    /// previews on the device.
    pub(crate) fn stop_and_join(&mut self) {
        self.signal_stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Stops, joins, and then (after the last `on_action` has returned)
    /// reports the tile closed, unless it already ended on its own.
    pub(crate) fn end(&mut self, reason: CloseReason) {
        self.stop_and_join();
        report_closed(&self.events, &self.closed_reported, self.id, reason);
    }
}

impl Drop for TileHandle {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

/// What a new tile shows first.
pub(crate) struct TileStart {
    pub(crate) id: u64,
    pub(crate) device: u64,
    pub(crate) channel: u8,
    pub(crate) tier: StreamTier,
}

/// Starts a tile thread. It does nothing until `gate` resolves (or its
/// sender is dropped), so no action runs before the app has the tile's ID.
/// `sub` is `None` when the device session has already ended; the tile then
/// ends at once with `login_failed`, as it does when its session ends later.
pub(crate) fn spawn(
    start: TileStart,
    sub: Option<Subscription>,
    ctx: TileContext,
    audio: Arc<TileAudioControl>,
    gate: oneshot::Receiver<()>,
) -> std::io::Result<(TileHandle, ResultSink)> {
    let TileStart {
        id,
        device,
        channel,
        tier,
    } = start;
    let (tx, rx) = mpsc::channel(RESULTS_CAPACITY);
    let (cmd_tx, cmd_rx) = mpsc::channel(COMMANDS_CAPACITY);
    let overflow = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (wake_tx, wake_rx) = oneshot::channel();
    let closed_reported = Arc::new(AtomicBool::new(false));
    let events = ctx.events.clone();
    let hub = ctx.audio.clone();
    let state = TileThread {
        closed_reported: closed_reported.clone(),
        id,
        ctx,
        stop: stop.clone(),
        results: rx,
        commands: cmd_rx,
        overflow: overflow.clone(),
        link: TileAudioLink {
            hub,
            control: audio,
            seen: 0,
            tapped: false,
        },
        audio: {
            let mut a = TileAudio::new(TileConfig::default());
            a.set_owner(id);
            a
        },
        talkback_ignored: 0,
        sequence: 0,
        last_video_pts: None,
        next_generation: 1,
        stale_results: 0,
    };
    let thread = std::thread::Builder::new()
        .name(format!("nvrview-tile-{id}"))
        .spawn(move || state.run(tier, sub, wake_rx, gate))?;
    Ok((
        TileHandle {
            device,
            channel,
            tier,
            id,
            events,
            closed_reported,
            stop,
            wake: Some(wake_tx),
            commands: cmd_tx,
            thread: Some(thread),
        },
        ResultSink { tx, overflow },
    ))
}

/// One generation of a tile: a stream tier, its subscription, and the
/// pipeline that drives one platform decoder.
struct Generation {
    generation: u32,
    tier: StreamTier,
    sub: Option<Subscription>,
    p: TilePipeline,
}

/// A generation being started, shown once its first picture is decoded.
struct Starting {
    g: Generation,
    requested_ns: u64,
    deadline: Instant,
    /// Set when it can no longer succeed, with the reason.
    failed: Option<&'static str>,
}

/// The tile's generations: the one shown, and at most one starting.
struct Streams {
    current: Generation,
    starting: Option<Starting>,
}

impl Streams {
    /// Applies a decoder result to the generation it belongs to. Results for
    /// a generation that is gone are dropped (counted).
    fn apply(&mut self, r: DecoderResult, stale: &mut u64) {
        let p = if r.generation == self.current.generation {
            &mut self.current.p
        } else if let Some(s) = self
            .starting
            .as_mut()
            .filter(|s| s.g.generation == r.generation)
        {
            &mut s.g.p
        } else {
            *stale += 1;
            return;
        };
        match r.kind {
            ResultKind::Decoded(pts) => p.decoded(pts),
            ResultKind::Failed(kind) => p.decode_failed(kind),
        }
    }
}

struct TileThread {
    closed_reported: Arc<AtomicBool>,
    id: u64,
    ctx: TileContext,
    stop: Arc<AtomicBool>,
    results: mpsc::Receiver<DecoderResult>,
    commands: mpsc::Receiver<TileCommand>,
    overflow: Arc<AtomicU64>,
    /// Audio goes here, in stream order (see [`crate::audio`]).
    audio: TileAudio,
    /// The app's settings for it.
    link: TileAudioLink,
    /// Talk-back records dropped.
    talkback_ignored: u64,
    /// Media items seen, video and audio alike.
    sequence: u64,
    last_video_pts: Option<u64>,
    /// Number of the next generation started.
    next_generation: u32,
    /// Decoder results for generations that are gone.
    stale_results: u64,
}

fn codec(c: MediaCodec) -> Codec {
    match c {
        MediaCodec::H264 => Codec::H264,
        MediaCodec::H265 => Codec::H265,
    }
}

/// Waits for the next subscription item; never resolves without one.
async fn next_item(sub: &mut Option<Subscription>) -> Option<Item> {
    match sub {
        Some(s) => s.recv().await,
        None => std::future::pending().await,
    }
}

/// Waits for the next item of the starting generation, if any. `Some(None)`
/// means its subscription ended.
async fn next_starting_item(starting: &mut Option<Starting>) -> Option<Item> {
    match starting {
        Some(s) if s.failed.is_none() => match &mut s.g.sub {
            Some(sub) => sub.recv().await,
            None => None,
        },
        _ => std::future::pending().await,
    }
}

fn tier_event(tile: u64, tier: StreamTier, state: &str, generation: u32) -> Object {
    Object::event("tile_tier")
        .u64("tile", tile)
        .str("tier", crate::device::tier_name(tier))
        .str("state", state)
        .u64("generation", u64::from(generation))
}

impl TileThread {
    fn run(
        mut self,
        tier: StreamTier,
        sub: Option<Subscription>,
        wake: oneshot::Receiver<()>,
        gate: oneshot::Receiver<()>,
    ) {
        in_callback::mark_core_thread();
        let runtime = self.ctx.runtime.clone();
        // This thread drives its own future; the runtime's workers only run
        // the sessions and timers, so a slow callback here holds up nothing
        // else.
        runtime.block_on(self.serve(tier, sub, wake, gate));
    }

    async fn serve(
        &mut self,
        tier: StreamTier,
        sub: Option<Subscription>,
        mut wake: oneshot::Receiver<()>,
        gate: oneshot::Receiver<()>,
    ) {
        // Released (or dropped) once the app has the tile's ID.
        tokio::select! {
            biased;
            _ = &mut wake => return,
            _ = gate => {}
        }
        let mut s = Streams {
            current: Generation {
                generation: 0,
                tier,
                sub,
                p: TilePipeline::new(self.id as u32, self.ctx.pipeline),
            },
            starting: None,
        };
        let mut tick = interval(self.ctx.tick_interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut next_stats = Instant::now() + self.ctx.stats_interval;
        let mut overflow_seen = 0;
        let mut commands_open = true;
        loop {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            tokio::select! {
                biased;
                _ = &mut wake => break,
                r = self.results.recv() => match r {
                    Some(r) => s.apply(r, &mut self.stale_results),
                    // The core dropped the sender: the tile is closing.
                    None => break,
                },
                c = self.commands.recv(), if commands_open => match c {
                    Some(c) => self.command(&mut s, c),
                    None => commands_open = false,
                },
                item = next_item(&mut s.current.sub) => match item {
                    Some(item) => self.item(&mut s.current, item),
                    None => s.current.sub = None,
                },
                item = next_starting_item(&mut s.starting) => {
                    if let Some(st) = s.starting.as_mut() {
                        match item {
                            Some(item) => Self::starting_item(self.id, st, item, &self.ctx),
                            None => st.failed = Some("stream_ended"),
                        }
                    }
                },
                // The session ended for good (the device refused the
                // login): nothing will ever arrive.
                _ = std::future::ready(()), if s.current.sub.is_none() => {
                    if !self.stop.load(Ordering::SeqCst) {
                        report_closed(
                            &self.ctx.events,
                            &self.closed_reported,
                            self.id,
                            CloseReason::LoginFailed,
                        );
                    }
                    break;
                },
                _ = tick.tick() => {}
            }
            self.link.apply(&mut self.audio);
            let now_us = self.ctx.clock.now_us();
            s.current.p.tick(now_us);
            if let Some(st) = s.starting.as_mut() {
                st.g.p.tick(now_us);
                if st.failed.is_none() && Instant::now() >= st.deadline {
                    st.failed = Some("timeout");
                }
            }
            self.deliver(&mut s);
            self.settle(&mut s);
            let now = Instant::now();
            if now >= next_stats {
                next_stats = now + self.ctx.stats_interval;
                self.stats(&s.current);
                let overflow = self.overflow.load(Ordering::Relaxed);
                if overflow != overflow_seen {
                    warn!(
                        tile = self.id,
                        dropped = overflow - overflow_seen,
                        "decoder results dropped: more than the core can match"
                    );
                    overflow_seen = overflow;
                }
            }
        }
        // The subscriptions drop here, on this thread, before the core's
        // close returns: the session stops the previews.
        drop(s);
    }

    /// A tier request from the app.
    fn command(&mut self, s: &mut Streams, c: TileCommand) {
        match c {
            TileCommand::Reaffirm => {
                if s.starting.is_none() {
                    self.ctx.events.send(
                        tier_event(self.id, s.current.tier, "active", s.current.generation)
                            .finish(),
                        Priority::State,
                    );
                }
            }
            TileCommand::Switch {
                tier,
                sub,
                requested_ns,
            } => {
                // A newer request replaces one still starting.
                if let Some(st) = s.starting.take() {
                    self.abandon(st, "superseded");
                }
                if tier == s.current.tier {
                    // Back to what is shown: nothing to start.
                    drop(sub);
                    self.ctx.events.send(
                        tier_event(self.id, tier, "active", s.current.generation).finish(),
                        Priority::State,
                    );
                    return;
                }
                let generation = self.next_generation;
                self.next_generation = self.next_generation.wrapping_add(1);
                let failed = sub.is_none().then_some("session_closed");
                info!(
                    tile = self.id,
                    generation,
                    tier = crate::device::tier_name(tier),
                    "tier switch starting"
                );
                self.ctx.events.send(
                    tier_event(self.id, tier, "starting", generation).finish(),
                    Priority::State,
                );
                s.starting = Some(Starting {
                    g: Generation {
                        generation,
                        tier,
                        sub,
                        p: TilePipeline::new(self.id as u32, self.ctx.pipeline),
                    },
                    requested_ns,
                    deadline: Instant::now() + self.ctx.tier_switch_timeout,
                    failed,
                });
            }
        }
    }

    /// Ends a generation that never became current: its decoder gets a
    /// final `Reset`, and the app hears why.
    fn abandon(&mut self, st: Starting, reason: &'static str) {
        let g = st.g;
        warn!(
            tile = self.id,
            generation = g.generation,
            reason,
            "tier switch failed"
        );
        self.ctx
            .callbacks
            .on_action(self.id, g.generation, &Action::Reset);
        self.ctx.events.send(
            tier_event(self.id, g.tier, "failed", g.generation)
                .str("reason", reason)
                .finish(),
            Priority::State,
        );
        // Its subscription drops here: the stream stops unless another tile
        // shows it.
        drop(g);
    }

    /// Finishes a switch whose first picture has been decoded, or abandons
    /// one that failed.
    fn settle(&mut self, s: &mut Streams) {
        let Some(st) = s.starting.as_ref() else {
            return;
        };
        if let Some(reason) = st.failed {
            let st = s.starting.take().expect("checked");
            self.abandon(st, reason);
            return;
        }
        if st.g.p.stats().decoded == 0 || self.stop.load(Ordering::SeqCst) {
            return;
        }
        let st = s.starting.take().expect("checked");
        let old = std::mem::replace(&mut s.current, st.g);
        // The old generation's last action: the app destroys its decoder
        // and shows the new generation from now on.
        self.ctx
            .callbacks
            .on_action(self.id, old.generation, &Action::Reset);
        let elapsed = crate::clock::now_ns().saturating_sub(st.requested_ns);
        info!(
            tile = self.id,
            generation = s.current.generation,
            tier = crate::device::tier_name(s.current.tier),
            ms = elapsed / 1_000_000,
            "tier switched"
        );
        self.ctx.events.send(
            tier_event(self.id, s.current.tier, "active", s.current.generation).finish(),
            Priority::State,
        );
        self.ctx.events.send(
            Object::event("timing")
                .str("what", "tier_switch")
                .u64("tile", self.id)
                .u64("ns", elapsed)
                .finish(),
            Priority::State,
        );
        // Audio now comes from the new stream: a fresh start.
        self.last_video_pts = None;
        self.audio.discontinuity();
        // Start the new stream, then stop the old one (unless another tile
        // shows it), as the official client does.
        drop(old);
    }

    /// An item of the generation being started: video goes to its pipeline;
    /// audio waits until it is shown.
    fn starting_item(tile: u64, st: &mut Starting, item: Item, ctx: &TileContext) {
        match item {
            Item::Media(Record::Video {
                codec: c,
                keyframe,
                pts_us,
                data,
            }) => {
                st.g.p.tick(ctx.clock.now_us());
                st.g.p.push(AccessUnit {
                    codec: codec(c),
                    keyframe,
                    pts: pts_us,
                    data,
                });
            }
            Item::Media(_) => {}
            Item::Discontinuity => st.g.p.discontinuity(),
            Item::StartRejected { status } => {
                warn!(tile, status, "the device refused to start the new tier");
                st.failed = Some("start_rejected");
            }
        }
    }

    fn item(&mut self, g: &mut Generation, item: Item) {
        let p = &mut g.p;
        if matches!(item, Item::Media(_)) {
            self.sequence += 1;
        }
        match item {
            Item::Media(Record::Video {
                codec: c,
                keyframe,
                pts_us,
                data,
            }) => {
                p.tick(self.ctx.clock.now_us());
                self.last_video_pts = Some(pts_us);
                p.push(AccessUnit {
                    codec: codec(c),
                    keyframe,
                    pts: pts_us,
                    data,
                });
            }
            Item::Media(Record::Audio { stream_index, data }) => {
                let frame = AudioFrame {
                    stream_index,
                    data,
                    sequence: self.sequence,
                    after_video_pts_us: self.last_video_pts,
                    arrived_us: self.ctx.clock.now_us(),
                    wall_clock_us: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .and_then(|d| i64::try_from(d.as_micros()).ok()),
                };
                if frame.is_talkback() {
                    self.talkback_ignored += 1;
                    return;
                }
                self.link.apply(&mut self.audio);
                self.audio.push(frame.record());
                self.link.pushed();
            }
            Item::Media(Record::Info { .. }) => {}
            Item::Discontinuity => {
                self.last_video_pts = None;
                self.audio.discontinuity();
                p.discontinuity();
            }
            Item::StartRejected { status } => {
                warn!(
                    tile = self.id,
                    status, "the device refused to start the stream"
                );
                self.ctx.events.send(
                    Object::event("tile_error")
                        .u64("tile", self.id)
                        .str("reason", "start_rejected")
                        .u64("status", u64::from(status))
                        .finish(),
                    Priority::State,
                );
            }
        }
    }

    /// Carries out every pending action of both generations in order.
    /// Results that arrive while an action runs (often from inside the
    /// callback) are applied before the next action, as a platform
    /// answering synchronously expects.
    fn deliver(&mut self, s: &mut Streams) {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return;
            }
            let Some(action) = s.current.p.next_action() else {
                break;
            };
            self.ctx
                .callbacks
                .on_action(self.id, s.current.generation, &action);
            while let Ok(r) = self.results.try_recv() {
                s.apply(r, &mut self.stale_results);
            }
        }
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return;
            }
            let Some(st) = s.starting.as_mut() else {
                return;
            };
            if st.failed.is_some() {
                return;
            }
            let Some(action) = st.g.p.next_action() else {
                return;
            };
            if action == Action::Status(TileStatus::Undecodable) {
                st.failed = Some("undecodable");
            }
            self.ctx
                .callbacks
                .on_action(self.id, st.g.generation, &action);
            while let Ok(r) = self.results.try_recv() {
                s.apply(r, &mut self.stale_results);
            }
        }
    }

    fn stats(&self, g: &Generation) {
        let s = g.p.stats();
        let queue_dropped = g.sub.as_ref().map_or(0, Subscription::dropped);
        self.ctx.events.send(
            Object::event("tile_stats")
                .u64("tile", self.id)
                .str("tier", crate::device::tier_name(g.tier))
                .u64("generation", u64::from(g.generation))
                .u64("received", s.received)
                .u64("decoded", s.decoded)
                .u64("gated", s.gated)
                .u64("dropped", s.dropped + queue_dropped)
                .u64("failures", s.failures)
                .u64("resets", s.resets)
                .u64("audio_frames", self.audio.stats().records)
                .finish(),
            Priority::Diagnostics,
        );
        self.audio_stats();
    }

    /// `audio_stats`, for tiles whose stream carries audio.
    fn audio_stats(&self) {
        let a = self.audio.stats();
        if a.records == 0 && self.talkback_ignored == 0 {
            return;
        }
        let c = &self.link.control;
        let mut o = Object::event("audio_stats")
            .u64("tile", self.id)
            .u64("records", a.records)
            .u64("bytes", a.bytes)
            .u64("decoded", a.decoded)
            .u64("concealed", a.concealed)
            .u64("rejected", a.rejected)
            .u64("talkback_ignored", self.talkback_ignored)
            .u64("epoch", u64::from(a.epoch))
            .bool("audible", c.audible())
            .f64("volume", f64::from(c.volume()))
            .bool("tapped", c.tapped())
            .bool("no_free_slot", a.no_free_slot)
            .f64("playout_delay_ms", a.playout_delay_us / 1e3)
            .u64("outliers", a.timestamps.outliers)
            .u64("timestamp_resets", a.timestamps.resets);
        if let Some(codec) = a.codec {
            o = o.str(
                "codec",
                match codec {
                    audio_pipeline::AudioCodec::Aac => "aac",
                    audio_pipeline::AudioCodec::ImaAdpcm => "ima_adpcm",
                    audio_pipeline::AudioCodec::MuLaw => "mulaw",
                },
            );
        }
        if let Some(f) = a.format {
            o = o
                .u64("sample_rate", u64::from(f.sample_rate))
                .u64("channels", f.channels as u64);
        }
        if let Some(r) = a.rate_estimate {
            o = o.f64("rate_estimate", r);
        }
        if let Some(t) = a.playout_delay_target_us {
            o = o.f64("playout_delay_target_ms", t / 1e3);
        }
        if let Some(p) = a.playout {
            if let Some(off) = p.av_offset_us {
                o = o.f64("av_offset_ms", off / 1e3);
            }
            let rate = self
                .link
                .hub
                .output()
                .map_or(48_000.0, |out| f64::from(out.format().sample_rate));
            o = o
                .f64("buffered_ms", p.buffered_frames as f64 * 1e3 / rate)
                .f64("correction_ppm", p.correction * 1e6)
                .u64("resyncs", p.resyncs)
                .u64("starts", p.starts)
                .u64("underruns", p.underruns)
                .u64("underrun_frames", p.underrun_frames)
                .u64("overflow_frames", p.overflow_frames)
                .u64("blocks_not_played", p.blocks_not_played);
        }
        if let Some(t) = a.tap {
            o = o.u64("tap_blocks", t.pushed).u64("tap_dropped", t.dropped);
        }
        self.ctx.events.send(o.finish(), Priority::Diagnostics);
    }
}
