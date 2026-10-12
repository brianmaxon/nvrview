//! The vocabulary shared by every stage: cameras, kinds, classes, severities.

use serde::{Deserialize, Serialize};
use std::fmt;

/// One camera angle: a device, a channel slot on it, and a lens (0 for
/// single-lens cameras).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CameraId {
    pub device: u32,
    pub channel: u16,
    #[serde(default)]
    pub lens: u8,
}

impl CameraId {
    pub const fn new(device: u32, channel: u16, lens: u8) -> Self {
        CameraId {
            device,
            channel,
            lens,
        }
    }
}

/// Where a trigger came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The camera's or NVR's own AI and status pushes.
    Device,
    /// nvrview's own pixel-motion detection (later).
    LocalMotion,
    /// nvrview's own sound analysis (smoke and CO alarm patterns).
    LocalAudio,
    /// A sensor or another system, through the webhook or MQTT.
    External,
    /// The NVR's own alarm inputs (not yet decoded; see docs/protocol.md).
    NvrInput,
}

/// What a trigger reports, normalized. Unknown device kinds pass through as
/// [`Kind::Other`] so they are logged, never dropped; they never alert on
/// their own and their text never reaches an explanation.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    People,
    Vehicle,
    DogCat,
    Face,
    Package,
    /// A doorbell press.
    Visitor,
    /// Device motion (`MD`), without a class.
    Motion,
    SmokeAlarm,
    CoAlarm,
    /// A motion, infrared, or beam sensor.
    SensorMotion,
    /// A door, window, or gate contact.
    Contact,
    /// Any other kind, kept as lowercase text (at most 32 bytes of
    /// `[a-z0-9_]`).
    Other(String),
}

impl Kind {
    /// Parses one kind name as a device, our sound analysis, or an external
    /// source writes it. Matching is case-insensitive. `none` and empty
    /// names are no kind at all.
    pub fn parse(name: &str) -> Option<Kind> {
        let lower = name.trim().to_ascii_lowercase();
        let kind = match lower.as_str() {
            "" | "none" => return None,
            "people" | "person" => Kind::People,
            "vehicle" => Kind::Vehicle,
            "dog_cat" | "animal" => Kind::DogCat,
            "face" => Kind::Face,
            "package" => Kind::Package,
            "visitor" => Kind::Visitor,
            "md" | "motion" => Kind::Motion,
            "smoke_alarm" => Kind::SmokeAlarm,
            "co_alarm" => Kind::CoAlarm,
            "sensor_motion" | "pir" | "beam" => Kind::SensorMotion,
            "contact" => Kind::Contact,
            _ => {
                let cleaned: String = lower
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .take(32)
                    .collect();
                Kind::Other(cleaned)
            }
        };
        Some(kind)
    }

    /// The class this kind stands for.
    pub fn class(&self) -> Class {
        match self {
            Kind::People => Class::Person,
            Kind::Vehicle => Class::Vehicle,
            Kind::DogCat => Class::Animal,
            Kind::Face => Class::Face,
            Kind::Package => Class::Package,
            Kind::Visitor => Class::Visitor,
            Kind::Motion => Class::Motion,
            Kind::SmokeAlarm => Class::Smoke,
            Kind::CoAlarm => Class::Co,
            Kind::SensorMotion | Kind::Contact => Class::Sensor,
            Kind::Other(_) => Class::Other,
        }
    }
}

/// What an alert is about. The detector classes (`Person`, `Vehicle`,
/// `Animal`) are the ones the default model really outputs; `Face` and
/// `Package` come only from a camera's own AI; `Visitor`, `Smoke`, and `Co`
/// are their own evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Person,
    Vehicle,
    Animal,
    Face,
    Package,
    Visitor,
    Motion,
    Smoke,
    Co,
    Sensor,
    Other,
}

impl Class {
    /// Classes the local detector can confirm or refute.
    pub fn is_detector_class(self) -> bool {
        matches!(self, Class::Person | Class::Vehicle | Class::Animal)
    }

    /// Classes that need no picture: the trigger is its own evidence.
    pub fn is_self_evident(self) -> bool {
        matches!(self, Class::Visitor | Class::Smoke | Class::Co)
    }

    /// The noun used in explanations, capitalized.
    pub fn noun(self) -> &'static str {
        match self {
            Class::Person => "Person",
            Class::Vehicle => "Vehicle",
            Class::Animal => "Animal",
            Class::Face => "Face",
            Class::Package => "Package",
            Class::Visitor => "Doorbell press",
            Class::Motion => "Motion",
            Class::Smoke => "Smoke alarm sound",
            Class::Co => "CO alarm sound",
            Class::Sensor => "Sensor event",
            Class::Other => "Event of another kind",
        }
    }

    /// The plural used in settings explanations ("Person alerts are off").
    pub fn lower(self) -> &'static str {
        match self {
            Class::Person => "person",
            Class::Vehicle => "vehicle",
            Class::Animal => "animal",
            Class::Face => "face",
            Class::Package => "package",
            Class::Visitor => "doorbell",
            Class::Motion => "motion",
            Class::Smoke => "smoke alarm",
            Class::Co => "CO alarm",
            Class::Sensor => "sensor",
            Class::Other => "other",
        }
    }
}

/// How urgent an alert is. Ordered: `Low < Normal < High < Critical`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Normal,
    High,
    Critical,
}

impl Severity {
    /// One step up, at most `cap`.
    pub fn raised(self, cap: Severity) -> Severity {
        let up = match self {
            Severity::Low => Severity::Normal,
            Severity::Normal => Severity::High,
            Severity::High | Severity::Critical => Severity::Critical,
        };
        if up > cap { self.max(cap) } else { up }
    }

    /// One step down, at least `Low`.
    pub fn lowered(self) -> Severity {
        match self {
            Severity::Low | Severity::Normal => Severity::Low,
            Severity::High => Severity::Normal,
            Severity::Critical => Severity::High,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Normal => "normal",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a camera looks at.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exposure {
    /// Open ground outside: a yard, a driveway, a street.
    #[default]
    Outside,
    /// The edge of the property: a fence line, a gate, a path along it.
    Perimeter,
    /// Inside a building.
    Inside,
}

impl Exposure {
    /// Outside and perimeter cameras both count as "outside" for the
    /// always-alert rules.
    pub fn is_outside(self) -> bool {
        !matches!(self, Exposure::Inside)
    }

    pub fn words(self) -> &'static str {
        match self {
            Exposure::Outside => "outside camera",
            Exposure::Perimeter => "perimeter camera",
            Exposure::Inside => "inside camera",
        }
    }
}

/// The household's global mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// People are home: people on inside cameras are expected.
    #[default]
    Home,
    /// Nobody is home: anything seen is one step more urgent.
    Away,
    /// Everyone is asleep: treated as night whatever the sun does.
    Night,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_parse_case_insensitively() {
        assert_eq!(Kind::parse("People"), Some(Kind::People));
        assert_eq!(Kind::parse(" MD "), Some(Kind::Motion));
        assert_eq!(Kind::parse("none"), None);
        assert_eq!(Kind::parse(""), None);
        assert_eq!(
            Kind::parse("Crossline-Alert!"),
            Some(Kind::Other("crossline_alert_".into()))
        );
        let long = "x".repeat(100);
        assert_eq!(Kind::parse(&long), Some(Kind::Other("x".repeat(32))));
    }

    #[test]
    fn severity_steps() {
        assert_eq!(Severity::Low.raised(Severity::High), Severity::Normal);
        assert_eq!(Severity::High.raised(Severity::High), Severity::High);
        assert_eq!(
            Severity::Critical.raised(Severity::High),
            Severity::Critical
        );
        assert_eq!(Severity::Normal.lowered(), Severity::Low);
        assert!(Severity::Low < Severity::Critical);
    }

    #[test]
    fn serde_names() {
        assert_eq!(serde_json::to_string(&Kind::DogCat).unwrap(), "\"dog_cat\"");
        assert_eq!(
            serde_json::to_string(&Severity::Critical).unwrap(),
            "\"critical\""
        );
        let c: CameraId = serde_json::from_str(r#"{"device":1,"channel":3}"#).unwrap();
        assert_eq!(c, CameraId::new(1, 3, 0));
    }
}
