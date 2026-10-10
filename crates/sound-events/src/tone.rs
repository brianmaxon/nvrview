//! Narrow-band tone detection per frame, and tone pulses with hysteresis.

use crate::SAMPLE_RATE;

/// Samples per analysis frame (16 ms at 16 kHz).
pub(crate) const FRAME: usize = 256;
/// Spacing of the filters: one frequency-resolution step of a frame.
const BIN_SPACING_HZ: f32 = SAMPLE_RATE as f32 / FRAME as f32;
/// Filters computed side by side.
const LANES: usize = 8;
/// Consecutive frames needed to start or stop a pulse.
const DEBOUNCE_FRAMES: u32 = 2;

/// What one frame says about a tone in the band.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct FrameTone {
    /// Power of the strongest filter, as the mean square of the sinusoid it
    /// stands for (a full-scale sine is 0.5).
    pub tone_power: f32,
    /// Strongest filter's power over the frame's whole power: about 1 for a
    /// pure tone in the band, about 0.01 for white noise.
    pub ratio: f32,
    /// Strongest filter's power over the sum of all filters' powers: near
    /// 0.6 for a tone (its main lobe spans a few filters), much lower for
    /// noise that fills the band.
    pub concentration: f32,
}

/// A bank of Goertzel filters across a band, on Hann-windowed frames.
#[derive(Clone, Debug)]
pub(crate) struct ToneBank {
    /// Filter coefficients (2 cos ω), in groups of [`LANES`] that run side
    /// by side; the last group is padded.
    coeffs: Vec<[f32; LANES]>,
    /// Number of real filters.
    bins: usize,
    window: Vec<f32>,
    /// 2 / (sum of the window)^2: scales a filter's power to a mean square.
    tone_scale: f32,
    /// 1 / (sum of the squared window): scales the windowed energy likewise.
    frame_scale: f32,
}

impl ToneBank {
    pub fn new(low_hz: f32, high_hz: f32) -> ToneBank {
        let nyquist = SAMPLE_RATE as f32 / 2.0;
        let low = low_hz.clamp(BIN_SPACING_HZ, nyquist - BIN_SPACING_HZ);
        let high = high_hz.clamp(low, nyquist - BIN_SPACING_HZ);
        // Enough filters to reach both edges, centered on the band.
        let n = ((high - low) / BIN_SPACING_HZ).ceil() as usize + 1;
        let first = (low + high) / 2.0 - (n - 1) as f32 * BIN_SPACING_HZ / 2.0;
        let coeff = |k: usize| {
            let f = first + k.min(n - 1) as f32 * BIN_SPACING_HZ;
            2.0 * (2.0 * std::f32::consts::PI * f / SAMPLE_RATE as f32).cos()
        };
        let coeffs = (0..n.div_ceil(LANES))
            .map(|g| std::array::from_fn(|l| coeff(g * LANES + l)))
            .collect();
        let window: Vec<f32> = (0..FRAME)
            .map(|i| {
                let x = 2.0 * std::f32::consts::PI * (i as f32 + 0.5) / FRAME as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        let sum: f32 = window.iter().sum();
        let sum_sq: f32 = window.iter().map(|w| w * w).sum();
        ToneBank {
            coeffs,
            bins: n,
            window,
            tone_scale: 2.0 / (sum * sum),
            frame_scale: 1.0 / sum_sq,
        }
    }

    pub fn analyze(&self, frame: &[f32; FRAME]) -> FrameTone {
        let mut w = [0.0f32; FRAME];
        let mut energy = 0.0f32;
        for ((o, &x), &h) in w.iter_mut().zip(frame).zip(&self.window) {
            *o = x * h;
            energy += *o * *o;
        }
        let mut best = 0.0f32;
        let mut total = 0.0f32;
        for (g, c) in self.coeffs.iter().enumerate() {
            // Several independent filters per pass: the recurrences do not
            // wait on each other.
            let mut s1 = [0.0f32; LANES];
            let mut s2 = [0.0f32; LANES];
            for &x in &w {
                for l in 0..LANES {
                    let s0 = x + c[l] * s1[l] - s2[l];
                    s2[l] = s1[l];
                    s1[l] = s0;
                }
            }
            for l in 0..LANES.min(self.bins - g * LANES) {
                let p = (s1[l] * s1[l] + s2[l] * s2[l] - c[l] * s1[l] * s2[l]).max(0.0);
                best = best.max(p);
                total += p;
            }
        }
        let tone_power = best * self.tone_scale;
        let frame_power = energy * self.frame_scale;
        let ratio = if frame_power > 1e-20 {
            (tone_power / frame_power).min(2.0)
        } else {
            0.0
        };
        let concentration = if total > 0.0 { best / total } else { 0.0 };
        FrameTone {
            tone_power,
            ratio,
            concentration,
        }
    }
}

/// A second-order Butterworth high-pass filter (RBJ biquad). Ahead of the
/// filter bank it keeps rumble and wind, which can carry most of a camera
/// microphone's energy, out of the frame's reference power.
#[derive(Clone, Debug)]
pub(crate) struct HighPass {
    b0: f32,
    b1: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl HighPass {
    pub fn new(cutoff_hz: f32) -> HighPass {
        let w = 2.0 * std::f32::consts::PI * cutoff_hz / SAMPLE_RATE as f32;
        let alpha = w.sin() / std::f32::consts::SQRT_2;
        let c = w.cos();
        let a0 = 1.0 + alpha;
        HighPass {
            b0: (1.0 + c) / 2.0 / a0,
            b1: -(1.0 + c) / a0,
            a1: -2.0 * c / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    pub fn run(&mut self, x: f32) -> f32 {
        // Transposed direct form II; b2 equals b0.
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b0 * x - self.a2 * y;
        y
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

/// Thresholds for starting and stopping a pulse.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ToneThresholds {
    pub on_ratio: f32,
    pub off_ratio: f32,
    /// Minimum tone power (mean square) to start a pulse.
    pub min_power: f32,
    /// A pulse stops below this tone power.
    pub stay_power: f32,
    /// A pulse stops when the tone falls below its peak times this.
    pub release: f32,
}

/// A change of the tone state, at the first frame that showed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    Start(u64),
    End(u64),
}

/// Turns per-frame decisions into pulse edges: two frames to start, two to
/// stop, with each edge dated at the first frame of the run that caused it.
#[derive(Clone, Debug, Default)]
pub(crate) struct ToneTracker {
    on: bool,
    run: u32,
    run_start: u64,
    peak: f32,
    /// Lowest tone power since the last pulse ended: a new pulse must rise
    /// [`REARM`] above it, so a decaying tail cannot start one.
    trough: f32,
}

/// Concentration needed to start a pulse, and to keep it going. Only bands
/// wider than a few filters can tell a tone from band-limited noise this way.
const START_CONCENTRATION: f32 = 0.4;
const STAY_CONCENTRATION: f32 = 0.25;

/// Rise over the trough needed to start a pulse (6 dB).
const REARM: f32 = 4.0;

impl ToneTracker {
    pub fn update(&mut self, t: &ToneThresholds, frame_start: u64, ft: FrameTone) -> Option<Edge> {
        let pass = if self.on {
            ft.ratio >= t.off_ratio
                && ft.concentration >= STAY_CONCENTRATION
                && ft.tone_power >= t.stay_power
                && ft.tone_power >= self.peak * t.release
        } else {
            ft.ratio >= t.on_ratio
                && ft.concentration >= START_CONCENTRATION
                && ft.tone_power >= t.min_power
                && ft.tone_power >= self.trough * REARM
        };
        if pass {
            if self.on {
                self.run = 0;
                self.peak = self.peak.max(ft.tone_power);
                return None;
            }
            if self.run == 0 {
                self.run_start = frame_start;
                self.peak = 0.0;
            }
            self.peak = self.peak.max(ft.tone_power);
            self.run += 1;
            if self.run >= DEBOUNCE_FRAMES {
                self.on = true;
                self.run = 0;
                return Some(Edge::Start(self.run_start));
            }
        } else if self.on {
            if self.run == 0 {
                self.run_start = frame_start;
            }
            self.run += 1;
            if self.run >= DEBOUNCE_FRAMES {
                self.on = false;
                self.run = 0;
                self.trough = ft.tone_power;
                return Some(Edge::End(self.run_start));
            }
        } else {
            self.run = 0;
            self.trough = self.trough.min(ft.tone_power);
        }
        None
    }

    /// Forgets the tone state (a discontinuity). Returns true if a pulse was
    /// in progress.
    pub fn reset(&mut self) -> bool {
        let was = self.on;
        *self = ToneTracker::default();
        was
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, amp: f32) -> [f32; FRAME] {
        let mut f = [0.0; FRAME];
        for (i, s) in f.iter_mut().enumerate() {
            *s = amp
                * (2.0 * std::f32::consts::PI * freq * i as f32 / SAMPLE_RATE as f32 + 0.3).sin();
        }
        f
    }

    #[test]
    fn pure_tone_in_band_has_ratio_near_one() {
        let bank = ToneBank::new(2900.0, 3500.0);
        for freq in [2900.0, 3031.25, 3200.0, 3460.0, 3500.0] {
            let ft = bank.analyze(&sine(freq, 0.5));
            assert!(ft.ratio > 0.6, "{freq} Hz: ratio {}", ft.ratio);
            // 0.125 is the mean square of a 0.5 sine; scalloping costs at
            // most about 1.5 dB.
            assert!(
                ft.tone_power > 0.125 * 0.7 && ft.tone_power < 0.125 * 1.05,
                "{}",
                ft.tone_power
            );
        }
    }

    #[test]
    fn tones_outside_the_band_do_not_count() {
        let bank = ToneBank::new(2900.0, 3500.0);
        for freq in [520.0, 1000.0, 2500.0, 4000.0, 6000.0] {
            let ft = bank.analyze(&sine(freq, 0.5));
            assert!(ft.ratio < 0.01, "{freq} Hz: ratio {}", ft.ratio);
        }
    }

    #[test]
    fn silence_has_no_tone() {
        let bank = ToneBank::new(2900.0, 3500.0);
        let ft = bank.analyze(&[0.0; FRAME]);
        assert_eq!(ft.ratio, 0.0);
        assert_eq!(ft.tone_power, 0.0);
    }
}
