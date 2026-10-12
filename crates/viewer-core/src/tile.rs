//! One tile: a subscription, a pipeline, and the thread that delivers its
//! actions.
//!
//! Each tile has its own OS thread, so a slow `on_action` delays only that
//! tile. Media waits in the subscription's bounded queue (filled by the
//! session without ever waiting for the tile); when the tile falls behind,
//! that queue drops to the next keyframe and reports a discontinuity, and the
//! pipeline's own backpressure bounds what is handed to the decoder. Decoder
//! results come in through a bounded channel that never blocks the sender.
//!
//! A playback tile (docs/FFI-RECORDINGS.md, section 6) is the same tile fed
//! by the playback engine instead of a subscription ([`Source::Feed`]): its
//! generation 0 is an empty placeholder, every seek is a new generation
//! ([`TileCommand::Playback`]) shown once the picture the engine names has
//! been decoded, it reports its decode latency and backlog back through the
//! feed, it sends no `tile_tier` events (the playback reports its own), and
//! it does no live sound analysis.

use crate::audio::{AudioFrame, AudioHub, TileAudio, TileAudioControl, TileAudioLink, TileConfig};
use crate::clock::HostClock;
use crate::events::{EventSink, Priority};
use crate::json::Object;
use crate::live_pacer::LivePacer;
use crate::playback::feed::{FeedItem, FeedRx};
use crate::sound::{SoundHub, TileSoundLink};
use crate::{Callbacks, in_callback};
use bc_protocol::media::{Codec as MediaCodec, Record};
use bc_protocol::stream::StreamTier;
use bc_session::{Item, Subscription};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
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
    /// A playback tile's next generation (a seek or a restart of its play),
    /// numbered by the playback; shown once the feed's target picture has
    /// been decoded, or at once, cleared, when the feed's first item is
    /// [`FeedItem::Clear`].
    Playback {
        generation: u32,
        tier: StreamTier,
        feed: FeedRx,
    },
}

/// Where a generation's media comes from.
pub(crate) enum Source {
    /// A live subscription.
    Live(Subscription),
    /// The playback engine.
    Feed(FeedRx),
    /// Nothing (a playback tile's generation 0).
    Placeholder,
}

impl Source {
    /// The next item; `None` once a live session has ended for good.
    async fn recv(&mut self) -> Option<FeedItem> {
        match self {
            Source::Live(s) => s.recv().await.map(FeedItem::Item),
            Source::Feed(f) => Some(f.recv().await),
            Source::Placeholder => std::future::pending().await,
        }
    }

    fn dropped(&self) -> u64 {
        match self {
            Source::Live(s) => s.dropped(),
            Source::Feed(f) => f.dropped(),
            Source::Placeholder => 0,
        }
    }

    fn feed(&self) -> Option<&FeedRx> {
        match self {
            Source::Feed(f) => Some(f),
            _ => None,
        }
    }
}

/// How long a playback generation may take to be shown before it is
/// abandoned (the engine gives up on a camera sooner and clears it).
const PLAYBACK_START_LIMIT: Duration = Duration::from_secs(60);

/// Decodes remembered per generation for measuring the decode latency.
const LATENCY_SAMPLES: usize = 32;

/// What a tile thread needs from the core.
#[derive(Clone)]
pub(crate) struct TileContext {
    pub(crate) runtime: Handle,
    pub(crate) callbacks: Arc<dyn Callbacks>,
    pub(crate) events: EventSink,
    pub(crate) pipeline: PipelineConfig,
    pub(crate) clock: HostClock,
    /// Each tile on a thread of its own (always, except on a test runtime:
    /// [`crate::TestRuntime`], where tiles are tasks on that runtime).
    pub(crate) tile_threads: bool,
    pub(crate) tick_interval: Duration,
    pub(crate) stats_interval: Duration,
    pub(crate) tier_switch_timeout: Duration,
    pub(crate) tier_switch_align_cap: Duration,
    pub(crate) info_only_limit: Duration,
    pub(crate) audio: Arc<AudioHub>,
    pub(crate) sound: Arc<SoundHub>,
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
    /// When the app last asked for a retry of this tile (core clock, ns).
    pub(crate) last_retry_ns: Option<u64>,
    id: u64,
    events: EventSink,
    /// Set once `tile_closed` has been sent.
    closed_reported: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    wake: Option<oneshot::Sender<()>>,
    commands: mpsc::Sender<TileCommand>,
    runner: Option<Runner>,
}

impl TileHandle {
    /// A sender of commands to the tile (for the playback engine).
    pub(crate) fn commands(&self) -> mpsc::Sender<TileCommand> {
        self.commands.clone()
    }
}

/// What runs a tile.
enum Runner {
    /// A thread of its own (apps).
    Thread(std::thread::JoinHandle<()>),
    /// A task on a test runtime ([`crate::TestRuntime`]).
    Task(tokio::task::JoinHandle<()>, Handle),
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
        match self.runner.take() {
            Some(Runner::Thread(t)) => {
                let _ = t.join();
            }
            // On the runtime itself it cannot be waited for; the task sees
            // the stop at its next step and ends on its own.
            Some(Runner::Task(t, runtime)) if Handle::try_current().is_err() => {
                let _ = runtime.block_on(t);
            }
            Some(Runner::Task(..)) | None => {}
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
    /// A playback tile: fed by the playback engine, no live sound analysis.
    pub(crate) playback: bool,
}

/// Starts a tile thread. It does nothing until `gate` resolves (or its
/// sender is dropped), so no action runs before the app has the tile's ID.
/// `sub` is `None` when the device session has already ended; the tile then
/// ends at once with `login_failed`, as it does when its session ends later.
pub(crate) fn spawn(
    start: TileStart,
    sub: Option<Source>,
    ctx: TileContext,
    audio: Arc<TileAudioControl>,
    gate: oneshot::Receiver<()>,
) -> std::io::Result<(TileHandle, ResultSink)> {
    let TileStart {
        id,
        device,
        channel,
        tier,
        playback,
    } = start;
    let (tx, rx) = mpsc::channel(RESULTS_CAPACITY);
    let (cmd_tx, cmd_rx) = mpsc::channel(COMMANDS_CAPACITY);
    let overflow = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let (wake_tx, wake_rx) = oneshot::channel();
    let closed_reported = Arc::new(AtomicBool::new(false));
    let events = ctx.events.clone();
    let hub = ctx.audio.clone();
    let meter = audio.meter.clone();
    // Recorded audio is not live: no alarms or loudness from playback.
    let sound = if playback {
        None
    } else {
        ctx.sound.add_tile(id, device, channel)
    };
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
            a.set_level_meter(meter);
            a.set_mono_output(sound.is_some());
            a
        },
        sound,
        talkback_ignored: 0,
        sequence: 0,
        last_video_pts: None,
        next_generation: 1,
        stale_results: 0,
        playback,
    };
    let runner = if state.ctx.tile_threads {
        Runner::Thread(
            std::thread::Builder::new()
                .name(format!("nvrview-tile-{id}"))
                .spawn(move || state.run(tier, sub, wake_rx, gate))?,
        )
    } else {
        let runtime = state.ctx.runtime.clone();
        let mut state = state;
        let task = runtime.spawn(async move {
            in_callback::mark_core_thread();
            state.serve(tier, sub, wake_rx, gate).await;
        });
        Runner::Task(task, runtime)
    };
    Ok((
        TileHandle {
            device,
            channel,
            tier,
            last_retry_ns: None,
            id,
            events,
            closed_reported,
            stop,
            wake: Some(wake_tx),
            commands: cmd_tx,
            runner: Some(runner),
        },
        ResultSink { tx, overflow },
    ))
}

/// Timestamps further apart than this (µs) in a switch between tiers of
/// one lens are taken to be on different clocks: no waiting for alignment.
const OTHER_CLOCK_US: i64 = 5_000_000;

/// One generation of a tile: a stream tier, its subscription, and the
/// pipeline that drives one platform decoder.
struct Generation {
    generation: u32,
    tier: StreamTier,
    sub: Option<Source>,
    p: TilePipeline,
    /// Playback: decodes sent and when (pts, core clock µs), to measure
    /// the decode latency.
    sent: VecDeque<(u64, u64)>,
    /// Camera timestamp of the picture the app reported decoded last.
    last_decoded_pts: Option<u64>,
    /// The device answered the start with 400 and the session is waiting
    /// for its stream (`tile_waiting` `waiting` was sent).
    waiting: bool,
    /// Video has arrived on this stream.
    video_seen: bool,
    /// When a 0x0 stream-info record arrived while no video had.
    info_only_since: Option<Instant>,
    /// Live: paces the pictures to their timestamps (live_pacer.rs).
    pacer: LivePacer,
    /// `tile_error` was sent for a stream that stayed info-only.
    info_only_reported: bool,
}

impl Generation {
    fn new(generation: u32, tier: StreamTier, sub: Option<Source>, p: TilePipeline) -> Self {
        Generation {
            generation,
            tier,
            sub,
            p,
            sent: VecDeque::new(),
            last_decoded_pts: None,
            waiting: false,
            video_seen: false,
            info_only_since: None,
            info_only_reported: false,
            pacer: LivePacer::default(),
        }
    }

    fn feed(&self) -> Option<&FeedRx> {
        self.sub.as_ref().and_then(Source::feed)
    }

    /// Playback: notes a `Decode` sent now.
    fn note_sent(&mut self, pts: u64, now_us: u64) {
        if self.feed().is_none() {
            return;
        }
        if self.sent.len() == LATENCY_SAMPLES {
            self.sent.pop_front();
        }
        self.sent.push_back((pts, now_us));
    }

    /// Playback: a picture was decoded; measures its latency.
    fn note_decoded(&mut self, pts: u64, now_us: u64) {
        let Some(i) = self.sent.iter().position(|(p, _)| *p == pts) else {
            return;
        };
        let (_, at) = self.sent[i];
        self.sent.drain(..=i);
        if let Some(f) = self.feed() {
            f.note_latency(now_us.saturating_sub(at));
        }
    }

    /// Playback: publishes the pipeline's backlog to the engine.
    fn publish_backlog(&self) {
        if let Some(f) = self.feed() {
            f.set_backlog((self.p.queued_units() + self.p.in_flight()) as u64);
        }
    }

    /// Notes the stream-info and video records for the info-only rule: a
    /// 0x0 info record starts a clock that the first video record stops.
    fn track_info_only(&mut self, record: &Record) {
        match record {
            Record::Video { .. } => {
                self.video_seen = true;
                self.info_only_since = None;
            }
            Record::Info { width, height, .. }
                if (*width == 0 || *height == 0) && !self.video_seen =>
            {
                self.info_only_since.get_or_insert_with(Instant::now);
            }
            _ => {}
        }
    }

    /// Reports (once) a stream that answered with a 0x0 info record and
    /// sent no video within `limit`, as a refused start with status 0.
    fn check_info_only(&mut self, tile: u64, limit: Duration, events: &EventSink) {
        let Some(since) = self.info_only_since else {
            return;
        };
        if self.info_only_reported || Instant::now() < since + limit {
            return;
        }
        self.info_only_reported = true;
        warn!(
            tile,
            generation = self.generation,
            "the device sent a 0x0 stream info and no video"
        );
        events.send(
            Object::event("tile_error")
                .u64("tile", tile)
                .str("reason", "start_rejected")
                .u64("status", 0)
                .finish(),
            Priority::State,
        );
    }

    /// Notes a subscription item for the `tile_waiting` event: the start of
    /// a wait for the device's stream, and its end at the first media.
    fn track_waiting(&mut self, tile: u64, item: &Item, events: &EventSink) {
        match item {
            Item::WaitingForDevice { status } if !self.waiting => {
                self.waiting = true;
                info!(
                    tile,
                    generation = self.generation,
                    status = *status,
                    "the device answered the start with 400; waiting for its stream"
                );
                events.send(
                    waiting_event(tile, self, "waiting")
                        .u64("status", u64::from(*status))
                        .finish(),
                    Priority::State,
                );
            }
            Item::Media(_) if self.waiting => {
                self.waiting = false;
                info!(
                    tile,
                    generation = self.generation,
                    "the device's stream arrived after a wait"
                );
                events.send(
                    waiting_event(tile, self, "streaming").finish(),
                    Priority::State,
                );
            }
            Item::StartRejected { .. } => self.waiting = false,
            _ => {}
        }
    }
}

fn waiting_event(tile: u64, g: &Generation, state: &str) -> Object {
    Object::event("tile_waiting")
        .u64("tile", tile)
        .str("tier", crate::device::tier_name(g.tier))
        .u64("generation", u64::from(g.generation))
        .str("state", state)
}

/// Whether two tiers of one channel are known to share one timestamp clock:
/// main and sub (observed, docs/protocol.md "Sub and main of one camera").
/// What the extern and mobile tiers show (another quality of the same lens,
/// or a second lens) and how their clocks relate is not measured yet.
fn one_clock(a: StreamTier, b: StreamTier) -> bool {
    let first = |t| matches!(t, StreamTier::Main | StreamTier::Sub);
    first(a) && first(b)
}

/// When a starting generation may be shown, and how it came to be.
#[derive(Clone, Copy, Debug)]
struct Ready {
    /// New picture's timestamp minus the old generation's latest decoded
    /// one, µs; `None` when nothing of the old generation was decoded.
    delta_us: Option<i64>,
    /// How long it waited for alignment after its first decoded picture.
    held: Duration,
    /// The wait hit the cap.
    capped: bool,
}

/// A generation being started, shown once it has decoded a picture that is
/// not older than the one shown (for tiers of one lens), or its first
/// picture otherwise; see [`Streams::apply`].
struct Starting {
    g: Generation,
    requested_ns: u64,
    deadline: Instant,
    /// Set when it can no longer succeed, with the reason.
    failed: Option<&'static str>,
    /// Compare timestamps with the shown generation: tiers on one clock.
    align: bool,
    /// When its first picture was reported decoded.
    first_decoded: Option<Instant>,
    /// Set once it may be shown.
    ready: Option<Ready>,
    /// Playback: shown cleared (`NV_ACTION_CLEAR` once it is current).
    clear: bool,
}

/// The tile's generations: the one shown, and at most one starting.
struct Streams {
    current: Generation,
    starting: Option<Starting>,
}

impl Streams {
    /// Applies a decoder result to the generation it belongs to. Results for
    /// a generation that is gone are dropped (counted).
    ///
    /// A starting generation becomes ready at its first decoded picture
    /// whose timestamp is at or after that of the shown generation's latest
    /// decoded picture (the picture on screen, near enough), so the switch
    /// never steps back in time; earlier pictures are decoded but not
    /// shown. Only for tiers on one clock ([`one_clock`]) and timestamps within
    /// [`OTHER_CLOCK_US`]; otherwise its first decoded picture will do.
    /// [`Streams::cap`] bounds the wait.
    fn apply(&mut self, r: DecoderResult, stale: &mut u64, now_us: u64) {
        let g = if r.generation == self.current.generation {
            &mut self.current
        } else if let Some(s) = self
            .starting
            .as_mut()
            .filter(|s| s.g.generation == r.generation)
        {
            &mut s.g
        } else {
            *stale += 1;
            return;
        };
        match r.kind {
            ResultKind::Decoded(pts) => {
                g.p.decoded(pts);
                g.last_decoded_pts = Some(pts);
                if let Some(f) = g.feed() {
                    f.set_decoded(pts);
                }
                g.note_decoded(pts, now_us);
            }
            ResultKind::Failed(kind) => {
                g.p.decode_failed(kind);
                return;
            }
        }
        let shown = self.current.last_decoded_pts;
        let Some(st) = self
            .starting
            .as_mut()
            .filter(|s| s.g.generation == r.generation && s.ready.is_none())
        else {
            return;
        };
        let ResultKind::Decoded(pts) = r.kind else {
            return;
        };
        let now = Instant::now();
        let first = *st.first_decoded.get_or_insert(now);
        let delta_us = shown.map(|o| pts as i64 - o as i64);
        let go = match (st.g.feed(), delta_us) {
            // Playback: the picture the engine named, or a later one.
            (Some(f), _) => f.target().is_some_and(|t| pts >= t),
            (None, None) => true,
            (None, Some(d)) => !st.align || d >= 0 || d.abs() > OTHER_CLOCK_US,
        };
        if go {
            st.ready = Some(Ready {
                delta_us,
                held: now - first,
                capped: false,
            });
        }
    }

    /// When a starting generation stops waiting for alignment: `cap` after
    /// its first decoded picture.
    fn align_deadline(&self, cap: Duration) -> Option<Instant> {
        let st = self.starting.as_ref()?;
        if st.ready.is_some() || st.failed.is_some() {
            return None;
        }
        st.first_decoded.map(|f| f + cap)
    }

    /// Makes a starting generation that has waited `cap` ready anyway: the
    /// old picture is held no longer than that.
    fn cap(&mut self, cap: Duration) {
        let Some(deadline) = self.align_deadline(cap) else {
            return;
        };
        let now = Instant::now();
        if now < deadline {
            return;
        }
        let shown = self.current.last_decoded_pts;
        let st = self.starting.as_mut().expect("checked");
        let first = st.first_decoded.expect("checked");
        st.ready = Some(Ready {
            delta_us: shown
                .zip(st.g.last_decoded_pts)
                .map(|(o, n)| n as i64 - o as i64),
            held: now - first,
            capped: true,
        });
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
    /// Sound analysis of the decoded audio (`None` when off).
    sound: Option<TileSoundLink>,
    /// Talk-back records dropped.
    talkback_ignored: u64,
    /// Media items seen, video and audio alike.
    sequence: u64,
    last_video_pts: Option<u64>,
    /// Number of the next generation started.
    next_generation: u32,
    /// Decoder results for generations that are gone.
    stale_results: u64,
    /// A playback tile.
    playback: bool,
}

fn codec(c: MediaCodec) -> Codec {
    match c {
        MediaCodec::H264 => Codec::H264,
        MediaCodec::H265 => Codec::H265,
    }
}

/// Waits for the next subscription item; never resolves without one.
async fn next_item(sub: &mut Option<Source>) -> Option<FeedItem> {
    match sub {
        Some(s) => s.recv().await,
        None => std::future::pending().await,
    }
}

/// Waits for the next item of the starting generation, if any. `Some(None)`
/// means its subscription ended.
async fn next_starting_item(starting: &mut Option<Starting>) -> Option<FeedItem> {
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
        sub: Option<Source>,
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
        sub: Option<Source>,
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
            current: Generation::new(
                0,
                tier,
                sub,
                TilePipeline::new(self.id as u32, self.pipeline_config()),
            ),
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
            let align_deadline = s.align_deadline(self.ctx.tier_switch_align_cap);
            let pace_deadline = s.current.pacer.next_due().map(|due| {
                tokio::time::Instant::now()
                    + Duration::from_micros(due.saturating_sub(self.ctx.clock.now_us()))
            });
            tokio::select! {
                biased;
                _ = &mut wake => break,
                r = self.results.recv() => match r {
                    Some(r) => s.apply(r, &mut self.stale_results, self.ctx.clock.now_us()),
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
                    if let Some(snd) = self.sound.as_mut() {
                        snd.end();
                    }
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
                _ = async {
                    match align_deadline {
                        Some(d) => tokio::time::sleep_until(d).await,
                        None => std::future::pending().await,
                    }
                } => {}
                _ = async {
                    match pace_deadline {
                        Some(d) => tokio::time::sleep_until(d).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
            self.link.apply(&mut self.audio);
            let now_us = self.ctx.clock.now_us();
            if let Some(snd) = self.sound.as_mut() {
                snd.tick(now_us.saturating_mul(1000));
            }
            s.current.p.tick(now_us);
            s.current
                .check_info_only(self.id, self.ctx.info_only_limit, &self.ctx.events);
            if let Some(st) = s.starting.as_mut() {
                st.g.p.tick(now_us);
                if st.failed.is_none() && Instant::now() >= st.deadline {
                    st.failed = Some("timeout");
                }
            }
            self.release_paced(&mut s.current, now_us);
            self.deliver(&mut s);
            s.cap(self.ctx.tier_switch_align_cap);
            self.settle(&mut s);
            s.current.publish_backlog();
            if let Some(st) = &s.starting {
                st.g.publish_backlog();
            }
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
        // Alarm episodes end with the tile (before `tile_closed`).
        if let Some(snd) = self.sound.as_mut() {
            snd.end();
        }
        // The subscriptions drop here, on this thread, before the core's
        // close returns: the session stops the previews.
        drop(s);
    }

    /// The pipeline settings: a playback tile keeps its picture however
    /// long it is paused (no clearing after a time without pictures).
    fn pipeline_config(&self) -> video_pipeline::PipelineConfig {
        let mut c = self.ctx.pipeline;
        if self.playback {
            c.clear_after_us = 0;
        }
        c
    }

    /// Sends a `tile_tier` event, except for playback tiles (the playback
    /// reports seeks and restarts itself).
    fn tier_event(&self, o: Object) {
        if !self.playback {
            self.ctx.events.send(o.finish(), Priority::State);
        }
    }

    /// A tier request from the app.
    fn command(&mut self, s: &mut Streams, c: TileCommand) {
        match c {
            TileCommand::Playback {
                generation,
                tier,
                feed,
            } => {
                if let Some(st) = s.starting.take() {
                    self.abandon(st, "superseded");
                }
                self.next_generation = generation.wrapping_add(1);
                info!(tile = self.id, generation, "playback generation starting");
                s.starting = Some(Starting {
                    g: Generation::new(
                        generation,
                        tier,
                        Some(Source::Feed(feed)),
                        TilePipeline::new(self.id as u32, self.pipeline_config()),
                    ),
                    requested_ns: self.ctx.clock.now_ns(),
                    deadline: Instant::now() + PLAYBACK_START_LIMIT,
                    failed: None,
                    align: false,
                    first_decoded: None,
                    ready: None,
                    clear: false,
                });
            }
            TileCommand::Reaffirm => {
                if s.starting.is_none() {
                    self.tier_event(tier_event(
                        self.id,
                        s.current.tier,
                        "active",
                        s.current.generation,
                    ));
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
                    self.tier_event(tier_event(self.id, tier, "active", s.current.generation));
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
                self.tier_event(tier_event(self.id, tier, "starting", generation));
                s.starting = Some(Starting {
                    g: Generation::new(
                        generation,
                        tier,
                        sub.map(Source::Live),
                        TilePipeline::new(self.id as u32, self.pipeline_config()),
                    ),
                    requested_ns,
                    deadline: Instant::now() + self.ctx.tier_switch_timeout,
                    failed,
                    align: one_clock(tier, s.current.tier),
                    first_decoded: None,
                    ready: None,
                    clear: false,
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
        self.tier_event(tier_event(self.id, g.tier, "failed", g.generation).str("reason", reason));
        // Its subscription drops here: the stream stops unless another tile
        // shows it.
        drop(g);
    }

    /// Finishes a switch that is ready (see [`Streams::apply`]), or abandons
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
        let Some(ready) = st.ready else {
            return;
        };
        if self.stop.load(Ordering::SeqCst) {
            return;
        }
        let st = s.starting.take().expect("checked");
        let old = std::mem::replace(&mut s.current, st.g);
        // The old generation's last action: the app destroys its decoder
        // and shows the new generation from now on.
        self.ctx
            .callbacks
            .on_action(self.id, old.generation, &Action::Reset);
        if let Some(f) = s.current.feed() {
            f.set_shown();
        }
        if st.clear {
            self.ctx
                .callbacks
                .on_action(self.id, s.current.generation, &Action::Clear);
        }
        if self.playback {
            info!(
                tile = self.id,
                generation = s.current.generation,
                "playback generation shown"
            );
            self.last_video_pts = None;
            self.audio.discontinuity();
            drop(old);
            return;
        }
        let elapsed = self.ctx.clock.now_ns().saturating_sub(st.requested_ns);
        info!(
            tile = self.id,
            generation = s.current.generation,
            tier = crate::device::tier_name(s.current.tier),
            ms = elapsed / 1_000_000,
            delta_ms = ready.delta_us.map(|d| d / 1_000),
            held_ms = ready.held.as_millis() as u64,
            capped = ready.capped,
            "tier switched"
        );
        self.tier_event(tier_event(
            self.id,
            s.current.tier,
            "active",
            s.current.generation,
        ));
        let mut timing = Object::event("timing")
            .str("what", "tier_switch")
            .u64("tile", self.id)
            .u64("ns", elapsed);
        if let Some(d) = ready.delta_us {
            timing = timing.f64("delta_ms", d as f64 / 1e3);
        }
        self.ctx.events.send(
            timing
                .f64("held_ms", ready.held.as_secs_f64() * 1e3)
                .bool("capped", ready.capped)
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
    fn starting_item(tile: u64, st: &mut Starting, item: FeedItem, ctx: &TileContext) {
        let item = match item {
            FeedItem::Item(i) => i,
            FeedItem::Clear => {
                // Nothing to show at the position: shown at once, cleared.
                st.clear = true;
                st.ready.get_or_insert(Ready {
                    delta_us: None,
                    held: Duration::ZERO,
                    capped: false,
                });
                return;
            }
            FeedItem::Target => {
                // Named after its picture was decoded already.
                let reached =
                    st.g.feed()
                        .and_then(FeedRx::target)
                        .zip(st.g.last_decoded_pts)
                        .is_some_and(|(t, last)| last >= t);
                if reached && st.ready.is_none() {
                    st.ready = Some(Ready {
                        delta_us: None,
                        held: Duration::ZERO,
                        capped: false,
                    });
                }
                return;
            }
        };
        st.g.track_waiting(tile, &item, &ctx.events);
        match item {
            Item::Media(Record::Video {
                codec: c,
                keyframe,
                pts_us,
                data,
                ..
            }) => {
                st.g.p.tick(ctx.clock.now_us());
                st.g.p.push(AccessUnit {
                    codec: codec(c),
                    keyframe,
                    pts: pts_us,
                    data,
                });
            }
            Item::Media(_) | Item::WaitingForDevice { .. } => {}
            Item::Discontinuity => st.g.p.discontinuity(),
            Item::StartRejected { status } => {
                warn!(tile, status, "the device refused to start the new tier");
                st.failed = Some("start_rejected");
            }
        }
    }

    /// Takes an item of the shown generation: a live stream's pictures (and
    /// the stream info between them) go through its pacer, the rest at once.
    fn item(&mut self, g: &mut Generation, item: FeedItem) {
        if !self.playback && matches!(g.sub, Some(Source::Live(_))) {
            match item {
                FeedItem::Item(Item::Media(r @ (Record::Video { .. } | Record::Info { .. }))) => {
                    let now = self.ctx.clock.now_us();
                    g.pacer.push(now, r);
                    self.release_paced(g, now);
                    return;
                }
                FeedItem::Item(Item::Discontinuity) => {
                    let held: Vec<Record> = g.pacer.flush().collect();
                    for r in held {
                        self.apply_item(g, FeedItem::Item(Item::Media(r)));
                    }
                    self.apply_item(g, item);
                    return;
                }
                other => return self.apply_item(g, other),
            }
        }
        self.apply_item(g, item);
    }

    /// Hands the paced records that are due to the pipeline.
    fn release_paced(&mut self, g: &mut Generation, now: u64) {
        while let Some(r) = g.pacer.pop_due(now) {
            self.apply_item(g, FeedItem::Item(Item::Media(r)));
        }
    }

    fn apply_item(&mut self, g: &mut Generation, item: FeedItem) {
        let item = match item {
            FeedItem::Item(i) => i,
            FeedItem::Clear => {
                self.ctx
                    .callbacks
                    .on_action(self.id, g.generation, &Action::Clear);
                return;
            }
            FeedItem::Target => return,
        };
        g.track_waiting(self.id, &item, &self.ctx.events);
        if let Item::Media(r) = &item {
            g.track_info_only(r);
        }
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
                ..
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
                    wall_clock_us: Some(self.ctx.clock.wall_now_us()).filter(|w| *w > 0),
                };
                if frame.is_talkback() {
                    self.talkback_ignored += 1;
                    return;
                }
                self.link.apply(&mut self.audio);
                self.audio.push(frame.record());
                if let (Some(snd), Some(b)) = (self.sound.as_mut(), self.audio.mono_block()) {
                    snd.push(&b);
                }
                self.link.pushed();
            }
            Item::Media(Record::Info { .. }) => {}
            Item::Discontinuity => {
                self.last_video_pts = None;
                self.audio.discontinuity();
                p.discontinuity();
            }
            // Reported by `track_waiting`; the session keeps trying.
            Item::WaitingForDevice { .. } => {}
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
            let decode = match action {
                Action::Decode { pts, .. } => Some(pts),
                _ => None,
            };
            self.ctx
                .callbacks
                .on_action(self.id, s.current.generation, &action);
            if let Some(pts) = decode {
                s.current.note_sent(pts, self.ctx.clock.now_us());
            }
            while let Ok(r) = self.results.try_recv() {
                s.apply(r, &mut self.stale_results, self.ctx.clock.now_us());
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
            let decode = match action {
                Action::Decode { pts, .. } => Some(pts),
                _ => None,
            };
            self.ctx
                .callbacks
                .on_action(self.id, st.g.generation, &action);
            if let Some(pts) = decode {
                st.g.note_sent(pts, self.ctx.clock.now_us());
            }
            while let Ok(r) = self.results.try_recv() {
                s.apply(r, &mut self.stale_results, self.ctx.clock.now_us());
            }
        }
    }

    fn stats(&self, g: &Generation) {
        let s = g.p.stats();
        let queue_dropped = g.sub.as_ref().map_or(0, Source::dropped);
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
