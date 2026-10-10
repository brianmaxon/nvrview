//! Session configuration and reconnect backoff.

use std::fmt;
use std::time::Duration;

/// Reconnect backoff: exponential, capped, with downward jitter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackoffConfig {
    /// Delay before the first retry.
    pub initial: Duration,
    /// Largest delay (before jitter, which only shortens it).
    pub max: Duration,
    /// Fraction (0.0 to 1.0) of the delay that may be randomly removed, so
    /// many clients do not retry in lockstep.
    pub jitter: f64,
    /// A connection must stay logged in this long before the backoff resets;
    /// a device that accepts a login and then drops us at once keeps backing
    /// off instead of being hammered.
    pub stable_after: Duration,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        BackoffConfig {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
            jitter: 0.5,
            stable_after: Duration::from_secs(30),
        }
    }
}

impl BackoffConfig {
    /// Delay before retry number `attempt` (0 for the first retry).
    /// `unit` is a random number in `[0, 1)`; it shortens the delay by up to
    /// `jitter` of its length. Never exceeds `max`.
    pub fn delay(&self, attempt: u32, unit: f64) -> Duration {
        let factor = 1u32.checked_shl(attempt.min(30)).unwrap_or(u32::MAX);
        let base = self.initial.saturating_mul(factor).min(self.max);
        let jitter = self.jitter.clamp(0.0, 1.0);
        let keep = 1.0 - jitter * unit.clamp(0.0, 1.0);
        base.mul_f64(keep)
    }
}

/// Settings for one [`crate::DeviceSession`].
#[derive(Clone)]
pub struct SessionConfig {
    /// Login user name.
    pub user: String,
    /// Login password. Never logged.
    pub password: String,
    /// Interval between keepalives (command 93). The official client was seen
    /// sending one every 30 to 140 s; the default is 60 s.
    pub keepalive_interval: Duration,
    /// How long to wait for the TCP connection.
    pub connect_timeout: Duration,
    /// How long handshake plus login may take in total.
    pub login_timeout: Duration,
    /// How long one write may block before the connection is declared dead.
    pub write_timeout: Duration,
    /// Reconnect backoff.
    pub backoff: BackoffConfig,
    /// Capacity, in frames, of each subscription's queue.
    pub queue_frames: usize,
}

impl SessionConfig {
    /// Defaults for the given credentials.
    pub fn new(user: impl Into<String>, password: impl Into<String>) -> SessionConfig {
        SessionConfig {
            user: user.into(),
            password: password.into(),
            keepalive_interval: Duration::from_secs(60),
            connect_timeout: Duration::from_secs(10),
            login_timeout: Duration::from_secs(15),
            write_timeout: Duration::from_secs(10),
            backoff: BackoffConfig::default(),
            queue_frames: 64,
        }
    }
}

impl fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionConfig")
            .field("keepalive_interval", &self.keepalive_interval)
            .field("backoff", &self.backoff)
            .field("queue_frames", &self.queue_frames)
            .finish_non_exhaustive()
    }
}

/// Tiny xorshift generator for jitter; not for anything security related.
pub(crate) struct Jitter(u64);

impl Jitter {
    pub(crate) fn new() -> Jitter {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15);
        let local = 0u8;
        Jitter((t ^ (&local as *const u8 as u64).rotate_left(32)) | 1)
    }

    /// Next value in `[0, 1)`.
    pub(crate) fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_caps_and_jitters_down() {
        let c = BackoffConfig {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(10),
            jitter: 0.5,
            stable_after: Duration::from_secs(30),
        };
        assert_eq!(c.delay(0, 0.0), Duration::from_secs(1));
        assert_eq!(c.delay(1, 0.0), Duration::from_secs(2));
        assert_eq!(c.delay(3, 0.0), Duration::from_secs(8));
        assert_eq!(c.delay(4, 0.0), Duration::from_secs(10));
        assert_eq!(c.delay(1000, 0.0), Duration::from_secs(10));
        let d = c.delay(3, 0.999_999);
        assert!(d > Duration::from_secs(4) && d < Duration::from_millis(4001));
        for a in 0..40 {
            let d = c.delay(a, 0.7);
            assert!(d <= c.max && d >= Duration::from_millis(500));
        }
    }

    #[test]
    fn jitter_in_range() {
        let mut j = Jitter::new();
        for _ in 0..1000 {
            let u = j.unit();
            assert!((0.0..1.0).contains(&u));
        }
    }

    #[test]
    fn debug_hides_credentials() {
        let c = SessionConfig::new("example-user", "example-password");
        let d = format!("{c:?}");
        assert!(!d.contains("example-user") && !d.contains("example-password"));
    }
}
