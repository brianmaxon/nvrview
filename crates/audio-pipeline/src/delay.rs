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
/// Window of the lateness percentile, seconds of arrivals (600 per-second
/// maxima, so the percentile skips the window's six worst seconds). Long
/// enough to remember a rough patch of a few seconds that recurs every ten
/// minutes or so (a replayed LAN recording had one; a 300 s window forgot
/// it between occurrences and underran there every pass), and that a stall
/// every five minutes stays above the percentile; short enough to follow a
/// change in the network within minutes.
pub const DELAY_WINDOW_S: f64 = 600.0;
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
/// Extra delay right after a (re)start, while the presentation estimate
/// (a median over its first minutes of deliveries) still moves: its early
/// errors arrive as lateness the window has not seen. Measured on a replayed
/// LAN recording: without it, most underruns fell in the first two minutes
/// after each restart.
pub const WARMUP_MARGIN_US: f64 = 50_000.0;
/// The warm-up allowance falls to nothing over this many seconds of
/// arrivals.
pub const WARMUP_MARGIN_S: f64 = 120.0;
/// Seconds of observations needed before the first target. (The target is
/// recomputed once per second of arrivals.)
const MIN_SECONDS: usize = 10;

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
    /// (host second, latest lateness in it), µs, over the last
    /// [`DELAY_WINDOW_S`] seconds of arrivals. Lateness is measured against
    /// the presentation map in use when the record arrived, and kept across
    /// stream restarts: the network has not changed, and forgetting it
    /// would make the first minutes after every reconnect underrun.
    seconds: VecDeque<(i64, f64)>,
    last_update: Option<i64>,
    scratch: Vec<f64>,
    /// Host time of the first lateness measured since the last restart.
    since_restart: Option<f64>,
}

impl DelayControl {
    /// `fixed`: always this delay (µs); `None` adapts.
    pub fn new(fixed: Option<f64>) -> DelayControl {
        DelayControl {
            fixed,
            current: fixed.unwrap_or(INITIAL_DELAY_US),
            target: None,
            seconds: VecDeque::new(),
            last_update: None,
            scratch: Vec::new(),
            since_restart: None,
        }
    }

    /// The stream (re)started: playout is placed afresh with the next block,
    /// so the delay may step up at once by the warm-up allowance, which then
    /// fades out.
    pub fn restart(&mut self) {
        if self.fixed.is_some() {
            return;
        }
        self.since_restart = None;
        let base = self.target.unwrap_or(self.current);
        self.current = (base + WARMUP_MARGIN_US)
            .max(self.current)
            .clamp(MIN_DELAY_US, MAX_DELAY_US);
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
    /// at host time `arrived_us`; `record_us` is its length. `map` is the
    /// presentation map, once it is settled enough to measure lateness
    /// against (`None` records nothing). Returns the delay for the block the
    /// record carries, `dt_s` (its length in seconds) after the previous one.
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
        if let Some(map) = map {
            self.since_restart.get_or_insert(arrived_us);
            let late = arrived_us - map.host_at(sent_us);
            let sec = (arrived_us / 1e6).floor() as i64;
            match self.seconds.back_mut() {
                Some((s, m)) if *s == sec => *m = m.max(late),
                _ => self.seconds.push_back((sec, late)),
            }
            while self
                .seconds
                .front()
                .is_some_and(|&(s, _)| s <= sec - DELAY_WINDOW_S as i64)
            {
                self.seconds.pop_front();
            }
            if self.seconds.len() >= MIN_SECONDS && self.last_update.is_none_or(|t| sec != t) {
                self.last_update = Some(sec);
                self.scratch.clear();
                self.scratch.extend(self.seconds.iter().map(|&(_, m)| m));
                let k = ((self.scratch.len() - 1) as f64 * DELAY_PERCENTILE).round() as usize;
                let (_, late, _) = self.scratch.select_nth_unstable_by(k, f64::total_cmp);
                let want = *late + record_us.max(0.0) + OUTPUT_LEAD_US;
                self.target = Some(want.clamp(MIN_DELAY_US, MAX_DELAY_US));
            }
        }
        let warmup = match self.since_restart {
            Some(t0) => {
                WARMUP_MARGIN_US
                    * (1.0 - (arrived_us - t0) / (WARMUP_MARGIN_S * 1e6)).clamp(0.0, 1.0)
            }
            None => WARMUP_MARGIN_US,
        };
        let rate = match self.target {
            Some(t) => {
                let t = (t + warmup).clamp(MIN_DELAY_US, MAX_DELAY_US);
                ((t - self.current) / (SETTLE_TAU_S * 1e6)).clamp(-MAX_SLEW, MAX_SLEW)
            }
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
    fn feed(
        d: &mut DelayControl,
        next: &mut u64,
        seconds: f64,
        mut late: impl FnMut(u64) -> f64,
    ) -> Vec<Delay> {
        let m = map();
        let n = (seconds / 0.064) as u64;
        let first = *next;
        *next += n;
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
        let mut k0 = 0;
        // Lateness uniform in 0..100 ms: p99 = 99 ms, target 64 + 99 + 20.
        let out = feed(&mut d, &mut k0, 600.0, |k| (k * 37 % 100) as f64 * 1000.0);
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
        feed(&mut d, &mut k0, 900.0, |_| 0.0);
        assert_eq!(d.target(), Some(MIN_DELAY_US));
        assert!((d.current() - MIN_DELAY_US).abs() < 1_000.0);
        feed(&mut d, &mut k0, 900.0, |_| 2e6);
        assert_eq!(d.target(), Some(MAX_DELAY_US));
        assert!((d.current() - MAX_DELAY_US).abs() < 1_000.0);
    }

    #[test]
    fn rare_stalls_do_not_move_the_target() {
        let mut d = DelayControl::new(None);
        let mut k0 = 0;
        // 20 ms of lateness, plus a 1.5 s stall every 5 minutes (its 23
        // records arrive together at its end).
        feed(&mut d, &mut k0, 1200.0, |k| {
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
        let mut k0 = 0;
        let out = feed(&mut d, &mut k0, 60.0, |_| 300_000.0);
        assert!(out.iter().all(|x| x.us == 280_000.0 && x.rate == 0.0));
        assert_eq!(d.target(), Some(280_000.0));
    }
}
