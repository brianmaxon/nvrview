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
use bc_session::{Item, Subscription};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, MissedTickBehavior, interval};
use tracing::{info, warn};
use video_pipeline::{AccessUnit, Codec, DecodeFailureKind, PipelineConfig, TilePipeline};

/// Decoder results waiting for a tile thread. Every `Decode` gets one result
/// and at most a few are in flight, so this is only reached by an app that
/// reports results that match nothing.
pub(crate) const RESULTS_CAPACITY: usize = 256;

/// A result reported by the platform decoder.
#[derive(Clone, Copy, Debug)]
pub(crate) enum DecoderResult {
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
    id: u64,
    events: EventSink,
    /// Set once `tile_closed` has been sent.
    closed_reported: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    wake: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl TileHandle {
    /// Asks the thread to stop: no further action is started.
    pub(crate) fn signal_stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(w) = self.wake.take() {
            let _ = w.send(());
        }
    }

    /// Stops and waits for the thread (and an `on_action` in progress) to
    /// end. Its subscription is dropped on the way out, which stops the
    /// preview on the device.
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

/// Starts a tile thread. It does nothing until `gate` resolves (or its
/// sender is dropped), so no action runs before the app has the tile's ID.
/// `sub` is `None` when the device session has already ended; the tile then
/// ends at once with `login_failed`, as it does when its session ends later.
pub(crate) fn spawn(
    id: u64,
    device: u64,
    sub: Option<Subscription>,
    ctx: TileContext,
    audio: Arc<TileAudioControl>,
    gate: oneshot::Receiver<()>,
) -> std::io::Result<(TileHandle, ResultSink)> {
    let (tx, rx) = mpsc::channel(RESULTS_CAPACITY);
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
        overflow: overflow.clone(),
        link: TileAudioLink {
            hub,
            control: audio,
            seen: 0,
            tapped: false,
        },
        audio: TileAudio::new(TileConfig::default()),
        talkback_ignored: 0,
        sequence: 0,
        last_video_pts: None,
    };
    let thread = std::thread::Builder::new()
        .name(format!("nvrview-tile-{id}"))
        .spawn(move || state.run(sub, wake_rx, gate))?;
    Ok((
        TileHandle {
            device,
            id,
            events,
            closed_reported,
            stop,
            wake: Some(wake_tx),
            thread: Some(thread),
        },
        ResultSink { tx, overflow },
    ))
}

struct TileThread {
    closed_reported: Arc<AtomicBool>,
    id: u64,
    ctx: TileContext,
    stop: Arc<AtomicBool>,
    results: mpsc::Receiver<DecoderResult>,
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
}

fn codec(c: MediaCodec) -> Codec {
    match c {
        MediaCodec::H264 => Codec::H264,
        MediaCodec::H265 => Codec::H265,
    }
}

fn apply(p: &mut TilePipeline, r: DecoderResult) {
    match r {
        DecoderResult::Decoded(pts) => p.decoded(pts),
        DecoderResult::Failed(kind) => p.decode_failed(kind),
    }
}

/// Waits for the next subscription item; never resolves without one.
async fn next_item(sub: &mut Option<Subscription>) -> Option<Item> {
    match sub {
        Some(s) => s.recv().await,
        None => std::future::pending().await,
    }
}

impl TileThread {
    fn run(
        mut self,
        sub: Option<Subscription>,
        wake: oneshot::Receiver<()>,
        gate: oneshot::Receiver<()>,
    ) {
        in_callback::mark_core_thread();
        let runtime = self.ctx.runtime.clone();
        // This thread drives its own future; the runtime's workers only run
        // the sessions and timers, so a slow callback here holds up nothing
        // else.
        runtime.block_on(self.serve(sub, wake, gate));
    }

    async fn serve(
        &mut self,
        mut sub: Option<Subscription>,
        mut wake: oneshot::Receiver<()>,
        gate: oneshot::Receiver<()>,
    ) {
        // Released (or dropped) once the app has the tile's ID.
        tokio::select! {
            biased;
            _ = &mut wake => return,
            _ = gate => {}
        }
        let mut p = TilePipeline::new(self.id as u32, self.ctx.pipeline);
        let mut tick = interval(self.ctx.tick_interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut next_stats = Instant::now() + self.ctx.stats_interval;
        let mut overflow_seen = 0;
        loop {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            tokio::select! {
                biased;
                _ = &mut wake => break,
                r = self.results.recv() => match r {
                    Some(r) => apply(&mut p, r),
                    // The core dropped the sender: the tile is closing.
                    None => break,
                },
                item = next_item(&mut sub) => match item {
                    Some(item) => self.item(&mut p, item),
                    None => sub = None,
                },
                // The session ended for good (the device refused the
                // login): nothing will ever arrive.
                _ = std::future::ready(()), if sub.is_none() => {
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
            p.tick(self.ctx.clock.now_us());
            self.deliver(&mut p);
            let now = Instant::now();
            if now >= next_stats {
                next_stats = now + self.ctx.stats_interval;
                self.stats(&p, sub.as_ref());
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
        // `sub` drops here, on this thread, before the core's close returns:
        // the session stops the preview.
        drop(sub);
    }

    fn item(&mut self, p: &mut TilePipeline, item: Item) {
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

    /// Carries out every pending action in order. Results that arrive while
    /// an action runs (often from inside the callback) are applied before
    /// the next action, as a platform answering synchronously expects.
    fn deliver(&mut self, p: &mut TilePipeline) {
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return;
            }
            let Some(action) = p.next_action() else {
                return;
            };
            self.ctx.callbacks.on_action(self.id, &action);
            while let Ok(r) = self.results.try_recv() {
                apply(p, r);
            }
        }
    }

    fn stats(&self, p: &TilePipeline, sub: Option<&Subscription>) {
        let s = p.stats();
        let queue_dropped = sub.map_or(0, Subscription::dropped);
        self.ctx.events.send(
            Object::event("tile_stats")
                .u64("tile", self.id)
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
