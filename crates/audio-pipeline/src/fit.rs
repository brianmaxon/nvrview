//! A smoothed line through noisy points, with a prior on its slope.
//!
//! Points are weighted by an exponential window over `x` (older points fade
//! with time constant `tau`), and the slope is pulled toward a prior
//! (ridge regression) so the first few seconds, when the points span too
//! little `x` to give a slope, still produce a usable line. The running
//! sums are centered (weighted means and co-moments, updated incrementally)
//! so hours of input lose no precision.

/// Exponentially weighted least-squares line `y = mean_y + slope * (x - mean_x)`.
#[derive(Clone, Debug)]
pub(crate) struct LineFit {
    tau: f64,
    prior_slope: f64,
    prior_strength: f64,
    weight: f64,
    mean_x: f64,
    mean_y: f64,
    cxx: f64,
    cxy: f64,
    last_x: Option<f64>,
    points: u64,
}

impl LineFit {
    /// `tau` is the window in `x` units; `prior_strength` is in `x²` units
    /// (the co-moment of `x` the data needs before it outweighs the prior).
    pub fn new(tau: f64, prior_slope: f64, prior_strength: f64) -> LineFit {
        LineFit {
            tau,
            prior_slope,
            prior_strength,
            weight: 0.0,
            mean_x: 0.0,
            mean_y: 0.0,
            cxx: 0.0,
            cxy: 0.0,
            last_x: None,
            points: 0,
        }
    }

    /// Adds one point.
    pub fn add(&mut self, x: f64, y: f64) {
        let decay = match self.last_x {
            Some(lx) => (-((x - lx).max(0.0)) / self.tau).exp(),
            None => 0.0,
        };
        self.last_x = Some(x);
        self.points += 1;
        self.weight = self.weight * decay + 1.0;
        self.cxx *= decay;
        self.cxy *= decay;
        let dx = x - self.mean_x;
        self.mean_x += dx / self.weight;
        self.mean_y += (y - self.mean_y) / self.weight;
        self.cxx += dx * (x - self.mean_x);
        self.cxy += dx * (y - self.mean_y);
    }

    /// The slope: data and prior combined.
    pub fn slope(&self) -> f64 {
        (self.cxy + self.prior_strength * self.prior_slope) / (self.cxx + self.prior_strength)
    }

    /// The line at `x`, or `None` before the first point.
    pub fn at(&self, x: f64) -> Option<f64> {
        (self.points > 0).then(|| self.mean_y + self.slope() * (x - self.mean_x))
    }
}

/// A one-sided envelope of residuals that leaks back toward the inside over
/// time, so a single extreme point or a slow change does not hold it forever.
#[derive(Clone, Debug)]
pub(crate) struct Envelope {
    upper: bool,
    leak_per_x: f64,
    value: Option<f64>,
    last_x: f64,
}

impl Envelope {
    /// Tracks the maximum (`upper`) or minimum of the values, relaxing by
    /// `leak_per_x` per unit of `x`.
    pub fn new(upper: bool, leak_per_x: f64) -> Envelope {
        Envelope {
            upper,
            leak_per_x,
            value: None,
            last_x: 0.0,
        }
    }

    /// Adds value `v` at position `x`.
    pub fn add(&mut self, x: f64, v: f64) {
        let leak = self.leak_per_x * (x - self.last_x).max(0.0);
        self.last_x = x;
        self.value = Some(match self.value {
            None => v,
            Some(e) if self.upper => (e - leak).max(v),
            Some(e) => (e + leak).min(v),
        });
    }

    /// The current envelope.
    pub fn value(&self) -> Option<f64> {
        self.value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_slope_and_intercept() {
        let mut f = LineFit::new(1e6, 1.0, 1.0);
        let mut seed = 1u64;
        for i in 0..10_000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let noise = ((seed >> 33) as f64 / (1u64 << 31) as f64 - 0.5) * 10.0;
            let x = 1e9 + f64::from(i) * 100.0;
            f.add(x, 3.0 + 0.5 * x + noise);
        }
        assert!((f.slope() - 0.5).abs() < 1e-5, "{}", f.slope());
        let y = f.at(1e9 + 5e5).unwrap();
        assert!((y - (3.0 + 0.5 * (1e9 + 5e5))).abs() < 0.5, "{y}");
    }

    #[test]
    fn prior_holds_the_slope_with_little_data() {
        let mut f = LineFit::new(1e6, 2.0, 1e6);
        f.add(0.0, 0.0);
        f.add(1.0, 5.0);
        assert!((f.slope() - 2.0).abs() < 1e-3);
    }

    #[test]
    fn envelope_leaks() {
        let mut e = Envelope::new(true, 0.1);
        e.add(0.0, 10.0);
        e.add(10.0, 0.0);
        assert_eq!(e.value(), Some(9.0));
        let mut e = Envelope::new(false, 0.1);
        e.add(0.0, -10.0);
        e.add(10.0, 0.0);
        assert_eq!(e.value(), Some(-9.0));
    }
}
