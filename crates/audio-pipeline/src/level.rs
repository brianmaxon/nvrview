//! The level of a tile's decoded audio, measured as it is decoded: for a
//! meter on a tile that is not audible, where no renderer measures it.
//!
//! The same meter as the renderer's ([`crate::SlotLevel`]): peak with a
//! release of [`LEVEL_PEAK_RELEASE_S`], RMS over about
//! [`LEVEL_RMS_WINDOW_S`], before volume. One thread writes (the tile's);
//! any thread reads, lock-free and allocation-free. A read applies the decay
//! since the last record (after [`JITTER_GRACE_US`]), so a stalled stream
//! falls to silence instead of keeping its last value, and reads exactly 0
//! after [`STALE_US`].

use crate::output::{LEVEL_PEAK_RELEASE_S, LEVEL_RMS_WINDOW_S};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// With no decoded audio for this long (µs), the level reads 0.
pub const STALE_US: u64 = 1_000_000;

/// Records arrive with jitter (bunched, then late by a few tens of
/// milliseconds, docs/protocol.md "Audio payloads"): a gap this long (µs)
/// after a record's own duration is not taken as silence, so the meter
/// does not flicker between records.
pub const JITTER_GRACE_US: u64 = 200_000;

/// A lock-free level meter fed with decoded samples.
#[derive(Debug, Default)]
pub struct DecodeLevel {
    peak: AtomicU32,
    mean_square: AtomicU32,
    /// When the last samples were measured (caller's clock, µs); 0: never.
    updated_us: AtomicU64,
}

impl DecodeLevel {
    /// A meter that reads 0.
    pub fn new() -> DecodeLevel {
        DecodeLevel::default()
    }

    /// Back to reading 0 (for a meter reused by another tile).
    pub fn clear(&self) {
        self.updated_us.store(0, Ordering::Release);
        self.peak.store(0, Ordering::Relaxed);
        self.mean_square.store(0, Ordering::Relaxed);
    }

    /// Measures interleaved samples (`channels` per frame at `sample_rate`)
    /// decoded at `now_us`. Only one thread may call this.
    pub fn observe(&self, pcm: &[f32], channels: usize, sample_rate: u32, now_us: u64) {
        let channels = channels.max(1);
        let frames = pcm.len() / channels;
        if frames == 0 || sample_rate == 0 {
            return;
        }
        let secs = frames as f32 / sample_rate as f32;
        // The samples stand for the `secs` before `now_us`; only a gap
        // beyond that (and the jitter grace) counts as silence.
        let (peak, ms) = self.decayed(now_us, (secs * 1e6) as u64 + JITTER_GRACE_US);
        let (mut block_peak, mut block_sq) = (0.0f32, 0.0f32);
        for v in &pcm[..frames * channels] {
            let v = if v.is_finite() { *v } else { 0.0 };
            block_peak = block_peak.max(v.abs());
            block_sq += v * v;
        }
        let a = 1.0 - (-secs / LEVEL_RMS_WINDOW_S).exp();
        let ms = ms + (block_sq / (frames * channels) as f32 - ms) * a;
        let release = (-secs / LEVEL_PEAK_RELEASE_S).exp();
        let peak = block_peak.min(1.0).max(peak * release);
        self.peak.store(peak.to_bits(), Ordering::Relaxed);
        self.mean_square.store(ms.to_bits(), Ordering::Relaxed);
        self.updated_us.store(now_us.max(1), Ordering::Release);
    }

    /// Peak and RMS, 0 to 1, at `now_us` (the clock `observe` was given).
    pub fn read(&self, now_us: u64) -> (f32, f32) {
        let (peak, ms) = self.decayed(now_us, JITTER_GRACE_US);
        (peak, ms.max(0.0).sqrt().min(1.0).min(peak))
    }

    /// Peak and mean square decayed to `now_us` as if silence had followed
    /// the last samples, not counting the first `covered_us`.
    fn decayed(&self, now_us: u64, covered_us: u64) -> (f32, f32) {
        let at = self.updated_us.load(Ordering::Acquire);
        if at == 0 {
            return (0.0, 0.0);
        }
        let dt = now_us.saturating_sub(at);
        if dt >= STALE_US {
            return (0.0, 0.0);
        }
        let peak = f32::from_bits(self.peak.load(Ordering::Relaxed));
        let ms = f32::from_bits(self.mean_square.load(Ordering::Relaxed));
        let secs = dt.saturating_sub(covered_us) as f32 / 1e6;
        (
            peak * (-secs / LEVEL_PEAK_RELEASE_S).exp(),
            ms * (-secs / LEVEL_RMS_WINDOW_S).exp(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amplitude: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| amplitude * (i as f32 * 0.2).sin())
            .collect()
    }

    #[test]
    fn follows_the_signal_and_falls_to_zero_when_it_stops() {
        let m = DecodeLevel::new();
        assert_eq!(m.read(5), (0.0, 0.0));
        let mut t = 1_000_000u64;
        for _ in 0..20 {
            m.observe(&tone(0.5, 1024), 1, 16_000, t);
            t += 64_000;
        }
        let (peak, rms) = m.read(t - 64_000);
        assert!((peak - 0.5).abs() < 0.01, "{peak}");
        assert!((rms - 0.5 / 2f32.sqrt()).abs() < 0.03, "{rms}");
        // Between records (late ones too) it holds; stalled, it decays, then
        // reads exactly 0.
        assert_eq!(m.read(t + 100_000), (peak, rms));
        let (p2, r2) = m.read(t + 500_000);
        assert!(p2 < peak * 0.5 && r2 < rms * 0.25, "{p2} {r2}");
        assert_eq!(m.read(t + STALE_US), (0.0, 0.0));
        // Silence after sound: the peak releases, RMS falls.
        for _ in 0..30 {
            m.observe(&[0.0; 1024], 1, 16_000, t);
            t += 64_000;
        }
        let (p3, r3) = m.read(t - 64_000);
        assert!(p3 < 0.01 && r3 < 0.001, "{p3} {r3}");
        m.clear();
        assert_eq!(m.read(t), (0.0, 0.0));
    }

    #[test]
    fn stereo_and_odd_input() {
        let m = DecodeLevel::new();
        let mut pcm = tone(0.25, 2048);
        pcm[3] = f32::NAN;
        m.observe(&pcm, 2, 48_000, 10);
        let (peak, rms) = m.read(10);
        assert!(peak > 0.2 && peak <= 0.25 && rms > 0.0 && rms <= peak);
        m.observe(&[], 2, 48_000, 20);
        m.observe(&[0.5], 2, 48_000, 20);
        m.observe(&[0.5, 0.5], 2, 0, 20);
        assert_eq!(m.read(10), (peak, rms));
    }
}
