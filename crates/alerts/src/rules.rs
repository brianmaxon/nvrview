//! Rules: night, schedules, severity, and the always-alert rules.
//!
//! Rules come first and are deterministic. Learned rarity (later) may only
//! move a normal or low severity by one step; it never touches an
//! always-alert outcome.

use serde::{Deserialize, Serialize};

use crate::model::{Class, Exposure, Mode, Severity, Source};
use crate::settings::{AlertSettings, AlwaysAlertSettings, Schedule};
use crate::sun::{is_fixed_night, is_night};
use crate::time::{LocalTime, Millis, local};

/// What decided whether it is night.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NightBasis {
    /// Sunset and sunrise at the configured location.
    Sun,
    /// The fixed hours (no location set).
    Fixed,
    /// The household is in night mode.
    Mode,
}

/// Time context for one instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub night: bool,
    pub basis: NightBasis,
    pub local: LocalTime,
}

pub fn context(settings: &AlertSettings, at_ms: Millis, offset_min: i32) -> Context {
    let n = &settings.night;
    let (night, basis) = if settings.mode == Mode::Night {
        (true, NightBasis::Mode)
    } else if let Some(loc) = settings.location {
        (
            is_night(
                at_ms,
                loc.lat,
                loc.lon,
                offset_min,
                n.after_sunset_min,
                n.before_sunrise_min,
            ),
            NightBasis::Sun,
        )
    } else {
        (
            is_fixed_night(at_ms, offset_min, n.fallback_start_min, n.fallback_end_min),
            NightBasis::Fixed,
        )
    };
    Context {
        night,
        basis,
        local: local(at_ms, offset_min),
    }
}

/// Whether a camera's schedule allows alerts now.
pub fn schedule_allows(schedule: &Schedule, ctx: &Context) -> bool {
    match schedule {
        Schedule::Always => true,
        Schedule::Night => ctx.night,
        Schedule::Windows { windows } => windows
            .iter()
            .any(|w| w.contains(ctx.local.weekday, ctx.local.minute_of_day)),
    }
}

/// An always-alert rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlwaysRule {
    SmokeCo,
    NightOutsidePerson,
    DoorbellNight,
}

impl AlwaysRule {
    pub fn words(self) -> &'static str {
        match self {
            AlwaysRule::SmokeCo => "smoke and CO alarms",
            AlwaysRule::NightOutsidePerson => "a person on an outside camera at night",
            AlwaysRule::DoorbellNight => "a doorbell press at night",
        }
    }
}

/// How well the trigger was confirmed, as the rules see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    /// Needs no picture (doorbell press, alarm sound).
    SelfEvident,
    /// k of n reached.
    Verified,
    /// Some hits, fewer than k.
    Weak,
    /// Checked, nothing seen: our check disagrees with the trigger.
    NotSeen,
    /// Could not look (no pictures, detector timed out).
    Unavailable,
    /// A class only the camera's own AI reports (package, face).
    DeviceOnly,
    /// An external sensor allowed to alert without confirmation.
    SensorOnly,
}

/// Everything the rules look at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuleInput {
    pub class: Class,
    pub source: Source,
    pub support: Support,
    pub exposure: Exposure,
    pub entrance: bool,
    pub trust_camera_ai: bool,
    pub night: bool,
    pub mode: Mode,
}

/// The rules' verdict before timeliness and rarity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleOutcome {
    /// `None` when the event is not alert-worthy on its support (our check
    /// did not confirm it).
    pub severity: Option<Severity>,
    pub always: Option<AlwaysRule>,
    /// Alerting without our own confirmation.
    pub unverified: bool,
}

/// Which always-alert rule could apply to a trigger before any picture is
/// checked (so the gate does not stop it).
pub fn could_be_always(
    class: Class,
    exposure: Exposure,
    night: bool,
    rules: &AlwaysAlertSettings,
) -> Option<AlwaysRule> {
    match class {
        Class::Smoke | Class::Co if rules.smoke_co => Some(AlwaysRule::SmokeCo),
        Class::Visitor if night && rules.doorbell_night => Some(AlwaysRule::DoorbellNight),
        Class::Person | Class::Motion | Class::Sensor
            if night && exposure.is_outside() && rules.night_outside_person =>
        {
            Some(AlwaysRule::NightOutsidePerson)
        }
        _ => None,
    }
}

fn base_severity(i: &RuleInput) -> Severity {
    let inside = i.exposure == Exposure::Inside;
    match i.class {
        Class::Person if inside => match i.mode {
            Mode::Home => Severity::Low,
            Mode::Night => Severity::Normal,
            Mode::Away => Severity::High,
        },
        Class::Person if i.night => Severity::High,
        Class::Person => Severity::Normal,
        Class::Vehicle if inside => Severity::Low,
        Class::Vehicle => Severity::Normal,
        Class::Package => Severity::Normal,
        Class::Visitor => Severity::High,
        Class::Smoke | Class::Co => Severity::High,
        Class::Sensor => Severity::Normal,
        Class::Animal | Class::Face | Class::Motion | Class::Other => Severity::Low,
    }
}

/// Applies the rules.
pub fn evaluate(i: &RuleInput, rules: &AlwaysAlertSettings) -> RuleOutcome {
    // Always-alert rules first.
    let always = match i.class {
        Class::Smoke | Class::Co if rules.smoke_co => Some(AlwaysRule::SmokeCo),
        Class::Visitor if i.night && rules.doorbell_night => Some(AlwaysRule::DoorbellNight),
        Class::Person if i.night && i.exposure.is_outside() && rules.night_outside_person => {
            Some(AlwaysRule::NightOutsidePerson)
        }
        _ => None,
    };
    if let Some(rule) = always {
        let unverified = match i.support {
            Support::SelfEvident | Support::Verified => Some(false),
            // Fail open: we could not look.
            Support::Unavailable => Some(true),
            // Our check disagrees: only cameras whose AI is trusted alert.
            Support::Weak | Support::NotSeen if i.trust_camera_ai && i.source == Source::Device => {
                Some(true)
            }
            Support::DeviceOnly | Support::SensorOnly => Some(true),
            Support::Weak | Support::NotSeen => None,
        };
        if let Some(unverified) = unverified {
            return RuleOutcome {
                severity: Some(Severity::Critical),
                always: Some(rule),
                unverified,
            };
        }
    }

    let (severity, unverified) = match i.support {
        Support::NotSeen => (None, false),
        Support::SelfEvident | Support::Verified | Support::DeviceOnly | Support::SensorOnly => {
            (Some(base_severity(i)), false)
        }
        // Fewer hits than needed, or no look at all: the trigger alone,
        // one step lower.
        Support::Weak => (Some(base_severity(i).lowered()), false),
        Support::Unavailable => (Some(base_severity(i).lowered()), true),
    };
    let severity = severity.map(|s| {
        if i.mode == Mode::Away {
            s.raised(Severity::High)
        } else {
            s
        }
    });
    RuleOutcome {
        severity,
        always: None,
        unverified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{DailyWindow, Location};
    use crate::time::{Date, HOUR_MS};

    fn input(class: Class, support: Support) -> RuleInput {
        RuleInput {
            class,
            source: Source::Device,
            support,
            exposure: Exposure::Outside,
            entrance: false,
            trust_camera_ai: false,
            night: false,
            mode: Mode::Home,
        }
    }

    const ON: AlwaysAlertSettings = AlwaysAlertSettings {
        smoke_co: true,
        night_outside_person: true,
        doorbell_night: true,
    };

    #[test]
    fn smoke_and_co_always_alert() {
        for c in [Class::Smoke, Class::Co] {
            let o = evaluate(&input(c, Support::SelfEvident), &ON);
            assert_eq!(o.always, Some(AlwaysRule::SmokeCo));
            assert_eq!(o.severity, Some(Severity::Critical));
        }
        let off = AlwaysAlertSettings {
            smoke_co: false,
            ..ON
        };
        let o = evaluate(&input(Class::Smoke, Support::SelfEvident), &off);
        assert_eq!((o.always, o.severity), (None, Some(Severity::High)));
    }

    #[test]
    fn night_outside_person() {
        let mut i = input(Class::Person, Support::Verified);
        i.night = true;
        let o = evaluate(&i, &ON);
        assert_eq!(o.always, Some(AlwaysRule::NightOutsidePerson));
        assert!(!o.unverified);
        // Perimeter counts as outside, inside does not.
        i.exposure = Exposure::Perimeter;
        assert!(evaluate(&i, &ON).always.is_some());
        i.exposure = Exposure::Inside;
        assert!(evaluate(&i, &ON).always.is_none());
        // Day: a normal alert.
        let mut d = input(Class::Person, Support::Verified);
        assert_eq!(evaluate(&d, &ON).severity, Some(Severity::Normal));
        d.night = true;
        d.support = Support::NotSeen;
        // Disagreement: quiet, unless the camera's AI is trusted.
        assert_eq!(evaluate(&d, &ON).severity, None);
        d.trust_camera_ai = true;
        let o = evaluate(&d, &ON);
        assert_eq!(o.always, Some(AlwaysRule::NightOutsidePerson));
        assert!(o.unverified);
        // Trust applies to the camera's own AI only.
        d.source = Source::External;
        assert_eq!(evaluate(&d, &ON).severity, None);
        // Could not look: fail open.
        let mut u = input(Class::Person, Support::Unavailable);
        u.night = true;
        let o = evaluate(&u, &ON);
        assert!(o.always.is_some() && o.unverified);
    }

    #[test]
    fn doorbell() {
        let mut i = input(Class::Visitor, Support::SelfEvident);
        assert_eq!(evaluate(&i, &ON).severity, Some(Severity::High));
        assert_eq!(evaluate(&i, &ON).always, None);
        i.night = true;
        assert_eq!(evaluate(&i, &ON).always, Some(AlwaysRule::DoorbellNight));
    }

    #[test]
    fn severity_table() {
        let s = |i: RuleInput| evaluate(&i, &ON).severity;
        let mut inside = input(Class::Person, Support::Verified);
        inside.exposure = Exposure::Inside;
        assert_eq!(s(inside), Some(Severity::Low));
        inside.mode = Mode::Away;
        // High, and Away raises at most to High.
        assert_eq!(s(inside), Some(Severity::High));
        inside.mode = Mode::Night;
        assert_eq!(s(inside), Some(Severity::Normal));
        assert_eq!(
            s(input(Class::Animal, Support::Verified)),
            Some(Severity::Low)
        );
        assert_eq!(s(input(Class::Vehicle, Support::Weak)), Some(Severity::Low));
        assert_eq!(
            s(input(Class::Package, Support::DeviceOnly)),
            Some(Severity::Normal)
        );
        let mut away = input(Class::Vehicle, Support::Verified);
        away.mode = Mode::Away;
        assert_eq!(s(away), Some(Severity::High));
        let o = evaluate(&input(Class::Vehicle, Support::Unavailable), &ON);
        assert_eq!((o.severity, o.unverified), (Some(Severity::Low), true));
    }

    #[test]
    fn gate_preview_of_always_rules() {
        assert_eq!(
            could_be_always(Class::Motion, Exposure::Outside, true, &ON),
            Some(AlwaysRule::NightOutsidePerson)
        );
        assert_eq!(
            could_be_always(Class::Motion, Exposure::Inside, true, &ON),
            None
        );
        assert_eq!(
            could_be_always(Class::Person, Exposure::Outside, false, &ON),
            None
        );
        assert_eq!(
            could_be_always(Class::Co, Exposure::Inside, false, &ON),
            Some(AlwaysRule::SmokeCo)
        );
    }

    #[test]
    fn context_and_schedules() {
        let mut s = AlertSettings::default();
        let noon = Date::new(2024, 12, 21).utc_midnight_ms() + 12 * HOUR_MS;
        let c = context(&s, noon, 0);
        assert_eq!((c.night, c.basis), (false, NightBasis::Fixed));
        assert!(context(&s, noon + 11 * HOUR_MS, 0).night);
        s.location = Some(Location {
            lat: 51.5,
            lon: 0.0,
        });
        let c = context(&s, noon + 5 * HOUR_MS, 0); // 17:00, after sunset + 30
        assert_eq!((c.night, c.basis), (true, NightBasis::Sun));
        s.mode = Mode::Night;
        assert_eq!(context(&s, noon, 0).basis, NightBasis::Mode);
        assert!(context(&s, noon, 0).night);

        let ctx = context(&AlertSettings::default(), noon, 0);
        assert!(schedule_allows(&Schedule::Always, &ctx));
        assert!(!schedule_allows(&Schedule::Night, &ctx));
        let w = Schedule::Windows {
            windows: vec![DailyWindow {
                days: 0x7f,
                start_min: 11 * 60,
                end_min: 13 * 60,
            }],
        };
        assert!(schedule_allows(&w, &ctx));
        assert!(!schedule_allows(
            &Schedule::Windows { windows: vec![] },
            &ctx
        ));
    }
}
