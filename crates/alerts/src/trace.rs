//! The verification trace: a typed, ordered list of every check made for
//! one trigger, emitted step by step so the app can show the process live,
//! and stored with the decision in the event log. The explanation is built
//! from it.
//!
//! The JSON form is versioned ([`TRACE_VERSION`]); field names are stable
//! within a version. A trace keeps at most [`MAX_STEPS`] steps: picture and
//! progress steps beyond the budget are counted in one `omitted` step, and
//! the steps that carry the decision (rules, timeliness, decision,
//! delivery) always have room.

use serde::{Deserialize, Serialize};

use crate::decision::Outcome;
use crate::episode::Admission;
use crate::filters::book::FilterKind;
use crate::model::{CameraId, Class, Exposure, Kind, Mode, Severity, Source};
use crate::rules::{AlwaysRule, NightBasis, Support};
use crate::settings::OutputKind;
use crate::time::Millis;
use crate::verify::{Check, Judged};

pub const TRACE_VERSION: u32 = 1;
/// The most steps one trace keeps, the `omitted` summary included.
pub const MAX_STEPS: usize = 64;
/// Room kept for the steps that carry the decision.
const RESERVED: usize = 12;
/// The most detections kept per picture step.
pub const MAX_DETECTIONS_PER_STEP: usize = 8;

/// What the gate decided before any picture was fetched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum GateResult {
    Pass,
    /// Passed because an always-alert rule could apply; only a mute stops
    /// those.
    PassAlways {
        rule: AlwaysRule,
    },
    Muted {
        until_ms: Millis,
    },
    Schedule,
    ClassOff,
    Cooldown {
        left_ms: Millis,
    },
    /// Passed although a cooldown is running: the detection is checked and
    /// logged, but will not interrupt the user.
    PassCooldown {
        left_ms: Millis,
    },
    MotionOnly,
    UnknownKind,
    UnknownSource,
    NoCamera,
    Duplicate,
}

/// What became of the learned rarity input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RarityEffect {
    None,
    Promoted,
    Demoted,
    /// Always-alert outcomes ignore rarity.
    IgnoredAlways,
    LearningOff,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum StepKind {
    TriggerReceived {
        source: Source,
        kind: Kind,
        class: Class,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        camera: Option<CameraId>,
        /// Position of the external source in settings.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sensor: Option<u32>,
        event_ms: Millis,
        late: bool,
        age_ms: Millis,
    },
    /// The same sensor fired again while its window was open.
    Retrigger { trigger_id: u64, until_ms: Millis },
    Gate {
        #[serde(flatten)]
        result: GateResult,
    },
    /// The host is asked to sample these angles in this window.
    Sampling {
        cameras: Vec<CameraId>,
        from_ms: Millis,
        until_ms: Millis,
        classes: Vec<Class>,
        k: f32,
        /// The score floor in force (by day or night, for this camera).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_score: Option<f32>,
    },
    /// A camera's stream was started for the check (reported by the host).
    CameraWoken {
        camera: CameraId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_picture_ms: Option<u32>,
    },
    /// One picture checked.
    FrameSampled {
        camera: CameraId,
        frame_ms: Millis,
        detections: Vec<Judged>,
        /// Detections left out of this step (over the per-step cap).
        #[serde(default)]
        more: u32,
    },
    /// k-of-n progress for one class after a picture.
    Progress {
        class: Class,
        weighted: f32,
        needed: f32,
        hits: u32,
        n: u32,
        angles: u32,
    },
    /// The check's result.
    Checked {
        check: Check,
        /// For a camera's own detection: whether our check agreed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agreed_with_device: Option<bool>,
    },
    /// What the cheap filters found for the class the check settled on.
    Filters {
        class: Class,
        /// Hit boxes of the class over all pictures.
        hits: u32,
        /// Of those, over hot cells of the static-scene memory.
        stationary: u32,
        /// How long the place has been warm, when every hit was stationary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stationary_for_ms: Option<Millis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        moving: Option<bool>,
        /// The alert whose boxes these repeat.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repeat_of: Option<u64>,
        /// The filter that changed the decision, if one did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        acted: Option<FilterKind>,
    },
    Rules {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        camera: Option<CameraId>,
        class: Class,
        night: bool,
        basis: NightBasis,
        exposure: Exposure,
        entrance: bool,
        mode: Mode,
        support: Support,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        always: Option<AlwaysRule>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        severity: Option<Severity>,
        unverified: bool,
    },
    Rarity {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        one_in: Option<u32>,
        effect: RarityEffect,
    },
    Timeliness {
        #[serde(flatten)]
        admission: Admission,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        incident: Option<String>,
    },
    /// For a simulated trigger only: what a real trigger for the same
    /// camera and class would have met right now (real episodes untouched).
    RealPreview {
        /// When the real cooldown ends, if one is running.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cooldown_until_ms: Option<Millis>,
        /// What the real timeliness would have done with this decision.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        admission: Option<Admission>,
    },
    Decision {
        #[serde(flatten)]
        outcome: Outcome,
    },
    /// Delivery to one output (reported by the host).
    Delivery {
        output: u32,
        kind: OutputKind,
        ok: bool,
        /// A short, generic result line from the output ("Home Assistant:
        /// `input_boolean.example_alert` pulsed (200)"), when it gave one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// Steps left out over the budget.
    Omitted { steps: u32 },
}

impl StepKind {
    /// Steps that may be dropped (counted in `omitted`) when the trace is
    /// long.
    fn is_bulk(&self) -> bool {
        matches!(
            self,
            StepKind::FrameSampled { .. }
                | StepKind::Progress { .. }
                | StepKind::CameraWoken { .. }
                | StepKind::Retrigger { .. }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub at_ms: Millis,
    #[serde(flatten)]
    pub kind: StepKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    pub version: u32,
    pub trigger_id: u64,
    pub steps: Vec<Step>,
}

impl Trace {
    pub fn new(trigger_id: u64) -> Self {
        Trace {
            version: TRACE_VERSION,
            trigger_id,
            steps: Vec::new(),
        }
    }

    /// Appends a step. Returns the step as stored (for live display), or
    /// the updated `omitted` summary when the step was over the budget.
    pub fn push(&mut self, at_ms: Millis, kind: StepKind) -> Step {
        let cap = if kind.is_bulk() {
            MAX_STEPS - RESERVED
        } else {
            MAX_STEPS - 1
        };
        if self.steps.len() < cap {
            let step = Step { at_ms, kind };
            self.steps.push(step.clone());
            return step;
        }
        self.omit(at_ms)
    }

    fn omit(&mut self, at_ms: Millis) -> Step {
        if let Some(step) = self
            .steps
            .iter_mut()
            .find(|s| matches!(s.kind, StepKind::Omitted { .. }))
        {
            if let StepKind::Omitted { steps } = &mut step.kind {
                *steps += 1;
            }
            step.at_ms = at_ms;
            return step.clone();
        }
        let step = Step {
            at_ms,
            kind: StepKind::Omitted { steps: 1 },
        };
        if self.steps.len() < MAX_STEPS {
            self.steps.push(step.clone());
        }
        step
    }

    /// Steps of one kind, by a predicate.
    pub fn find<T>(&self, f: impl Fn(&StepKind) -> Option<T>) -> Option<T> {
        self.steps.iter().find_map(|s| f(&s.kind))
    }

    /// The last step matching a predicate.
    pub fn find_last<T>(&self, f: impl Fn(&StepKind) -> Option<T>) -> Option<T> {
        self.steps.iter().rev().find_map(|s| f(&s.kind))
    }
}

/// A picture step from the detector's judgments, capped.
pub fn frame_step(camera: CameraId, frame_ms: Millis, mut judged: Vec<Judged>) -> StepKind {
    // Keep the hits first, then the highest scores.
    judged.sort_by(|a, b| {
        b.hit.cmp(&a.hit).then(
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    let more = judged.len().saturating_sub(MAX_DETECTIONS_PER_STEP) as u32;
    judged.truncate(MAX_DETECTIONS_PER_STEP);
    StepKind::FrameSampled {
        camera,
        frame_ms,
        detections: judged,
        more,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::QuietReason;

    const CAM: CameraId = CameraId::new(1, 2, 0);

    #[test]
    fn golden_json() {
        let mut t = Trace::new(7);
        t.push(
            1000,
            StepKind::TriggerReceived {
                source: Source::Device,
                kind: Kind::People,
                class: Class::Person,
                camera: Some(CAM),
                sensor: None,
                event_ms: 1000,
                late: false,
                age_ms: 0,
            },
        );
        t.push(
            1000,
            StepKind::Gate {
                result: GateResult::Pass,
            },
        );
        t.push(
            1250,
            frame_step(
                CAM,
                1200,
                vec![Judged {
                    class: Class::Person,
                    score: 0.75,
                    above: true,
                    in_region: true,
                    hit: true,
                    stationary: false,
                }],
            ),
        );
        t.push(
            1250,
            StepKind::Checked {
                check: Check::NotSeen {
                    n: 2,
                    outside_region: false,
                },
                agreed_with_device: Some(false),
            },
        );
        t.push(
            1300,
            StepKind::Decision {
                outcome: Outcome::Quiet {
                    reason: QuietReason::NotConfirmed,
                },
            },
        );
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(
            json,
            concat!(
                r#"{"version":1,"trigger_id":7,"steps":["#,
                r#"{"at_ms":1000,"step":"trigger_received","source":"device","kind":"people","class":"person","camera":{"device":1,"channel":2,"lens":0},"event_ms":1000,"late":false,"age_ms":0},"#,
                r#"{"at_ms":1000,"step":"gate","result":"pass"},"#,
                r#"{"at_ms":1250,"step":"frame_sampled","camera":{"device":1,"channel":2,"lens":0},"frame_ms":1200,"detections":[{"class":"person","score":0.75,"above":true,"in_region":true,"hit":true}],"more":0},"#,
                r#"{"at_ms":1250,"step":"checked","check":{"status":"not_seen","n":2,"outside_region":false},"agreed_with_device":false},"#,
                r#"{"at_ms":1300,"step":"decision","decision":"quiet","reason":"not_confirmed"}"#,
                "]}"
            )
        );
        let back: Trace = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn bounded_with_room_for_the_decision() {
        let mut t = Trace::new(1);
        for i in 0..200 {
            t.push(i, frame_step(CAM, i, vec![]));
        }
        assert_eq!(t.steps.len(), MAX_STEPS - RESERVED + 1);
        let omitted = t.find(|k| match k {
            StepKind::Omitted { steps } => Some(*steps),
            _ => None,
        });
        assert_eq!(omitted, Some(200 - (MAX_STEPS - RESERVED) as u32));
        for _ in 0..5 {
            t.push(
                300,
                StepKind::Rarity {
                    one_in: None,
                    effect: RarityEffect::None,
                },
            );
        }
        t.push(
            301,
            StepKind::Decision {
                outcome: Outcome::Quiet {
                    reason: QuietReason::CouldNotLook,
                },
            },
        );
        assert!(matches!(
            t.steps.last().unwrap().kind,
            StepKind::Decision { .. }
        ));
        // Even essential steps stop at the hard cap.
        for _ in 0..100 {
            t.push(
                400,
                StepKind::Gate {
                    result: GateResult::Pass,
                },
            );
        }
        assert!(t.steps.len() <= MAX_STEPS);
        let omitted = t.find(|k| match k {
            StepKind::Omitted { steps } => Some(*steps),
            _ => None,
        });
        assert!(omitted.unwrap() > 200 - (MAX_STEPS - RESERVED) as u32);
    }

    #[test]
    fn frame_steps_are_capped_hits_first() {
        let mut judged: Vec<Judged> = (0..12)
            .map(|i| Judged {
                class: Class::Person,
                score: i as f32 / 20.0,
                above: false,
                in_region: true,
                hit: false,
                stationary: false,
            })
            .collect();
        judged[0].hit = true;
        let StepKind::FrameSampled {
            detections, more, ..
        } = frame_step(CAM, 0, judged)
        else {
            unreachable!()
        };
        assert_eq!(more, 4);
        assert_eq!(detections.len(), MAX_DETECTIONS_PER_STEP);
        assert!(detections[0].hit);
        assert_eq!(detections[1].score, 11.0 / 20.0);
    }
}
