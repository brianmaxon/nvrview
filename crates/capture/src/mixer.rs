//! The call mixer: the visitor's audio and ours at 48 kHz in AAC-ready
//! 1024-sample frames (docs/CAPTURE.md, "The mix"; docs/AUDIO.md,
//! "Recording calls").
//!
//! Each side arrives as PCM chunks stamped on the segment's clock
//! (microseconds after the segment start, from the [`Timeline`]
//! (crate::epoch::Timeline) and the segment policy), at its own rate
//! (typically 16 kHz for both). The mixer produces three tracks of
//! [`MixedFrame`]s on one grid of 1024 samples at 48 kHz, frame `n`
//! covering samples `n * 1024 ..`: **Conversation** (the sum through the
//! limiter), **Visitor**, and **You** (each through the same limiter, which
//! leaves normal levels untouched).
//!
//! # Rules
//!
//! - **Exact placement.** A chunk's position comes from an
//!   [`AudioPlacer`] per side, in samples at the side's rate; the
//!   resampler runs at a constant ratio from position 0, so output sample
//!   `j` is the input at `j * rate / 48000` and no drift can build up. A
//!   chunk within 20 ms of contiguous is snapped to contiguous, a later one
//!   leaves silence, an overlapping one is trimmed.
//! - **Silence.** Where a side has no data it is zero, so the track is
//!   silent when nobody talks. The mixer never waits for a side.
//! - **Time drives output.** [`CallMixer::advance`] takes the file time the
//!   recording has reached and emits the frames that end at least
//!   `latency_us` (250 ms) before it, giving late chunks that long to
//!   arrive. A chunk that arrives after its time was emitted is dropped
//!   and counted (`late_dropped_ms`).
//! - **Unity gain, soft limiter.** Conversation is `visitor + you` with no
//!   gain change, through [`soft_limit`]: identity up to 0.8, then a
//!   smooth knee to a ceiling of 0.98 that is never exceeded, so the mix
//!   cannot clip however loud both sides are. Non-finite samples become 0.
//! - **Bounds.** A side accepts audio at most `max_ahead_us` (10 s) ahead
//!   of what was emitted, drops the rest (`ahead_dropped_ms`), and
//!   `advance` emits at most `max_frames` per call, so neither input nor
//!   output grows without bound. [`EncodeQueue`] holds at most 10 s of
//!   frames for an encoder and turns overflow into silence
//!   (`encode_dropped_ms`).
//! - **Flush.** [`CallMixer::flush`] emits everything up to a time,
//!   rounded up to whole frames and padded with silence, for the end of a
//!   segment.
//!
//! One mixer serves one segment; a later part of a call recording starts a
//! new mixer (positions restart at its keyframe).

use crate::epoch::{AudioPlacer, MICROS};
use audio_pipeline::resample::Resampler;
use std::collections::VecDeque;

/// The output rate.
pub const OUT_RATE: u32 = 48_000;
/// Samples per frame.
pub const FRAME: usize = 1024;
/// Level up to which the limiter does nothing.
pub const LIMITER_THRESHOLD: f32 = 0.8;
/// The level the limiter never exceeds.
pub const LIMITER_CEILING: f32 = 0.98;

/// The soft limiter: identity up to [`LIMITER_THRESHOLD`], a `tanh` knee
/// from there toward [`LIMITER_CEILING`], which is never exceeded. It is
/// monotonic and has no memory, so it adds no delay and cannot overshoot.
/// A non-finite input gives 0.
pub fn soft_limit(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    let a = x.abs();
    if a <= LIMITER_THRESHOLD {
        return x;
    }
    let room = LIMITER_CEILING - LIMITER_THRESHOLD;
    let y = LIMITER_THRESHOLD + room * ((a - LIMITER_THRESHOLD) / room).tanh();
    y.min(LIMITER_CEILING).copysign(x)
}

/// 16-bit PCM as floats in -1 to 1.
pub fn pcm_i16_to_f32(pcm: &[i16]) -> Vec<f32> {
    pcm.iter().map(|&s| f32::from(s) / 32768.0).collect()
}

/// Mixer settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixerConfig {
    /// The visitor's PCM rate.
    pub visitor_rate: u32,
    /// Our PCM rate.
    pub you_rate: u32,
    /// How far behind the file time frames are emitted.
    pub latency_us: i64,
    /// Furthest ahead of the emitted position a chunk may be placed.
    pub max_ahead_us: i64,
    /// Chunks this close to contiguous are snapped to contiguous.
    pub place_tolerance_us: i64,
}

impl MixerConfig {
    /// The contract's values for the given input rates.
    pub fn new(visitor_rate: u32, you_rate: u32) -> MixerConfig {
        MixerConfig {
            visitor_rate,
            you_rate,
            latency_us: 250_000,
            max_ahead_us: 10 * MICROS,
            place_tolerance_us: 20_000,
        }
    }
}

/// One 1024-sample frame of each track.
#[derive(Clone, Debug, PartialEq)]
pub struct MixedFrame {
    /// Sample position (48 kHz, 0 at the segment start) of the first sample.
    pub start_sample: i64,
    /// Visitor plus you, limited.
    pub conversation: Vec<f32>,
    /// The visitor alone.
    pub visitor: Vec<f32>,
    /// You alone.
    pub you: Vec<f32>,
}

impl MixedFrame {
    /// The frame's start in microseconds after the segment start. Give an
    /// encoded access unit this time (docs/CAPTURE.md, priming).
    pub fn start_us(&self) -> i64 {
        self.start_sample * MICROS / i64::from(OUT_RATE)
    }
}

/// Which side of the call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The camera's microphone.
    Visitor,
    /// What we sent.
    You,
}

/// Counters, in milliseconds of audio.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MixerStats {
    /// Audio that arrived after its frame was emitted.
    pub late_dropped_ms: u64,
    /// Audio further ahead than `max_ahead_us`.
    pub ahead_dropped_ms: u64,
}

struct SideState {
    rate: u32,
    placer: AudioPlacer,
    rs: Resampler,
    /// Output samples produced so far (48 kHz).
    produced: i64,
    ready: VecDeque<f32>,
    late: u64,
    ahead: u64,
}

impl SideState {
    fn new(rate: u32, tolerance_us: i64) -> SideState {
        let mut rs = Resampler::new(1, rate, OUT_RATE);
        rs.reset(0.0);
        SideState {
            rate,
            placer: AudioPlacer::new(rate, tolerance_us),
            rs,
            produced: 0,
            ready: VecDeque::new(),
            late: 0,
            ahead: 0,
        }
    }

    fn step(&self) -> f64 {
        f64::from(self.rate) / f64::from(OUT_RATE)
    }

    fn push(&mut self, rel_us: i64, samples: &[f32], max_ahead: usize) {
        let Some(p) = self.placer.place(rel_us, samples.len()) else {
            return;
        };
        let mut s = &samples[p.skip..];
        let n = s.len() as i64;
        let end = self.rs.input_end();
        // Part of the chunk that is already behind what was emitted.
        self.late += (end - p.pos).clamp(0, n) as u64;
        // Part beyond the window ahead of it.
        let over = (p.pos + n - (end + max_ahead as i64)).clamp(0, n);
        self.ahead += over as u64;
        s = &s[..(n - over) as usize];
        if !s.is_empty() {
            self.rs.push(p.pos, s);
        }
    }

    /// Produces output up to (not including) sample `target` at 48 kHz.
    fn produce_until(&mut self, target: i64) {
        let step = self.step();
        let mut guard = 0;
        while self.produced < target {
            let want = (target - self.produced) as usize;
            let need = ((target as f64) * step).ceil() as i64 + self.rs.half_width() as i64 + 2;
            let end = self.rs.input_end();
            if end < need {
                let zeros = vec![0.0f32; (need - end) as usize];
                self.rs.push(end, &zeros);
            }
            let mut out = Vec::with_capacity(want);
            let n = self.rs.produce(step, want, &mut out);
            self.ready.extend(out);
            self.produced += n as i64;
            guard += 1;
            if n == 0 && guard > 8 {
                // Cannot happen with the fill above; never spin.
                let pad = want;
                self.ready.extend(std::iter::repeat_n(0.0, pad));
                self.produced += pad as i64;
            }
        }
    }

    fn take_frame(&mut self) -> Vec<f32> {
        self.ready.drain(..FRAME).collect()
    }
}

/// The mixer. See the module documentation.
pub struct CallMixer {
    cfg: MixerConfig,
    visitor: SideState,
    you: SideState,
    next_frame: i64,
}

impl CallMixer {
    /// A mixer for one segment.
    pub fn new(cfg: MixerConfig) -> CallMixer {
        CallMixer {
            cfg,
            visitor: SideState::new(cfg.visitor_rate, cfg.place_tolerance_us),
            you: SideState::new(cfg.you_rate, cfg.place_tolerance_us),
            next_frame: 0,
        }
    }

    /// Counters.
    pub fn stats(&self) -> MixerStats {
        let ms = |n: u64, rate: u32| n * 1000 / u64::from(rate.max(1));
        MixerStats {
            late_dropped_ms: ms(self.visitor.late, self.cfg.visitor_rate)
                + ms(self.you.late, self.cfg.you_rate),
            ahead_dropped_ms: ms(self.visitor.ahead, self.cfg.visitor_rate)
                + ms(self.you.ahead, self.cfg.you_rate),
        }
    }

    /// Sample position (48 kHz) of the next frame to be emitted.
    pub fn next_frame_sample(&self) -> i64 {
        self.next_frame
    }

    /// Adds PCM for `side` whose first sample is `rel_us` microseconds
    /// after the segment start (negative: before it; the front is
    /// trimmed).
    pub fn push(&mut self, side: Side, rel_us: i64, samples: &[f32]) {
        let (s, rate) = match side {
            Side::Visitor => (&mut self.visitor, self.cfg.visitor_rate),
            Side::You => (&mut self.you, self.cfg.you_rate),
        };
        let max_ahead = (self.cfg.max_ahead_us * i64::from(rate) / MICROS).max(1) as usize;
        s.push(rel_us, samples, max_ahead);
    }

    fn emit(&mut self, until_sample: i64, max_frames: usize, out: &mut Vec<MixedFrame>) -> usize {
        let mut n = 0;
        while n < max_frames && self.next_frame + FRAME as i64 <= until_sample {
            let target = self.next_frame + FRAME as i64;
            self.visitor.produce_until(target);
            self.you.produce_until(target);
            let v = self.visitor.take_frame();
            let y = self.you.take_frame();
            let conversation = v.iter().zip(&y).map(|(a, b)| soft_limit(a + b)).collect();
            out.push(MixedFrame {
                start_sample: self.next_frame,
                conversation,
                visitor: v.into_iter().map(soft_limit).collect(),
                you: y.into_iter().map(soft_limit).collect(),
            });
            self.next_frame = target;
            n += 1;
        }
        n
    }

    /// The recording has reached `file_rel_us` (microseconds after the
    /// segment start): appends to `out` the frames that end at least
    /// `latency_us` before it, at most `max_frames`. Returns how many.
    pub fn advance(
        &mut self,
        file_rel_us: i64,
        max_frames: usize,
        out: &mut Vec<MixedFrame>,
    ) -> usize {
        let horizon = (file_rel_us - self.cfg.latency_us).max(0);
        let until = horizon * i64::from(OUT_RATE) / MICROS;
        self.emit(until, max_frames, out)
    }

    /// Emits everything up to `end_rel_us`, the last frame padded with
    /// silence. Returns how many frames (at most `max_frames`; call again
    /// if it returns that many).
    pub fn flush(
        &mut self,
        end_rel_us: i64,
        max_frames: usize,
        out: &mut Vec<MixedFrame>,
    ) -> usize {
        let end = end_rel_us.max(0) * i64::from(OUT_RATE) / MICROS;
        let frames = (end + FRAME as i64 - 1) / FRAME as i64;
        self.emit(frames * FRAME as i64, max_frames, out)
    }
}

/// Frames waiting for an AAC encoder: at most `cap_frames` real frames
/// (10 s by default); beyond that frames are replaced by silence so the
/// track keeps its length (docs/CAPTURE.md, "Bounds").
#[derive(Debug)]
pub struct EncodeQueue {
    items: VecDeque<QItem>,
    real: usize,
    cap_frames: usize,
    dropped_frames: u64,
}

#[derive(Debug)]
enum QItem {
    Frame(Vec<f32>),
    Silence(u32),
}

/// Frames in 10 seconds at 48 kHz.
pub const TEN_SECONDS_OF_FRAMES: usize = (10 * OUT_RATE as usize).div_ceil(FRAME);

impl EncodeQueue {
    /// A queue holding at most `cap_frames` real frames.
    pub fn new(cap_frames: usize) -> EncodeQueue {
        EncodeQueue {
            items: VecDeque::new(),
            real: 0,
            cap_frames,
            dropped_frames: 0,
        }
    }

    /// Queues a frame. Returns false when the queue was full and silence
    /// was queued in its place.
    pub fn push(&mut self, frame: Vec<f32>) -> bool {
        if self.real >= self.cap_frames {
            self.dropped_frames += 1;
            match self.items.back_mut() {
                Some(QItem::Silence(n)) => *n += 1,
                _ => self.items.push_back(QItem::Silence(1)),
            }
            return false;
        }
        self.real += 1;
        self.items.push_back(QItem::Frame(frame));
        true
    }

    /// The next frame for the encoder (silence expands to zeros).
    pub fn pop(&mut self) -> Option<Vec<f32>> {
        match self.items.front_mut()? {
            QItem::Frame(_) => {
                self.real -= 1;
                match self.items.pop_front() {
                    Some(QItem::Frame(f)) => Some(f),
                    _ => None,
                }
            }
            QItem::Silence(n) => {
                *n -= 1;
                if *n == 0 {
                    self.items.pop_front();
                }
                Some(vec![0.0; FRAME])
            }
        }
    }

    /// Real frames waiting.
    pub fn queued_frames(&self) -> usize {
        self.real
    }

    /// Milliseconds of audio replaced by silence so far.
    pub fn encode_dropped_ms(&self) -> u64 {
        self.dropped_frames * FRAME as u64 * 1000 / u64::from(OUT_RATE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> MixerConfig {
        MixerConfig::new(16_000, 16_000)
    }

    fn run_flush(m: &mut CallMixer, end_us: i64) -> Vec<MixedFrame> {
        let mut out = Vec::new();
        while m.flush(end_us, 64, &mut out) == 64 {}
        out
    }

    fn argmax(v: &[f32]) -> usize {
        v.iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap()
            .0
    }

    fn concat(frames: &[MixedFrame], pick: impl Fn(&MixedFrame) -> &Vec<f32>) -> Vec<f32> {
        frames
            .iter()
            .flat_map(|f| pick(f).iter().copied())
            .collect()
    }

    #[test]
    fn limiter_is_identity_below_the_knee_and_never_exceeds_the_ceiling() {
        for x in [0.0f32, 0.1, -0.5, 0.8, -0.8] {
            assert_eq!(soft_limit(x), x);
        }
        let mut prev = soft_limit(0.8);
        let mut x = 0.8f32;
        while x < 1000.0 {
            x += 0.01;
            let y = soft_limit(x);
            assert!(y >= prev, "monotonic at {x}");
            assert!(y <= LIMITER_CEILING, "ceiling at {x}: {y}");
            assert_eq!(soft_limit(-x), -y);
            prev = y;
        }
        for x in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            assert_eq!(soft_limit(x), 0.0);
        }
        assert!(soft_limit(f32::MAX) <= LIMITER_CEILING);
        // Loud inputs approach but stay under the ceiling.
        assert!(soft_limit(2.0) > 0.95);
    }

    #[test]
    fn two_full_scale_sides_never_clip() {
        let mut m = CallMixer::new(cfg());
        let loud: Vec<f32> = (0..16_000)
            .map(|i| if (i / 20) % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        m.push(Side::Visitor, 0, &loud);
        m.push(Side::You, 0, &loud);
        let frames = run_flush(&mut m, 1_000_000);
        let mix = concat(&frames, |f| &f.conversation);
        assert!(mix.iter().all(|s| s.abs() <= LIMITER_CEILING));
        assert!(mix.iter().any(|s| s.abs() > 0.9), "loud signal stays loud");
        let v = concat(&frames, |f| &f.visitor);
        assert!(v.iter().all(|s| s.abs() <= LIMITER_CEILING));
    }

    #[test]
    fn sides_sum_at_unity_gain_below_the_knee() {
        let mut m = CallMixer::new(cfg());
        let dc = vec![0.2f32; 16_000];
        let dc2 = vec![0.3f32; 16_000];
        m.push(Side::Visitor, 0, &dc);
        m.push(Side::You, 0, &dc2);
        let frames = run_flush(&mut m, 500_000);
        let mix = concat(&frames, |f| &f.conversation);
        // Away from the edges (the resampler's ramp-in), 0.2 + 0.3.
        for s in &mix[2000..20_000] {
            assert!((s - 0.5).abs() < 0.01, "{s}");
        }
        let you = concat(&frames, |f| &f.you);
        assert!((you[10_000] - 0.3).abs() < 0.01);
    }

    #[test]
    fn an_impulse_lands_on_its_exact_output_sample() {
        // 16 kHz impulse at input sample 8000 (0.5 s) -> 48 kHz sample 24000.
        for (rate, at_in, at_out) in [(16_000u32, 8_000usize, 24_000usize), (8_000, 4_000, 24_000)]
        {
            let mut m = CallMixer::new(MixerConfig::new(rate, 16_000));
            let mut v = vec![0.0f32; at_in + 200];
            v[at_in] = 0.5;
            m.push(Side::Visitor, 0, &v);
            let frames = run_flush(&mut m, 1_000_000);
            let pcm = concat(&frames, |f| &f.visitor);
            assert_eq!(argmax(&pcm), at_out, "rate {rate}");
        }
    }

    #[test]
    fn a_chunk_stamped_later_lands_later_with_silence_between() {
        let mut m = CallMixer::new(cfg());
        let mut imp = vec![0.0f32; 160];
        imp[80] = 0.5; // 5 ms into the chunk
        // Chunk at 400 ms: impulse at 405 ms = 19_440 samples at 48 kHz.
        m.push(Side::Visitor, 400_000, &imp);
        let frames = run_flush(&mut m, 600_000);
        let pcm = concat(&frames, |f| &f.visitor);
        assert_eq!(argmax(&pcm), 19_440);
        assert!(pcm[..18_000].iter().all(|s| s.abs() < 1e-6));
        // Frame times are on the 1024 grid.
        assert_eq!(frames[3].start_sample, 3072);
        assert_eq!(frames[3].start_us(), 64_000);
    }

    #[test]
    fn jittered_chunk_times_do_not_move_samples() {
        // 20 ms chunks stamped with +-5 ms jitter stay one continuous
        // tone: the impulse train keeps its exact spacing.
        let mut m = CallMixer::new(cfg());
        for i in 0..50i64 {
            let jitter = if i % 2 == 0 { 5_000 } else { -4_000 };
            let mut chunk = vec![0.0f32; 320];
            chunk[0] = 0.5; // one impulse per chunk, 20 ms apart
            m.push(Side::Visitor, i * 20_000 + jitter.max(-i * 20_000), &chunk);
        }
        let frames = run_flush(&mut m, 1_000_000);
        let pcm = concat(&frames, |f| &f.visitor);
        let peaks: Vec<usize> = (1..pcm.len() - 1)
            .filter(|&i| pcm[i] > 0.3 && pcm[i] >= pcm[i - 1] && pcm[i] >= pcm[i + 1])
            .collect();
        assert_eq!(peaks.len(), 50);
        for w in peaks.windows(2) {
            assert_eq!(w[1] - w[0], 960);
        }
    }

    #[test]
    fn silence_when_nobody_talks_and_frames_cover_the_time() {
        let mut m = CallMixer::new(cfg());
        let mut out = Vec::new();
        // The recording reached 1 s; with 250 ms latency, 750 ms of frames.
        m.advance(1_000_000, 100, &mut out);
        let samples = out.len() * FRAME;
        assert!((36_000 - 1024..=36_000).contains(&samples), "{samples}");
        assert!(out.iter().all(|f| f.conversation.iter().all(|&s| s == 0.0)));
        assert_eq!(out.len(), 35);
        // Starts are contiguous.
        for (i, f) in out.iter().enumerate() {
            assert_eq!(f.start_sample, (i * FRAME) as i64);
            assert_eq!(f.conversation.len(), FRAME);
        }
    }

    #[test]
    fn advance_is_bounded_per_call() {
        let mut m = CallMixer::new(cfg());
        let mut out = Vec::new();
        assert_eq!(m.advance(3_600_000_000, 16, &mut out), 16);
        assert_eq!(out.len(), 16);
        assert_eq!(m.next_frame_sample(), 16 * FRAME as i64);
    }

    #[test]
    fn flush_pads_the_last_frame() {
        let mut m = CallMixer::new(cfg());
        m.push(Side::Visitor, 0, &vec![0.25f32; 1000]);
        let frames = run_flush(&mut m, 40_000); // 1920 samples -> 2 frames
        assert_eq!(frames.len(), 2);
        assert_eq!(run_flush(&mut m, 40_000).len(), 0);
    }

    #[test]
    fn audio_after_its_frame_was_emitted_is_dropped_and_counted() {
        let mut m = CallMixer::new(cfg());
        let mut out = Vec::new();
        m.advance(2_000_000, 1000, &mut out); // emitted through 1.75 s
        let chunk = vec![0.5f32; 1600]; // 100 ms
        m.push(Side::Visitor, 1_000_000, &chunk); // entirely late
        assert_eq!(m.stats().late_dropped_ms, 100);
        // A chunk spanning the boundary keeps only the part after it.
        m.push(Side::You, 1_700_000, &chunk);
        let st = m.stats();
        assert!(
            st.late_dropped_ms > 100 && st.late_dropped_ms < 200,
            "{st:?}"
        );
    }

    #[test]
    fn audio_too_far_ahead_is_dropped_and_counted() {
        let mut m = CallMixer::new(cfg());
        let chunk = vec![0.5f32; 16_000]; // 1 s
        m.push(Side::Visitor, 60_000_000, &chunk);
        assert_eq!(m.stats().ahead_dropped_ms, 1000);
        // Within 10 s is kept.
        m.push(Side::Visitor, 5_000_000, &chunk);
        assert_eq!(m.stats().ahead_dropped_ms, 1000);
        // A chunk reaching past the window is cut at it.
        let long = vec![0.5f32; 16_000 * 4];
        m.push(Side::You, 8_000_000, &long); // ends at 12 s
        assert!(m.stats().ahead_dropped_ms >= 1000 + 1999);
    }

    #[test]
    fn before_the_keyframe_is_trimmed() {
        let mut m = CallMixer::new(cfg());
        let mut v = vec![0.0f32; 1600]; // 100 ms starting 50 ms early
        v[800] = 0.5; // the impulse is at t = 0
        m.push(Side::Visitor, -50_000, &v);
        let frames = run_flush(&mut m, 100_000);
        let pcm = concat(&frames, |f| &f.visitor);
        assert_eq!(argmax(&pcm), 0);
    }

    #[test]
    fn a_long_call_keeps_exact_alignment() {
        // Two minutes of 20 ms chunks, an impulse in the last one: the
        // output sample is exactly where the stamp says, no drift.
        let mut m = CallMixer::new(cfg());
        let mut out = Vec::new();
        let total = 120_000_000i64;
        let zeros = vec![0.0f32; 320];
        let mut peak = None;
        let mut t = 0i64;
        let scan = |out: &mut Vec<MixedFrame>, peak: &mut Option<usize>| {
            for f in out.iter() {
                if f.you.iter().any(|s| s.abs() > 0.3) {
                    *peak = Some(f.start_sample as usize + argmax(&f.you));
                }
            }
            out.clear();
        };
        while t < total {
            let mut chunk = zeros.clone();
            if t == total - 20_000 {
                chunk[0] = 0.5;
            }
            m.push(Side::You, t, &chunk);
            t += 20_000;
            if t % 1_000_000 == 0 {
                m.advance(t, 1000, &mut out);
                scan(&mut out, &mut peak);
            }
        }
        m.flush(total, 1000, &mut out);
        scan(&mut out, &mut peak);
        assert_eq!(peak, Some(((total - 20_000) * 48 / 1000) as usize));
    }

    #[test]
    fn encode_queue_turns_overflow_into_silence_and_counts_it() {
        let mut q = EncodeQueue::new(3);
        let f = |v: f32| vec![v; FRAME];
        assert!(q.push(f(1.0)));
        assert!(q.push(f(2.0)));
        assert!(q.push(f(3.0)));
        assert!(!q.push(f(4.0)));
        assert!(!q.push(f(5.0)));
        assert_eq!(q.queued_frames(), 3);
        assert_eq!(q.encode_dropped_ms(), 2 * 1024 * 1000 / 48_000);
        assert_eq!(q.pop().unwrap()[0], 1.0);
        // Room again: a new frame queues after the silence.
        assert!(q.push(f(6.0)));
        let order: Vec<f32> = std::iter::from_fn(|| q.pop()).map(|v| v[0]).collect();
        assert_eq!(order, [2.0, 3.0, 0.0, 0.0, 6.0]);
        assert_eq!(q.queued_frames(), 0);
    }

    #[test]
    fn ten_seconds_is_469_frames() {
        assert_eq!(TEN_SECONDS_OF_FRAMES, 469);
    }

    #[test]
    fn i16_conversion() {
        assert_eq!(pcm_i16_to_f32(&[0, 16384, -32768]), [0.0, 0.5, -1.0]);
    }
}
