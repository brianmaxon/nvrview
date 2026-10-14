//! The engine: intake, gate, check bookkeeping, rules, timeliness, and the
//! decision, driven by the host with explicit times.
//!
//! The host's loop, per trigger:
//!
//! 1. [`Engine::intake`] normalizes and gates it, and says what to do next:
//!    sample pictures from some camera angles in a window, decide now, or
//!    nothing (decided at the gate, merged into an open check, a stop).
//! 2. While sampling, [`Engine::camera_woken`] and [`Engine::sample`] add
//!    steps to the trace (returned for live display); `sample` says when
//!    enough was seen to stop early.
//! 3. [`Engine::decide`] finishes: rules, rarity, timeliness, the decision,
//!    and its explanation.
//!
//! [`Engine::tick`] settles idle episodes and decides checks the host never
//! finished (no pictures counts as "could not look", which fails open for
//! always-alert cases).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

use crate::decision::{Decision, Outcome, QuietReason, Quieted, SuppressReason};
use crate::episode::{Admission, EpisodeKey, EpisodeState, Timeliness};
use crate::explain::explain;
use crate::external::{SourceHealth, Window};
use crate::model::{CameraId, Class, Severity, Source};
use crate::rules::{RuleInput, Support, context, could_be_always, evaluate, schedule_allows};
use crate::settings::{AlertSettings, FailPolicy, Issue, Require, SourceMapping};
use crate::time::{HOUR_MS, Millis, SECOND_MS, clamp_offset};
use crate::trace::{GateResult, RarityEffect, Step, StepKind, Trace, frame_step};
use crate::trigger::{Trigger, TriggerState};
use crate::verify::{Angle, Check, CheckSpec, Counter, Rarity, Sample};

/// The host's clock reading: now, and its current UTC offset (minutes east).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Now {
    pub ms: Millis,
    pub utc_offset_min: i32,
}

impl Now {
    pub fn new(ms: Millis, utc_offset_min: i32) -> Self {
        Now {
            ms,
            utc_offset_min: clamp_offset(utc_offset_min),
        }
    }
}

/// What the host does next with a trigger.
#[derive(Clone, Debug, PartialEq)]
pub enum Next {
    /// Sample these camera angles between `from_ms` and `until_ms`, report
    /// each picture with [`Engine::sample`], then call [`Engine::decide`].
    Sample {
        cameras: Vec<CameraId>,
        from_ms: Millis,
        until_ms: Millis,
        classes: Vec<Class>,
    },
    /// Nothing to look at: call [`Engine::decide`].
    DecideNow,
    /// Merged into the open check `trigger_id` of the same source; its
    /// window now ends at `until_ms`.
    Extended { trigger_id: u64, until_ms: Millis },
    /// Decided at intake (the gate, a duplicate, an unknown source).
    Done(Box<Decision>),
    /// A stop event: it ends episodes, and has no decision of its own.
    Ignored,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Intake {
    /// The check this trigger belongs to (its own id, or the open check it
    /// was merged into).
    pub trigger_id: u64,
    /// The steps added, for live display.
    pub steps: Vec<Step>,
    pub next: Next,
}

/// Progress after one picture.
#[derive(Clone, Debug, PartialEq)]
pub struct SampleProgress {
    pub steps: Vec<Step>,
    /// Enough was seen; the host may stop sampling and decide.
    pub reached: bool,
}

/// Inputs to a decision that come from elsewhere.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecideInputs {
    /// From learned baselines (a later slice); `None` when unknown.
    pub rarity: Option<Rarity>,
    /// The recordings' incident key this event belongs to, if known.
    pub incident: Option<String>,
}

/// What [`Engine::tick`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tick {
    pub settled: Vec<EpisodeKey>,
    /// The same for simulated triggers' own episodes.
    pub settled_tests: Vec<EpisodeKey>,
    /// Checks the host never finished, decided now.
    pub decided: Vec<Decision>,
}

#[derive(Clone, Debug)]
struct Open {
    trigger: Trigger,
    trace: Trace,
    counter: Option<Counter>,
    sensor: Option<(usize, SourceMapping)>,
    window: Option<Window>,
    deadline_ms: Millis,
    late: bool,
}

/// The most trigger ids remembered for duplicate detection.
const RECENT_IDS: usize = 4096;
/// The most checks open at once; beyond it the oldest is decided.
pub const MAX_OPEN: usize = 256;
/// The longest mute.
pub const MAX_MUTE_MS: Millis = 24 * HOUR_MS;

/// The timeliness a trigger belongs to: simulated ones have their own.
macro_rules! tl {
    ($s:expr, $test:expr) => {
        if $test {
            &mut $s.test_timeliness
        } else {
            &mut $s.timeliness
        }
    };
}

pub struct Engine {
    settings: AlertSettings,
    timeliness: Timeliness,
    /// Episodes and cooldowns of simulated triggers, kept apart so a test
    /// never touches a real one (and real activity never quiets a test).
    /// Same rules, so repeated tests behave like repeated real triggers.
    test_timeliness: Timeliness,
    health: BTreeMap<String, SourceHealth>,
    mutes: BTreeMap<CameraId, Millis>,
    open: BTreeMap<u64, Open>,
    recent: VecDeque<u64>,
}

/// Serializable counters for a status view.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EngineStatus {
    pub open_checks: usize,
    pub episodes: usize,
    pub muted: usize,
}

impl Engine {
    /// An engine with validated settings; returns what validation changed.
    pub fn new(settings: AlertSettings) -> (Self, Vec<Issue>) {
        let (settings, issues) = settings.validated();
        (
            Engine {
                settings,
                timeliness: Timeliness::new(),
                test_timeliness: Timeliness::new(),
                health: BTreeMap::new(),
                mutes: BTreeMap::new(),
                open: BTreeMap::new(),
                recent: VecDeque::new(),
            },
            issues,
        )
    }

    pub fn settings(&self) -> &AlertSettings {
        &self.settings
    }

    /// Replaces the settings; open checks keep the mapping they started
    /// with.
    pub fn set_settings(&mut self, settings: AlertSettings) -> Vec<Issue> {
        let (settings, issues) = settings.validated();
        self.settings = settings;
        issues
    }

    /// Makes the next new alert's ID at least `next` (alert IDs then stay
    /// unique across runs of a host that logs them).
    pub fn set_next_alert_id(&mut self, next: u64) {
        self.timeliness.set_next_alert_id(next);
    }

    /// The camera's mute end, if it is muted at `now`.
    pub fn muted(&self, camera: CameraId, now: Millis) -> Option<Millis> {
        self.muted_until(camera, now)
    }

    pub fn status(&self) -> EngineStatus {
        EngineStatus {
            open_checks: self.open.len(),
            episodes: self.timeliness.len(),
            muted: self.mutes.len(),
        }
    }

    pub fn episode_state(&self, camera: CameraId, class: Class) -> Option<EpisodeState> {
        self.timeliness
            .get(&EpisodeKey { camera, class })
            .map(|e| e.state)
    }

    /// The cooldown in force for a camera and class (0 when none).
    pub fn cooldown_ms(&self, camera: CameraId, class: Class) -> Millis {
        self.timeliness
            .get(&EpisodeKey { camera, class })
            .map_or(0, |e| e.cooldown_ms)
    }

    pub fn source_health(&self, key: &str) -> SourceHealth {
        self.health.get(key).copied().unwrap_or_default()
    }

    /// Mutes a camera until `until_ms` (at most 24 h ahead). A mute is the
    /// only thing that stops always-alert rules, and it is always explicit
    /// and time-limited.
    pub fn mute(&mut self, camera: CameraId, until_ms: Millis, now: Now) {
        let until = until_ms.min(now.ms + MAX_MUTE_MS);
        if until > now.ms {
            self.mutes.insert(camera, until);
        } else {
            self.mutes.remove(&camera);
        }
    }

    pub fn unmute(&mut self, camera: CameraId) {
        self.mutes.remove(&camera);
    }

    fn muted_until(&self, camera: CameraId, now: Millis) -> Option<Millis> {
        self.mutes.get(&camera).copied().filter(|&u| u > now)
    }

    fn remember(&mut self, id: u64) -> bool {
        if self.recent.contains(&id) {
            return false;
        }
        if self.recent.len() >= RECENT_IDS {
            self.recent.pop_front();
        }
        self.recent.push_back(id);
        true
    }

    fn finish(
        &self,
        mut trace: Trace,
        camera: Option<CameraId>,
        class: Class,
        outcome: Outcome,
        incident: Option<String>,
        now: Now,
    ) -> Decision {
        trace.push(
            now.ms,
            StepKind::Decision {
                outcome: outcome.clone(),
            },
        );
        let explanation = explain(&trace, &self.settings, now.utc_offset_min);
        Decision {
            trigger_id: trace.trigger_id,
            camera,
            class,
            outcome,
            incident,
            explanation,
            decided_ms: now.ms,
            trace,
        }
    }

    fn done_at_gate(
        &self,
        trace: Trace,
        steps: &mut Vec<Step>,
        camera: Option<CameraId>,
        class: Class,
        outcome: Outcome,
        now: Now,
    ) -> Next {
        let d = self.finish(trace, camera, class, outcome, None, now);
        if let Some(s) = d.trace.steps.last() {
            steps.push(s.clone());
        }
        Next::Done(Box::new(d))
    }

    /// Takes in one normalized trigger.
    pub fn intake(&mut self, trigger: Trigger, now: Now) -> Intake {
        let id = trigger.id;
        let mut trace = Trace::new(id);
        let mut steps = Vec::new();
        let class = trigger.kind.class();
        let late = trigger.is_late(self.settings.timing.stale_ms());
        let sensor = match (&trigger.source, &trigger.origin) {
            (Source::External, Some(key)) => self.settings.source(key).map(|(i, m)| {
                let mut m = m.clone();
                // A simulated trigger for one mapped camera.
                if let Some(only) = trigger.only_camera {
                    m.cameras.retain(|c| c.camera == only);
                }
                (i, m)
            }),
            _ => None,
        };

        if trigger.state == TriggerState::Stop {
            if let Some(cam) = trigger.camera {
                let tl = if trigger.test {
                    &mut self.test_timeliness
                } else {
                    &mut self.timeliness
                };
                tl.device_stop(&EpisodeKey { camera: cam, class }, &self.settings.timing);
            }
            return Intake {
                trigger_id: id,
                steps,
                next: Next::Ignored,
            };
        }

        let received = StepKind::TriggerReceived {
            source: trigger.source,
            kind: trigger.kind.clone(),
            class,
            camera: trigger.camera,
            sensor: sensor.as_ref().map(|(i, _)| *i as u32),
            event_ms: trigger.at_ms,
            late,
            age_ms: trigger.age_ms(),
        };

        // Retriggers of an open external check join it.
        if let Some((idx, mapping)) = &sensor
            && let Some((&open_id, open)) = self.open.iter_mut().find(|(_, o)| {
                o.trigger.test == trigger.test
                    && o.sensor.as_ref().is_some_and(|(i, _)| i == idx)
                    && o.window.is_some_and(|w| w.is_open(trigger.at_ms))
            })
        {
            if !self.recent.contains(&id) {
                let h = self.health.entry(mapping.key.clone()).or_default();
                h.retriggers += 1;
                h.last_fired_ms = Some(trigger.received_ms);
            }
            let w = open.window.as_mut().expect("checked");
            w.extend(trigger.at_ms, mapping);
            let until = w.until_ms;
            if let Some(c) = open.counter.as_mut() {
                c.extend_until(until);
            }
            open.deadline_ms = open
                .deadline_ms
                .max(until + self.settings.timing.pending_timeout_ms());
            steps.push(open.trace.push(
                now.ms,
                StepKind::Retrigger {
                    trigger_id: id,
                    until_ms: until,
                },
            ));
            self.remember(id);
            return Intake {
                trigger_id: open_id,
                steps,
                next: Next::Extended {
                    trigger_id: open_id,
                    until_ms: until,
                },
            };
        }

        steps.push(trace.push(now.ms, received));

        if !self.remember(id) {
            steps.push(trace.push(
                now.ms,
                StepKind::Gate {
                    result: GateResult::Duplicate,
                },
            ));
            let next = self.done_at_gate(
                trace,
                &mut steps,
                trigger.camera,
                class,
                Outcome::Suppress {
                    reason: SuppressReason::Duplicate,
                },
                now,
            );
            return Intake {
                trigger_id: id,
                steps,
                next,
            };
        }

        if trigger.source == Source::External {
            return self.intake_external(trigger, sensor, trace, steps, late, now);
        }
        self.intake_camera(trigger, trace, steps, late, now)
    }

    fn intake_external(
        &mut self,
        trigger: Trigger,
        sensor: Option<(usize, SourceMapping)>,
        mut trace: Trace,
        mut steps: Vec<Step>,
        late: bool,
        now: Now,
    ) -> Intake {
        let id = trigger.id;
        let class = trigger.kind.class();
        let Some((idx, mapping)) = sensor else {
            steps.push(trace.push(
                now.ms,
                StepKind::Gate {
                    result: GateResult::UnknownSource,
                },
            ));
            let next = self.done_at_gate(
                trace,
                &mut steps,
                None,
                class,
                Outcome::Suppress {
                    reason: SuppressReason::UnknownSource,
                },
                now,
            );
            return Intake {
                trigger_id: id,
                steps,
                next,
            };
        };
        let h = self.health.entry(mapping.key.clone()).or_default();
        h.fired += 1;
        h.last_fired_ms = Some(trigger.received_ms);
        steps.push(trace.push(
            now.ms,
            StepKind::Gate {
                result: GateResult::Pass,
            },
        ));

        let pending = self.settings.timing.pending_timeout_ms();
        if mapping.require == Require::None {
            self.open_check(Open {
                trigger,
                trace,
                counter: None,
                sensor: Some((idx, mapping)),
                window: None,
                deadline_ms: now.ms + pending,
                late,
            });
            return Intake {
                trigger_id: id,
                steps,
                next: Next::DecideNow,
            };
        }

        let window = Window::new(trigger.at_ms, &mapping);
        let angles: Vec<Angle> = mapping
            .cameras
            .iter()
            .map(|c| Angle {
                camera: c.camera,
                roi: c.roi.clone(),
                zones: self.settings.camera(c.camera).zones,
                weight: c.weight,
            })
            .collect();
        let spec = CheckSpec {
            angles,
            classes: mapping.classes.clone(),
            min_score: mapping.min_score,
            k: mapping.k,
            from_ms: window.from_ms,
            until_ms: window.until_ms,
        };
        let cameras: Vec<CameraId> = mapping.cameras.iter().map(|c| c.camera).collect();
        steps.push(trace.push(
            now.ms,
            StepKind::Sampling {
                cameras: cameras.clone(),
                from_ms: window.from_ms,
                until_ms: window.until_ms,
                classes: mapping.classes.clone(),
                k: mapping.k,
            },
        ));
        let next = Next::Sample {
            cameras,
            from_ms: window.from_ms,
            until_ms: window.until_ms,
            classes: mapping.classes.clone(),
        };
        self.open_check(Open {
            trigger,
            trace,
            counter: Some(Counter::new(spec)),
            sensor: Some((idx, mapping)),
            window: Some(window),
            deadline_ms: window.until_ms + pending,
            late,
        });
        Intake {
            trigger_id: id,
            steps,
            next,
        }
    }

    fn intake_camera(
        &mut self,
        trigger: Trigger,
        mut trace: Trace,
        mut steps: Vec<Step>,
        late: bool,
        now: Now,
    ) -> Intake {
        let id = trigger.id;
        let class = trigger.kind.class();
        let Some(cam) = trigger.camera else {
            steps.push(trace.push(
                now.ms,
                StepKind::Gate {
                    result: GateResult::NoCamera,
                },
            ));
            let next = self.done_at_gate(
                trace,
                &mut steps,
                None,
                class,
                Outcome::Suppress {
                    reason: SuppressReason::NoCamera,
                },
                now,
            );
            return Intake {
                trigger_id: id,
                steps,
                next,
            };
        };
        let cs = self.settings.camera(cam);
        let ctx = context(&self.settings, trigger.at_ms, now.utc_offset_min);
        let could = could_be_always(class, cs.exposure, ctx.night, &self.settings.always_alert);
        let key = EpisodeKey { camera: cam, class };
        let test = trigger.test;

        let gate: Option<(GateResult, Outcome)> = if let Some(until) = self.muted_until(cam, now.ms)
        {
            Some((
                GateResult::Muted { until_ms: until },
                Outcome::Suppress {
                    reason: SuppressReason::Muted { until_ms: until },
                },
            ))
        } else if class == Class::Other {
            Some((
                GateResult::UnknownKind,
                Outcome::Quiet {
                    reason: QuietReason::UnknownKind,
                },
            ))
        } else if could.is_some() {
            None
        } else if !schedule_allows(&cs.schedule, &ctx) {
            Some((
                GateResult::Schedule,
                Outcome::Suppress {
                    reason: SuppressReason::Schedule,
                },
            ))
        } else if class == Class::Motion && !cs.verify_motion {
            Some((
                GateResult::MotionOnly,
                Outcome::Quiet {
                    reason: QuietReason::MotionOnly,
                },
            ))
        } else if class != Class::Motion && !cs.class_enabled(class) {
            Some((
                GateResult::ClassOff,
                Outcome::Suppress {
                    reason: SuppressReason::ClassOff,
                },
            ))
        } else if let Some(left) = tl!(self, test).cooldown_left(&key, now.ms)
            && !tl!(self, test).probe_due(&key, now.ms, self.settings.timing.check_spacing_ms())
        {
            // Inside the cooldown, and a check for this camera and class
            // started moments ago: a repeat of the same event. (Later ones
            // are looked at and logged; see `probe_due`.)
            let since = tl!(self, test)
                .get(&key)
                .and_then(|e| e.last_alert_ms)
                .map_or(0, |t| now.ms - t);
            tl!(self, test).touch(key, now.ms);
            Some((
                GateResult::Cooldown { left_ms: left },
                Outcome::Suppress {
                    reason: SuppressReason::Cooldown {
                        left_ms: left,
                        since_ms: since,
                    },
                },
            ))
        } else {
            None
        };
        if let Some((result, outcome)) = gate {
            steps.push(trace.push(now.ms, StepKind::Gate { result }));
            if test {
                let until = self
                    .timeliness
                    .cooldown_left(&key, now.ms)
                    .map(|l| now.ms + l);
                steps.push(trace.push(
                    now.ms,
                    StepKind::RealPreview {
                        cooldown_until_ms: until,
                        admission: None,
                    },
                ));
            }
            let next = self.done_at_gate(trace, &mut steps, Some(cam), class, outcome, now);
            return Intake {
                trigger_id: id,
                steps,
                next,
            };
        }
        let result = match (could, tl!(self, test).cooldown_left(&key, now.ms)) {
            (Some(rule), _) => GateResult::PassAlways { rule },
            (None, Some(left_ms)) => {
                tl!(self, test).note_probe(&key, now.ms);
                GateResult::PassCooldown { left_ms }
            }
            (None, None) => GateResult::Pass,
        };
        steps.push(trace.push(now.ms, StepKind::Gate { result }));

        // What to look for.
        let classes: Vec<Class> = if class.is_detector_class() {
            vec![class]
        } else if class == Class::Motion {
            let mut c: Vec<Class> = [Class::Person, Class::Vehicle, Class::Animal]
                .into_iter()
                .filter(|&c| cs.verify_motion && cs.class_enabled(c))
                .collect();
            if could.is_some() && !c.contains(&Class::Person) {
                c.insert(0, Class::Person);
            }
            c
        } else {
            Vec::new()
        };
        let pending = self.settings.timing.pending_timeout_ms();
        if classes.is_empty() {
            self.open_check(Open {
                trigger,
                trace,
                counter: None,
                sensor: None,
                window: None,
                deadline_ms: now.ms + pending,
                late,
            });
            return Intake {
                trigger_id: id,
                steps,
                next: Next::DecideNow,
            };
        }
        if class.is_detector_class() {
            tl!(self, test).touch(key, now.ms);
        }
        let v = self.settings.verification;
        let from = trigger.at_ms - Millis::from(v.window_before_s) * SECOND_MS;
        let until =
            trigger.received_ms.max(trigger.at_ms) + Millis::from(v.window_after_s) * SECOND_MS;
        let spec = CheckSpec {
            angles: vec![Angle {
                camera: cam,
                roi: Default::default(),
                zones: cs.zones.clone(),
                weight: 1.0,
            }],
            classes: classes.clone(),
            min_score: v.min_score,
            k: f32::from(v.k),
            from_ms: from,
            until_ms: until,
        };
        steps.push(trace.push(
            now.ms,
            StepKind::Sampling {
                cameras: vec![cam],
                from_ms: from,
                until_ms: until,
                classes: classes.clone(),
                k: f32::from(v.k),
            },
        ));
        self.open_check(Open {
            trigger,
            trace,
            counter: Some(Counter::new(spec)),
            sensor: None,
            window: None,
            deadline_ms: until + pending,
            late,
        });
        Intake {
            trigger_id: id,
            steps,
            next: Next::Sample {
                cameras: vec![cam],
                from_ms: from,
                until_ms: until,
                classes,
            },
        }
    }

    fn open_check(&mut self, open: Open) {
        // Bounded: the oldest open check is dropped from the table (its
        // host will find no check to decide; tick never misses it because
        // this only happens past MAX_OPEN).
        if self.open.len() >= MAX_OPEN
            && let Some(&oldest) = self.open.keys().next()
        {
            self.open.remove(&oldest);
        }
        self.open.insert(open.trigger.id, open);
    }

    /// The host started a camera's stream for a check.
    pub fn camera_woken(
        &mut self,
        trigger_id: u64,
        camera: CameraId,
        first_picture_ms: Option<u32>,
        now: Now,
    ) -> Option<Step> {
        let open = self.open.get_mut(&trigger_id)?;
        Some(open.trace.push(
            now.ms,
            StepKind::CameraWoken {
                camera,
                first_picture_ms,
            },
        ))
    }

    /// One picture's detections for a check.
    pub fn sample(&mut self, trigger_id: u64, sample: &Sample, now: Now) -> Option<SampleProgress> {
        let open = self.open.get_mut(&trigger_id)?;
        let counter = open.counter.as_mut()?;
        let mut steps = Vec::new();
        let Some(judged) = counter.add(sample) else {
            return Some(SampleProgress {
                steps,
                reached: counter.reached(),
            });
        };
        steps.push(
            open.trace
                .push(now.ms, frame_step(sample.camera, sample.at_ms, judged)),
        );
        let needed = counter.spec().k;
        let n = counter.n();
        let progress: Vec<StepKind> = counter
            .counts()
            .iter()
            .filter(|(_, c)| c.hits > 0)
            .map(|(class, c)| StepKind::Progress {
                class: *class,
                weighted: c.weighted,
                needed,
                hits: c.hits,
                n,
                angles: c.angles(),
            })
            .collect();
        let reached = counter.reached();
        for p in progress {
            steps.push(open.trace.push(now.ms, p));
        }
        Some(SampleProgress { steps, reached })
    }

    /// Decides an open check.
    pub fn decide(&mut self, trigger_id: u64, inputs: DecideInputs, now: Now) -> Option<Decision> {
        let open = self.open.remove(&trigger_id)?;
        Some(self.decide_open(open, inputs, now))
    }

    fn decide_open(&mut self, open: Open, inputs: DecideInputs, now: Now) -> Decision {
        let Open {
            trigger,
            mut trace,
            counter,
            sensor,
            late,
            ..
        } = open;
        let trigger_class = trigger.kind.class();
        let check = counter.as_ref().map(Counter::result);
        let incident = inputs.incident.clone();

        // Support, the final camera, and the final class.
        let (camera, class, support, forced_unverified) = if let Some((_, mapping)) = &sensor {
            let first = mapping.cameras.first().map(|c| c.camera);
            let h = self.health.entry(mapping.key.clone()).or_default();
            if mapping.require == Require::None {
                h.unconfirmed_alerts += 1;
                (first, Class::Sensor, Support::SensorOnly, true)
            } else {
                let check = check.unwrap_or(Check::Unavailable);
                trace.push(
                    now.ms,
                    StepKind::Checked {
                        check,
                        agreed_with_device: None,
                    },
                );
                match check {
                    Check::Verified { class, .. } => {
                        h.confirmed += 1;
                        let cam = counter.as_ref().and_then(Counter::best_camera).or(first);
                        (cam, class, Support::Verified, false)
                    }
                    Check::Unavailable => {
                        h.could_not_look += 1;
                        if mapping.on_unavailable == FailPolicy::AlertUnverified {
                            h.unconfirmed_alerts += 1;
                            (first, Class::Sensor, Support::SensorOnly, true)
                        } else {
                            let o = Outcome::Quiet {
                                reason: QuietReason::CouldNotLook,
                            };
                            return self.finish(trace, first, Class::Sensor, o, incident, now);
                        }
                    }
                    Check::Weak { n, .. } | Check::NotSeen { n, .. } => {
                        h.nothing_seen += 1;
                        let hits = if let Check::Weak { hits, .. } = check {
                            hits
                        } else {
                            0
                        };
                        let reason = match check {
                            Check::NotSeen {
                                outside_region: true,
                                ..
                            } => QuietReason::OutsideRegion,
                            _ => QuietReason::NothingSeen { hits, n },
                        };
                        let o = Outcome::Quiet { reason };
                        return self.finish(trace, first, Class::Sensor, o, incident, now);
                    }
                }
            }
        } else {
            let cam = trigger.camera;
            let (class, support) = if trigger_class.is_self_evident() {
                (trigger_class, Support::SelfEvident)
            } else if matches!(trigger_class, Class::Face | Class::Package) {
                (trigger_class, Support::DeviceOnly)
            } else {
                let check = check.unwrap_or(Check::Unavailable);
                // Agreement with the camera's own AI, when we could look.
                let agreed = (trigger.source == Source::Device
                    && trigger_class.is_detector_class()
                    && check != Check::Unavailable)
                    .then(|| check.is_verified());
                trace.push(
                    now.ms,
                    StepKind::Checked {
                        check,
                        agreed_with_device: agreed,
                    },
                );
                match check {
                    Check::Verified { class, .. } => (class, Support::Verified),
                    Check::Weak { class, .. } => (class, Support::Weak),
                    Check::NotSeen { .. } => (
                        if trigger_class == Class::Motion {
                            Class::Motion
                        } else {
                            trigger_class
                        },
                        Support::NotSeen,
                    ),
                    Check::Unavailable => (trigger_class, Support::Unavailable),
                }
            };
            (cam, class, support, false)
        };

        // Motion that turned out to be nothing.
        let Some(cam) = camera else {
            let o = Outcome::Suppress {
                reason: SuppressReason::NoCamera,
            };
            return self.finish(trace, None, class, o, incident, now);
        };
        let cs = self.settings.camera(cam);
        let ctx = context(&self.settings, trigger.at_ms, now.utc_offset_min);
        let input = RuleInput {
            class,
            source: trigger.source,
            support,
            exposure: cs.exposure,
            entrance: cs.entrance,
            trust_camera_ai: cs.trust_camera_ai,
            night: ctx.night,
            mode: self.settings.mode,
        };
        let mut rules = evaluate(&input, &self.settings.always_alert);
        rules.unverified |= forced_unverified;
        trace.push(
            now.ms,
            StepKind::Rules {
                camera: Some(cam),
                class,
                night: ctx.night,
                basis: ctx.basis,
                exposure: cs.exposure,
                entrance: cs.entrance,
                mode: self.settings.mode,
                support,
                always: rules.always,
                severity: rules.severity,
                unverified: rules.unverified,
            },
        );
        let key = EpisodeKey { camera: cam, class };
        let test = trigger.test;
        let quiet = |reason| Outcome::Quiet { reason };

        // A mute stops everything, always-alert included.
        if let Some(until) = self.muted_until(cam, now.ms) {
            tl!(self, test).discard(&key);
            let o = Outcome::Suppress {
                reason: SuppressReason::Muted { until_ms: until },
            };
            return self.finish(trace, Some(cam), class, o, incident, now);
        }

        let Some(mut severity) = rules.severity else {
            tl!(self, test).discard(&key);
            let reason = match check {
                Some(Check::NotSeen {
                    outside_region: true,
                    ..
                }) => QuietReason::OutsideRegion,
                _ if class == Class::Motion => QuietReason::MotionOnly,
                _ => QuietReason::NotConfirmed,
            };
            return self.finish(trace, Some(cam), class, quiet(reason), incident, now);
        };

        if rules.always.is_none() {
            // The gate let it through as a possible always-alert case, or it
            // came from a sensor: the camera's own settings apply now.
            if !schedule_allows(&cs.schedule, &ctx) {
                tl!(self, test).discard(&key);
                let o = Outcome::Suppress {
                    reason: SuppressReason::Schedule,
                };
                return self.finish(trace, Some(cam), class, o, incident, now);
            }
            if class != Class::Sensor && !cs.class_enabled(class) {
                tl!(self, test).discard(&key);
                let o = Outcome::Suppress {
                    reason: SuppressReason::ClassOff,
                };
                return self.finish(trace, Some(cam), class, o, incident, now);
            }
        }

        // Learned rarity: advisory, one step, never on always-alert.
        let learning = self.settings.learning;
        let effect = if rules.always.is_some() {
            RarityEffect::IgnoredAlways
        } else if !learning.enabled {
            RarityEffect::LearningOff
        } else {
            match inputs.rarity {
                Some(r) if severity == Severity::Low && r.one_in >= learning.promote_one_in => {
                    severity = Severity::Normal;
                    RarityEffect::Promoted
                }
                Some(r) if severity == Severity::Normal && r.one_in <= learning.demote_one_in => {
                    severity = Severity::Low;
                    RarityEffect::Demoted
                }
                _ => RarityEffect::None,
            }
        };
        if inputs.rarity.is_some() {
            trace.push(
                now.ms,
                StepKind::Rarity {
                    one_in: inputs.rarity.map(|r| r.one_in),
                    effect,
                },
            );
        }

        if rules.always.is_none() {
            if severity < self.settings.min_alert_severity {
                tl!(self, test).discard(&key);
                let o = quiet(QuietReason::LowSeverity { severity });
                return self.finish(trace, Some(cam), class, o, incident, now);
            }
            if late {
                tl!(self, test).discard(&key);
                let o = quiet(QuietReason::Late {
                    age_ms: trigger.age_ms(),
                });
                return self.finish(trace, Some(cam), class, o, incident, now);
            }
        }

        if test {
            // What a real trigger would meet right now, without touching
            // any real episode.
            let real = &self.timeliness;
            let would = real.preview_admit(
                key,
                severity,
                rules.always.is_some(),
                incident.as_deref(),
                now.ms,
                &self.settings.timing,
            );
            trace.push(
                now.ms,
                StepKind::RealPreview {
                    cooldown_until_ms: real.cooldown_left(&key, now.ms).map(|l| now.ms + l),
                    admission: Some(would),
                },
            );
            // Alert IDs stay unique across both books.
            self.test_timeliness
                .set_next_alert_id(self.timeliness.next_alert_id());
        }
        let admission = tl!(self, test).admit(
            key,
            severity,
            rules.always.is_some(),
            incident.as_deref(),
            now.ms,
            &self.settings.timing,
        );
        if test {
            self.timeliness
                .set_next_alert_id(self.test_timeliness.next_alert_id());
        }
        trace.push(
            now.ms,
            StepKind::Timeliness {
                admission,
                incident: incident.clone(),
            },
        );
        let outcome = match admission {
            Admission::New { alert_id } => Outcome::Alert {
                severity,
                alert_id,
                rev: 1,
                late,
                unverified: rules.unverified,
                always: rules.always,
                quieted: None,
            },
            Admission::Update { alert_id, rev, .. } => Outcome::Alert {
                severity,
                alert_id,
                rev,
                late,
                unverified: rules.unverified,
                always: rules.always,
                quieted: None,
            },
            // A real detection inside the cooldown: logged with its
            // evidence as an alert, but nothing interrupts the user.
            Admission::Cooldown {
                alert_id,
                rev,
                left_ms,
                since_ms,
            } => Outcome::Alert {
                severity,
                alert_id,
                rev,
                late,
                unverified: rules.unverified,
                always: rules.always,
                quieted: Some(Quieted::Cooldown { left_ms, since_ms }),
            },
        };
        self.finish(trace, Some(cam), class, outcome, incident, now)
    }

    /// A "still present" re-check result for an alerted episode.
    pub fn observe(
        &mut self,
        camera: CameraId,
        class: Class,
        seen: bool,
        now: Now,
    ) -> Option<EpisodeState> {
        self.timeliness.observe(
            &EpisodeKey { camera, class },
            seen,
            now.ms,
            &self.settings.timing,
        )
    }

    /// Settles idle episodes, ends expired mutes, and decides checks whose
    /// deadline passed.
    pub fn tick(&mut self, now: Now) -> Tick {
        self.mutes.retain(|_, &mut until| until > now.ms);
        let due: Vec<u64> = self
            .open
            .iter()
            .filter(|(_, o)| o.deadline_ms <= now.ms)
            .map(|(&id, _)| id)
            .collect();
        let mut decided = Vec::new();
        for id in due {
            if let Some(open) = self.open.remove(&id) {
                decided.push(self.decide_open(open, DecideInputs::default(), now));
            }
        }
        let settled_tests = self.test_timeliness.tick(now.ms, &self.settings.timing);
        let settled = self.timeliness.tick(now.ms, &self.settings.timing);
        Tick {
            settled,
            settled_tests,
            decided,
        }
    }
}
