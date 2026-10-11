//! The shared playback clock: a position in device-local milliseconds that
//! advances at the rate on the core's clock while it runs.

/// The position of a playback (docs/FFI-RECORDINGS.md, section 6, "The
/// shared clock").
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PlayClock {
    /// Position at `base_ns`, device-local ms.
    base_ms: i64,
    /// Core clock time of `base_ms`, ns.
    base_ns: u64,
    rate: f64,
    running: bool,
}

impl PlayClock {
    /// A clock held at `position_ms`, rate 1.
    pub(crate) fn new(position_ms: i64) -> PlayClock {
        PlayClock {
            base_ms: position_ms,
            base_ns: 0,
            rate: 1.0,
            running: false,
        }
    }

    /// The position at `now_ns`.
    pub(crate) fn position(&self, now_ns: u64) -> i64 {
        if !self.running {
            return self.base_ms;
        }
        let elapsed_ms = now_ns.saturating_sub(self.base_ns) as f64 / 1e6;
        self.base_ms + (elapsed_ms * self.rate).floor() as i64
    }

    /// Whether it advances.
    pub(crate) fn running(&self) -> bool {
        self.running
    }

    /// The rate.
    pub(crate) fn rate(&self) -> f64 {
        self.rate
    }

    /// Starts advancing from where it is.
    pub(crate) fn run(&mut self, now_ns: u64) {
        if !self.running {
            self.base_ns = now_ns;
            self.running = true;
        }
    }

    /// Stops where it is.
    pub(crate) fn hold(&mut self, now_ns: u64) {
        if self.running {
            self.base_ms = self.position(now_ns);
            self.running = false;
        }
    }

    /// Moves to `position_ms` (running or not).
    pub(crate) fn set(&mut self, position_ms: i64, now_ns: u64) {
        self.base_ms = position_ms;
        self.base_ns = now_ns;
    }

    /// Changes the rate from now on.
    pub(crate) fn set_rate(&mut self, rate: f64, now_ns: u64) {
        let p = self.position(now_ns);
        self.set(p, now_ns);
        self.rate = rate;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn runs_holds_and_changes_rate() {
        let mut c = PlayClock::new(10_000);
        assert_eq!(c.position(5_000 * MS), 10_000);
        c.run(1_000 * MS);
        assert_eq!(c.position(1_500 * MS), 10_500);
        c.set_rate(4.0, 2_000 * MS);
        assert_eq!(c.position(2_000 * MS), 11_000);
        assert_eq!(c.position(2_250 * MS), 12_000);
        c.hold(2_500 * MS);
        assert!(!c.running());
        assert_eq!(c.position(9_000 * MS), 13_000);
        c.set(5_000, 9_000 * MS);
        c.set_rate(0.25, 9_000 * MS);
        c.run(9_000 * MS);
        assert_eq!(c.position(13_000 * MS), 6_000);
        assert!((c.rate() - 0.25).abs() < f64::EPSILON);
    }
}
