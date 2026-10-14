//! The decision: alert with a severity, quiet log, or suppress with a
//! reason. Every trigger gets one (stops excepted), and none is ever
//! deleted: quiet and suppressed decisions go to the event log too.

use serde::{Deserialize, Serialize};

use crate::model::{CameraId, Class, Severity};
use crate::rules::AlwaysRule;
use crate::time::Millis;
use crate::trace::Trace;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Outcome {
    Alert {
        severity: Severity,
        alert_id: u64,
        /// 1 for a new alert, higher for an update of the same alert.
        rev: u32,
        late: bool,
        unverified: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        always: Option<AlwaysRule>,
        /// A real, verified detection that must not interrupt the user
        /// (no sound, notification, or Home Assistant message). It is
        /// logged with its evidence like any alert.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quieted: Option<Quieted>,
    },
    Quiet {
        #[serde(flatten)]
        reason: QuietReason,
    },
    Suppress {
        #[serde(flatten)]
        reason: SuppressReason,
    },
}

impl Outcome {
    pub fn is_alert(&self) -> bool {
        matches!(self, Outcome::Alert { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Outcome::Alert { .. } => "alert",
            Outcome::Quiet { .. } => "quiet",
            Outcome::Suppress { .. } => "suppress",
        }
    }
}

/// Why an alert is logged but does not interrupt the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "why", rename_all = "snake_case")]
pub enum Quieted {
    /// Inside the cooldown of an alert for the same camera and class.
    Cooldown { left_ms: Millis, since_ms: Millis },
}

/// Why something went to the quiet log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum QuietReason {
    /// The camera's AI reported it; our check did not confirm it.
    NotConfirmed,
    /// An external sensor fired; the mapped cameras did not see enough.
    NothingSeen { hits: u32, n: u32 },
    /// Seen, but only outside the zones or region.
    OutsideRegion,
    /// No picture could be checked.
    CouldNotLook,
    /// Below the lowest alerting severity.
    LowSeverity { severity: Severity },
    /// Arrived too late for a "happening now" alert.
    Late { age_ms: Millis },
    /// Device motion with no class, on a camera that does not verify motion.
    MotionOnly,
    /// A kind the engine does not know.
    UnknownKind,
    /// The host never finished the check.
    NoDecision,
}

/// Why something was not alerted, by the user's settings or timing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum SuppressReason {
    Muted { until_ms: Millis },
    Schedule,
    ClassOff,
    Cooldown { left_ms: Millis, since_ms: Millis },
    Duplicate,
    UnknownSource,
    NoCamera,
}

/// One decision with its trace and explanation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub trigger_id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<CameraId>,
    pub class: Class,
    #[serde(flatten)]
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incident: Option<String>,
    /// Built from the trace by templates; never free text from inputs.
    pub explanation: String,
    pub decided_ms: Millis,
    pub trace: Trace,
}
