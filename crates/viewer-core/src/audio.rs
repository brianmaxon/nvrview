//! Listening: each tile's audio through `audio-pipeline` (docs/AUDIO.md).
//!
//! - Every tile runs a [`TileAudio`] on its own thread. Live streams
//!   interleave audio records with video, and a record has no timestamp of
//!   its own: its position after the last video picture and its arrival time
//!   on the host clock ([`crate::clock`]) are its timing, handed over with
//!   [`AudioFrame::record`].
//! - The core owns one [`AudioOutput`] (tiles claim playout slots on it) and
//!   its [`Renderer`], which the app's real-time callback drives through
//!   [`crate::Core::render_audio`]. The renderer changes hands without locks
//!   ([`RenderHandoff`]).
//! - Per-tile settings (audible, volume, tap) are set by the app and picked
//!   up by the tile thread on its next item or tick ([`TileAudioControl`]).
//! - Taps: each tapped tile pushes 16 kHz mono blocks into its own bounded,
//!   drop-oldest queue; one core thread ([`TapHub`]) delivers them to the
//!   app's `on_audio_tap`.
//!
//! **Talk-back records are ignored.** The official client sends its own
//! talk-back audio as `01wb` records (reported, docs/protocol.md). Should a
//! device ever echo such records in a live stream, they would be the
//! viewer's own voice, not the camera's, so records with stream index
//! [`TALKBACK_STREAM_INDEX`] are counted (`talkback_ignored` in
//! `audio_stats`) and dropped. Every other index is camera audio and goes
//! to the decoder, which picks the codec from it.

use crate::in_callback;
use crate::{Callbacks, CoreError};
use audio_pipeline::{
    AudioOutput, AudioRecord, DecodeLevel, OutputConfig, OutputFormat, TapReceiver, TapSender,
};
use crossbeam_queue::ArrayQueue;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{JoinHandle, Thread};
use std::time::Duration;

pub use audio_pipeline::{AudioStats, Renderer, TAP_RATE, TapBlock, TileAudio, TileConfig};

/// Stream index of talk-back records (`01wb`), which are never played.
pub const TALKBACK_STREAM_INDEX: u8 = 1;
/// Blocks a tap holds for a slow consumer (64 ms each: about 4 s).
pub const TAP_CAPACITY: usize = 64;
/// Output sample rates the core accepts.
pub const OUTPUT_RATES: std::ops::RangeInclusive<u32> = 8_000..=384_000;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One audio record with the timing context the stream gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFrame {
    /// The two-digit stream index from the record magic.
    pub stream_index: u8,
    /// The record's payload, as received.
    pub data: Vec<u8>,
    /// Position of this record among all media items of the tile's stream
    /// (video and audio share the counter), preserving interleave order.
    pub sequence: u64,
    /// Timestamp of the last video picture before this record, if any since
    /// the last discontinuity.
    pub after_video_pts_us: Option<u64>,
    /// Arrival time on the host clock, microseconds.
    pub arrived_us: u64,
    /// Arrival wall-clock time, microseconds since the Unix epoch.
    pub wall_clock_us: Option<i64>,
}

impl AudioFrame {
    /// The record as `audio-pipeline` takes it.
    pub fn record(&self) -> AudioRecord<'_> {
        AudioRecord {
            stream_index: self.stream_index,
            data: &self.data,
            after_video_pts_us: self.after_video_pts_us,
            arrived_us: self.arrived_us,
            wall_clock_us: self.wall_clock_us,
        }
    }

    /// True for talk-back records, which are never played.
    pub fn is_talkback(&self) -> bool {
        self.stream_index == TALKBACK_STREAM_INDEX
    }
}

/// What the app set for one tile; read by the tile thread.
#[derive(Debug)]
pub(crate) struct TileAudioControl {
    audible: AtomicBool,
    volume: AtomicU32,
    /// The tap to attach (`Some`) or detach (`None`), taken by the tile.
    tap: Mutex<Option<Option<TapSender>>>,
    tapped: AtomicBool,
    /// Where the tile measures its decoded audio ([`AudioHub::level`]);
    /// `None` when every meter is taken.
    pub(crate) meter: Option<Arc<DecodeLevel>>,
}

impl Default for TileAudioControl {
    fn default() -> Self {
        TileAudioControl {
            audible: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            tap: Mutex::new(None),
            tapped: AtomicBool::new(false),
            meter: None,
        }
    }
}

impl TileAudioControl {
    pub(crate) fn audible(&self) -> bool {
        self.audible.load(Ordering::Acquire)
    }

    pub(crate) fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Acquire))
    }

    pub(crate) fn tapped(&self) -> bool {
        self.tapped.load(Ordering::Acquire)
    }
}

/// Hands the renderer between the app's real-time thread and the control
/// thread that replaces it, without locks or allocation on the real-time
/// side.
///
/// `current` holds the renderer while nobody renders. A render call pops it,
/// renders, and pushes it back. If a new output was installed meanwhile
/// (`current` is full), the old renderer goes to `retired`, which the next
/// control call drops (never the real-time thread). Should even `retired` be
/// full, the renderer is leaked rather than freed on the real-time thread
/// (counted; it cannot happen with one rendering thread, since every
/// install drains `retired` first and only a render that overlapped an
/// install retires).
pub(crate) struct RenderHandoff {
    /// Each playout slot's level after the last render, for
    /// [`crate::Core::tile_audio_level`]; written only by the render side.
    levels: Box<[LevelCell]>,
    current: ArrayQueue<Renderer>,
    retired: ArrayQueue<Renderer>,
    leaked: AtomicU64,
    busy: AtomicU64,
    installed: AtomicBool,
    /// `host_time_ns` of the last successful render; 0: none yet.
    rendered_ns: AtomicU64,
}

impl RenderHandoff {
    fn new() -> RenderHandoff {
        RenderHandoff {
            levels: (0..OutputConfig::default().max_tiles)
                .map(|_| LevelCell::default())
                .collect(),
            current: ArrayQueue::new(1),
            retired: ArrayQueue::new(4),
            leaked: AtomicU64::new(0),
            busy: AtomicU64::new(0),
            installed: AtomicBool::new(false),
            rendered_ns: AtomicU64::new(0),
        }
    }

    /// Control side: installs `r` (or removes the renderer with `None`).
    fn install(&self, r: Option<Renderer>) {
        while self.retired.pop().is_some() {}
        for c in self.levels.iter() {
            c.clear();
        }
        self.installed.store(r.is_some(), Ordering::Release);
        drop(self.current.pop());
        if let Some(r) = r {
            // Only control calls (serialized by the caller) push here; a
            // render in progress pushes back into a full queue and retires.
            drop(self.current.force_push(r));
        }
    }

    /// Real-time side. Fills `out` with the mix, or with silence when there
    /// is no output or another thread is rendering
    /// ([`CoreError::Closed`]), or its channel count differs
    /// ([`CoreError::InvalidArgument`]).
    fn render(&self, out: &mut [f32], channels: usize, host_time_ns: u64) -> Result<(), CoreError> {
        let Some(mut r) = self.current.pop() else {
            if self.installed.load(Ordering::Acquire) {
                // Another render holds it (or an install is mid-swap).
                self.busy.fetch_add(1, Ordering::Relaxed);
            }
            out.fill(0.0);
            return Err(CoreError::Closed);
        };
        let ok = if r.format().channels == channels {
            r.render(out, host_time_ns);
            for (cell, l) in self.levels.iter().zip(r.levels()) {
                cell.set(l.owner, l.peak, l.rms);
            }
            self.rendered_ns
                .store(host_time_ns.max(1), Ordering::Release);
            Ok(())
        } else {
            out.fill(0.0);
            Err(CoreError::InvalidArgument)
        };
        if let Err(r) = self.current.push(r)
            && let Err(r) = self.retired.push(r)
        {
            self.leaked.fetch_add(1, Ordering::Relaxed);
            std::mem::forget(r);
        }
        ok
    }
}

/// One slot's level, readable from any thread without a lock.
#[derive(Default)]
struct LevelCell {
    owner: AtomicU64,
    peak: AtomicU32,
    rms: AtomicU32,
}

impl LevelCell {
    fn set(&self, owner: u64, peak: f32, rms: f32) {
        self.peak.store(peak.to_bits(), Ordering::Relaxed);
        self.rms.store(rms.to_bits(), Ordering::Relaxed);
        self.owner.store(owner, Ordering::Release);
    }

    fn clear(&self) {
        self.set(0, 0.0, 0.0);
    }
}

/// Render levels older than this (the last render's host time, ns) are not
/// used: the app stopped rendering, so the decode-time level stands in.
const RENDER_LEVEL_STALE_NS: u64 = 500_000_000;

/// Meters beyond `max_tiles` (see [`AudioHub::new`]).
const METER_SPARE: usize = 64;

/// A tile's decode-time meter, claimed while the tile exists.
struct MeterCell {
    owner: AtomicU64,
    level: Arc<DecodeLevel>,
}

/// The core's audio state.
pub(crate) struct AudioHub {
    output: Mutex<Option<AudioOutput>>,
    handoff: RenderHandoff,
    /// One decode-time meter per possible tile, found by owner without a
    /// lock ([`AudioHub::level`]); claimed and released under `controls`.
    meters: Box<[MeterCell]>,
    /// Bumped on every change a tile must pick up.
    generation: AtomicU64,
    controls: Mutex<HashMap<u64, Arc<TileAudioControl>>>,
    pub(crate) taps: TapHub,
}

impl AudioHub {
    /// `max_tiles`: how many tiles can exist at once. Each gets a meter;
    /// a few spare ones cover tiles opened while closed ones are still
    /// releasing theirs.
    pub(crate) fn new(
        callbacks: Arc<dyn Callbacks>,
        max_tiles: usize,
    ) -> std::io::Result<AudioHub> {
        Ok(AudioHub {
            output: Mutex::new(None),
            handoff: RenderHandoff::new(),
            meters: (0..max_tiles.saturating_add(METER_SPARE))
                .map(|_| MeterCell {
                    owner: AtomicU64::new(0),
                    level: Arc::new(DecodeLevel::new()),
                })
                .collect(),
            generation: AtomicU64::new(1),
            controls: Mutex::new(HashMap::new()),
            taps: TapHub::spawn(callbacks)?,
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn bump(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// The output tiles should play into.
    pub(crate) fn output(&self) -> Option<AudioOutput> {
        lock(&self.output).clone()
    }

    /// Makes a new output (`None` removes it). Tiles move to it on their next
    /// item or tick.
    pub(crate) fn set_output(&self, format: Option<OutputFormat>) {
        let mut o = lock(&self.output);
        match format {
            Some(f) => {
                let (output, renderer) = audio_pipeline::output(f, OutputConfig::default());
                self.handoff.install(Some(renderer));
                *o = Some(output);
            }
            None => {
                self.handoff.install(None);
                *o = None;
            }
        }
        drop(o);
        self.bump();
    }

    pub(crate) fn render(
        &self,
        out: &mut [f32],
        channels: usize,
        host_time_ns: u64,
    ) -> Result<(), CoreError> {
        self.handoff.render(out, channels, host_time_ns)
    }

    /// A tile's level (peak, RMS) before volume: as the last render
    /// measured it while the tile plays and the app renders, otherwise as
    /// its audio was decoded (decaying to 0 when none arrives). Zero for an
    /// unknown tile or one without audio. Lock-free and allocation-free.
    pub(crate) fn level(&self, tile: u64) -> (f32, f32) {
        let now_ns = crate::clock::now_ns();
        let rendered = self.handoff.rendered_ns.load(Ordering::Acquire);
        if rendered != 0 && now_ns <= rendered.saturating_add(RENDER_LEVEL_STALE_NS) {
            for c in self.handoff.levels.iter() {
                if c.owner.load(Ordering::Acquire) == tile {
                    return (
                        f32::from_bits(c.peak.load(Ordering::Relaxed)),
                        f32::from_bits(c.rms.load(Ordering::Relaxed)),
                    );
                }
            }
        }
        for m in self.meters.iter() {
            if m.owner.load(Ordering::Acquire) == tile {
                return m.level.read(now_ns / 1000);
            }
        }
        (0.0, 0.0)
    }

    /// The control of a new tile, with a decode-time meter when one is free.
    pub(crate) fn add_tile(&self, tile: u64) -> Arc<TileAudioControl> {
        let mut controls = lock(&self.controls);
        let meter = self
            .meters
            .iter()
            .find(|m| m.owner.load(Ordering::Acquire) == 0)
            .map(|m| {
                m.level.clear();
                m.owner.store(tile, Ordering::Release);
                m.level.clone()
            });
        let c = Arc::new(TileAudioControl {
            meter,
            ..TileAudioControl::default()
        });
        controls.insert(tile, c.clone());
        c
    }

    /// Forgets a tile that ended (its thread has been joined): no tap block
    /// for it is delivered after this returns.
    pub(crate) fn remove_tile(&self, tile: u64) {
        let mut controls = lock(&self.controls);
        controls.remove(&tile);
        for m in self.meters.iter() {
            if m.owner.load(Ordering::Acquire) == tile {
                m.owner.store(0, Ordering::Release);
            }
        }
        drop(controls);
        self.taps.unregister(tile);
    }

    fn control(&self, tile: u64) -> Option<Arc<TileAudioControl>> {
        lock(&self.controls).get(&tile).cloned()
    }

    /// Returns false for an unknown tile.
    pub(crate) fn set_tile(&self, tile: u64, audible: bool, volume: f32) -> bool {
        let Some(c) = self.control(tile) else {
            return false;
        };
        c.audible.store(audible, Ordering::Release);
        c.volume.store(volume.to_bits(), Ordering::Release);
        self.bump();
        true
    }

    /// Returns false for an unknown tile.
    pub(crate) fn set_tap(&self, tile: u64, enabled: bool) -> bool {
        let Some(c) = self.control(tile) else {
            return false;
        };
        if c.tapped.swap(enabled, Ordering::AcqRel) == enabled {
            return true;
        }
        if enabled {
            let (tx, rx) = audio_pipeline::tap(TAP_CAPACITY);
            self.taps.register(tile, rx);
            *lock(&c.tap) = Some(Some(tx));
        } else {
            *lock(&c.tap) = Some(None);
            self.taps.unregister(tile);
        }
        self.bump();
        true
    }

    /// Real-time renders that found another render in progress, and
    /// renderers leaked instead of freed on the real-time thread.
    pub(crate) fn render_counters(&self) -> (u64, u64) {
        (
            self.handoff.busy.load(Ordering::Relaxed),
            self.handoff.leaked.load(Ordering::Relaxed),
        )
    }

    pub(crate) fn shutdown(&self) {
        self.taps.stop();
        self.set_output(None);
    }
}

/// A tile's view of its audio settings.
pub(crate) struct TileAudioLink {
    pub(crate) hub: Arc<AudioHub>,
    pub(crate) control: Arc<TileAudioControl>,
    pub(crate) seen: u64,
    pub(crate) tapped: bool,
}

impl TileAudioLink {
    /// Applies whatever changed since the last call. Never blocks for long:
    /// the locks it takes are held only briefly by control calls.
    pub(crate) fn apply(&mut self, audio: &mut TileAudio) {
        let g = self.hub.generation();
        if g == self.seen {
            return;
        }
        self.seen = g;
        audio.set_output(self.hub.output());
        audio.set_volume(self.control.volume());
        audio.set_audible(self.control.audible());
        if let Some(tap) = lock(&self.control.tap).take() {
            self.tapped = tap.is_some();
            audio.set_tap(tap);
        }
    }

    /// Wakes the tap thread after a record, if this tile is tapped.
    pub(crate) fn pushed(&self) {
        if self.tapped {
            self.hub.taps.wake();
        }
    }
}

/// The tap delivery thread and its registry of tapped tiles.
pub(crate) struct TapHub {
    shared: Arc<TapShared>,
    thread: Mutex<Option<JoinHandle<()>>>,
    handle: Thread,
}

struct TapShared {
    /// Held while delivering, so unregistering waits for a delivery to the
    /// tile in progress.
    registry: Mutex<Vec<(u64, TapReceiver)>>,
    stop: AtomicBool,
}

/// How long the tap thread sleeps without a wake-up (a backstop).
const TAP_POLL: Duration = Duration::from_millis(100);

impl TapHub {
    fn spawn(callbacks: Arc<dyn Callbacks>) -> std::io::Result<TapHub> {
        let shared = Arc::new(TapShared {
            registry: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("nvrview-audio-tap".into())
            .spawn(move || {
                in_callback::mark_core_thread();
                while !s.stop.load(Ordering::Acquire) {
                    {
                        let reg = lock(&s.registry);
                        for (tile, rx) in reg.iter() {
                            while let Some(block) = rx.pop() {
                                if s.stop.load(Ordering::Acquire) {
                                    return;
                                }
                                callbacks.on_audio_tap(*tile, &block);
                            }
                        }
                    }
                    std::thread::park_timeout(TAP_POLL);
                }
            })?;
        let handle = thread.thread().clone();
        Ok(TapHub {
            shared,
            thread: Mutex::new(Some(thread)),
            handle,
        })
    }

    fn register(&self, tile: u64, rx: TapReceiver) {
        let mut reg = lock(&self.shared.registry);
        reg.retain(|(t, _)| *t != tile);
        reg.push((tile, rx));
    }

    /// No block for `tile` is delivered after this returns.
    fn unregister(&self, tile: u64) {
        lock(&self.shared.registry).retain(|(t, _)| *t != tile);
    }

    fn wake(&self) {
        self.handle.unpark();
    }

    /// Stops and joins the thread; no `on_audio_tap` runs after this.
    fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.handle.unpark();
        if let Some(t) = lock(&self.thread).take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer(channels: usize) -> Renderer {
        audio_pipeline::output(
            OutputFormat {
                sample_rate: 48_000,
                channels,
            },
            OutputConfig::default(),
        )
        .1
    }

    #[test]
    fn handoff_renders_replaces_and_never_frees_on_the_render_side() {
        let h = RenderHandoff::new();
        let mut buf = vec![1.0f32; 960];
        assert_eq!(
            h.render(&mut buf, 2, 0),
            Err(CoreError::Closed),
            "nothing installed"
        );
        assert!(buf.iter().all(|&s| s == 0.0));
        h.install(Some(renderer(2)));
        assert!(h.render(&mut buf, 2, 0).is_ok());
        buf.fill(1.0);
        assert_eq!(h.render(&mut buf, 1, 0), Err(CoreError::InvalidArgument));
        assert!(buf.iter().all(|&s| s == 0.0));

        // An install while a render holds the renderer: the render retires
        // the old one; the next install frees it.
        let held = h.current.pop().unwrap();
        h.install(Some(renderer(1)));
        if let Err(r) = h.current.push(held) {
            h.retired.push(r).ok().unwrap();
        }
        assert_eq!(h.retired.len(), 1);
        assert!(h.render(&mut buf[..480], 1, 0).is_ok());
        h.install(None);
        assert!(h.retired.is_empty() && h.current.is_empty());
        assert_eq!(h.leaked.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn talkback_is_recognized_by_stream_index() {
        let mut f = AudioFrame {
            stream_index: 5,
            data: vec![],
            sequence: 0,
            after_video_pts_us: None,
            arrived_us: 0,
            wall_clock_us: None,
        };
        assert!(!f.is_talkback());
        f.stream_index = 1;
        assert!(f.is_talkback());
        f.stream_index = 15;
        assert!(!f.is_talkback());
    }

    struct NoCallbacks;

    impl Callbacks for NoCallbacks {
        fn on_action(&self, _: u64, _: u32, _: &crate::Action<'_>) {}
        fn on_event(&self, _: &str) {}
    }

    /// A playing tile reads the render's level while the app renders; any
    /// other tile (or one whose renders stopped) reads its decode-time
    /// meter; an unknown tile reads 0.
    #[test]
    fn levels_come_from_the_render_or_from_decoding() {
        let hub = AudioHub::new(Arc::new(NoCallbacks), 2).unwrap();
        let (a, b) = (7, 8);
        let ca = hub.add_tile(a);
        let cb = hub.add_tile(b);
        let now_us = crate::clock::now_ns() / 1000;
        let tone: Vec<f32> = (0..1024).map(|i| 0.4 * (i as f32 * 0.3).sin()).collect();
        for c in [&ca, &cb] {
            c.meter.as_ref().unwrap().observe(&tone, 1, 16_000, now_us);
        }
        let (peak, rms) = hub.level(b);
        assert!(peak > 0.3 && rms > 0.1, "{peak} {rms}");
        assert_eq!(hub.level(99), (0.0, 0.0));

        // Tile a plays: a fresh render measured 0.9 for it.
        hub.handoff.levels[0].set(a, 0.9, 0.5);
        hub.handoff
            .rendered_ns
            .store(crate::clock::now_ns(), Ordering::Release);
        assert_eq!(hub.level(a), (0.9, 0.5));
        assert!(hub.level(b).0 < 0.5, "b is not playing: its own meter");
        // The app stopped rendering: no frozen render value.
        hub.handoff.rendered_ns.store(
            crate::clock::now_ns() - 2 * RENDER_LEVEL_STALE_NS,
            Ordering::Release,
        );
        assert!(hub.level(a).0 < 0.5);

        // Meters are reused only after their tile is gone, and start at 0.
        hub.remove_tile(b);
        assert_eq!(hub.level(b), (0.0, 0.0));
        let cc = hub.add_tile(9);
        assert_eq!(hub.level(9), (0.0, 0.0));
        assert!(cc.meter.is_some());
        hub.shutdown();
    }

    /// Reading a level scans every meter without locking: cheap enough for
    /// many tiles at display rate.
    #[test]
    fn reading_levels_is_cheap() {
        let hub = AudioHub::new(Arc::new(NoCallbacks), 256).unwrap();
        for t in 1..=256 {
            hub.add_tile(t);
        }
        let start = std::time::Instant::now();
        let mut sum = 0.0;
        for i in 0..20_000u64 {
            sum += hub.level(i % 300).0;
        }
        let per_read = start.elapsed() / 20_000;
        assert_eq!(sum, 0.0);
        // Even unoptimized, a read (a full scan) is a few microseconds.
        assert!(
            per_read < std::time::Duration::from_micros(50),
            "{per_read:?}"
        );
        hub.shutdown();
    }
}
