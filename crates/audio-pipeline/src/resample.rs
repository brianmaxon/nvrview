//! A variable-ratio windowed-sinc resampler.
//!
//! Positions are absolute input sample numbers (of the current epoch), so
//! the caller always knows which input instant the next output sample
//! stands for: that is how playout ties output samples to camera time. The
//! ratio (input samples advanced per output sample) may change between
//! calls, which is how drift is corrected.
//!
//! The kernel is a Kaiser-windowed sinc, tabulated at [`PHASES`] fractional
//! offsets and linearly interpolated between them, normalized to unity gain
//! at DC for every offset. Its cutoff is 90% of the lower of the two Nyquist
//! frequencies, so it also serves for downsampling (the tap's 48 kHz to
//! 16 kHz, for example).

/// Kernel half-width, in input samples, when upsampling.
const HALF_TAPS: usize = 16;
/// Fractional offsets tabulated.
const PHASES: usize = 256;
/// Kaiser window shape.
const BETA: f64 = 8.0;
/// Cutoff as a fraction of the lower Nyquist frequency.
const CUTOFF: f64 = 0.9;

/// The resampler. Interleaved samples, any channel count.
#[derive(Clone, Debug)]
pub struct Resampler {
    channels: usize,
    half: usize,
    /// `(PHASES + 1)` rows of `2 * half` taps.
    table: Vec<f32>,
    /// Buffered input, interleaved; `hist[0]` is input sample `hist_start`.
    hist: Vec<f32>,
    hist_start: i64,
    /// Input position of the next output sample.
    pos: f64,
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > sum * 1e-12 {
        term *= (x / (2.0 * k)).powi(2);
        sum += term;
        k += 1.0;
    }
    sum
}

impl Resampler {
    /// A resampler for `channels` interleaved channels whose nominal ratio is
    /// `in_rate` to `out_rate` (the ratio only sets the cutoff; the actual
    /// step is given to every [`Resampler::produce`] call). Starts at input
    /// position 0.
    pub fn new(channels: usize, in_rate: u32, out_rate: u32) -> Resampler {
        let channels = channels.max(1);
        let scale = (f64::from(out_rate) / f64::from(in_rate.max(1))).min(1.0);
        let fc = CUTOFF * scale;
        let half = (HALF_TAPS as f64 / scale).ceil() as usize;
        let taps = 2 * half;
        let mut table = vec![0.0f32; (PHASES + 1) * taps];
        let i0b = bessel_i0(BETA);
        for ph in 0..=PHASES {
            let frac = ph as f64 / PHASES as f64;
            let row = &mut table[ph * taps..(ph + 1) * taps];
            let mut sum = 0.0;
            let mut tmp = vec![0.0f64; taps];
            for (k, t) in tmp.iter_mut().enumerate() {
                // Tap k weighs input sample floor(pos) + k + 1 - half.
                let x = (k as f64 + 1.0 - half as f64) - frac;
                let sinc = if x.abs() < 1e-12 {
                    1.0
                } else {
                    let a = std::f64::consts::PI * fc * x;
                    a.sin() / a
                };
                let r = x / half as f64;
                let w = if r.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(BETA * (1.0 - r * r).sqrt()) / i0b
                };
                *t = sinc * w;
                sum += *t;
            }
            for (o, t) in row.iter_mut().zip(&tmp) {
                *o = (t / sum) as f32;
            }
        }
        Resampler {
            channels,
            half,
            table,
            hist: Vec::new(),
            hist_start: 0,
            pos: 0.0,
        }
    }

    /// Channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Delay of the kernel in input samples: output at position `p` needs
    /// input up to `p + half`.
    pub fn half_width(&self) -> usize {
        self.half
    }

    /// Input position of the next output sample.
    pub fn position(&self) -> f64 {
        self.pos
    }

    /// Input sample number one past the last buffered sample.
    pub fn input_end(&self) -> i64 {
        self.hist_start + (self.hist.len() / self.channels) as i64
    }

    /// Forgets all input and starts at input position `pos`, as if silence
    /// preceded it.
    pub fn reset(&mut self, pos: f64) {
        self.hist.clear();
        let first = pos.floor() as i64 - self.half as i64 + 1;
        self.hist_start = first;
        let pad = (pos.floor() as i64 - first + 1).max(0) as usize;
        self.hist.resize(pad * self.channels, 0.0);
        // The padding ends at floor(pos); input continues from floor(pos) + 1
        // unless the position is integral.
        if pos.fract() == 0.0 {
            self.hist.truncate((pad - 1) * self.channels);
        }
        self.pos = pos;
    }

    /// Appends input that starts at sample `first`. A gap since the
    /// buffered input is filled with silence; input that overlaps what is
    /// buffered is ignored up to the overlap.
    pub fn push(&mut self, first: i64, samples: &[f32]) {
        let end = self.input_end();
        let mut samples = samples;
        if first > end {
            self.hist.resize(
                self.hist.len() + (first - end) as usize * self.channels,
                0.0,
            );
        } else if first < end {
            let skip = ((end - first) as usize * self.channels).min(samples.len());
            samples = &samples[skip..];
        }
        self.hist.extend_from_slice(samples);
    }

    /// Moves the next output position forward to `pos` (dropping input).
    pub fn skip_to(&mut self, pos: f64) {
        if pos > self.pos {
            self.pos = pos;
            self.trim();
        }
    }

    /// Produces as many output frames as the buffered input allows, at
    /// `step` input samples per output sample, at most `max_frames`,
    /// appending them to `out`. Returns the number of frames produced.
    pub fn produce(&mut self, step: f64, max_frames: usize, out: &mut Vec<f32>) -> usize {
        let taps = 2 * self.half;
        let ch = self.channels;
        let end = self.input_end();
        let mut n = 0;
        while n < max_frames {
            let base = self.pos.floor();
            let last_needed = base as i64 + self.half as i64;
            if last_needed >= end {
                break;
            }
            let first = base as i64 + 1 - self.half as i64;
            let off = (first - self.hist_start) as usize;
            let phf = (self.pos - base) * PHASES as f64;
            let ph = (phf as usize).min(PHASES - 1);
            let t = (phf - ph as f64) as f32;
            let r0 = &self.table[ph * taps..(ph + 1) * taps];
            let r1 = &self.table[(ph + 1) * taps..(ph + 2) * taps];
            for c in 0..ch {
                let mut acc = 0.0f32;
                for k in 0..taps {
                    let w = r0[k] + (r1[k] - r0[k]) * t;
                    acc += w * self.hist[(off + k) * ch + c];
                }
                out.push(acc);
            }
            self.pos += step;
            n += 1;
        }
        self.trim();
        n
    }

    /// Drops input no longer needed.
    fn trim(&mut self) {
        let keep_from = self.pos.floor() as i64 + 1 - self.half as i64;
        let drop = (keep_from - self.hist_start).max(0) as usize;
        let drop = drop.min(self.hist.len() / self.channels);
        if drop > 0 {
            self.hist.drain(..drop * self.channels);
            self.hist_start += drop as i64;
        }
        if self.hist.is_empty() && keep_from > self.hist_start {
            self.hist_start = keep_from;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: f64, f: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * f * i as f64 / rate).sin() as f32 * 0.5)
            .collect()
    }

    /// Error of `out` against the ideal tone sampled at the output positions.
    fn snr(out: &[f32], positions: &[f64], in_rate: f64, f: f64) -> f64 {
        let (mut s, mut e) = (0.0, 0.0);
        for (o, p) in out.iter().zip(positions) {
            let want = (2.0 * std::f64::consts::PI * f * p / in_rate).sin() * 0.5;
            s += want * want;
            e += (want - f64::from(*o)).powi(2);
        }
        10.0 * (s / e).log10()
    }

    #[test]
    fn upsamples_a_tone_cleanly_at_any_ratio() {
        for step in [
            16_000.0 / 48_000.0,
            16_013.0 / 48_000.0 * 1.004,
            16_000.0 / 44_100.0,
        ] {
            let input = tone(16_000.0, 1000.0, 16_000);
            let mut r = Resampler::new(1, 16_000, 48_000);
            r.reset(0.0);
            let mut out = Vec::new();
            let mut positions = Vec::new();
            for (i, chunk) in input.chunks(1024).enumerate() {
                r.push((i * 1024) as i64, chunk);
                let before = r.position();
                let n = r.produce(step, usize::MAX, &mut out);
                positions.extend((0..n).map(|k| before + k as f64 * step));
            }
            // Skip the start (silence before position 0 in the kernel).
            let s = snr(&out[200..], &positions[200..], 16_000.0, 1000.0);
            assert!(s > 70.0, "step {step}: SNR {s:.1} dB");
        }
    }

    #[test]
    fn downsamples_with_the_lower_cutoff() {
        // 48 kHz stereo to 16 kHz: a 1 kHz tone passes, a 12 kHz tone is
        // removed.
        let n = 48_000;
        let low = tone(48_000.0, 1000.0, n);
        let high = tone(48_000.0, 12_000.0, n);
        let mut input = Vec::with_capacity(2 * n);
        for i in 0..n {
            input.push(low[i]);
            input.push(high[i]);
        }
        let mut r = Resampler::new(2, 48_000, 16_000);
        r.reset(0.0);
        r.push(0, &input);
        let mut out = Vec::new();
        let frames = r.produce(3.0, usize::MAX, &mut out);
        assert!(frames > 15_900);
        let rms = |c: usize| {
            let v: f64 = out
                .chunks(2)
                .skip(200)
                .map(|f| f64::from(f[c]).powi(2))
                .sum();
            (v / (frames - 200) as f64).sqrt()
        };
        assert!((rms(0) - 0.5 / 2f64.sqrt()).abs() < 0.01, "{}", rms(0));
        assert!(rms(1) < 0.001, "{}", rms(1));
    }

    #[test]
    fn positions_track_input_and_skips() {
        let mut r = Resampler::new(1, 16_000, 48_000);
        r.reset(100.0);
        r.push(100, &[0.0; 1024]);
        let mut out = Vec::new();
        let n = r.produce(1.0 / 3.0, usize::MAX, &mut out);
        // Output stops `half` samples before the end of the input.
        assert_eq!(n, ((1024 - HALF_TAPS) as f64 * 3.0).ceil() as usize);
        r.skip_to(5000.0);
        assert_eq!(r.position(), 5000.0);
        r.push(1124, &[0.0; 3000]);
        assert_eq!(r.produce(1.0 / 3.0, usize::MAX, &mut out), 0);
        r.push(4124, &[0.0; 1000]);
        assert!(r.produce(1.0 / 3.0, usize::MAX, &mut out) > 0);
    }
}
