//! External triggers: the confirmation window around a sensor event and
//! per-source health counters.
//!
//! An external trigger never alerts on its own (unless its source is set to
//! `require: none`). It opens a window from `window_before_ms` before the
//! event to `window_after_ms` after it; a retrigger of the same source
//! while the window is open pushes the end out to `window_after_ms` after
//! the retrigger, but never past `window_max_ms` after the first event.
//! The k-of-n count over the mapped angles is [`crate::verify::Counter`].

use serde::{Deserialize, Serialize};

use crate::settings::SourceMapping;
use crate::time::Millis;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub first_ms: Millis,
    pub from_ms: Millis,
    pub until_ms: Millis,
}

impl Window {
    pub fn new(at_ms: Millis, m: &SourceMapping) -> Self {
        Window {
            first_ms: at_ms,
            from_ms: at_ms - m.window_before_ms.max(0),
            until_ms: at_ms + m.window_after_ms.max(0),
        }
    }

    pub fn is_open(&self, at_ms: Millis) -> bool {
        at_ms >= self.from_ms && at_ms <= self.until_ms
    }

    /// Extends the window for a retrigger at `at_ms`. Returns whether the
    /// end moved.
    pub fn extend(&mut self, at_ms: Millis, m: &SourceMapping) -> bool {
        let limit = self.first_ms + m.window_max_ms.max(m.window_after_ms);
        let end = (at_ms + m.window_after_ms).min(limit);
        if end > self.until_ms {
            self.until_ms = end;
            true
        } else {
            false
        }
    }
}

/// Counters per external source, for the "how often does this sensor fire
/// for nothing" view and for tuning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceHealth {
    pub fired: u64,
    pub retriggers: u64,
    pub confirmed: u64,
    pub nothing_seen: u64,
    pub could_not_look: u64,
    /// Alerts without confirmation (`require: none` or the fail policy).
    pub unconfirmed_alerts: u64,
    pub last_fired_ms: Option<Millis>,
}

impl SourceHealth {
    /// The share of checked events where nothing was seen.
    pub fn false_trigger_rate(&self) -> Option<f64> {
        let checked = self.confirmed + self.nothing_seen;
        (checked > 0).then(|| self.nothing_seen as f64 / checked as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_and_retriggers() {
        let m = SourceMapping::new("s", vec![]);
        let mut w = Window::new(10_000, &m);
        assert_eq!((w.from_ms, w.until_ms), (8_000, 18_000));
        assert!(w.is_open(8_000) && w.is_open(18_000) && !w.is_open(18_001));
        assert!(w.extend(15_000, &m));
        assert_eq!(w.until_ms, 23_000);
        // Never past 30 s after the first event.
        assert!(w.extend(35_000, &m));
        assert_eq!(w.until_ms, 40_000);
        assert!(!w.extend(39_000, &m));
        // An earlier retrigger never shortens it.
        assert!(!w.extend(11_000, &m));
    }

    #[test]
    fn health() {
        let mut h = SourceHealth::default();
        assert_eq!(h.false_trigger_rate(), None);
        h.confirmed = 1;
        h.nothing_seen = 3;
        assert_eq!(h.false_trigger_rate(), Some(0.75));
    }
}
