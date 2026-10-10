//! When the video picture with a given camera time is on screen.
//!
//! Playout aims every audio sample at the moment the picture with the same
//! camera time is shown, plus a lip-sync allowance. The caller can supply
//! that mapping ([`PresentationMap`]) from what it knows about its video
//! path, or let [`PresentationEstimator`] derive it from arrival times.

use crate::fit::LineFit;
use std::collections::VecDeque;

/// Affine map from camera time (video timestamps, µs) to host time (µs, on
/// the clock the render callback's host times use).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresentationMap {
    /// A camera time...
    pub camera_us: f64,
    /// ...and the host time its picture is shown.
    pub host_us: f64,
    /// Host microseconds per camera microsecond (1 plus the clocks' skew).
    pub host_per_camera: f64,
}

impl PresentationMap {
    /// Host time at which camera time `camera_us` is on screen.
    pub fn host_at(&self, camera_us: f64) -> f64 {
        self.host_us + (camera_us - self.camera_us) * self.host_per_camera
    }
}

/// Window of the slope fit, camera seconds. Long, because the slope is a
/// ratio of two crystal clocks (a few ppm from 1) and delivery jitter is
/// tens of milliseconds, correlated over seconds: a short window would turn
/// jitter into a slope error, and the slope feeds the playout rate.
const WINDOW_S: f64 = 1800.0;
/// Weight of the slope prior (1.0), in seconds of data at about 16 points
/// per second.
const PRIOR_S: f64 = 600.0;
/// The slope used never leaves 1 ± this.
const MAX_SKEW: f64 = 200e-6;
/// Window of the delay percentile, camera seconds.
const DELAY_WINDOW_S: f64 = 120.0;
/// The delay reference: this fraction of deliveries in the window were
/// faster.
pub const DELAY_PERCENTILE: f64 = 0.5;

/// Estimates a [`PresentationMap`] from (camera time, arrival time) pairs:
/// the camera time at which the device sent something and the host time it
/// arrived. Pictures are assumed to be shown as soon as they arrive (the
/// renderer keeps only the newest), so delivery delay is presentation delay;
/// decode and display time come on top and belong in the lip-sync
/// allowance.
///
/// Delivery delay jitters a lot (tens of milliseconds of standard deviation
/// with long tails were measured on a wired LAN), and stalls add seconds to
/// some deliveries. The map therefore uses a *low percentile* of `arrival -
/// camera time` ([`DELAY_PERCENTILE`] over the last [`DELAY_WINDOW_S`]
/// seconds): stable under jitter, untouched by stalls and the bursts after
/// them (which only add delay), so latency cannot ratchet upward. Its slope
/// (the host-to-camera clock ratio) comes from a least-squares fit.
///
/// [`TileAudio`](crate::TileAudio) feeds it each audio record's send time,
/// which its timeline gives.
#[derive(Clone, Debug)]
pub struct PresentationEstimator {
    origin: Option<(f64, f64)>,
    fit: LineFit,
    window: VecDeque<(f64, f64)>,
    scratch: Vec<f64>,
    offset: Option<f64>,
    last_camera: Option<f64>,
}

impl Default for PresentationEstimator {
    fn default() -> Self {
        PresentationEstimator::new()
    }
}

impl PresentationEstimator {
    /// An empty estimator.
    pub fn new() -> PresentationEstimator {
        let span = PRIOR_S * 1e6;
        PresentationEstimator {
            origin: None,
            fit: LineFit::new(WINDOW_S * 1e6, 1.0, PRIOR_S * 16.0 * span * span / 12.0),
            window: VecDeque::new(),
            scratch: Vec::new(),
            offset: None,
            last_camera: None,
        }
    }

    /// Forgets everything (the stream restarted).
    pub fn reset(&mut self) {
        *self = PresentationEstimator::new();
    }

    /// Something sent at camera time `camera_us` arrived at `host_us`. Camera
    /// time going backwards by more than a second starts over.
    pub fn observe(&mut self, camera_us: f64, host_us: f64) {
        if self.last_camera.is_some_and(|l| camera_us < l - 1e6) {
            self.reset();
        }
        self.last_camera = Some(camera_us);
        let (c0, h0) = *self.origin.get_or_insert((camera_us, host_us));
        let (x, y) = (camera_us - c0, host_us - h0);
        self.fit.add(x, y);
        self.window.push_back((x, y));
        while self
            .window
            .front()
            .is_some_and(|&(fx, _)| fx < x - DELAY_WINDOW_S * 1e6)
        {
            self.window.pop_front();
        }
        self.scratch.clear();
        for &(wx, wy) in &self.window {
            self.scratch.push(wy - self.fit.at(wx).unwrap_or(wy));
        }
        let k = ((self.scratch.len() - 1) as f64 * DELAY_PERCENTILE) as usize;
        let (_, v, _) = self.scratch.select_nth_unstable_by(k, f64::total_cmp);
        self.offset = Some(*v);
    }

    /// The current map, once anything has been observed.
    pub fn map(&self) -> Option<PresentationMap> {
        let (c0, h0) = self.origin?;
        Some(PresentationMap {
            camera_us: c0,
            host_us: h0 + self.fit.at(0.0)? + self.offset?,
            host_per_camera: self.fit.slope().clamp(1.0 - MAX_SKEW, 1.0 + MAX_SKEW),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_median_delivery_and_ignores_stalls() {
        let mut e = PresentationEstimator::new();
        let mut seed = 7u64;
        let skew = 1.0 + 40e-6;
        for i in 0..(7 * 600) {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let jitter = (seed >> 40) as f64 / (1u64 << 24) as f64 * 30_000.0;
            let pts = 1_000_000.0 + i as f64 * 142_857.0;
            let mut host = 5e9 + pts * skew + 20_000.0 + jitter;
            // A 2 s stall every minute: everything in it arrives late.
            if i % 420 < 14 {
                host += 2e6 - (i % 420) as f64 * 142_857.0;
            }
            e.observe(pts, host);
        }
        let m = e.map().unwrap();
        let pts = 1_000_000.0 + 4199.0 * 142_857.0;
        let truth = 5e9 + pts * skew + 20_000.0;
        let err = m.host_at(pts) - truth;
        assert!((12_000.0..18_000.0).contains(&err), "error {err} µs");
        assert!((m.host_per_camera - 1.0).abs() < 50e-6);
    }
}
