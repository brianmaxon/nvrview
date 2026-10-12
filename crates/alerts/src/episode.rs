//! Timeliness: one episode per camera and class, joined to the recordings'
//! incident key, with an escalating cooldown.
//!
//! ```text
//! Pending ──alert──► Active ──seen again──► Lingering ──misses + stop / idle──► Settled
//!    └──quiet, or no decision in time──► Settled
//! ```
//!
//! The first alert of an episode starts the initial cooldown (1 min by
//! default); every later alert of the same episode uses the escalated one
//! (5 min). Within a cooldown, a trigger is suppressed unless it breaks
//! through: a higher severity, or the first always-alert outcome of the
//! episode. Always-alert outcomes are never suppressed; inside a cooldown
//! they update the existing alert instead of starting a new one.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::model::{CameraId, Class, Severity};
use crate::settings::TimingSettings;
use crate::time::Millis;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EpisodeKey {
    pub camera: CameraId,
    pub class: Class,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeState {
    Pending,
    Active,
    Lingering,
    Settled,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Episode {
    pub key: EpisodeKey,
    pub state: EpisodeState,
    pub incident: Option<String>,
    pub started_ms: Millis,
    pub last_activity_ms: Millis,
    pub alert_id: Option<u64>,
    pub rev: u32,
    /// Alerts (new or update, not counting always-alert repeats inside a
    /// cooldown).
    pub alerts: u32,
    pub last_alert_ms: Option<Millis>,
    pub cooldown_ms: Millis,
    pub severity: Option<Severity>,
    pub always: bool,
    pub misses: u8,
    pub device_active: bool,
}

impl Episode {
    fn new(key: EpisodeKey, at_ms: Millis) -> Self {
        Episode {
            key,
            state: EpisodeState::Pending,
            incident: None,
            started_ms: at_ms,
            last_activity_ms: at_ms,
            alert_id: None,
            rev: 0,
            alerts: 0,
            last_alert_ms: None,
            cooldown_ms: 0,
            severity: None,
            always: false,
            misses: 0,
            device_active: true,
        }
    }

    fn is_open_alert(&self) -> bool {
        matches!(self.state, EpisodeState::Active | EpisodeState::Lingering)
            && self.alert_id.is_some()
    }

    /// Time left in the cooldown at `now`, if any.
    pub fn cooldown_left(&self, now: Millis) -> Option<Millis> {
        if !self.is_open_alert() {
            return None;
        }
        let end = self.last_alert_ms? + self.cooldown_ms;
        (now < end).then_some(end - now)
    }
}

/// Why an alert updates an earlier one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateWhy {
    /// The cooldown ended while the activity went on.
    StillPresent,
    /// A higher severity inside the cooldown.
    HigherSeverity,
    /// The episode's first always-alert outcome, inside the cooldown.
    NowAlwaysAlert,
    /// An always-alert repeat inside the cooldown (never suppressed).
    AlwaysRepeat,
    /// The same class on another camera of the same incident.
    AlsoSeen,
}

/// What timeliness made of an alert-worthy decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "admission", rename_all = "snake_case")]
pub enum Admission {
    New {
        alert_id: u64,
    },
    Update {
        alert_id: u64,
        rev: u32,
        why: UpdateWhy,
        /// For `AlsoSeen`: the camera that alerted first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_camera: Option<CameraId>,
    },
    Cooldown {
        alert_id: u64,
        left_ms: Millis,
        since_ms: Millis,
    },
}

/// The most episodes kept; the oldest settled ones go first.
pub const MAX_EPISODES: usize = 1024;

#[derive(Clone, Debug, Default)]
pub struct Timeliness {
    episodes: BTreeMap<EpisodeKey, Episode>,
    next_alert_id: u64,
}

impl Timeliness {
    pub fn new() -> Self {
        Timeliness {
            episodes: BTreeMap::new(),
            next_alert_id: 1,
        }
    }

    pub fn get(&self, key: &EpisodeKey) -> Option<&Episode> {
        self.episodes.get(key)
    }

    pub fn len(&self) -> usize {
        self.episodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.episodes.is_empty()
    }

    fn entry(&mut self, key: EpisodeKey, now: Millis) -> &mut Episode {
        if !self.episodes.contains_key(&key) && self.episodes.len() >= MAX_EPISODES {
            self.evict();
        }
        let ep = self
            .episodes
            .entry(key)
            .or_insert_with(|| Episode::new(key, now));
        if ep.state == EpisodeState::Settled {
            *ep = Episode::new(key, now);
        }
        ep
    }

    fn evict(&mut self) {
        let victim = self
            .episodes
            .values()
            .min_by_key(|e| (e.state != EpisodeState::Settled, e.last_activity_ms))
            .map(|e| e.key);
        if let Some(k) = victim {
            self.episodes.remove(&k);
        }
    }

    /// Activity on a camera and class: opens a pending episode when none is
    /// open, and keeps an open one alive.
    pub fn touch(&mut self, key: EpisodeKey, now: Millis) -> EpisodeState {
        let ep = self.entry(key, now);
        ep.last_activity_ms = ep.last_activity_ms.max(now);
        ep.device_active = true;
        ep.misses = 0;
        ep.state
    }

    /// The cooldown left for a camera and class.
    pub fn cooldown_left(&self, key: &EpisodeKey, now: Millis) -> Option<Millis> {
        self.episodes.get(key)?.cooldown_left(now)
    }

    /// A pending episode that did not alert settles.
    pub fn discard(&mut self, key: &EpisodeKey) {
        if let Some(ep) = self.episodes.get_mut(key)
            && ep.state == EpisodeState::Pending
        {
            ep.state = EpisodeState::Settled;
        }
    }

    /// Admits an alert-worthy decision.
    pub fn admit(
        &mut self,
        key: EpisodeKey,
        severity: Severity,
        always: bool,
        incident: Option<&str>,
        now: Millis,
        timing: &TimingSettings,
    ) -> Admission {
        let initial = timing.initial_cooldown_ms();
        let escalated = timing.escalated_cooldown_ms().max(initial);

        // The same class already alerted on another camera of the incident.
        let sibling = incident.and_then(|inc| {
            self.episodes
                .values()
                .filter(|e| {
                    e.key.class == key.class
                        && e.key.camera != key.camera
                        && e.is_open_alert()
                        && e.incident.as_deref() == Some(inc)
                })
                .min_by_key(|e| e.started_ms)
                .map(|e| e.key)
        });

        let next_id = self.next_alert_id;
        let ep = self.entry(key, now);
        ep.last_activity_ms = ep.last_activity_ms.max(now);
        if incident.is_some() {
            ep.incident = incident.map(str::to_owned);
        }

        if !ep.is_open_alert() {
            ep.state = EpisodeState::Active;
            ep.alerts = 1;
            ep.last_alert_ms = Some(now);
            ep.cooldown_ms = initial;
            ep.severity = Some(severity);
            ep.always = always;
            if let Some(sk) = sibling {
                let (alert_id, rev) = {
                    let s = self.episodes.get_mut(&sk).expect("sibling exists");
                    s.rev += 1;
                    (s.alert_id.expect("open alert"), s.rev)
                };
                let ep = self.episodes.get_mut(&key).expect("just inserted");
                ep.alert_id = Some(alert_id);
                ep.rev = rev;
                return Admission::Update {
                    alert_id,
                    rev,
                    why: UpdateWhy::AlsoSeen,
                    first_camera: Some(sk.camera),
                };
            }
            ep.alert_id = Some(next_id);
            ep.rev = 1;
            self.next_alert_id += 1;
            return Admission::New { alert_id: next_id };
        }

        let alert_id = ep.alert_id.expect("open alert");
        let why = match ep.cooldown_left(now) {
            None => Some(UpdateWhy::StillPresent),
            Some(_) if always && !ep.always => Some(UpdateWhy::NowAlwaysAlert),
            Some(_) if ep.severity.is_none_or(|s| severity > s) => Some(UpdateWhy::HigherSeverity),
            Some(_) if always => Some(UpdateWhy::AlwaysRepeat),
            Some(_) => None,
        };
        match why {
            None => {
                let since = ep.last_alert_ms.map_or(0, |t| now - t);
                Admission::Cooldown {
                    alert_id,
                    left_ms: ep.cooldown_left(now).unwrap_or(0),
                    since_ms: since,
                }
            }
            Some(why) => {
                ep.rev += 1;
                ep.always |= always;
                ep.severity = Some(ep.severity.map_or(severity, |s| s.max(severity)));
                if why != UpdateWhy::AlwaysRepeat {
                    ep.alerts += 1;
                    ep.last_alert_ms = Some(now);
                    ep.cooldown_ms = ep.cooldown_ms.max(escalated);
                }
                Admission::Update {
                    alert_id,
                    rev: ep.rev,
                    why,
                    first_camera: None,
                }
            }
        }
    }

    /// A "still present" re-check result. Returns the new state.
    pub fn observe(
        &mut self,
        key: &EpisodeKey,
        seen: bool,
        now: Millis,
        timing: &TimingSettings,
    ) -> Option<EpisodeState> {
        let ep = self.episodes.get_mut(key)?;
        if !ep.is_open_alert() {
            return Some(ep.state);
        }
        if seen {
            ep.misses = 0;
            ep.last_activity_ms = ep.last_activity_ms.max(now);
            ep.state = EpisodeState::Lingering;
        } else {
            ep.misses = ep.misses.saturating_add(1);
            if ep.misses >= timing.settle_misses && !ep.device_active {
                ep.state = EpisodeState::Settled;
            }
        }
        Some(ep.state)
    }

    /// The camera's own detection stopped.
    pub fn device_stop(&mut self, key: &EpisodeKey, timing: &TimingSettings) {
        if let Some(ep) = self.episodes.get_mut(key) {
            ep.device_active = false;
            if ep.is_open_alert() && ep.misses >= timing.settle_misses {
                ep.state = EpisodeState::Settled;
            }
        }
    }

    /// Settles idle episodes and drops pending ones that never got a
    /// decision. Returns the keys that settled now; settled episodes are
    /// then forgotten.
    pub fn tick(&mut self, now: Millis, timing: &TimingSettings) -> Vec<EpisodeKey> {
        let mut settled = Vec::new();
        for ep in self.episodes.values_mut() {
            let idle = now - ep.last_activity_ms;
            let done = match ep.state {
                EpisodeState::Pending => idle >= timing.pending_timeout_ms(),
                EpisodeState::Active | EpisodeState::Lingering => idle >= timing.settle_ms(),
                EpisodeState::Settled => true,
            };
            if done {
                if ep.state != EpisodeState::Settled {
                    settled.push(ep.key);
                }
                ep.state = EpisodeState::Settled;
            }
        }
        self.episodes
            .retain(|_, e| e.state != EpisodeState::Settled);
        settled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{MINUTE_MS, SECOND_MS};

    fn key(ch: u16, class: Class) -> EpisodeKey {
        EpisodeKey {
            camera: CameraId::new(1, ch, 0),
            class,
        }
    }

    #[test]
    fn escalating_cooldown() {
        let t = TimingSettings::default();
        let mut tl = Timeliness::new();
        let k = key(0, Class::Person);
        assert_eq!(tl.touch(k, 0), EpisodeState::Pending);
        assert_eq!(
            tl.admit(k, Severity::Normal, false, None, 0, &t),
            Admission::New { alert_id: 1 }
        );
        assert_eq!(tl.cooldown_left(&k, 20 * SECOND_MS), Some(40 * SECOND_MS));
        assert!(matches!(
            tl.admit(k, Severity::Normal, false, None, 30 * SECOND_MS, &t),
            Admission::Cooldown {
                alert_id: 1,
                left_ms: 30_000,
                since_ms: 30_000
            }
        ));
        // After 1 min: an update, and the cooldown escalates to 5 min.
        assert!(matches!(
            tl.admit(k, Severity::Normal, false, None, 61 * SECOND_MS, &t),
            Admission::Update {
                alert_id: 1,
                rev: 2,
                why: UpdateWhy::StillPresent,
                ..
            }
        ));
        assert_eq!(tl.get(&k).unwrap().cooldown_ms, 5 * MINUTE_MS);
        assert!(matches!(
            tl.admit(k, Severity::Normal, false, None, 3 * MINUTE_MS, &t),
            Admission::Cooldown { .. }
        ));
    }

    #[test]
    fn breakthroughs() {
        let t = TimingSettings::default();
        let mut tl = Timeliness::new();
        let k = key(0, Class::Person);
        tl.admit(k, Severity::Normal, false, None, 0, &t);
        assert!(matches!(
            tl.admit(k, Severity::High, false, None, 10_000, &t),
            Admission::Update {
                why: UpdateWhy::HigherSeverity,
                rev: 2,
                ..
            }
        ));
        assert!(matches!(
            tl.admit(k, Severity::Critical, true, None, 20_000, &t),
            Admission::Update {
                why: UpdateWhy::NowAlwaysAlert,
                rev: 3,
                ..
            }
        ));
        // Always-alert repeats update without moving the cooldown.
        let before = tl.get(&k).unwrap().last_alert_ms;
        assert!(matches!(
            tl.admit(k, Severity::Critical, true, None, 25_000, &t),
            Admission::Update {
                why: UpdateWhy::AlwaysRepeat,
                rev: 4,
                ..
            }
        ));
        assert_eq!(tl.get(&k).unwrap().last_alert_ms, before);
        // A different class is a different episode.
        assert_eq!(
            tl.admit(
                key(0, Class::Vehicle),
                Severity::Normal,
                false,
                None,
                26_000,
                &t
            ),
            Admission::New { alert_id: 2 }
        );
    }

    #[test]
    fn also_seen_joins_the_incident() {
        let t = TimingSettings::default();
        let mut tl = Timeliness::new();
        tl.admit(
            key(0, Class::Person),
            Severity::Normal,
            false,
            Some("i-1"),
            0,
            &t,
        );
        assert_eq!(
            tl.admit(
                key(1, Class::Person),
                Severity::Normal,
                false,
                Some("i-1"),
                5000,
                &t
            ),
            Admission::Update {
                alert_id: 1,
                rev: 2,
                why: UpdateWhy::AlsoSeen,
                first_camera: Some(CameraId::new(1, 0, 0))
            }
        );
        // Another incident: a new alert.
        assert_eq!(
            tl.admit(
                key(2, Class::Person),
                Severity::Normal,
                false,
                Some("i-2"),
                6000,
                &t
            ),
            Admission::New { alert_id: 2 }
        );
    }

    #[test]
    fn lingering_and_settling() {
        let t = TimingSettings::default();
        let mut tl = Timeliness::new();
        let k = key(0, Class::Person);
        tl.admit(k, Severity::Normal, false, None, 0, &t);
        assert_eq!(
            tl.observe(&k, true, 10_000, &t),
            Some(EpisodeState::Lingering)
        );
        assert_eq!(
            tl.observe(&k, false, 20_000, &t),
            Some(EpisodeState::Lingering)
        );
        assert_eq!(
            tl.observe(&k, false, 30_000, &t),
            Some(EpisodeState::Lingering)
        );
        // Two misses, then the device's own stop: settled.
        tl.device_stop(&k, &t);
        assert_eq!(tl.get(&k).unwrap().state, EpisodeState::Settled);
        assert_eq!(tl.tick(31_000, &t), Vec::<EpisodeKey>::new());
        assert!(tl.is_empty());
        // A new trigger starts a fresh episode with the initial cooldown.
        assert_eq!(
            tl.admit(k, Severity::Normal, false, None, 40_000, &t),
            Admission::New { alert_id: 2 }
        );
    }

    #[test]
    fn idle_and_pending_timeouts() {
        let t = TimingSettings::default();
        let mut tl = Timeliness::new();
        let a = key(0, Class::Person);
        let p = key(1, Class::Vehicle);
        tl.admit(a, Severity::Normal, false, None, 0, &t);
        tl.touch(p, 0);
        assert_eq!(tl.tick(29_000, &t), vec![]);
        assert_eq!(tl.tick(30_000, &t), vec![p]);
        assert_eq!(tl.tick(119_000, &t), vec![]);
        assert_eq!(tl.tick(120_000, &t), vec![a]);
        // Discarded pending episodes settle at once.
        tl.touch(p, 200_000);
        tl.discard(&p);
        assert_eq!(tl.get(&p).unwrap().state, EpisodeState::Settled);
    }

    #[test]
    fn bounded() {
        let t = TimingSettings::default();
        let mut tl = Timeliness::new();
        for i in 0..(MAX_EPISODES as u32 + 10) {
            let k = EpisodeKey {
                camera: CameraId::new(i, 0, 0),
                class: Class::Person,
            };
            tl.admit(k, Severity::Normal, false, None, Millis::from(i), &t);
        }
        assert_eq!(tl.len(), MAX_EPISODES);
    }
}
