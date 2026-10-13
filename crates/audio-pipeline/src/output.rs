//! The audio output: per-tile lock-free rings and the real-time mixer.
//!
//! [`output`] makes the two halves: an [`AudioOutput`] handle that tiles use
//! to claim a playout slot, and the [`Renderer`] that the app's real-time
//! audio callback owns and calls.
//!
//! # Real-time guarantees of [`Renderer::render`]
//!
//! - **No locks.** It touches shared state only through atomic loads,
//!   stores, and swaps (`AtomicU32`/`AtomicU64`/`AtomicBool`), and
//!   `OnceLock::get`, which is a single atomic load once initialized. No
//!   `Mutex`, no condition variable, no channel; nothing it does can wait for
//!   another thread. (On targets without native 64-bit atomics the crate does
//!   not build, rather than falling back to locks.)
//! - **No allocation or deallocation.** Every buffer it reads or writes is
//!   allocated before the call (the slots and per-slot state when the output
//!   is made, a slot's ring by the producer when it first claims the slot),
//!   and it never drops an `Arc`. A test runs it under a counting allocator.
//! - **Bounded work**: `O(frames × (channels + slots))`.
//!
//! Each slot's ring is single-producer (the tile's thread, through its
//! [`SlotLease`]) and single-consumer (the renderer). The producer never
//! waits either: when the ring is full it drops what does not fit and counts
//! it.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, atomic::fence};

#[cfg(not(target_has_atomic = "64"))]
compile_error!("audio-pipeline needs native 64-bit atomics for its lock-free render path");

/// No skip requested.
const NO_SKIP: u64 = u64::MAX;
/// A skip request packs the target frame (low 48 bits; centuries of audio)
/// and a generation number (high 16 bits) into one atomic word.
const SKIP_FRAME_BITS: u32 = 48;
const SKIP_FRAME_MASK: u64 = (1 << SKIP_FRAME_BITS) - 1;
/// Frames of fade-out before a skip and around an underrun.
pub(crate) const FADE_FRAMES: usize = 64;
/// Volume changes ramp over this many seconds.
const GAIN_RAMP_S: f32 = 0.01;
/// Render clock: the share of each call's host-time error taken into the
/// estimate of when its first frame plays, once settled (the frame
/// period's share follows from it, critically damped). The first calls
/// after a start take larger shares, so the clock locks within a few
/// dozen calls.
const CLOCK_PHASE_GAIN: f64 = 0.05;
/// A host time this far from the estimate is a real jump (rendering
/// stopped, or the host clock was reset): the estimate starts over there.
const CLOCK_JUMP_NS: f64 = 50_000_000.0;
/// The estimated frame period stays within these multiples of nominal: a
/// timer loop's calls can come well over the frames they render apart
/// (a 10 ms sleep was measured at 12.2 ms on macOS, and a Windows timer
/// tick is 15.6 ms).
const CLOCK_PERIOD_RANGE: (f64, f64) = (0.5, 2.0);

/// The app's output stream format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputFormat {
    /// Frames per second (for example 48,000).
    pub sample_rate: u32,
    /// Interleaved channels: 1 or 2. Mono and stereo sources are mapped to
    /// it (mono to both sides, stereo averaged for mono).
    pub channels: usize,
}

/// Sizes of the output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputConfig {
    /// Tiles that can play at the same time.
    pub max_tiles: usize,
    /// Ring capacity per tile, in milliseconds (rounded up to a power of two
    /// frames).
    pub buffer_ms: u32,
}

impl Default for OutputConfig {
    fn default() -> Self {
        OutputConfig {
            max_tiles: 8,
            buffer_ms: 2000,
        }
    }
}

/// Counters of the render side, across all slots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Render calls.
    pub calls: u64,
    /// Frames rendered.
    pub frames: u64,
}

/// Single-producer single-consumer ring of interleaved f32 frames, stored
/// as bits in atomics so that both sides stay in safe code.
pub(crate) struct Ring {
    buf: Box<[AtomicU32]>,
    mask: u64,
    channels: usize,
    /// Frames written, ever (producer-owned).
    write: AtomicU64,
    /// Frames read, ever (consumer-owned).
    read: AtomicU64,
}

impl Ring {
    /// A ring whose frame counters start at `first` (0 except in tests).
    fn new(frames: usize, channels: usize, first: u64) -> Ring {
        let frames = frames.next_power_of_two().max(1024);
        Ring {
            buf: (0..frames * channels).map(|_| AtomicU32::new(0)).collect(),
            mask: frames as u64 - 1,
            channels,
            write: AtomicU64::new(first),
            read: AtomicU64::new(first),
        }
    }

    pub fn capacity(&self) -> u64 {
        self.mask + 1
    }

    /// Producer: frames written so far.
    pub fn write_index(&self) -> u64 {
        self.write.load(Ordering::Relaxed)
    }

    /// Producer: frames buffered (written, not yet read).
    pub fn queued(&self) -> u64 {
        self.write_index() - self.read.load(Ordering::Acquire)
    }

    /// Producer: appends whole frames; returns how many fit.
    pub fn push(&self, samples: &[f32]) -> usize {
        let ch = self.channels;
        let w = self.write.load(Ordering::Relaxed);
        let r = self.read.load(Ordering::Acquire);
        let free = (self.capacity() - (w - r)) as usize;
        let n = (samples.len() / ch).min(free);
        for i in 0..n {
            let base = ((w + i as u64) & self.mask) as usize * ch;
            for c in 0..ch {
                self.buf[base + c].store(samples[i * ch + c].to_bits(), Ordering::Relaxed);
            }
        }
        self.write.store(w + n as u64, Ordering::Release);
        n
    }

    #[inline]
    fn sample(&self, frame: u64, c: usize) -> f32 {
        f32::from_bits(
            self.buf[(frame & self.mask) as usize * self.channels + c].load(Ordering::Relaxed),
        )
    }
}

/// Where the consumer was at the start of its last render call.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Snapshot {
    /// Ring frame that plays at `host_ns`.
    pub read: u64,
    /// Host time (ns) at which frame `read` plays, from the renderer's
    /// smoothed clock ([`RenderClock`]).
    pub host_ns: u64,
    /// Host nanoseconds per frame, as the renderer measures its calls
    /// (nominally `1e9 / sample_rate`).
    pub ns_per_frame: f64,
    /// Frames the call rendered.
    pub frames: u64,
    /// Generation of the last skip request the consumer carried out.
    pub skip_gen: u16,
}

/// One playout slot.
pub(crate) struct Slot {
    claimed: AtomicBool,
    /// Whoever claimed it ([`SlotLevel::owner`]); 0 when free.
    owner: AtomicU64,
    playing: AtomicBool,
    gain: AtomicU32,
    skip_to: AtomicU64,
    ring: OnceLock<Ring>,
    seq: AtomicU64,
    snap_read: AtomicU64,
    snap_host: AtomicU64,
    snap_frames: AtomicU64,
    snap_ns_per_frame: AtomicU64,
    snap_skip_gen: AtomicU64,
    skip_gen: AtomicU64,
    underruns: AtomicU64,
    underrun_frames: AtomicU64,
}

impl Slot {
    fn new() -> Slot {
        Slot {
            claimed: AtomicBool::new(false),
            owner: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            gain: AtomicU32::new(1.0f32.to_bits()),
            skip_to: AtomicU64::new(NO_SKIP),
            ring: OnceLock::new(),
            seq: AtomicU64::new(0),
            snap_read: AtomicU64::new(0),
            snap_host: AtomicU64::new(0),
            snap_frames: AtomicU64::new(0),
            snap_ns_per_frame: AtomicU64::new(0),
            snap_skip_gen: AtomicU64::new(0),
            skip_gen: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            underrun_frames: AtomicU64::new(0),
        }
    }

    /// Consumer side of a seqlock: never waits.
    fn publish(&self, s: Snapshot) {
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        self.snap_read.store(s.read, Ordering::Relaxed);
        self.snap_host.store(s.host_ns, Ordering::Relaxed);
        self.snap_frames.store(s.frames, Ordering::Relaxed);
        self.snap_ns_per_frame
            .store(s.ns_per_frame.to_bits(), Ordering::Relaxed);
        self.snap_skip_gen
            .store(u64::from(s.skip_gen), Ordering::Relaxed);
        self.seq.store(seq + 2, Ordering::Release);
    }

    /// Producer side: retries while a publish is in progress.
    fn snapshot(&self) -> Option<Snapshot> {
        for _ in 0..1000 {
            let s1 = self.seq.load(Ordering::Acquire);
            if s1 % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let s = Snapshot {
                read: self.snap_read.load(Ordering::Relaxed),
                host_ns: self.snap_host.load(Ordering::Relaxed),
                frames: self.snap_frames.load(Ordering::Relaxed),
                ns_per_frame: f64::from_bits(self.snap_ns_per_frame.load(Ordering::Relaxed)),
                skip_gen: self.snap_skip_gen.load(Ordering::Relaxed) as u16,
            };
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == s1 {
                return (s1 > 0).then_some(s);
            }
        }
        None
    }
}

struct Shared {
    format: OutputFormat,
    ring_frames: usize,
    /// Where the rings' frame counters start (0 except in tests).
    first_frame: u64,
    slots: Box<[Slot]>,
    calls: AtomicU64,
    frames: AtomicU64,
}

/// Handle for tiles to claim playout slots. Cheap to clone; `Send + Sync`.
#[derive(Clone)]
pub struct AudioOutput {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for AudioOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioOutput")
            .field("format", &self.shared.format)
            .finish()
    }
}

/// Makes an output for `format`. Give the [`Renderer`] to the real-time
/// callback and the [`AudioOutput`] to the tiles. To change the format, make
/// a new output and hand it to the tiles again.
pub fn output(format: OutputFormat, config: OutputConfig) -> (AudioOutput, Renderer) {
    output_at_frame(format, config, 0)
}

/// Like [`output()`], with every ring's frame counters starting at
/// `first_frame` instead of 0, as if the output had played for a long time.
/// For tests of long-running numerics.
#[doc(hidden)]
pub fn output_at_frame(
    format: OutputFormat,
    config: OutputConfig,
    first_frame: u64,
) -> (AudioOutput, Renderer) {
    let format = OutputFormat {
        sample_rate: format.sample_rate.max(1),
        channels: format.channels.clamp(1, 2),
    };
    let ring_frames =
        (u64::from(format.sample_rate) * u64::from(config.buffer_ms.max(100)) / 1000) as usize;
    let shared = Arc::new(Shared {
        format,
        ring_frames,
        first_frame,
        slots: (0..config.max_tiles.max(1)).map(|_| Slot::new()).collect(),
        calls: AtomicU64::new(0),
        frames: AtomicU64::new(0),
    });
    let local = (0..shared.slots.len())
        .map(|_| SlotState::default())
        .collect();
    (
        AudioOutput {
            shared: Arc::clone(&shared),
        },
        Renderer {
            shared,
            local,
            clock: RenderClock::new(format.sample_rate),
        },
    )
}

impl AudioOutput {
    /// The output format.
    pub fn format(&self) -> OutputFormat {
        self.shared.format
    }

    /// Render-side counters.
    pub fn render_stats(&self) -> RenderStats {
        RenderStats {
            calls: self.shared.calls.load(Ordering::Relaxed),
            frames: self.shared.frames.load(Ordering::Relaxed),
        }
    }

    /// Claims a free slot for `owner` (any tag; the renderer's levels name
    /// it), or `None` when all are in use.
    pub(crate) fn claim(&self, owner: u64) -> Option<SlotLease> {
        let index = self.shared.slots.iter().position(|s| {
            s.claimed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        })?;
        let slot = &self.shared.slots[index];
        slot.owner.store(owner, Ordering::Release);
        let ring = slot.ring.get_or_init(|| {
            Ring::new(
                self.shared.ring_frames,
                self.shared.format.channels,
                self.shared.first_frame,
            )
        });
        // Start from an empty ring: drop whatever a previous owner left.
        let gen_ = (slot.skip_gen.fetch_add(1, Ordering::Relaxed) + 1) & 0xFFFF;
        slot.skip_to.store(
            (gen_ << SKIP_FRAME_BITS) | (ring.write_index() & SKIP_FRAME_MASK),
            Ordering::Release,
        );
        slot.gain.store(0.0f32.to_bits(), Ordering::Relaxed);
        Some(SlotLease {
            shared: Arc::clone(&self.shared),
            index,
        })
    }

    /// True when `self` and `other` are the same output.
    pub fn same_as(&self, other: &AudioOutput) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }
}

/// A tile's claim on one slot; releases it when dropped.
pub(crate) struct SlotLease {
    shared: Arc<Shared>,
    index: usize,
}

impl SlotLease {
    fn slot(&self) -> &Slot {
        &self.shared.slots[self.index]
    }

    pub fn ring(&self) -> &Ring {
        self.slot().ring.get().expect("initialized when claimed")
    }

    pub fn format(&self) -> OutputFormat {
        self.shared.format
    }

    pub fn set_playing(&self, on: bool) {
        self.slot().playing.store(on, Ordering::Release);
    }

    pub fn set_gain(&self, gain: f32) {
        self.slot()
            .gain
            .store(gain.max(0.0).to_bits(), Ordering::Relaxed);
    }

    /// Asks the consumer to fade out and jump to ring frame `frame`
    /// (everything before it is dropped). Call before writing what follows.
    /// Returns the request's generation; [`Snapshot::skip_gen`] equals it
    /// once the consumer has carried it out (or a later one).
    pub fn request_skip(&self, frame: u64) -> u16 {
        let s = self.slot();
        let gen_ = (s.skip_gen.fetch_add(1, Ordering::Relaxed) + 1) & 0xFFFF;
        s.skip_to.store(
            (gen_ << SKIP_FRAME_BITS) | (frame & SKIP_FRAME_MASK),
            Ordering::Release,
        );
        gen_ as u16
    }

    pub fn snapshot(&self) -> Option<Snapshot> {
        self.slot().snapshot()
    }

    /// (underrun events, frames of silence they caused)
    pub fn underruns(&self) -> (u64, u64) {
        let s = self.slot();
        (
            s.underruns.load(Ordering::Relaxed),
            s.underrun_frames.load(Ordering::Relaxed),
        )
    }
}

impl Drop for SlotLease {
    fn drop(&mut self) {
        let s = self.slot();
        s.playing.store(false, Ordering::Release);
        s.underruns.store(0, Ordering::Relaxed);
        s.underrun_frames.store(0, Ordering::Relaxed);
        s.owner.store(0, Ordering::Release);
        s.claimed.store(false, Ordering::Release);
    }
}

/// Consumer-local state per slot (owned by the renderer).
#[derive(Clone, Copy, Debug, Default)]
struct SlotState {
    gain: f32,
    /// Frames of fade-in left (after an underrun).
    fade_in: usize,
    /// Frames that should have played during underruns and will be skipped.
    owed: u64,
    /// Generation of the last skip carried out.
    skip_gen: u16,
    /// The ring ran dry in the last call.
    starved: bool,
    /// Data has been seen since the slot started playing.
    had_data: bool,
    was_playing: bool,
    /// Peak level with a slow release, before volume.
    peak: f32,
    /// Mean square level, smoothed, before volume.
    mean_square: f32,
    /// The owner of the slot while playing, else 0.
    owner: u64,
}

/// The level of one playing slot, measured by the renderer before volume:
/// what the tile's audio carries, whether or not it is muted.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SlotLevel {
    /// The tag given when the slot was claimed (0: the slot is free or not
    /// playing).
    pub owner: u64,
    /// Peak sample magnitude, 0 to 1, falling off over about
    /// [`LEVEL_PEAK_RELEASE_S`].
    pub peak: f32,
    /// RMS level, 0 to 1, over about [`LEVEL_RMS_WINDOW_S`].
    pub rms: f32,
}

/// Time constant of the peak level's release.
pub const LEVEL_PEAK_RELEASE_S: f32 = 0.3;
/// Time constant of the RMS level's smoothing.
pub const LEVEL_RMS_WINDOW_S: f32 = 0.1;

/// When each render call's first frame plays, smoothed: the host times an
/// app passes can jitter by milliseconds per call (timestamps taken in a
/// callback, or `now + latency` in a timer loop), and a timer loop may
/// render fewer frames than the time between its calls. A second-order
/// loop follows the host times with the frames rendered as its clock:
/// jitter averages out, a steady rate difference becomes the frame period,
/// and a jump beyond [`CLOCK_JUMP_NS`] restarts it (a real gap). Arithmetic
/// only: real-time safe.
#[derive(Clone, Copy, Debug)]
struct RenderClock {
    nominal_ns: f64,
    /// Estimated host time of the last call's first frame; `None` before
    /// the first call.
    at_ns: Option<f64>,
    ns_per_frame: f64,
    /// Frames rendered since `at_ns`.
    frames: u64,
    /// Updates since the clock (re)started.
    updates: u32,
}

impl RenderClock {
    fn new(sample_rate: u32) -> RenderClock {
        let nominal_ns = 1e9 / f64::from(sample_rate.max(1));
        RenderClock {
            nominal_ns,
            at_ns: None,
            ns_per_frame: nominal_ns,
            frames: 0,
            updates: 0,
        }
    }

    /// Takes a call's host time and frame count; returns the estimated host
    /// time of its first frame.
    fn update(&mut self, host_ns: u64, frames: usize) -> f64 {
        let host = host_ns as f64;
        let at = match self.at_ns {
            Some(prev) if self.frames > 0 => {
                let predicted = prev + self.frames as f64 * self.ns_per_frame;
                let e = host - predicted;
                if e.abs() > CLOCK_JUMP_NS {
                    self.updates = 0;
                    host
                } else {
                    let a = (2.0 / (f64::from(self.updates) + 2.0)).max(CLOCK_PHASE_GAIN);
                    self.updates = self.updates.saturating_add(1);
                    self.ns_per_frame = (self.ns_per_frame + a * a / 4.0 * e / self.frames as f64)
                        .clamp(
                            self.nominal_ns * CLOCK_PERIOD_RANGE.0,
                            self.nominal_ns * CLOCK_PERIOD_RANGE.1,
                        );
                    predicted + a * e
                }
            }
            Some(prev) => prev,
            None => host,
        };
        if self.at_ns.is_none() || self.frames > 0 || frames > 0 {
            self.at_ns = Some(at);
        }
        self.frames = frames as u64;
        at
    }
}

/// The real-time side: mixes every playing slot into the app's buffer.
pub struct Renderer {
    shared: Arc<Shared>,
    local: Box<[SlotState]>,
    clock: RenderClock,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("format", &self.shared.format)
            .finish()
    }
}

impl Renderer {
    /// The level of each slot after the last [`Renderer::render`], in slot
    /// order (owner 0 for slots that are not playing). Real-time safe.
    pub fn levels(&self) -> impl Iterator<Item = SlotLevel> + '_ {
        self.local.iter().map(|st| SlotLevel {
            owner: st.owner,
            peak: st.peak,
            rms: st.mean_square.max(0.0).sqrt().min(1.0),
        })
    }

    /// Slots (tiles that can play at once).
    pub fn slots(&self) -> usize {
        self.local.len()
    }

    /// The output format.
    pub fn format(&self) -> OutputFormat {
        self.shared.format
    }

    /// Fills `out` (interleaved, `channels` per frame) with the mix of every
    /// playing tile. `host_time_ns` is the host time at which `out`'s first
    /// frame will be heard, on the same clock as the arrival times given to
    /// the tiles; it may jitter by milliseconds per call, and calls may
    /// come further apart than the frames they render ([`RenderClock`]).
    /// See the module documentation for the real-time guarantees.
    pub fn render(&mut self, out: &mut [f32], host_time_ns: u64) {
        let fmt = self.shared.format;
        let ch = fmt.channels;
        let frames = out.len() / ch;
        out.fill(0.0);
        let start_ns = self.clock.update(host_time_ns, frames);
        let ns_per_frame = self.clock.ns_per_frame;
        let gain_step = 1.0 / (GAIN_RAMP_S * fmt.sample_rate as f32);
        for (slot, st) in self.shared.slots.iter().zip(self.local.iter_mut()) {
            if !slot.playing.load(Ordering::Acquire) {
                st.was_playing = false;
                st.owner = 0;
                st.peak = 0.0;
                st.mean_square = 0.0;
                continue;
            }
            let Some(ring) = slot.ring.get() else {
                continue;
            };
            if !st.was_playing {
                *st = SlotState {
                    was_playing: true,
                    ..SlotState::default()
                };
            }
            st.owner = slot.owner.load(Ordering::Acquire);
            let (mut block_peak, mut block_sq) = (0.0f32, 0.0f32);
            let w = ring.write.load(Ordering::Acquire);
            let mut r = ring.read.load(Ordering::Relaxed);
            let target_gain = f32::from_bits(slot.gain.load(Ordering::Relaxed));
            let mut i = 0usize;

            let req = slot.skip_to.swap(NO_SKIP, Ordering::AcqRel);
            if req != NO_SKIP {
                st.skip_gen = (req >> SKIP_FRAME_BITS) as u16;
                // Restore the high bits of the frame number.
                let low = req & SKIP_FRAME_MASK;
                let skip = (r & !SKIP_FRAME_MASK) | low;
                st.owed = 0;
                if skip > r {
                    let skip = skip.min(w);
                    // Fade out what is playing, then jump.
                    let n = ((skip - r) as usize).min(FADE_FRAMES).min(frames);
                    for k in 0..n {
                        let ramp = 1.0 - (k + 1) as f32 / n as f32;
                        st.gain += (target_gain - st.gain).clamp(-gain_step, gain_step);
                        for c in 0..ch {
                            out[k * ch + c] += ring.sample(r + k as u64, c) * st.gain * ramp;
                        }
                    }
                    i = n;
                    r = skip;
                }
            }

            // Frames owed from an underrun: the time they should have played
            // has passed, so drop them as they arrive and keep every later
            // frame on its schedule (an underrun never adds latency).
            if st.owed > 0 && w > r {
                let k = st.owed.min(w - r);
                r += k;
                st.owed -= k;
                st.fade_in = FADE_FRAMES;
            }

            // Frame `r + owed` is the one scheduled for this call's first
            // frame (after any fade-out).
            slot.publish(Snapshot {
                read: r + st.owed,
                host_ns: (start_ns + i as f64 * ns_per_frame).max(0.0) as u64,
                frames: frames as u64,
                ns_per_frame,
                skip_gen: st.skip_gen,
            });

            let want = frames - i;
            let avail = (w - r) as usize;
            let n = avail.min(want);
            if avail > 0 {
                st.had_data = true;
            }
            let short = n < want;
            if short && st.had_data {
                if !st.starved {
                    slot.underruns.fetch_add(1, Ordering::Relaxed);
                }
                slot.underrun_frames
                    .fetch_add((want - n) as u64, Ordering::Relaxed);
                st.owed = (st.owed + (want - n) as u64).min(16 * ring.capacity());
            }
            for k in 0..n {
                st.gain += (target_gain - st.gain).clamp(-gain_step, gain_step);
                let mut g = st.gain;
                if st.fade_in > 0 {
                    g *= 1.0 - st.fade_in as f32 / (FADE_FRAMES + 1) as f32;
                    st.fade_in -= 1;
                }
                // Running dry: fade the last frames out.
                if short && n - k <= FADE_FRAMES {
                    g *= (n - k) as f32 / (FADE_FRAMES.min(n) + 1) as f32;
                }
                let o = (i + k) * ch;
                for c in 0..ch {
                    let v = ring.sample(r + k as u64, c);
                    block_peak = block_peak.max(v.abs());
                    block_sq += v * v;
                    out[o + c] += v * g;
                }
            }
            // Levels over the whole call (frames not played count as
            // silence), smoothed across calls by their length.
            if frames > 0 {
                let secs = frames as f32 / fmt.sample_rate as f32;
                let ms = block_sq / (frames * ch) as f32;
                let a = 1.0 - (-secs / LEVEL_RMS_WINDOW_S).exp();
                st.mean_square += (ms - st.mean_square) * a;
                let release = (-secs / LEVEL_PEAK_RELEASE_S).exp();
                st.peak = block_peak.min(1.0).max(st.peak * release);
            }
            if short {
                st.fade_in = FADE_FRAMES;
            }
            st.starved = short && st.had_data;
            ring.read.store(r + n as u64, Ordering::Release);
        }
        for s in out.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
        self.shared.calls.fetch_add(1, Ordering::Relaxed);
        self.shared
            .frames
            .fetch_add(frames as u64, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt() -> OutputFormat {
        OutputFormat {
            sample_rate: 48_000,
            channels: 2,
        }
    }

    #[test]
    fn plays_ring_content_with_volume_and_reports_position() {
        let (o, mut r) = output(fmt(), OutputConfig::default());
        let lease = o.claim(1).unwrap();
        lease.set_gain(0.5);
        lease.set_playing(true);
        lease.ring().push(&vec![1.0; 2 * 4800]);
        let mut buf = vec![0.0; 2 * 480];
        // Let the gain ramp settle (10 ms).
        r.render(&mut buf, 1_000_000_000);
        r.render(&mut buf, 1_010_000_000);
        assert!(buf.iter().all(|&s| (s - 0.5).abs() < 1e-6));
        let s = lease.snapshot().unwrap();
        assert_eq!(s.read, 480);
        assert_eq!(s.host_ns, 1_010_000_000);
        assert_eq!(lease.ring().queued(), 4800 - 960);
    }

    #[test]
    fn the_render_clock_smooths_jitter_and_follows_jumps() {
        let mut c = RenderClock::new(48_000);
        let mut seed = 7u64;
        let mut unit = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        // 480 frames per call, host times jittering by up to ±5 ms.
        let mut worst = 0.0f64;
        for k in 0..2000u64 {
            let ideal = 1e9 + k as f64 * 10e6;
            let at = c.update((ideal + (unit() * 2.0 - 1.0) * 5e6) as u64, 480);
            if k > 200 {
                worst = worst.max((at - ideal).abs());
            }
        }
        assert!(worst < 2e6, "estimate off by {worst} ns");
        assert!((c.ns_per_frame * 48_000.0 - 1e9).abs() < 1e6);
        // A timer loop: 480 frames per 15.6 ms of host time. The clock
        // locks without a jump.
        let mut c = RenderClock::new(48_000);
        let mut worst = 0.0f64;
        for k in 0..2000u64 {
            let host = 1_000_000_000 + k * 15_600_000;
            let at = c.update(host, 480);
            worst = worst.max((at - host as f64).abs());
        }
        assert!(worst < CLOCK_JUMP_NS / 2.0, "worst {worst} ns");
        assert!((c.ns_per_frame * 480.0 - 15.6e6).abs() < 1e3);
        // A real gap: rendering stopped for a second.
        let host = 1_000_000_000u64 + 2000 * 15_600_000 + 1_000_000_000;
        assert_eq!(c.update(host, 480), host as f64);
    }

    #[test]
    fn levels_name_the_owner_and_ignore_volume() {
        let (o, mut r) = output(fmt(), OutputConfig::default());
        assert_eq!(r.slots(), 8);
        let lease = o.claim(42).unwrap();
        lease.set_gain(0.0); // muted: the level still shows the signal
        lease.set_playing(true);
        lease.ring().push(&vec![0.5; 2 * 48_000]);
        let mut buf = vec![0.0; 2 * 480];
        for k in 0..50 {
            r.render(&mut buf, k * 10_000_000);
        }
        let l: Vec<SlotLevel> = r.levels().collect();
        assert_eq!(l[0].owner, 42);
        assert!((l[0].peak - 0.5).abs() < 1e-4, "{l:?}");
        assert!((l[0].rms - 0.5).abs() < 0.01, "{l:?}");
        assert!(l[1..].iter().all(|s| s.owner == 0 && s.peak == 0.0));
        assert!(buf.iter().all(|&s| s == 0.0));
        // Silence: the peak falls off over a fraction of a second.
        let silent = o.claim(7).unwrap();
        drop(lease);
        for k in 0..100 {
            r.render(&mut buf, k * 10_000_000);
        }
        assert_eq!(r.levels().next().unwrap().owner, 0);
        silent.set_playing(true);
        r.render(&mut buf, 0);
        let l = r.levels().nth(1).unwrap();
        assert_eq!(l.owner, 7);
        assert_eq!((l.peak, l.rms), (0.0, 0.0));
    }

    #[test]
    fn skip_fades_out_and_jumps() {
        let (o, mut r) = output(fmt(), OutputConfig::default());
        let lease = o.claim(1).unwrap();
        lease.set_gain(1.0);
        lease.set_playing(true);
        let mut buf = vec![0.0; 2 * 480];
        lease.ring().push(&vec![0.25; 2 * 1000]);
        r.render(&mut buf, 0);
        r.render(&mut buf, 10_000_000);
        lease.request_skip(lease.ring().write_index());
        lease.ring().push(&vec![-0.25; 2 * 1000]);
        r.render(&mut buf, 20_000_000);
        // First the fade-out of the old content, then the new content.
        assert!(buf[0] > 0.0 && buf[2 * (FADE_FRAMES - 1)] < 0.01);
        assert!((buf[2 * FADE_FRAMES + 2] + 0.25).abs() < 1e-6);
        assert_eq!(lease.snapshot().unwrap().read, 1000);
    }

    #[test]
    fn underruns_are_counted_and_slots_released() {
        let (o, mut r) = output(
            fmt(),
            OutputConfig {
                max_tiles: 1,
                buffer_ms: 100,
            },
        );
        let lease = o.claim(1).unwrap();
        assert!(o.claim(1).is_none());
        lease.set_gain(1.0);
        lease.set_playing(true);
        assert_eq!(lease.ring().push(&vec![0.1; 2 * 100_000]), 8192);
        let mut buf = vec![0.0; 2 * 8000];
        r.render(&mut buf, 0);
        r.render(&mut buf, 0);
        r.render(&mut buf, 0);
        assert_eq!(lease.underruns(), (1, 8000 + 7808));
        // The schedule continues through the underrun: the published
        // position counts the frames that should have played.
        assert_eq!(lease.snapshot().unwrap().read, 16_000);
        // Late frames are dropped when they arrive; later ones play on time.
        assert_eq!(lease.ring().push(&vec![0.2; 2 * 8192]), 8192);
        r.render(&mut buf, 0);
        assert_eq!(lease.snapshot().unwrap().read, 24_000);
        drop(lease);
        assert!(o.claim(1).is_some());
    }
}
