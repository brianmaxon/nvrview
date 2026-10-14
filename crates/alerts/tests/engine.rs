//! Engine behavior on explicit times, with golden explanations.

use crate::common::*;
use alerts::decision::{Outcome, QuietReason, SuppressReason};
use alerts::episode::EpisodeState;
use alerts::model::{Exposure, Severity};
use alerts::rules::AlwaysRule;
use alerts::settings::{
    CameraSettings, FailPolicy, Require, Schedule, SourceCamera, SourceMapping,
};
use alerts::time::{MINUTE_MS, SECOND_MS};
use alerts::trace::{GateResult, StepKind};
use alerts::trigger::TriggerState;
use alerts::{AlertSettings, Class, DecideInputs, Engine, Next, Rarity};

fn engine(s: AlertSettings) -> Engine {
    Engine::new(s).0
}

fn alert(d: &alerts::Decision) -> (Severity, u64, u32, bool, Option<AlwaysRule>) {
    match d.outcome {
        Outcome::Alert {
            severity,
            alert_id,
            rev,
            unverified,
            always,
            ..
        } => (severity, alert_id, rev, unverified, always),
        ref o => panic!("expected an alert, got {o:?}: {}", d.explanation),
    }
}

#[test]
fn day_person_verified() {
    let mut e = engine(AlertSettings::default());
    let d = run(
        &mut e,
        device(1, cam(0), "people", at(12, 0)),
        seen(cam(0), 0.81),
        DecideInputs::default(),
    );
    assert_eq!(alert(&d), (Severity::Normal, 1, 1, false, None));
    assert_eq!(
        d.explanation,
        "Person on Camera 1 at 12:00 (day, outside camera). Verified in 2 of 2 pictures (0.81). The camera's AI agreed."
    );
    assert_eq!(
        e.episode_state(cam(0), Class::Person),
        Some(EpisodeState::Active)
    );
}

#[test]
fn night_person_outside_always_alerts() {
    let mut e = engine(AlertSettings::default());
    let d = run(
        &mut e,
        device(1, cam(0), "people", at(23, 0)),
        seen(cam(0), 0.81),
        DecideInputs::default(),
    );
    assert_eq!(
        alert(&d),
        (
            Severity::Critical,
            1,
            1,
            false,
            Some(AlwaysRule::NightOutsidePerson)
        )
    );
    assert_eq!(
        d.explanation,
        "Person on Camera 1 at 23:00 (night, outside camera). Verified in 2 of 2 pictures (0.81). The camera's AI agreed. Always alerts: a person on an outside camera at night."
    );
}

#[test]
fn disagreement_is_quiet_unless_trusted() {
    let mut e = engine(AlertSettings::default());
    let d = run(
        &mut e,
        device(1, cam(0), "people", at(23, 0)),
        empty(cam(0)),
        DecideInputs::default(),
    );
    assert_eq!(
        d.outcome,
        Outcome::Quiet {
            reason: QuietReason::NotConfirmed
        }
    );
    assert_eq!(
        d.explanation,
        "Person on Camera 1 at 23:00 (night, outside camera). The camera's AI reported it. Not alerted: our check did not confirm it."
    );
    // The pending episode was discarded.
    assert_eq!(
        e.episode_state(cam(0), Class::Person),
        Some(EpisodeState::Settled)
    );

    let mut s = AlertSettings::default();
    let mut c = CameraSettings::new(cam(0));
    c.trust_camera_ai = true;
    c.name = Some("Example Camera".into());
    s.cameras.push(c);
    let mut e = engine(s);
    let d = run(
        &mut e,
        device(1, cam(0), "people", at(23, 0)),
        empty(cam(0)),
        DecideInputs::default(),
    );
    assert_eq!(
        alert(&d),
        (
            Severity::Critical,
            1,
            1,
            true,
            Some(AlwaysRule::NightOutsidePerson)
        )
    );
    assert_eq!(
        d.explanation,
        "Person on Example Camera at 23:00 (night, outside camera). The camera's AI reported it. Always alerts: a person on an outside camera at night. Not confirmed by our check; alerted because this camera's AI is trusted."
    );
    // Trust does not turn a daytime disagreement into an alert.
    let d = run(
        &mut e,
        device(2, cam(0), "people", at(12, 0)),
        empty(cam(0)),
        DecideInputs::default(),
    );
    assert!(!d.outcome.is_alert());
}

#[test]
fn smoke_ignores_schedule_and_classes_but_not_a_mute() {
    let mut s = AlertSettings::default();
    let mut c = CameraSettings::new(cam(2));
    c.exposure = Exposure::Inside;
    c.schedule = Schedule::Windows { windows: vec![] };
    c.classes = vec![];
    s.cameras.push(c);
    let mut e = engine(s);
    let d = run(
        &mut e,
        audio(1, cam(2), "smoke_alarm", at(12, 0)),
        vec![],
        DecideInputs::default(),
    );
    assert_eq!(
        alert(&d),
        (Severity::Critical, 1, 1, false, Some(AlwaysRule::SmokeCo))
    );
    assert_eq!(
        d.explanation,
        "Smoke alarm sound on Camera 3 at 12:00 (day, inside camera). Always alerts: smoke and CO alarms."
    );
    // A person on the same camera is stopped by its schedule.
    let d = run(
        &mut e,
        device(2, cam(2), "people", at(12, 0)),
        seen(cam(2), 0.9),
        DecideInputs::default(),
    );
    assert_eq!(
        d.outcome,
        Outcome::Suppress {
            reason: SuppressReason::Schedule
        }
    );
    assert!(
        d.explanation
            .ends_with("Not alerted: outside this camera's alert schedule.")
    );

    e.mute(cam(2), at(12, 15), now(at(12, 1)));
    let d = run(
        &mut e,
        audio(3, cam(2), "co_alarm", at(12, 2)),
        vec![],
        DecideInputs::default(),
    );
    assert_eq!(
        d.outcome,
        Outcome::Suppress {
            reason: SuppressReason::Muted {
                until_ms: at(12, 15)
            }
        }
    );
    assert_eq!(
        d.explanation,
        "CO alarm sound on Camera 3 at 12:02. Not alerted: this camera is muted until 12:15."
    );
    // Mutes end on their own.
    e.tick(now(at(12, 16)));
    let d = run(
        &mut e,
        audio(4, cam(2), "co_alarm", at(12, 16)),
        vec![],
        DecideInputs::default(),
    );
    assert!(d.outcome.is_alert());
}

#[test]
fn doorbell_day_and_night() {
    let mut e = engine(AlertSettings::default());
    let d = run(
        &mut e,
        device(1, cam(5), "visitor", at(15, 0)),
        vec![],
        DecideInputs::default(),
    );
    assert_eq!(alert(&d), (Severity::High, 1, 1, false, None));
    assert_eq!(
        d.explanation,
        "Doorbell press on Camera 6 at 15:00 (day, outside camera)."
    );
    let d = run(
        &mut e,
        device(2, cam(5), "visitor", at(2, 0)),
        vec![],
        DecideInputs::default(),
    );
    assert_eq!(alert(&d).4, Some(AlwaysRule::DoorbellNight));
}

#[test]
fn cooldown_then_update() {
    let mut e = engine(AlertSettings::default());
    let d = run(
        &mut e,
        device(1, cam(0), "people", at(12, 0)),
        seen(cam(0), 0.8),
        DecideInputs::default(),
    );
    assert_eq!(alert(&d).2, 1);
    // A repeat 2 s later is the same event: stopped at the gate.
    let t = at(12, 0) + 2 * SECOND_MS;
    let intake = e.intake(device(2, cam(0), "people", t), now(t));
    let Next::Done(d) = intake.next else { panic!() };
    assert!(matches!(
        d.outcome,
        Outcome::Suppress {
            reason: SuppressReason::Cooldown { .. }
        }
    ));
    assert!(
        d.explanation
            .ends_with("Not alerted: already alerted 0 s ago."),
        "{}",
        d.explanation
    );
    let t = at(12, 1) + 5 * SECOND_MS;
    let d = run(
        &mut e,
        device(3, cam(0), "people", t),
        seen(cam(0), 0.8),
        DecideInputs::default(),
    );
    assert_eq!(alert(&d), (Severity::Normal, 1, 2, false, None));
    assert!(d.explanation.ends_with("Update: still present."));
    assert_eq!(e.cooldown_ms(cam(0), Class::Person), 5 * MINUTE_MS);
}

#[test]
fn late_triggers() {
    let mut e = engine(AlertSettings::default());
    let mut t = device(1, cam(0), "people", at(12, 2));
    t.at_ms = at(12, 0);
    let d = run(&mut e, t, seen(cam(0), 0.8), DecideInputs::default());
    assert_eq!(
        d.outcome,
        Outcome::Quiet {
            reason: QuietReason::Late {
                age_ms: 2 * MINUTE_MS
            }
        }
    );
    assert!(d.explanation.ends_with("Not alerted: reported 2 min late."));
    let mut t = audio(2, cam(1), "smoke_alarm", at(12, 4));
    t.at_ms = at(12, 0);
    let d = run(&mut e, t, vec![], DecideInputs::default());
    assert!(matches!(d.outcome, Outcome::Alert { late: true, .. }));
    assert!(
        d.explanation
            .ends_with("Always alerts: smoke and CO alarms. Reported 4 min late.")
    );
}

fn sensor_settings() -> AlertSettings {
    let mut s = AlertSettings::default();
    s.sources.push(SourceMapping::new(
        "sensor-1",
        vec![
            SourceCamera {
                camera: cam(0),
                roi: Default::default(),
                weight: 1.0,
            },
            SourceCamera {
                camera: cam(1),
                roi: Default::default(),
                weight: 1.0,
            },
        ],
    ));
    s
}

#[test]
fn external_trigger_needs_confirmation() {
    let mut e = engine(sensor_settings());
    let t0 = at(12, 0);
    let intake = e.intake(sensor(1, "sensor-1", t0), now(t0));
    let Next::Sample {
        cameras,
        from_ms,
        until_ms,
        ..
    } = intake.next
    else {
        panic!("{:?}", intake.next)
    };
    assert_eq!(cameras, vec![cam(0), cam(1)]);
    assert_eq!((from_ms, until_ms), (t0 - 2000, t0 + 8000));
    // The steps so far, for live display.
    assert!(matches!(
        intake.steps[0].kind,
        StepKind::TriggerReceived { .. }
    ));
    assert!(matches!(
        intake.steps[1].kind,
        StepKind::Gate {
            result: GateResult::Pass
        }
    ));
    assert!(matches!(intake.steps[2].kind, StepKind::Sampling { .. }));

    e.camera_woken(1, cam(1), Some(210), now(t0 + 50)).unwrap();
    let p = e
        .sample(1, &sample(cam(0), t0 + 300, vec![]), now(t0 + 400))
        .unwrap();
    assert!(!p.reached);
    let p = e
        .sample(
            1,
            &sample(cam(1), t0 + 300, vec![det(Class::Person, 0.7)]),
            now(t0 + 400),
        )
        .unwrap();
    assert!(!p.reached);
    assert!(matches!(
        p.steps[1].kind,
        StepKind::Progress { hits: 1, .. }
    ));
    // A retrigger extends the window.
    let intake = e.intake(sensor(2, "sensor-1", t0 + 6000), now(t0 + 6000));
    assert_eq!(
        intake.next,
        Next::Extended {
            trigger_id: 1,
            until_ms: t0 + 14_000
        }
    );
    let p = e
        .sample(
            1,
            &sample(cam(1), t0 + 12_000, vec![det(Class::Person, 0.66)]),
            now(t0 + 12_100),
        )
        .unwrap();
    assert!(p.reached);
    let d = e
        .decide(1, DecideInputs::default(), now(t0 + 12_200))
        .unwrap();
    assert_eq!(alert(&d), (Severity::Normal, 1, 1, false, None));
    assert_eq!(d.camera, Some(cam(1)));
    assert_eq!(
        d.explanation,
        "Person on Camera 2 at 12:00 (day, outside camera), after Sensor 1 fired. Verified in 2 of 3 pictures (0.70)."
    );
    let h = e.source_health("sensor-1");
    assert_eq!((h.fired, h.retriggers, h.confirmed), (1, 1, 1));

    // Nothing seen: the quiet log, and the false-trigger count.
    let t1 = at(13, 0);
    let intake = e.intake(sensor(3, "sensor-1", t1), now(t1));
    assert!(matches!(intake.next, Next::Sample { .. }));
    e.sample(3, &sample(cam(0), t1 + 500, vec![]), now(t1 + 600));
    e.sample(
        3,
        &sample(cam(1), t1 + 500, vec![det(Class::Person, 0.2)]),
        now(t1 + 600),
    );
    let d = e
        .decide(3, DecideInputs::default(), now(t1 + 9000))
        .unwrap();
    assert_eq!(
        d.outcome,
        Outcome::Quiet {
            reason: QuietReason::NothingSeen { hits: 0, n: 2 }
        }
    );
    assert_eq!(
        d.explanation,
        "Sensor 1 fired at 13:00. Not alerted: the cameras did not confirm it (0 of 2 pictures)."
    );
    assert_eq!(e.source_health("sensor-1").false_trigger_rate(), Some(0.5));
}

#[test]
fn external_without_pictures_and_without_confirmation() {
    let mut s = sensor_settings();
    s.sources[0].name = Some("Example sensor".into());
    let mut e = engine(s.clone());
    let t0 = at(12, 0);
    e.intake(sensor(1, "sensor-1", t0), now(t0));
    // The host never reports: decided at the deadline, quiet by default.
    let tick = e.tick(now(t0 + 60_000));
    assert_eq!(tick.decided.len(), 1);
    assert_eq!(
        tick.decided[0].explanation,
        "Example sensor fired at 12:00. Not alerted: no picture could be checked."
    );
    // With the fail policy set to alert.
    s.sources[0].on_unavailable = FailPolicy::AlertUnverified;
    let mut e = engine(s.clone());
    e.intake(sensor(1, "sensor-1", t0), now(t0));
    let d = e.tick(now(t0 + 60_000)).decided.remove(0);
    assert_eq!(alert(&d), (Severity::Normal, 1, 1, true, None));
    // A source allowed to alert alone.
    s.sources[0].require = Require::None;
    let mut e = engine(s);
    let intake = e.intake(sensor(1, "sensor-1", t0), now(t0));
    assert_eq!(intake.next, Next::DecideNow);
    let d = e.decide(1, DecideInputs::default(), now(t0)).unwrap();
    assert_eq!(alert(&d), (Severity::Normal, 1, 1, true, None));
    assert_eq!(
        d.explanation,
        "Example sensor fired at 12:00. Alerted on the sensor alone, unverified."
    );
    // Unknown sources are refused.
    let intake = e.intake(sensor(2, "other", t0), now(t0));
    let Next::Done(d) = intake.next else { panic!() };
    assert_eq!(
        d.outcome,
        Outcome::Suppress {
            reason: SuppressReason::UnknownSource
        }
    );
}

#[test]
fn abandoned_night_check_fails_open() {
    let mut e = engine(AlertSettings::default());
    let t0 = at(23, 30);
    let intake = e.intake(device(1, cam(0), "people", t0), now(t0));
    assert!(matches!(intake.next, Next::Sample { .. }));
    assert!(e.tick(now(t0 + 5_000)).decided.is_empty());
    let d = e.tick(now(t0 + 60_000)).decided.remove(0);
    assert_eq!(
        alert(&d),
        (
            Severity::Critical,
            1,
            1,
            true,
            Some(AlwaysRule::NightOutsidePerson)
        )
    );
    assert!(
        d.explanation
            .ends_with("No picture could be checked; alerted unverified.")
    );
}

#[test]
fn duplicates_stops_and_unknown_kinds() {
    let mut e = engine(AlertSettings::default());
    run(
        &mut e,
        device(1, cam(0), "people", at(12, 0)),
        seen(cam(0), 0.8),
        DecideInputs::default(),
    );
    let Next::Done(d) = e
        .intake(device(1, cam(0), "people", at(12, 0)), now(at(12, 0)))
        .next
    else {
        panic!()
    };
    assert_eq!(
        d.outcome,
        Outcome::Suppress {
            reason: SuppressReason::Duplicate
        }
    );
    let mut stop = device(2, cam(0), "people", at(12, 1));
    stop.state = TriggerState::Stop;
    assert_eq!(e.intake(stop, now(at(12, 1))).next, Next::Ignored);
    let Next::Done(d) = e
        .intake(device(3, cam(0), "crossline", at(12, 2)), now(at(12, 2)))
        .next
    else {
        panic!()
    };
    assert_eq!(
        d.outcome,
        Outcome::Quiet {
            reason: QuietReason::UnknownKind
        }
    );
    // The unknown text never reaches the explanation.
    assert!(!d.explanation.contains("crossline"), "{}", d.explanation);
}

#[test]
fn motion_and_classes() {
    let mut e = engine(AlertSettings::default());
    let d = run(
        &mut e,
        device(1, cam(0), "md", at(12, 0)),
        vec![],
        DecideInputs::default(),
    );
    assert_eq!(
        d.outcome,
        Outcome::Quiet {
            reason: QuietReason::MotionOnly
        }
    );
    let mut s = AlertSettings::default();
    let mut c = CameraSettings::new(cam(0));
    c.verify_motion = true;
    c.classes.retain(|&k| k != Class::Vehicle);
    s.cameras.push(c);
    let mut e = engine(s);
    let d = run(
        &mut e,
        device(1, cam(0), "md", at(12, 0)),
        seen(cam(0), 0.9),
        DecideInputs::default(),
    );
    assert_eq!(d.class, Class::Person);
    assert!(d.outcome.is_alert());
    let d = run(
        &mut e,
        device(2, cam(0), "vehicle", at(12, 5)),
        vec![],
        DecideInputs::default(),
    );
    assert_eq!(
        d.outcome,
        Outcome::Suppress {
            reason: SuppressReason::ClassOff
        }
    );
    assert!(
        d.explanation
            .ends_with("Not alerted: vehicle alerts are off for this camera.")
    );
}

#[test]
fn rarity_moves_one_step_and_never_always() {
    let mut e = engine(AlertSettings::default());
    let animal = vec![
        (cam(0), 200, vec![det(Class::Animal, 0.8)]),
        (cam(0), 1200, vec![det(Class::Animal, 0.8)]),
    ];
    let d = run(
        &mut e,
        device(1, cam(0), "dog_cat", at(12, 0)),
        animal.clone(),
        DecideInputs::default(),
    );
    assert_eq!(
        d.outcome,
        Outcome::Quiet {
            reason: QuietReason::LowSeverity {
                severity: Severity::Low
            }
        }
    );
    let rare = DecideInputs {
        rarity: Some(Rarity { one_in: 40 }),
        incident: None,
    };
    let d = run(
        &mut e,
        device(2, cam(0), "dog_cat", at(12, 10)),
        animal,
        rare,
    );
    assert_eq!(alert(&d).0, Severity::Normal);
    assert_eq!(
        d.explanation,
        "Animal on Camera 1 at 12:10 (day, outside camera). Verified in 2 of 2 pictures (0.80). The camera's AI agreed. Rare here at this hour (about 1 in 40)."
    );
    let common = DecideInputs {
        rarity: Some(Rarity { one_in: 1 }),
        incident: None,
    };
    let d = run(
        &mut e,
        device(3, cam(1), "people", at(12, 0)),
        seen(cam(1), 0.8),
        common.clone(),
    );
    assert!(
        d.explanation
            .ends_with("Common here at this hour. Not alerted: low priority here.")
    );
    // Never on an always-alert outcome.
    let d = run(
        &mut e,
        device(4, cam(1), "people", at(23, 0)),
        seen(cam(1), 0.8),
        common,
    );
    assert_eq!(alert(&d).0, Severity::Critical);
}

#[test]
fn also_seen_on_another_camera_of_the_incident() {
    let mut e = engine(AlertSettings::default());
    let inc = DecideInputs {
        rarity: None,
        incident: Some("i-1".into()),
    };
    run(
        &mut e,
        device(1, cam(0), "people", at(12, 0)),
        seen(cam(0), 0.8),
        inc.clone(),
    );
    let d = run(
        &mut e,
        device(2, cam(1), "people", at(12, 0) + 5000),
        seen(cam(1), 0.8),
        inc,
    );
    assert_eq!(alert(&d), (Severity::Normal, 1, 2, false, None));
    assert!(d.explanation.ends_with("Also seen after Camera 1."));
    assert_eq!(d.incident.as_deref(), Some("i-1"));
}

#[test]
fn decision_json_is_stable() {
    let mut e = engine(AlertSettings::default());
    let d = run(
        &mut e,
        audio(9, cam(2), "smoke_alarm", at(12, 0)),
        vec![],
        DecideInputs::default(),
    );
    let v: serde_json::Value = serde_json::to_value(&d).unwrap();
    assert_eq!(v["decision"], "alert");
    assert_eq!(v["severity"], "critical");
    assert_eq!(v["always"], "smoke_co");
    assert_eq!(v["class"], "smoke");
    assert_eq!(v["trace"]["version"], 1);
    let steps: Vec<&str> = v["trace"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["step"].as_str().unwrap())
        .collect();
    assert_eq!(
        steps,
        [
            "trigger_received",
            "gate",
            "rules",
            "timeliness",
            "decision"
        ]
    );
    assert_eq!(v["trace"]["steps"][1]["result"], "pass_always");
    assert_eq!(v["trace"]["steps"][3]["admission"], "new");
    let back: alerts::Decision = serde_json::from_value(v).unwrap();
    assert_eq!(back, d);
}
