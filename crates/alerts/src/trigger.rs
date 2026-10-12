//! Trigger intake: everything that might become an alert, normalized to one
//! record.

use serde::{Deserialize, Serialize};

use crate::model::{CameraId, Kind, Source};
use crate::time::Millis;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerState {
    Start,
    Stop,
    /// A momentary event with no stop (a doorbell press, a beam break).
    Pulse,
}

/// One normalized trigger.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trigger {
    /// Assigned by the host, unique within a run; a repeated id is a
    /// duplicate delivery and is dropped.
    pub id: u64,
    pub source: Source,
    /// For external triggers, the source key from settings that the
    /// delivery was authenticated as. Matched against settings only; never
    /// shown in an explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// The camera the trigger belongs to; `None` for external triggers,
    /// whose cameras come from the source's mapping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<CameraId>,
    pub kind: Kind,
    pub state: TriggerState,
    /// When it happened, as well as the source knows (device pushes carry
    /// no time, so for them this is the arrival time).
    pub at_ms: Millis,
    /// When nvrview received it.
    pub received_ms: Millis,
    /// Found by an event-log search after a reconnect, not pushed live.
    #[serde(default)]
    pub catch_up: bool,
}

impl Trigger {
    /// How long after the event it arrived (never negative).
    pub fn age_ms(&self) -> Millis {
        (self.received_ms - self.at_ms).max(0)
    }

    /// A trigger is late when it arrived more than `stale_ms` after the
    /// event, or came from a catch-up search.
    pub fn is_late(&self, stale_ms: Millis) -> bool {
        self.catch_up || self.age_ms() > stale_ms
    }
}

/// The kinds named by a device push's status and AI fields. Each field may
/// hold several comma-separated names (reported, not yet observed);
/// duplicates and `none` are dropped, unknown names pass through as
/// [`Kind::Other`].
pub fn device_kinds(status: &str, ai_type: &str) -> Vec<Kind> {
    let mut out: Vec<Kind> = Vec::new();
    for name in status.split(',').chain(ai_type.split(',')) {
        if let Some(k) = Kind::parse(name)
            && !out.contains(&k)
        {
            out.push(k);
        }
    }
    out
}

/// A device or local-audio detection event, as the core reports it.
pub fn from_detection(
    id: u64,
    source: Source,
    camera: CameraId,
    kind_name: &str,
    state: TriggerState,
    received_ms: Millis,
) -> Option<Trigger> {
    let kind = Kind::parse(kind_name)?;
    Some(Trigger {
        id,
        source,
        origin: None,
        camera: Some(camera),
        kind,
        state,
        at_ms: received_ms,
        received_ms,
        catch_up: false,
    })
}

/// The body an external source posts (webhook or MQTT). The source itself
/// comes from the credential the delivery was made with, never from the
/// body.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ExternalBody {
    pub kind: String,
    #[serde(default = "default_pulse")]
    pub state: TriggerState,
    /// When the sensor fired, if it knows; otherwise the arrival time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_ms: Option<Millis>,
}

fn default_pulse() -> TriggerState {
    TriggerState::Pulse
}

/// Why an external delivery was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalError {
    /// The source key is not a valid key.
    BadSource,
    /// The kind is empty or `none`.
    NoKind,
}

/// The longest accepted source key.
pub const MAX_SOURCE_KEY: usize = 32;

/// Whether a source key is well formed: 1 to 32 bytes of `[a-z0-9_-]`.
pub fn valid_source_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_SOURCE_KEY
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Normalizes an external delivery. A time in the future is replaced by the
/// arrival time.
pub fn from_external(
    id: u64,
    source_key: &str,
    body: &ExternalBody,
    received_ms: Millis,
) -> Result<Trigger, ExternalError> {
    if !valid_source_key(source_key) {
        return Err(ExternalError::BadSource);
    }
    let kind = Kind::parse(&body.kind).ok_or(ExternalError::NoKind)?;
    let at_ms = body.at_ms.map_or(received_ms, |t| t.min(received_ms));
    Ok(Trigger {
        id,
        source: Source::External,
        origin: Some(source_key.to_owned()),
        camera: None,
        kind,
        state: body.state,
        at_ms,
        received_ms,
        catch_up: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_fields_split_and_dedupe() {
        assert_eq!(
            device_kinds("MD", "people"),
            vec![Kind::Motion, Kind::People]
        );
        assert_eq!(device_kinds("none", "none"), Vec::<Kind>::new());
        assert_eq!(
            device_kinds("IO,MD", "people,people"),
            vec![Kind::Other("io".into()), Kind::Motion, Kind::People]
        );
        assert_eq!(device_kinds("visitor", "none"), vec![Kind::Visitor]);
    }

    #[test]
    fn detection_events() {
        let c = CameraId::new(1, 2, 0);
        let t = from_detection(
            7,
            Source::LocalAudio,
            c,
            "smoke_alarm",
            TriggerState::Start,
            1000,
        )
        .unwrap();
        assert_eq!(t.kind, Kind::SmokeAlarm);
        assert_eq!(t.at_ms, 1000);
        assert!(!t.is_late(30_000));
        assert!(from_detection(8, Source::Device, c, "none", TriggerState::Start, 0).is_none());
    }

    #[test]
    fn external_deliveries() {
        let body: ExternalBody =
            serde_json::from_str(r#"{"kind":"sensor_motion","at_ms":500}"#).unwrap();
        let t = from_external(1, "sensor-1", &body, 1000).unwrap();
        assert_eq!(t.state, TriggerState::Pulse);
        assert_eq!(t.at_ms, 500);
        assert_eq!(t.origin.as_deref(), Some("sensor-1"));
        assert_eq!(t.age_ms(), 500);
        // A future time is clamped to the arrival.
        let future = ExternalBody {
            kind: "contact".into(),
            state: TriggerState::Start,
            at_ms: Some(5000),
        };
        assert_eq!(
            from_external(2, "door_1", &future, 1000).unwrap().at_ms,
            1000
        );
        assert_eq!(
            from_external(3, "Bad Key", &future, 1000),
            Err(ExternalError::BadSource)
        );
        let none = ExternalBody {
            kind: "none".into(),
            state: TriggerState::Pulse,
            at_ms: None,
        };
        assert_eq!(from_external(4, "s", &none, 0), Err(ExternalError::NoKind));
    }

    #[test]
    fn lateness() {
        let mut t = from_detection(
            1,
            Source::Device,
            CameraId::new(1, 0, 0),
            "people",
            TriggerState::Start,
            100_000,
        )
        .unwrap();
        t.at_ms = 60_000;
        assert!(t.is_late(30_000));
        assert!(!t.is_late(40_000));
        t.at_ms = 100_000;
        t.catch_up = true;
        assert!(t.is_late(30_000));
    }
}
