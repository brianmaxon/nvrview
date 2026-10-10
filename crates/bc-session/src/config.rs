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
    /// Send a connection test (command 93) after this long with nothing
    /// received, and at most once per this interval. Any received bytes,
    /// media included, count as life, so a session carrying video sends
    /// none. Default 6 s, as the official client (docs/protocol.md).
    pub probe_after_idle: Duration,
    /// Declare the connection dead and reconnect after this long with
    /// nothing received (not within `probe_after_idle` of logging in).
    /// Default 10 s, as the official client on direct connections.
    pub dead_after: Duration,
    /// How long a graceful close waits for the logout reply (command 2).
    pub logout_timeout: Duration,
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
    /// How long [`crate::DeviceSession::query`] waits for its reply,
    /// including time spent waiting for the session to log in. Also the
    /// limit for each step of a recording search and for a cover image.
    pub query_timeout: Duration,
    /// Recording searches and cover requests running at once on this device
    /// (each holds its search open across several requests). Default 1:
    /// searches are serialized.
    pub max_searches: usize,
    /// Downloads (143) running at once on this device. Default 1: the
    /// device's limit is not known, and live view keeps its share.
    pub max_downloads: usize,
    /// A download that delivers nothing for this long has stalled
    /// ([`crate::DownloadError::Stalled`]). Default 10 s.
    pub download_stall_timeout: Duration,
    /// Received download data a consumer may leave unread before the
    /// download is stopped as overflowed. Downloads are not paced (about
    /// 20 MB for a minute of a main stream arrived in 1.5 s), and the
    /// session cannot slow one down without stalling live view. Default
    /// 64 MiB.
    pub download_buffer_bytes: usize,
}

impl SessionConfig {
    /// Defaults for the given credentials.
    pub fn new(user: impl Into<String>, password: impl Into<String>) -> SessionConfig {
        SessionConfig {
            user: user.into(),
            password: password.into(),
            probe_after_idle: Duration::from_secs(6),
            dead_after: Duration::from_secs(10),
            logout_timeout: Duration::from_secs(2),
            connect_timeout: Duration::from_secs(10),
            login_timeout: Duration::from_secs(15),
            write_timeout: Duration::from_secs(10),
            backoff: BackoffConfig::default(),
            queue_frames: 64,
            query_timeout: Duration::from_secs(15),
            max_searches: 1,
            max_downloads: 1,
            download_stall_timeout: Duration::from_secs(10),
            download_buffer_bytes: 64 * 1024 * 1024,
        }
    }
}

impl fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SessionConfig")
            .field("probe_after_idle", &self.probe_after_idle)
            .field("dead_after", &self.dead_after)
            .field("backoff", &self.backoff)
            .field("queue_frames", &self.queue_frames)
            .field("max_downloads", &self.max_downloads)
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
