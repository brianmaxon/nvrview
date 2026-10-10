//! The loudness envelope: peak and RMS per second in whole dBFS.

use crate::{SAMPLE_RATE, for_each_f32_chunk};

/// A level in whole dBFS, or "no audio analyzed", in its one-byte storage
/// form (docs/FFI-RECORDINGS.md, section 2a).
///
/// Byte 0 means no audio was analyzed; bytes 1 to 128 are -127 to 0 dBFS
/// (`dBFS = byte - 128`). Digital silence and anything quieter than -127
/// dBFS store as -127, so silence is a level, distinct from "no audio".
/// Anything louder than full scale stores as 0 dBFS. Bytes above 128 are not
/// produced and read as 0 dBFS. Ordering follows loudness, with "no audio"
/// lowest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Level(u8);

impl Level {
    /// No audio was analyzed.
    pub const NONE: Level = Level(0);
    /// The floor: -127 dBFS, digital silence included.
    pub const SILENCE: Level = Level(1);
    /// Lowest level stored, in dBFS.
    pub const MIN_DB: i8 = -127;

    /// The level of a linear amplitude (peak or RMS, 1.0 = full scale).
    pub fn from_amplitude(amplitude: f32) -> Level {
        if amplitude > 0.0 {
            Level::from_dbfs(20.0 * amplitude.log10())
        } else {
            Level::SILENCE
        }
    }

    /// The level of a power (mean square, 1.0 = full scale squared).
    pub fn from_power(power: f64) -> Level {
        if power > 0.0 {
            Level::from_dbfs((10.0 * power.log10()) as f32)
        } else {
            Level::SILENCE
        }
    }

    /// Rounds a dBFS value to whole decibels and clamps it to -127..=0. NaN
    /// counts as silence.
    pub fn from_dbfs(db: f32) -> Level {
        if db.is_nan() {
            return Level::SILENCE;
        }
        let r = db.round().clamp(f32::from(Level::MIN_DB), 0.0) as i16;
        Level((r + 128) as u8)
    }

    /// The stored byte.
    pub fn to_byte(self) -> u8 {
        self.0
    }

    /// The level a stored byte stands for.
    pub fn from_byte(b: u8) -> Level {
        Level(b.min(128))
    }

    /// Whole dBFS, or `None` when no audio was analyzed.
    pub fn db(self) -> Option<i8> {
        if self.0 == 0 {
            None
        } else {
            Some((i16::from(self.0) - 128) as i8)
        }
    }

    /// True unless no audio was analyzed.
    pub fn is_some(self) -> bool {
        self.0 != 0
    }

    fn power(self) -> Option<f64> {
        self.db().map(|d| 10f64.powf(f64::from(d) / 10.0))
    }
}

/// The levels of one second of audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SecondLevels {
    /// Number of the second: sample number / 16000.
    pub second: u64,
    /// Largest absolute sample value.
    pub peak: Level,
    /// Root mean square over the samples analyzed in this second.
    pub rms: Level,
    /// Samples analyzed in this second (16000 for a full second; fewer at a
    /// stream's start or end, or around a gap).
    pub samples: u32,
}

/// Measures peak and RMS per second of 16 kHz mono audio.
///
/// A second is reported once a sample of a later second arrives, or on
/// [`LoudnessMeter::flush`]. Seconds with no samples are never reported:
/// the caller stores them as [`Level::NONE`]. Non-finite samples count as 0.
#[derive(Clone, Debug, Default)]
pub struct LoudnessMeter {
    current: Option<u64>,
    next_sample: u64,
    sum_sq: f64,
    peak: f32,
    count: u32,
}

impl LoudnessMeter {
    /// A meter with nothing measured.
    pub fn new() -> LoudnessMeter {
        LoudnessMeter::default()
    }

    /// Measures `samples` (-1..1), the first of which is sample number
    /// `first_sample`. Blocks may have any size and may skip ahead (a gap);
    /// a block that starts before the end of the previous one is treated as
    /// a new stream, after flushing. Calls `emit` for each completed second.
    pub fn push(&mut self, first_sample: u64, samples: &[f32], mut emit: impl FnMut(SecondLevels)) {
        if self.current.is_some() && first_sample < self.next_sample {
            self.flush(&mut emit);
        }
        let rate = u64::from(SAMPLE_RATE);
        let mut pos = first_sample;
        let mut rest = samples;
        while !rest.is_empty() {
            let second = pos / rate;
            if self.current != Some(second) {
                self.flush(&mut emit);
                self.current = Some(second);
            }
            let room = ((second + 1) * rate - pos) as usize;
            let (now, later) = rest.split_at(room.min(rest.len()));
            let mut sum = 0.0f32;
            let mut peak = self.peak;
            for &x in now {
                let x = if x.is_finite() { x } else { 0.0 };
                sum += x * x;
                peak = peak.max(x.abs());
            }
            self.sum_sq += f64::from(sum);
            self.peak = peak;
            self.count += now.len() as u32;
            pos += now.len() as u64;
            rest = later;
        }
        self.next_sample = pos;
    }

    /// [`LoudnessMeter::push`] for 16-bit samples.
    pub fn push_i16(
        &mut self,
        first_sample: u64,
        samples: &[i16],
        mut emit: impl FnMut(SecondLevels),
    ) {
        for_each_f32_chunk(first_sample, samples, |pos, chunk| {
            self.push(pos, chunk, &mut emit)
        });
    }

    /// Reports the second in progress, if any (end of stream, or before a
    /// discontinuity).
    pub fn flush(&mut self, mut emit: impl FnMut(SecondLevels)) {
        if let Some(second) = self.current.take() {
            if self.count > 0 {
                emit(SecondLevels {
                    second,
                    peak: Level::from_amplitude(self.peak),
                    rms: Level::from_power(self.sum_sq / f64::from(self.count)),
                    samples: self.count,
                });
            }
            self.sum_sq = 0.0;
            self.peak = 0.0;
            self.count = 0;
        }
    }
}

/// The peak of a bucket of seconds: the loudest, or [`Level::NONE`] when no
/// second in it has audio.
pub fn aggregate_peak(levels: &[Level]) -> Level {
    levels.iter().copied().max().unwrap_or(Level::NONE)
}

/// The RMS of a bucket of seconds: the power mean of the seconds that have
/// audio (each second weighs the same), or [`Level::NONE`] when none has.
pub fn aggregate_rms(levels: &[Level]) -> Level {
    let (sum, n) = levels
        .iter()
        .filter_map(|l| l.power())
        .fold((0.0f64, 0u32), |(s, n), p| (s + p, n + 1));
    if n == 0 {
        Level::NONE
    } else {
        Level::from_power(sum / f64::from(n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(meter: &mut LoudnessMeter, first: u64, samples: &[f32]) -> Vec<SecondLevels> {
        let mut out = Vec::new();
        meter.push(first, samples, |s| out.push(s));
        out
    }

    #[test]
    fn byte_form_round_trips() {
        assert_eq!(Level::NONE.db(), None);
        assert_eq!(Level::SILENCE.db(), Some(-127));
        assert_eq!(Level::from_dbfs(0.4).db(), Some(0));
        assert_eq!(Level::from_dbfs(6.0).db(), Some(0));
        assert_eq!(Level::from_dbfs(-20.6).db(), Some(-21));
        assert_eq!(Level::from_dbfs(-300.0), Level::SILENCE);
        assert_eq!(Level::from_dbfs(f32::NAN), Level::SILENCE);
        for b in 0..=128u8 {
            assert_eq!(Level::from_byte(b).to_byte(), b);
        }
        assert_eq!(Level::from_byte(200).db(), Some(0));
        assert!(Level::NONE < Level::SILENCE);
        assert!(Level::from_dbfs(-60.0) < Level::from_dbfs(-10.0));
    }

    #[test]
    fn sine_levels_and_silence() {
        let mut m = LoudnessMeter::new();
        let sine: Vec<f32> = (0..32_000)
            .map(|n| 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / 16_000.0).sin())
            .collect();
        let mut out = collect(&mut m, 0, &sine);
        out.extend(collect(&mut m, 32_000, &vec![0.0; 16_000]));
        m.flush(|s| out.push(s));
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].peak.db(), Some(-6));
        assert_eq!(out[0].rms.db(), Some(-9));
        assert_eq!(out[1].second, 1);
        assert_eq!(out[2].peak, Level::SILENCE);
        assert_eq!(out[2].rms, Level::SILENCE);
        assert_eq!(out[2].samples, 16_000);
    }

    #[test]
    fn block_size_does_not_matter() {
        let sig: Vec<f32> = (0..50_000)
            .map(|n| ((n * 7919) % 2001) as f32 / 1000.0 - 1.0)
            .collect();
        let mut whole = LoudnessMeter::new();
        let mut a = collect(&mut whole, 0, &sig);
        whole.flush(|s| a.push(s));
        let mut parts = LoudnessMeter::new();
        let mut b = Vec::new();
        let mut pos = 0;
        for size in [1usize, 17, 1023, 4096, 333].iter().cycle() {
            if pos >= sig.len() {
                break;
            }
            let end = (pos + size).min(sig.len());
            parts.push(pos as u64, &sig[pos..end], |s| b.push(s));
            pos = end;
        }
        parts.flush(|s| b.push(s));
        assert_eq!(a, b);
    }

    #[test]
    fn gaps_leave_seconds_unreported() {
        let mut m = LoudnessMeter::new();
        let mut out = collect(&mut m, 8_000, &[0.1; 8_000]);
        out.extend(collect(&mut m, 5 * 16_000, &[0.1; 100]));
        m.flush(|s| out.push(s));
        let secs: Vec<u64> = out.iter().map(|s| s.second).collect();
        assert_eq!(secs, vec![0, 5]);
        assert_eq!(out[0].samples, 8_000);
        assert_eq!(out[1].samples, 100);
    }

    #[test]
    fn i16_matches_f32() {
        let ints: Vec<i16> = (0..20_000)
            .map(|n| ((n * 31) % 20_000 - 10_000) as i16)
            .collect();
        let floats: Vec<f32> = ints.iter().map(|&s| f32::from(s) / 32768.0).collect();
        let mut a = Vec::new();
        let mut b = Vec::new();
        let mut m1 = LoudnessMeter::new();
        m1.push_i16(0, &ints, |s| a.push(s));
        m1.flush(|s| a.push(s));
        let mut m2 = LoudnessMeter::new();
        m2.push(0, &floats, |s| b.push(s));
        m2.flush(|s| b.push(s));
        assert_eq!(a, b);
    }

    #[test]
    fn aggregation() {
        let l = |d: f32| Level::from_dbfs(d);
        assert_eq!(aggregate_peak(&[]), Level::NONE);
        assert_eq!(aggregate_peak(&[Level::NONE, Level::NONE]), Level::NONE);
        assert_eq!(
            aggregate_peak(&[l(-40.0), Level::NONE, l(-12.0)]).db(),
            Some(-12)
        );
        assert_eq!(aggregate_rms(&[Level::NONE]), Level::NONE);
        // The power mean of -20 and -60 dB is about -23 dB: the loud second
        // dominates, and seconds without audio do not count.
        assert_eq!(aggregate_rms(&[l(-20.0), l(-20.0)]).db(), Some(-20));
        assert_eq!(
            aggregate_rms(&[l(-20.0), l(-60.0), Level::NONE]).db(),
            Some(-23)
        );
        assert_eq!(
            aggregate_rms(&[Level::SILENCE, Level::SILENCE]),
            Level::SILENCE
        );
    }
}
