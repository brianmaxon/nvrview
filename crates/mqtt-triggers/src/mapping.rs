//! From an MQTT message to a trigger: size cap, rate limit, retained
//! handling, payload parsing, the transitions that count, and debounce. Pure
//! (the time is passed in), so every rule is tested without a broker.

use crate::config::{Mapping, Mode, RetainedPolicy, SourceConfig};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerState {
    Start,
    Stop,
    Pulse,
}

/// Why a message produced no trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// The topic does not match the subscription (a broker misbehaving).
    Topic,
    Oversized,
    RateLimited,
    Retained,
    /// Not a recognized on or off value.
    Unmapped,
    /// Same state as before.
    Repeat,
    /// A transition the settings say does not count.
    Transition,
    Debounced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Emit(TriggerState),
    /// Recorded the state (retained `Seed`) without a trigger.
    Seeded,
    Dropped(Reason),
}

/// Whether `topic` matches the filter `filter` (MQTT wildcard rules).
pub fn topic_matches(filter: &str, topic: &str) -> bool {
    let mut f = filter.split('/');
    let mut t = topic.split('/');
    loop {
        match (f.next(), t.next()) {
            (Some("#"), _) => return true,
            (Some("+"), Some(_)) => {}
            (Some(a), Some(b)) if a == b => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

struct Bucket {
    /// Tokens in units of 1/60000 message, so a per-minute rate refills by
    /// exactly `rate_per_min` per elapsed millisecond.
    tokens: u64,
    cap: u64,
    per_ms: u64,
    last_ms: u64,
}

const COST: u64 = 60_000;

impl Bucket {
    fn new(per_min: u32, burst: u32, now_ms: u64) -> Bucket {
        let cap = u64::from(burst.max(1)) * COST;
        Bucket {
            tokens: cap,
            cap,
            per_ms: u64::from(per_min),
            last_ms: now_ms,
        }
    }

    fn take(&mut self, now_ms: u64) -> bool {
        let dt = now_ms.saturating_sub(self.last_ms);
        self.last_ms = self.last_ms.max(now_ms);
        self.tokens = (self.tokens + dt.saturating_mul(self.per_ms)).min(self.cap);
        if self.tokens >= COST {
            self.tokens -= COST;
            true
        } else {
            false
        }
    }
}

pub struct Mapper {
    filter: String,
    max_payload: usize,
    map: Mapping,
    bucket: Bucket,
    /// The last on/off state seen (or seeded).
    state: Option<bool>,
    last_emit_ms: Option<u64>,
}

impl Mapper {
    pub fn new(cfg: &SourceConfig, now_ms: u64) -> Mapper {
        Mapper {
            filter: cfg.topic.clone(),
            max_payload: cfg.limits.max_payload_bytes,
            map: cfg.mapping.clone(),
            bucket: Bucket::new(cfg.limits.rate_per_min, cfg.limits.rate_burst, now_ms),
            state: None,
            last_emit_ms: None,
        }
    }

    pub fn on_message(
        &mut self,
        topic: &str,
        payload: &[u8],
        retain: bool,
        now_ms: u64,
    ) -> Outcome {
        if !topic_matches(&self.filter, topic) {
            return Outcome::Dropped(Reason::Topic);
        }
        if payload.len() > self.max_payload {
            return Outcome::Dropped(Reason::Oversized);
        }
        if !self.bucket.take(now_ms) {
            return Outcome::Dropped(Reason::RateLimited);
        }
        if retain && self.map.retained == RetainedPolicy::Ignore {
            return Outcome::Dropped(Reason::Retained);
        }
        let Some(on) = parse(&self.map, payload) else {
            return Outcome::Dropped(Reason::Unmapped);
        };
        if retain && self.map.retained == RetainedPolicy::Seed {
            self.state = Some(on);
            return Outcome::Seeded;
        }
        let previous = self.state;
        let state = match self.map.mode {
            Mode::Pulse => {
                if !on {
                    return Outcome::Dropped(Reason::Unmapped);
                }
                TriggerState::Pulse
            }
            Mode::Level => {
                if previous == Some(on) {
                    return Outcome::Dropped(Reason::Repeat);
                }
                match on {
                    true if self.map.start_on_on => TriggerState::Start,
                    // The first message being "off" has nothing to stop.
                    false if self.map.stop_on_off && previous.is_some() => TriggerState::Stop,
                    _ => {
                        self.state = Some(on);
                        return Outcome::Dropped(Reason::Transition);
                    }
                }
            }
        };
        if self.map.debounce_ms > 0
            && let Some(last) = self.last_emit_ms
            && now_ms.saturating_sub(last) < self.map.debounce_ms
        {
            // The state is left as it was: what was last reported stays
            // true until a later message changes it.
            return Outcome::Dropped(Reason::Debounced);
        }
        self.state = Some(on);
        self.last_emit_ms = Some(now_ms);
        Outcome::Emit(state)
    }
}

/// `Some(true)` for on, `Some(false)` for off, `None` when unrecognized.
fn parse(map: &Mapping, payload: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(payload).ok()?;
    let value: String = match &map.json_path {
        None => text.trim().to_string(),
        Some(path) => {
            let mut v: serde_json::Value = serde_json::from_str(text).ok()?;
            for key in path.split('.') {
                v = match v {
                    serde_json::Value::Object(mut o) => o.remove(key)?,
                    serde_json::Value::Array(mut a) => {
                        let i: usize = key.parse().ok()?;
                        if i >= a.len() {
                            return None;
                        }
                        a.swap_remove(i)
                    }
                    _ => return None,
                };
            }
            match v {
                serde_json::Value::String(s) => s.trim().to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => return None,
            }
        }
    };
    let is = |list: &[String]| list.iter().any(|v| v.trim().eq_ignore_ascii_case(&value));
    match (is(&map.on_values), is(&map.off_values)) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper(edit: impl FnOnce(&mut SourceConfig)) -> Mapper {
        let mut c = SourceConfig::new("s", "motion", "broker.example.net", 1883, "example/topic");
        c.mapping.debounce_ms = 0;
        edit(&mut c);
        Mapper::new(&c, 0)
    }

    fn msg(m: &mut Mapper, payload: &str, retain: bool, at: u64) -> Outcome {
        m.on_message("example/topic", payload.as_bytes(), retain, at)
    }

    use Outcome::*;
    use Reason::*;
    use TriggerState::*;

    #[test]
    fn level_transitions_and_plain_values() {
        let mut m = mapper(|_| {});
        assert_eq!(msg(&mut m, "OFF", false, 1), Dropped(Transition));
        assert_eq!(msg(&mut m, "ON", false, 2), Emit(Start));
        assert_eq!(msg(&mut m, "on", false, 3), Dropped(Repeat));
        assert_eq!(msg(&mut m, " off\n", false, 4), Emit(Stop));
        assert_eq!(msg(&mut m, "True", false, 5), Emit(Start));
        assert_eq!(msg(&mut m, "0", false, 6), Emit(Stop));
        assert_eq!(msg(&mut m, "1", false, 7), Emit(Start));
        assert_eq!(msg(&mut m, "false", false, 8), Emit(Stop));
        assert_eq!(msg(&mut m, "maybe", false, 9), Dropped(Unmapped));
        assert_eq!(
            m.on_message("example/topic", &[0xff, 0xfe], false, 10),
            Dropped(Unmapped)
        );
    }

    #[test]
    fn first_off_has_nothing_to_stop_but_sets_state() {
        let mut m = mapper(|_| {});
        assert_eq!(msg(&mut m, "OFF", false, 1), Dropped(Transition));
        // Now known to be off, so on starts.
        assert_eq!(msg(&mut m, "ON", false, 2), Emit(Start));
    }

    #[test]
    fn transitions_can_be_disabled() {
        let mut m = mapper(|c| c.mapping.stop_on_off = false);
        assert_eq!(msg(&mut m, "ON", false, 1), Emit(Start));
        assert_eq!(msg(&mut m, "OFF", false, 2), Dropped(Transition));
        assert_eq!(msg(&mut m, "ON", false, 3), Emit(Start));
        let mut m = mapper(|c| c.mapping.start_on_on = false);
        assert_eq!(msg(&mut m, "ON", false, 1), Dropped(Transition));
    }

    #[test]
    fn pulse_mode() {
        let mut m = mapper(|c| c.mapping.mode = Mode::Pulse);
        assert_eq!(msg(&mut m, "ON", false, 1), Emit(Pulse));
        assert_eq!(msg(&mut m, "ON", false, 2), Emit(Pulse));
        assert_eq!(msg(&mut m, "OFF", false, 3), Dropped(Unmapped));
    }

    #[test]
    fn custom_values_replace_the_defaults() {
        let mut m = mapper(|c| {
            c.mapping.on_values = vec!["open".into()];
            c.mapping.off_values = vec!["closed".into()];
        });
        assert_eq!(msg(&mut m, "ON", false, 1), Dropped(Unmapped));
        assert_eq!(msg(&mut m, "OPEN", false, 2), Emit(Start));
        assert_eq!(msg(&mut m, "closed", false, 3), Emit(Stop));
    }

    #[test]
    fn json_paths() {
        let mut m = mapper(|c| c.mapping.json_path = Some("state".into()));
        assert_eq!(
            msg(&mut m, r#"{"state":"on","x":1}"#, false, 1),
            Emit(Start)
        );
        assert_eq!(msg(&mut m, r#"{"state":"OFF"}"#, false, 2), Emit(Stop));
        assert_eq!(msg(&mut m, r#"{"state":true}"#, false, 3), Emit(Start));
        assert_eq!(msg(&mut m, r#"{"state":0}"#, false, 4), Emit(Stop));
        assert_eq!(
            msg(&mut m, r#"{"other":"on"}"#, false, 5),
            Dropped(Unmapped)
        );
        assert_eq!(msg(&mut m, "ON", false, 6), Dropped(Unmapped));
        assert_eq!(
            msg(&mut m, r#"{"state":{"a":1}}"#, false, 7),
            Dropped(Unmapped)
        );
        let mut m = mapper(|c| c.mapping.json_path = Some("items.1.s".into()));
        assert_eq!(
            msg(&mut m, r#"{"items":[{"s":"off"},{"s":"on"}]}"#, false, 1),
            Emit(Start)
        );
        assert_eq!(
            msg(&mut m, r#"{"items":[{"s":"off"}]}"#, false, 2),
            Dropped(Unmapped)
        );
    }

    #[test]
    fn retained_policies() {
        let mut m = mapper(|_| {});
        assert_eq!(msg(&mut m, "ON", true, 1), Dropped(Retained));
        // Not seen, so the live "ON" is still a start.
        assert_eq!(msg(&mut m, "ON", false, 2), Emit(Start));

        let mut m = mapper(|c| c.mapping.retained = RetainedPolicy::Seed);
        assert_eq!(msg(&mut m, "ON", true, 1), Seeded);
        assert_eq!(msg(&mut m, "ON", false, 2), Dropped(Repeat));
        assert_eq!(msg(&mut m, "OFF", false, 3), Emit(Stop));

        let mut m = mapper(|c| c.mapping.retained = RetainedPolicy::Accept);
        assert_eq!(msg(&mut m, "ON", true, 1), Emit(Start));
    }

    #[test]
    fn debounce() {
        let mut m = mapper(|c| c.mapping.debounce_ms = 1000);
        assert_eq!(msg(&mut m, "ON", false, 100), Emit(Start));
        assert_eq!(msg(&mut m, "OFF", false, 600), Dropped(Debounced));
        // The dropped OFF did not change the state: a later OFF still stops.
        assert_eq!(msg(&mut m, "OFF", false, 1100), Emit(Stop));
        assert_eq!(msg(&mut m, "ON", false, 2099), Dropped(Debounced));
        assert_eq!(msg(&mut m, "ON", false, 2100), Emit(Start));
    }

    #[test]
    fn size_cap_and_topic() {
        let mut m = mapper(|c| c.limits.max_payload_bytes = 4);
        assert_eq!(msg(&mut m, "12345", false, 1), Dropped(Oversized));
        assert_eq!(msg(&mut m, "ON", false, 1), Emit(Start));
        assert_eq!(
            m.on_message("other/topic", b"OFF", false, 2),
            Dropped(Topic)
        );
    }

    #[test]
    fn rate_limit_burst_then_refill() {
        // 6 a minute is one every 10 s; burst 3.
        let mut m = mapper(|c| {
            c.limits.rate_per_min = 6;
            c.limits.rate_burst = 3;
            c.mapping.mode = Mode::Pulse;
        });
        for _ in 0..3 {
            assert_eq!(msg(&mut m, "ON", false, 0), Emit(Pulse));
        }
        assert_eq!(msg(&mut m, "ON", false, 0), Dropped(RateLimited));
        assert_eq!(msg(&mut m, "ON", false, 9_999), Dropped(RateLimited));
        assert_eq!(msg(&mut m, "ON", false, 10_000), Emit(Pulse));
        assert_eq!(msg(&mut m, "ON", false, 10_000), Dropped(RateLimited));
        // A long quiet time refills to the burst, not beyond.
        for _ in 0..3 {
            assert_eq!(msg(&mut m, "ON", false, 1_000_000), Emit(Pulse));
        }
        assert_eq!(msg(&mut m, "ON", false, 1_000_000), Dropped(RateLimited));
    }

    #[test]
    fn wildcards() {
        assert!(topic_matches("a/b", "a/b"));
        assert!(!topic_matches("a/b", "a/b/c"));
        assert!(!topic_matches("a/b/c", "a/b"));
        assert!(topic_matches("a/+/c", "a/x/c"));
        assert!(!topic_matches("a/+/c", "a/x/y/c"));
        assert!(topic_matches("a/#", "a/x/y"));
        assert!(topic_matches("a/#", "a"));
        assert!(topic_matches("#", "q/r"));
    }
}
