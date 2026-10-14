//! Explanations: one short paragraph per decision, built by templates from
//! the typed steps of its trace. Only the user's own names from settings
//! (cameras, sensors) and numbers appear; nothing from a trigger's payload
//! does.

use crate::decision::{Outcome, QuietReason, Quieted, SuppressReason};
use crate::episode::{Admission, UpdateWhy};
use crate::model::{CameraId, Class, Mode};
use crate::rules::Support;
use crate::settings::AlertSettings;
use crate::time::{duration_words, hhmm, local};
use crate::trace::{RarityEffect, StepKind, Trace};
use crate::verify::Check;

fn camera_name(settings: &AlertSettings, c: CameraId) -> String {
    settings.camera(c).display_name()
}

fn sensor_name(settings: &AlertSettings, idx: u32) -> String {
    let i = idx as usize;
    settings
        .sources
        .get(i)
        .map_or_else(|| format!("Sensor {}", i + 1), |s| s.display_name(i))
}

/// Builds the explanation for a trace (its decision step included).
pub fn explain(trace: &Trace, settings: &AlertSettings, offset_min: i32) -> String {
    let mut out: Vec<String> = Vec::new();

    let received = trace.find(|k| match k {
        StepKind::TriggerReceived {
            class,
            camera,
            sensor,
            event_ms,
            late,
            age_ms,
            ..
        } => Some((*class, *camera, *sensor, *event_ms, *late, *age_ms)),
        _ => None,
    });
    let rules = trace.find(|k| match k {
        StepKind::Rules {
            camera,
            class,
            night,
            exposure,
            entrance,
            mode,
            support,
            always,
            unverified,
            ..
        } => Some((
            *camera,
            *class,
            *night,
            *exposure,
            *entrance,
            *mode,
            *support,
            *always,
            *unverified,
        )),
        _ => None,
    });
    let outcome = trace.find_last(|k| match k {
        StepKind::Decision { outcome } => Some(outcome.clone()),
        _ => None,
    });

    // Headline: what, where, when, and the context the rules used.
    if let Some((class, camera, sensor, event_ms, _, _)) = received {
        let time = hhmm(local(event_ms, offset_min).minute_of_day);
        let context = rules.map(|(_, _, night, exposure, entrance, mode, ..)| {
            let mut parts = vec![if night { "night" } else { "day" }, exposure.words()];
            if entrance {
                parts.push("entrance");
            }
            match mode {
                Mode::Away => parts.push("away mode"),
                Mode::Night => parts.push("night mode"),
                Mode::Home => {}
            }
            format!(" ({})", parts.join(", "))
        });
        let head = match (sensor, rules) {
            (Some(s), Some((Some(cam), rclass, ..))) if rclass != Class::Sensor => format!(
                "{} on {} at {time}{}, after {} fired.",
                rclass.noun(),
                camera_name(settings, cam),
                context.unwrap_or_default(),
                sensor_name(settings, s)
            ),
            (Some(s), _) => format!("{} fired at {time}.", sensor_name(settings, s)),
            (None, _) => match camera {
                Some(cam) => format!(
                    "{} on {} at {time}{}.",
                    class.noun(),
                    camera_name(settings, cam),
                    context.unwrap_or_default()
                ),
                None => format!("{} at {time}.", class.noun()),
            },
        };
        out.push(head);
    }

    // What the check found.
    let checked = trace.find_last(|k| match k {
        StepKind::Checked {
            check,
            agreed_with_device,
        } => Some((*check, *agreed_with_device)),
        _ => None,
    });
    if let Some((check, agreed)) = checked {
        match check {
            Check::Verified {
                hits,
                n,
                angles,
                score,
                moving,
                ..
            } => {
                let on = if angles > 1 {
                    format!(" on {angles} cameras")
                } else {
                    String::new()
                };
                let mv = match moving {
                    Some(true) => ", moving",
                    Some(false) => ", not moving",
                    None => "",
                };
                out.push(format!(
                    "Verified in {hits} of {n} pictures{on} ({score:.2}{mv})."
                ));
            }
            Check::Weak { hits, n, score, .. } => {
                out.push(format!("Seen in only {hits} of {n} pictures ({score:.2})."));
            }
            Check::NotSeen { .. } | Check::Unavailable => {}
        }
        match agreed {
            Some(true) => out.push("The camera's AI agreed.".into()),
            Some(false) => out.push("The camera's AI reported it.".into()),
            None => {}
        }
    }

    // Rules.
    if let Some((.., support, always, unverified)) = rules {
        if let Some(rule) = always {
            out.push(format!("Always alerts: {}.", rule.words()));
        }
        let is_alert = outcome.as_ref().is_some_and(Outcome::is_alert);
        if unverified && is_alert {
            out.push(
                match support {
                    Support::Weak | Support::NotSeen => {
                        "Not confirmed by our check; alerted because this camera's AI is trusted."
                    }
                    Support::Unavailable => "No picture could be checked; alerted unverified.",
                    Support::SensorOnly => "Alerted on the sensor alone, unverified.",
                    _ => "Alerted unverified.",
                }
                .into(),
            );
        } else if support == Support::DeviceOnly {
            out.push("From the camera's AI only; nvrview cannot check this kind.".into());
        } else if support == Support::SensorOnly {
            out.push("This sensor may alert without a camera check.".into());
        }
    }

    // Learned rarity.
    if let Some((one_in, effect)) = trace.find(|k| match k {
        StepKind::Rarity { one_in, effect } => Some((*one_in, *effect)),
        _ => None,
    }) {
        match (effect, one_in) {
            (RarityEffect::Promoted, Some(n)) => {
                out.push(format!("Rare here at this hour (about 1 in {n})."))
            }
            (RarityEffect::Demoted, _) => out.push("Common here at this hour.".into()),
            _ => {}
        }
    }

    // Timeliness.
    if let Some(Admission::Update {
        why, first_camera, ..
    }) = trace.find_last(|k| match k {
        StepKind::Timeliness { admission, .. } => Some(*admission),
        _ => None,
    }) {
        out.push(match why {
            UpdateWhy::StillPresent => "Update: still present.".into(),
            UpdateWhy::HigherSeverity => "Update: more urgent now.".into(),
            UpdateWhy::NowAlwaysAlert => "Update: now an always-alert case.".into(),
            UpdateWhy::AlwaysRepeat => "Update: still active.".into(),
            UpdateWhy::AlsoSeen => match first_camera {
                Some(c) => format!("Also seen after {}.", camera_name(settings, c)),
                None => "Also seen on another camera.".into(),
            },
        });
    }

    // Lateness and the decision.
    let late = received.filter(|r| r.4).map(|r| r.5);
    match outcome {
        Some(Outcome::Alert { late: true, .. }) => {
            out.push(format!(
                "Reported {} late.",
                duration_words(late.unwrap_or_default())
            ));
        }
        Some(Outcome::Alert {
            quieted: Some(Quieted::Cooldown { since_ms, .. }),
            ..
        }) => out.push(format!(
            "Not notified: already alerted {} ago.",
            duration_words(since_ms)
        )),
        Some(Outcome::Alert { .. }) | None => {}
        Some(Outcome::Quiet { reason }) => out.push(format!(
            "Not alerted: {}.",
            quiet_words(reason, settings, offset_min)
        )),
        Some(Outcome::Suppress { reason }) => {
            let class = rules.map(|r| r.1).or(received.map(|r| r.0));
            out.push(format!(
                "Not alerted: {}.",
                suppress_words(reason, class, offset_min)
            ))
        }
    }

    // A simulated trigger also says what a real one would meet right now.
    if let Some((until, admission)) = trace.find_last(|k| match k {
        StepKind::RealPreview {
            cooldown_until_ms,
            admission,
        } => Some((*cooldown_until_ms, *admission)),
        _ => None,
    }) {
        let until_words = |ms: i64| hhmm(local(ms, offset_min).minute_of_day);
        out.push(match (admission, until) {
            (Some(Admission::Cooldown { .. }), Some(u)) | (None, Some(u)) => format!(
                "A real trigger now would be in cooldown until {}.",
                until_words(u)
            ),
            (Some(Admission::Update { .. }), _) => {
                "A real trigger now would update the open alert.".into()
            }
            _ => "A real trigger now would alert normally.".into(),
        });
    }

    out.join(" ")
}

fn quiet_words(r: QuietReason, _settings: &AlertSettings, _offset: i32) -> String {
    match r {
        QuietReason::NotConfirmed => "our check did not confirm it".into(),
        QuietReason::NothingSeen { hits, n } => {
            format!("the cameras did not confirm it ({hits} of {n} pictures)")
        }
        QuietReason::OutsideRegion => "seen only outside the zones".into(),
        QuietReason::CouldNotLook => "no picture could be checked".into(),
        QuietReason::LowSeverity { .. } => "low priority here".into(),
        QuietReason::Late { age_ms } => format!("reported {} late", duration_words(age_ms)),
        QuietReason::MotionOnly => "motion with no person, vehicle, or animal reported".into(),
        QuietReason::UnknownKind => "an event of a kind nvrview does not know".into(),
        QuietReason::NoDecision => "the check did not finish".into(),
    }
}

fn suppress_words(r: SuppressReason, class: Option<Class>, offset_min: i32) -> String {
    match r {
        SuppressReason::Muted { until_ms } => format!(
            "this camera is muted until {}",
            hhmm(local(until_ms, offset_min).minute_of_day)
        ),
        SuppressReason::Schedule => "outside this camera's alert schedule".into(),
        SuppressReason::ClassOff => format!(
            "{} alerts are off for this camera",
            class.map_or("these", Class::lower)
        ),
        SuppressReason::Cooldown { since_ms, .. } => {
            format!("already alerted {} ago", duration_words(since_ms))
        }
        SuppressReason::Duplicate => "a repeat of the same event".into(),
        SuppressReason::UnknownSource => "from a source that is not set up".into(),
        SuppressReason::NoCamera => "no camera given".into(),
    }
}
