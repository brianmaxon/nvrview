//! The adaptive playout delay: how long after its picture is shown each
//! sample is heard.
//!
//! Video is never delayed (docs/AUDIO.md), so audio trails the picture by
//! this delay, and it should be as short as the network allows. It must
//! cover, for nearly every record,
//!
//! ```text
//! record length + (arrival - presentation of its send time) + output lead
//! ```
//!
//! because a record is sent only once its last sample is captured, it can
//! arrive late against the presentation reference (the median delivery when
//! the map is estimated), and the renderer reads the ring a little before
//! the host time it is heard at. A record that misses this is an underrun
//! (its audio is dropped; timing is kept).
//!
//! So the target covers the latest record of each second of the recent past
//! ([`DELAY_WINDOW_S`]) in all but a few of those seconds (a percentile,
//! [`DELAY_PERCENTILE`], of the per-second maxima of lateness), plus the
//! record length and the output lead ([`OUTPUT_LEAD_US`]), within
//! [`MIN_DELAY_US`]..[`MAX_DELAY_US`]. Counting seconds rather than records
//! bounds how *often* an underrun can happen (at most about one second in
//! a hundred has one), and makes a stall, whose burst of late records
//! fills only a second or two, count as that, so rare stalls do not raise
//! the delay. (A plain 99th percentile of records was measured in the
//! simulation to underrun every few seconds under LAN-like jitter, because
//! late records come one or two at a time, spread over many seconds.)
//! The delay in use follows the target slowly (at most [`MAX_SLEW`], a
//! 0.2% change of playback speed), and playout feeds that rate forward into
//! its resampling ratio, so a change is a tiny, inaudible tempo change: no
//! jump, no gap.

use crate::presentation::PresentationMap;
use std::collections::VecDeque;

/// The fraction of seconds in which every record should arrive in time. A
/// larger value costs latency for every sample to save a few underruns;
/// 99% allows about one underrun per 100 s at most, and stalls of a second
/// or two every few minutes (under 1% of the window's seconds) stay above
/// it.
pub const DELAY_PERCENTILE: f64 = 0.99;
/// Window of the lateness percentile, camera seconds (300 per-second
/// maxima). Long enough that one stall in it stays above the percentile,
/// short enough to follow a change in the network within minutes.
pub const DELAY_WINDOW_S: f64 = 300.0;
/// How far ahead of the host time it is heard the renderer reads a frame:
/// the output's latency plus up to one callback. Core Audio and WASAPI
/// typically run 10 ms callbacks with a few ms of device latency.
pub const OUTPUT_LEAD_US: f64 = 20_000.0;
/// Lower bound: one AAC record (64 ms) plus the output lead plus a little
/// slack for scheduling on the host. Below this even a perfect network
/// delivers every record too late to be heard on time.
pub const MIN_DELAY_US: f64 = 100_000.0;
/// Upper bound. Audio that trails the picture by more than half a second
/// stops feeling like the picture's sound, and a network that needs more
/// is stalling, which resyncs handle better than waiting. It also keeps the
/// delay well inside the output ring (2 s by default).
pub const MAX_DELAY_US: f64 = 500_000.0;
/// The delay in use until the first target: the measured LAN case needs
/// about this much, so a typical stream starts without underruns or a
/// long adjustment.
pub const INITIAL_DELAY_US: f64 = 250_000.0;
/// Fastest change of the delay, seconds per second. Playback speed changes
/// by the same fraction (0.2% is 3.5 cents of pitch, inaudible), leaving
/// most of the resampler's ±0.5% for clock skew and the sync controller.
pub const MAX_SLEW: f64 = 0.002;
/// Near the target the delay approaches it with this time constant, so it
/// settles without overshooting.
const SETTLE_TAU_S: f64 = 10.0;
/// The target is recomputed this often, camera seconds.
const UPDATE_S: f64 = 1.0;
/// Observations needed before the first target (about 10 s of records).
const MIN_OBSERVATIONS: usize = 150;

/// The delay a block is played with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Delay {
    /// Playout delay, µs.
    pub us: f64,
    /// Its rate of change (µs per µs of host time); playout slows down
    /// (positive) or speeds up by this fraction to follow it.
    pub rate: f64,
}

/// Adaptive (or fixed) playout delay of one tile.
#[derive(Clone, Debug)]
pub(crate) struct DelayControl {
    fixed: Option<f64>,
    current: f64,
    target: Option<f64>,
    /// (camera time sent, host time arrived), µs.
    window: VecDeque<(f64, f64)>,
    last_update: Option<f64>,
    scratch: Vec<f64>,
}

impl DelayControl {
    /// `fixed`: always this delay (µs); `None` adapts.
    pub fn new(fixed: Option<f64>) -> DelayControl {
        DelayControl {
            fixed,
            current: fixed.unwrap_or(INITIAL_DELAY_US),
            target: None,
            window: VecDeque::new(),
            last_update: None,
            scratch: Vec::new(),
        }
    }

    /// Forgets the observations (the stream restarted or its timeline was
    /// rough), keeping the delay in use: the network is still the same.
    pub fn reset(&mut self) {
        self.window.clear();
        self.last_update = None;
    }

    /// The delay in use, µs.
    pub fn current(&self) -> f64 {
        self.current
    }

    /// The delay it is moving to, once known.
    pub fn target(&self) -> Option<f64> {
        if self.fixed.is_some() {
            return self.fixed;
        }
        self.target
    }

    /// A record whose last sample was sent at camera time `sent_us` arrived
    /// at host time `arrived_us`; `record_us` is its length. Returns the
    /// delay for the block it carries, `dt_s` (its length in seconds) after
    /// the previous one.
    pub fn observe(
        &mut self,
        sent_us: f64,
        arrived_us: f64,
        record_us: f64,
        map: Option<&PresentationMap>,
        dt_s: f64,
    ) -> Delay {
        if self.fixed.is_some() {
            return Delay {
                us: self.current,
                rate: 0.0,
            };
        }
        self.window.push_back((sent_us, arrived_us));
        while self
            .window
            .front()
            .is_some_and(|&(s, _)| s < sent_us - DELAY_WINDOW_S * 1e6)
        {
            self.window.pop_front();
        }
        if let Some(map) = map
            && self.window.len() >= MIN_OBSERVATIONS
            && self
                .last_update
                .is_none_or(|t| sent_us - t >= UPDATE_S * 1e6 || sent_us < t)
        {
            self.last_update = Some(sent_us);
            // The latest arrival (against its target) of each second.
            self.scratch.clear();
            let mut second = None;
            for &(s, a) in &self.window {
                let late = a - map.host_at(s);
                let sec = (s / 1e6).floor();
                match self.scratch.last_mut() {
                    Some(m) if second == Some(sec) => *m = m.max(late),
                    _ => {
                        self.scratch.push(late);
                        second = Some(sec);
                    }
                }
            }
            let k = ((self.scratch.len() - 1) as f64 * DELAY_PERCENTILE).round() as usize;
            let (_, late, _) = self.scratch.select_nth_unstable_by(k, f64::total_cmp);
            let want = *late + record_us.max(0.0) + OUTPUT_LEAD_US;
            self.target = Some(want.clamp(MIN_DELAY_US, MAX_DELAY_US));
        }
        let rate = match self.target {
            Some(t) => ((t - self.current) / (SETTLE_TAU_S * 1e6)).clamp(-MAX_SLEW, MAX_SLEW),
            None => 0.0,
        };
        let dt = dt_s.clamp(0.0, 1.0);
        let us = self.current;
        self.current = (self.current + rate * dt * 1e6).clamp(MIN_DELAY_US, MAX_DELAY_US);
        Delay { us, rate }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> PresentationMap {
        PresentationMap {
            camera_us: 0.0,
            host_us: 1e9,
            host_per_camera: 1.0,
        }
    }

    /// Feeds `seconds` of 64 ms records whose lateness is `late(k)`,
    /// continuing from the last record fed.
    fn feed(d: &mut DelayControl, seconds: f64, mut late: impl FnMut(u64) -> f64) -> Vec<Delay> {
        let m = map();
        let n = (seconds / 0.064) as u64;
        let first = d
            .window
            .back()
            .map_or(0, |&(s, _)| (s / 64_000.0) as u64 + 1);
        (first..first + n)
            .map(|k| {
                let sent = k as f64 * 64_000.0;
                d.observe(sent, m.host_at(sent) + late(k), 64_000.0, Some(&m), 0.064)
            })
            .collect()
    }

    #[test]
    fn follows_the_lateness_percentile_slowly_and_within_bounds() {
        let mut d = DelayControl::new(None);
        // Lateness uniform in 0..100 ms: p99 = 99 ms, target 64 + 99 + 20.
        let out = feed(&mut d, 600.0, |k| (k * 37 % 100) as f64 * 1000.0);
        let t = d.target().unwrap();
        assert!((t - 183_000.0).abs() < 2_000.0, "target {t}");
        assert!((d.current() - t).abs() < 1_000.0);
        for w in out.windows(2) {
            let step = (w[1].us - w[0].us).abs();
            assert!(step <= MAX_SLEW * 64_000.0 + 1e-6, "step {step}");
            assert!(w[0].rate.abs() <= MAX_SLEW);
        }
        // A clean network shrinks it to the floor; a terrible one grows it
        // to the ceiling.
        feed(&mut d, 900.0, |_| 0.0);
        assert_eq!(d.target(), Some(MIN_DELAY_US));
        assert!((d.current() - MIN_DELAY_US).abs() < 1_000.0);
        feed(&mut d, 900.0, |_| 2e6);
        assert_eq!(d.target(), Some(MAX_DELAY_US));
        assert!((d.current() - MAX_DELAY_US).abs() < 1_000.0);
    }

    #[test]
    fn rare_stalls_do_not_move_the_target() {
        let mut d = DelayControl::new(None);
        // 20 ms of lateness, plus a 1.5 s stall every 5 minutes (its 23
        // records arrive together at its end).
        feed(&mut d, 1200.0, |k| {
            let in_cycle = k % 4688;
            if in_cycle < 23 {
                1.5e6 - in_cycle as f64 * 64_000.0
            } else {
                (k % 21) as f64 * 1000.0
            }
        });
        let t = d.target().unwrap();
        assert!(t < 110_000.0, "target {t}");
    }

    #[test]
    fn fixed_never_moves() {
        let mut d = DelayControl::new(Some(280_000.0));
        let out = feed(&mut d, 60.0, |_| 300_000.0);
        assert!(out.iter().all(|x| x.us == 280_000.0 && x.rate == 0.0));
        assert_eq!(d.target(), Some(280_000.0));
    }
}
