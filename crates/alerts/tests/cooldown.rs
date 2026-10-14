//! The cooldown governs outputs only: a second detection inside it is still
//! looked at and logged.

use crate::common::*;
use alerts::decision::{Outcome, Quieted};
use alerts::time::{Millis, SECOND_MS};
use alerts::{AlertSettings, CameraId, Class, DecideInputs, Detection, Engine, Next};

fn engine() -> Engine {
    Engine::new(AlertSettings::default()).0
}

type Frames = Vec<(CameraId, Millis, Vec<Detection>)>;

fn car(c: CameraId) -> Frames {
    vec![
        (c, 200, vec![det(Class::Vehicle, 0.8)]),
        (c, 1200, vec![det(Class::Vehicle, 0.75)]),
    ]
}

#[test]
fn second_vehicle_in_cooldown_is_verified_and_quieted() {
    let mut e = engine();
    let c = cam(0);
    let d = run(
        &mut e,
        device(1, c, "vehicle", at(12, 0)),
        car(c),
        DecideInputs::default(),
    );
    assert!(d.outcome.is_alert());
    let t = at(12, 0) + 10 * SECOND_MS;
    let intake = e.intake(device(2, c, "vehicle", t), now(t));
    assert!(
        matches!(intake.next, Next::Sample { .. }),
        "got {:?}",
        intake.next
    );
    for (c, off, dets) in car(c) {
        e.sample(
            intake.trigger_id,
            &sample(c, t + off, dets),
            now(t + off + 50),
        );
    }
    let d = e
        .decide(intake.trigger_id, DecideInputs::default(), now(t + 1300))
        .unwrap();
    let Outcome::Alert {
        alert_id,
        rev,
        quieted: Some(Quieted::Cooldown { .. }),
        ..
    } = d.outcome
    else {
        panic!("expected a quieted alert, got {:?}", d.outcome)
    };
    assert_eq!(
        (alert_id, rev),
        (1, 1),
        "no new revision inside the cooldown"
    );
    assert!(
        d.explanation
            .ends_with("Not notified: already alerted 10 s ago."),
        "{}",
        d.explanation
    );
}

#[test]
fn repeats_within_the_spacing_are_still_dropped_at_the_gate() {
    let mut e = engine();
    let c = cam(0);
    run(
        &mut e,
        device(1, c, "vehicle", at(12, 0)),
        car(c),
        DecideInputs::default(),
    );
    // The first check started inside the cooldown; the next one 3 s later
    // is a repeat.
    let t = at(12, 0) + 10 * SECOND_MS;
    let first = e.intake(device(2, c, "vehicle", t), now(t));
    assert!(matches!(first.next, Next::Sample { .. }));
    let t2 = t + 3 * SECOND_MS;
    let again = e.intake(device(3, c, "vehicle", t2), now(t2));
    assert!(matches!(again.next, Next::Done(_)));
    // 6 s after the check began, another one is looked at.
    let t3 = t + 6 * SECOND_MS;
    let later = e.intake(device(4, c, "vehicle", t3), now(t3));
    assert!(matches!(later.next, Next::Sample { .. }));
}

#[test]
fn an_always_alert_case_inside_the_cooldown_still_alerts() {
    let mut e = engine();
    let c = cam(0);
    let d = run(
        &mut e,
        device(1, c, "people", at(23, 0)),
        seen(c, 0.8),
        DecideInputs::default(),
    );
    assert!(d.outcome.is_alert());
    let t = at(23, 0) + 20 * SECOND_MS;
    let d = run(
        &mut e,
        device(2, c, "people", t),
        seen(c, 0.8),
        DecideInputs::default(),
    );
    let Outcome::Alert {
        quieted, always, ..
    } = d.outcome
    else {
        panic!("{:?}", d.outcome)
    };
    assert_eq!(quieted, None);
    assert!(always.is_some());
}
