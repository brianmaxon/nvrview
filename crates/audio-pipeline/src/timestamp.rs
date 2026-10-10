//! Camera time for audio samples that carry no timestamp.
//!
//! The stream gives each audio record only its position among timestamped
//! video records (docs/protocol.md). We assume the device sends a record
//! when its last sample has been captured, after every video picture
//! captured before that moment and before every later one. So for a record
//! whose last sample is number `x` (counting samples since the last reset),
//! with `P` the timestamp of the video picture just before it and `U` that
//! of the next picture after it:
//!
//! ```text
//! P <= T(x) < U,   T(x) = t0 + x / rate   (camera video time)
//! ```
//!
//! `rate` is the camera's real audio rate in video time (16,005 to 16,013
//! samples per second were measured for a nominal 16,000). The slope comes
//! from an exponentially weighted least-squares fit of `P` against `x`, which
//! is unbiased for the slope whatever the spread of `P` below `T`. The offset
//! comes from the two bounds: the largest `P - line(x)` approaches the true
//! offset from below (a record sent just after a picture), the smallest
//! `U - line(x)` approaches it from above (a record sent just before one), and
//! the estimate is their midpoint. Both are slowly leaking envelopes, so one
//! odd record does not hold them.
//!
//! A record far off the line (more than [`OUTLIER_US`]) is ignored; several
//! in a row mean the relation changed (a gap in the audio, a jump in the
//! video timestamps), and the fit starts over in a new epoch.

use crate::fit::{Envelope, LineFit};
use std::collections::VecDeque;

/// A record this far from the fitted line is an outlier.
pub const OUTLIER_US: f64 = 500_000.0;
/// This many outliers in a row reset the fit.
pub const OUTLIERS_TO_RESET: u32 = 4;
/// Window of the fit, in seconds of audio.
const WINDOW_S: f64 = 60.0;
/// The slope prior weighs as much as this many seconds of data.
const PRIOR_S: f64 = 4.0;
/// Envelope leak, µs of camera time per second of audio.
const LEAK_US_PER_S: f64 = 1_000.0;
/// Records waiting for the next picture's timestamp.
const MAX_PENDING: usize = 256;

/// Maps sample numbers of one epoch to camera time:
/// `T(n) = t0_us + n * us_per_sample`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timeline {
    /// Camera time (video timestamp units, µs) of sample 0 of the epoch.
    pub t0_us: f64,
    /// Camera microseconds per sample: the inverse of the estimated rate.
    pub us_per_sample: f64,
}

impl Timeline {
    /// Camera time of sample `n` (fractional samples allowed).
    pub fn at(&self, n: f64) -> f64 {
        self.t0_us + n * self.us_per_sample
    }

    /// Estimated samples per second of camera time.
    pub fn rate(&self) -> f64 {
        1e6 / self.us_per_sample
    }
}

/// Where one record's samples sit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stamp {
    /// Increments on every reset; sample numbers restart at 0.
    pub epoch: u32,
    /// Number of the record's first sample in the epoch.
    pub first_sample: u64,
    /// The mapping after this record, if any video timestamp has been seen
    /// in this epoch; `None` also for a record far off the current mapping
    /// (a suspected gap, see [`OUTLIER_US`]).
    pub timeline: Option<Timeline>,
    /// True for the first record of a new epoch.
    pub reset: bool,
}

/// Counters for diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimestampStats {
    /// Records ignored as outliers.
    pub outliers: u64,
    /// Resets for any reason (discontinuity, format change, outliers).
    pub resets: u64,
}

/// The per-tile timestamper.
#[derive(Debug)]
pub struct Timestamper {
    rate: u32,
    epoch: u32,
    count: u64,
    pending_reset: bool,
    pts_origin: Option<f64>,
    fit: LineFit,
    lower: Envelope,
    upper: Envelope,
    pending: VecDeque<(f64, f64)>,
    last_pts: Option<f64>,
    outliers_in_row: u32,
    stats: TimestampStats,
}

impl Timestamper {
    /// A timestamper for a source with nominal `rate` samples per second.
    pub fn new(rate: u32) -> Timestamper {
        let rate = rate.max(1);
        let r = f64::from(rate);
        // Prior strength: the x co-moment of PRIOR_S seconds of records
        // spread evenly (about 16 records per second).
        let span = PRIOR_S * r;
        let prior = PRIOR_S * 16.0 * span * span / 12.0;
        Timestamper {
            rate,
            epoch: 0,
            count: 0,
            pending_reset: true,
            pts_origin: None,
            fit: LineFit::new(WINDOW_S * r, 1e6 / r, prior),
            lower: Envelope::new(true, LEAK_US_PER_S / r),
            upper: Envelope::new(false, LEAK_US_PER_S / r),
            pending: VecDeque::new(),
            last_pts: None,
            outliers_in_row: 0,
            stats: TimestampStats::default(),
        }
    }

    /// Nominal rate this timestamper was made for.
    pub fn nominal_rate(&self) -> u32 {
        self.rate
    }

    /// Counters.
    pub fn stats(&self) -> TimestampStats {
        self.stats
    }

    /// Starts a new epoch: the stream restarted, the format changed, or
    /// samples were lost in a way the count cannot account for.
    pub fn reset(&mut self) {
        self.reset_with_rate(self.rate);
    }

    /// Like [`Timestamper::reset`], for a source with a new nominal rate.
    pub fn reset_with_rate(&mut self, rate: u32) {
        let (epoch, stats) = (self.epoch.wrapping_add(1), self.stats);
        *self = Timestamper::new(rate);
        self.epoch = epoch;
        self.stats = stats;
        self.stats.resets += 1;
    }

    /// The current mapping, if one exists.
    pub fn timeline(&self) -> Option<Timeline> {
        let origin = self.pts_origin?;
        let k = self.fit.slope();
        let c = match (self.lower.value(), self.upper.value()) {
            (Some(lo), Some(hi)) => 0.5 * (lo + hi),
            (Some(lo), None) => lo,
            (None, Some(hi)) => hi,
            (None, None) => 0.0,
        };
        Some(Timeline {
            t0_us: origin + self.fit.at(0.0)? + c,
            us_per_sample: k,
        })
    }

    /// Accounts for one record of `frames` samples that followed the video
    /// picture stamped `after_video_pts_us` (if any since the last reset).
    pub fn push(&mut self, frames: usize, after_video_pts_us: Option<u64>) -> Stamp {
        if let Some(p) = after_video_pts_us {
            let p = p as f64;
            let x_end = (self.count + frames as u64) as f64;
            if let Some(tl) = self.timeline() {
                let d = p - tl.at(x_end);
                let backwards = self.last_pts.is_some_and(|l| p < l);
                if d.abs() > OUTLIER_US || backwards {
                    self.stats.outliers += 1;
                    self.outliers_in_row += 1;
                    if self.outliers_in_row < OUTLIERS_TO_RESET && !backwards {
                        // Its time is unknown: neither the old line nor a
                        // new one fits it yet.
                        let mut st = self.stamp(frames);
                        st.timeline = None;
                        return st;
                    }
                    self.reset();
                }
            }
            self.outliers_in_row = 0;
            self.observe(self.count as f64 + frames as f64, p);
        }
        self.stamp(frames)
    }

    fn observe(&mut self, x: f64, p: f64) {
        let origin = *self.pts_origin.get_or_insert(p);
        let y = p - origin;
        self.fit.add(x, y);
        // Upper bounds for records that were waiting for the next picture.
        if self.last_pts.is_some_and(|l| p > l) {
            while let Some((px, _)) = self.pending.pop_front() {
                let line = self.fit.at(px).unwrap_or(y);
                self.upper.add(px, y - line);
            }
        }
        self.last_pts = Some(p);
        if self.pending.len() == MAX_PENDING {
            self.pending.pop_front();
        }
        self.pending.push_back((x, y));
        let line = self.fit.at(x).unwrap_or(y);
        self.lower.add(x, y - line);
    }

    fn stamp(&mut self, frames: usize) -> Stamp {
        let s = Stamp {
            epoch: self.epoch,
            first_sample: self.count,
            timeline: self.timeline(),
            reset: std::mem::take(&mut self.pending_reset),
        };
        self.count += frames as u64;
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulated device: audio at `rate` samples per video second, video at
    /// `fps`, records of 1024 samples placed after every picture captured
    /// before their last sample. Returns (frames, after_pts, true end time).
    fn device(rate: f64, fps: f64, seconds: f64, t0: f64) -> Vec<(usize, u64, f64)> {
        let mut out = Vec::new();
        let frame_us = 1e6 / fps;
        let mut k = 0u64;
        loop {
            let t_end = t0 + (k + 1) as f64 * 1024.0 * 1e6 / rate;
            if t_end > t0 + seconds * 1e6 {
                break;
            }
            // Last picture at or before t_end, on a grid that starts at t0.
            let n = ((t_end - t0) / frame_us).floor();
            out.push((1024, (t0 + n * frame_us).round() as u64, t_end));
            k += 1;
        }
        out
    }

    #[test]
    fn recovers_a_known_skew() {
        for fps in [4.0, 7.5, 15.0] {
            let mut ts = Timestamper::new(16_000);
            let recs = device(16_013.0, fps, 120.0, 5e6);
            let mut worst = 0.0f64;
            for (i, (frames, pts, t_end)) in recs.iter().enumerate() {
                let s = ts.push(*frames, Some(*pts));
                let tl = s.timeline.unwrap();
                if i > recs.len() / 4 {
                    let err = tl.at((s.first_sample + 1024) as f64) - t_end;
                    worst = worst.max(err.abs());
                }
            }
            let rate = ts.timeline().unwrap().rate();
            assert!((rate - 16_013.0).abs() < 1.0, "fps {fps}: rate {rate}");
            println!("fps {fps}: rate {rate:.2}, worst {worst:.0} µs");
            assert!(worst < 5_000.0, "fps {fps}: worst {worst} µs");
            assert_eq!(ts.stats().resets, 0);
        }
    }

    #[test]
    fn gap_resets_into_a_new_epoch() {
        let mut ts = Timestamper::new(16_000);
        let recs = device(16_000.0, 10.0, 30.0, 0.0);
        for (f, p, _) in &recs[..200] {
            ts.push(*f, Some(*p));
        }
        // 3 s of audio lost: the next records' pictures are 3 s later than
        // their sample count says.
        let mut epochs = Vec::new();
        for (f, p, _) in &recs[247..] {
            epochs.push(ts.push(*f, Some(*p)));
        }
        let first_new = epochs.iter().position(|s| s.reset).unwrap();
        assert_eq!(first_new, OUTLIERS_TO_RESET as usize - 1);
        assert_eq!(epochs[first_new].epoch, 1);
        assert_eq!(epochs[first_new].first_sample, 0);
        assert_eq!(ts.stats().resets, 1);
        let s = epochs.last().unwrap();
        let tl = s.timeline.unwrap();
        let (_, _, t_end) = recs.last().unwrap();
        assert!((tl.at((s.first_sample + 1024) as f64) - t_end).abs() < 10_000.0);
    }

    #[test]
    fn no_video_means_no_timeline() {
        let mut ts = Timestamper::new(16_000);
        let s = ts.push(1024, None);
        assert!(s.timeline.is_none() && s.reset);
        let s = ts.push(1024, Some(1_000_000));
        assert_eq!(s.first_sample, 1024);
        assert!(!s.reset);
        assert!(s.timeline.is_some());
    }
}
