//! Simulated (test) triggers keep their own episodes and cooldowns: a test
//! never opens, extends, or uses a real one, real activity never quiets a
//! test, and a test's explanation says what a real trigger would meet now.

use crate::common::*;
use alerts::decision::{Outcome, Quieted};
use alerts::time::SECOND_MS;
use alerts::{AlertSettings, CameraId, Class, DecideInputs, Decision, Engine};

fn engine() -> Engine {
    Engine::new(AlertSettings::default()).0
}

fn car(c: CameraId) -> Vec<(CameraId, i64, Vec<alerts::Detection>)> {
    vec![
        (c, 200, vec![det(Class::Vehicle, 0.8)]),
        (c, 1200, vec![det(Class::Vehicle, 0.75)]),
    ]
}

fn go(e: &mut Engine, id: u64, t: i64, test: bool) -> Decision {
    let c = cam(0);
    let mut trig = device(id, c, "vehicle", t);
    trig.test = test;
    run(e, trig, car(c), DecideInputs::default())
}

fn quieted(d: &Decision) -> bool {
    matches!(
        d.outcome,
        Outcome::Alert {
            quieted: Some(Quieted::Cooldown { .. }),
            ..
        }
    )
}

#[test]
fn test_then_real_alerts_normally() {
    let mut e = engine();
    let t = at(12, 0);
    let d = go(&mut e, 1, t, true);
    assert!(d.outcome.is_alert() && !quieted(&d), "{:?}", d.outcome);
    assert!(
        d.explanation
            .ends_with("A real trigger now would alert normally."),
        "{}",
        d.explanation
    );
    // A real trigger moments later is a fresh episode, not in cooldown.
    let d = go(&mut e, 2, t + 5 * SECOND_MS, false);
    let Outcome::Alert {
        quieted: None,
        rev: 1,
        alert_id,
        ..
    } = d.outcome
    else {
        panic!("{:?}", d.outcome)
    };
    assert_eq!(alert_id, 2, "alert IDs stay unique across both books");
    assert!(!d.explanation.contains("real trigger"), "{}", d.explanation);
}

#[test]
fn real_then_test_runs_fully_and_reports_the_real_cooldown() {
    let mut e = engine();
    let t = at(12, 0);
    assert!(go(&mut e, 1, t, false).outcome.is_alert());
    let t2 = t + 20 * SECOND_MS;
    let d = go(&mut e, 2, t2, true);
    // The test is a full alert of its own, not quieted by the real one.
    let Outcome::Alert {
        quieted: None,
        rev: 1,
        ..
    } = d.outcome
    else {
        panic!("{:?}", d.outcome)
    };
    // 12:01 is when the real cooldown (1 min from 12:00) ends.
    assert!(
        d.explanation
            .ends_with("A real trigger now would be in cooldown until 12:01."),
        "{}",
        d.explanation
    );
    // The real episode is untouched: it still has its own cooldown, and the
    // test did not extend it.
    assert_eq!(e.cooldown_ms(cam(0), Class::Vehicle), 60 * SECOND_MS);
}

#[test]
fn repeated_tests_keep_their_own_cooldown() {
    let mut e = engine();
    let t = at(12, 0);
    assert!(go(&mut e, 1, t, true).outcome.is_alert());
    let d = go(&mut e, 2, t + 10 * SECOND_MS, true);
    assert!(quieted(&d), "{:?}", d.outcome);
    // After the test cooldown, a test alerts again.
    let d = go(&mut e, 3, t + 90 * SECOND_MS, true);
    assert!(d.outcome.is_alert());
}

#[test]
fn a_test_does_not_open_a_real_episode() {
    let mut e = engine();
    go(&mut e, 1, at(12, 0), true);
    assert_eq!(e.cooldown_ms(cam(0), Class::Vehicle), 0);
    assert_eq!(e.status().episodes, 0);
}
