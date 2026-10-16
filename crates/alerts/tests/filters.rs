//! The cheap filters in the engine, on a simulated clock with synthetic
//! boxes: static-scene memory, moving versus parked, repeat suppression,
//! night score floors, and the insight layers.

use crate::common::*;
use alerts::decision::{Outcome, QuietReason};
use alerts::filters::book::FilterKind;
use alerts::insight::{LayerData, insights};
use alerts::model::Severity;
use alerts::rules::AlwaysRule;
use alerts::settings::CameraSettings;
use alerts::time::{MINUTE_MS, Millis};
use alerts::trace::StepKind;
use alerts::zone::BBox;
use alerts::{AlertSettings, CameraId, Class, DecideInputs, Decision, Detection, Engine, Next};

fn person_at(x: f32, score: f32) -> Detection {
    Detection {
        class: Class::Person,
        score,
        bbox: BBox::new(x, 0.4, 0.08, 0.3),
    }
}

/// A camera's own trigger with two pictures 1 s apart, boxes as given.
fn check(
    e: &mut Engine,
    id: u64,
    c: CameraId,
    t: Millis,
    a: Vec<Detection>,
    b: Vec<Detection>,
) -> Decision {
    let intake = e.intake(device(id, c, "people", t), now(t));
    let Next::Sample { .. } = intake.next else {
        panic!("expected sampling, got {:?}", intake.next);
    };
    e.sample(id, &sample(c, t + 200, a), now(t + 250));
    e.sample(id, &sample(c, t + 1200, b), now(t + 1250));
    e.decide(id, DecideInputs::default(), now(t + 1300))
        .unwrap()
}

fn filters_step(d: &Decision) -> Option<(u32, u32, Option<bool>, Option<FilterKind>)> {
    d.trace.find(|k| match k {
        StepKind::Filters {
            hits,
            stationary,
            moving,
            acted,
            ..
        } => Some((*hits, *stationary, *moving, *acted)),
        _ => None,
    })
}

fn severity(d: &Decision) -> Option<Severity> {
    match d.outcome {
        Outcome::Alert { severity, .. } => Some(severity),
        _ => None,
    }
}

#[test]
fn a_person_standing_still_at_night_is_not_critical_by_itself() {
    let mut e = Engine::new(AlertSettings::default()).0;
    let still = person_at(0.3, 0.7);
    let d = check(
        &mut e,
        1,
        cam(0),
        at(23, 0),
        vec![still.clone()],
        vec![still],
    );
    assert_eq!(severity(&d), Some(Severity::High), "{}", d.explanation);
    assert_eq!(
        filters_step(&d),
        Some((2, 0, Some(false), Some(FilterKind::Parked)))
    );
    assert!(
        d.explanation
            .contains("It did not move between the pictures, so it is not an always-alert case."),
        "{}",
        d.explanation
    );
    assert!(d.explanation.contains("(0.70, not moving)"));
    // Walking: the always-alert rule applies.
    let d = check(
        &mut e,
        2,
        cam(1),
        at(23, 0),
        vec![person_at(0.3, 0.7)],
        vec![person_at(0.36, 0.7)],
    );
    assert!(matches!(
        d.outcome,
        Outcome::Alert {
            severity: Severity::Critical,
            always: Some(AlwaysRule::NightOutsidePerson),
            ..
        }
    ));
    assert_eq!(filters_step(&d).unwrap().2, Some(true));
    // Turned off, parked changes nothing.
    let mut s = AlertSettings::default();
    s.filters.parked_blocks_always = false;
    let mut e = Engine::new(s).0;
    let still = person_at(0.3, 0.7);
    let d = check(
        &mut e,
        3,
        cam(0),
        at(23, 0),
        vec![still.clone()],
        vec![still],
    );
    assert_eq!(severity(&d), Some(Severity::Critical));
}

#[test]
fn the_same_boxes_again_do_not_alert_again() {
    let mut e = Engine::new(AlertSettings::default()).0;
    let a = vec![person_at(0.3, 0.7), person_at(0.6, 0.65)];
    let walk = |v: &[Detection]| -> Vec<Detection> {
        v.iter()
            .map(|d| Detection {
                bbox: BBox::new(d.bbox.x + 0.05, d.bbox.y, d.bbox.w, d.bbox.h),
                ..d.clone()
            })
            .collect()
    };
    let d1 = check(&mut e, 1, cam(0), at(12, 0), a.clone(), walk(&a));
    assert!(d1.outcome.is_alert());
    // 40 s later, the same two boxes: a repeat.
    let t2 = at(12, 0) + 40_000;
    let d2 = check(&mut e, 2, cam(0), t2, a.clone(), walk(&a));
    assert_eq!(
        d2.outcome,
        Outcome::Quiet {
            reason: QuietReason::Repeat {
                alert_id: 1,
                since_ms: 40_000 + 1300 - 1300
            }
        }
    );
    assert!(
        d2.explanation
            .ends_with("Not alerted: the same detections as the alert 40 s ago (repeat)."),
        "{}",
        d2.explanation
    );
    // One person fewer is a different set; so is another camera.
    let d3 = check(
        &mut e,
        3,
        cam(0),
        t2 + 10 * MINUTE_MS,
        a[..1].to_vec(),
        walk(&a[..1]),
    );
    assert!(d3.outcome.is_alert(), "{}", d3.explanation);
    let d4 = check(&mut e, 4, cam(1), t2, a.clone(), walk(&a));
    assert!(d4.outcome.is_alert());
    // Past the window the set alerts again.
    let mut s = AlertSettings::default();
    s.filters.repeat_window_min = 1;
    let mut e = Engine::new(s).0;
    check(&mut e, 1, cam(0), at(12, 0), a.clone(), walk(&a));
    let d = check(
        &mut e,
        2,
        cam(0),
        at(12, 0) + 2 * MINUTE_MS,
        a.clone(),
        walk(&a),
    );
    assert!(d.outcome.is_alert(), "{}", d.explanation);
}

#[test]
fn something_that_stays_in_place_is_learned_and_held_back() {
    let mut s = AlertSettings::default();
    // Repeat suppression would hold these back first; this is about memory.
    s.filters.repeat = false;
    let mut e = Engine::new(s).0;
    let web = person_at(0.2, 0.6);
    let mut quiet = None;
    for i in 0..8u64 {
        let t = at(23, 0) + i as Millis * 3 * MINUTE_MS;
        let d = check(
            &mut e,
            10 + i,
            cam(0),
            t,
            vec![web.clone()],
            vec![web.clone()],
        );
        if let Outcome::Quiet {
            reason: QuietReason::Stationary { for_ms },
        } = d.outcome
        {
            quiet = Some((i, for_ms, d));
            break;
        }
    }
    let (i, for_ms, d) = quiet.expect("held back within 8 checks");
    assert!(i >= 2, "never on the first look ({i})");
    assert_eq!(for_ms / MINUTE_MS, i as Millis * 3);
    assert_eq!(filters_step(&d).unwrap().3, Some(FilterKind::SceneMemory));
    assert!(
        d.explanation.ends_with(&format!(
            "Not alerted: a person in the same place as for the last {} min (scene memory).",
            i * 3
        )),
        "{}",
        d.explanation
    );
    // The picture steps mark the stationary hits.
    let marked = d.trace.steps.iter().any(|s| {
        matches!(&s.kind, StepKind::FrameSampled { detections, .. } if detections.iter().any(|j| j.stationary))
    });
    assert!(marked);
    // Someone walking elsewhere at the same time still alerts.
    let t = at(23, 0) + (i as Millis * 3 + 1) * MINUTE_MS;
    let d = check(
        &mut e,
        100,
        cam(0),
        t,
        vec![person_at(0.7, 0.7)],
        vec![person_at(0.75, 0.7)],
    );
    assert!(d.outcome.is_alert(), "{}", d.explanation);
    // A simulated trigger is shown the memory but never held back by it.
    let mut trig = device(101, cam(0), "people", t + MINUTE_MS);
    trig.test = true;
    let intake = e.intake(trig, now(t + MINUTE_MS));
    assert!(matches!(intake.next, Next::Sample { .. }));
    let t = t + MINUTE_MS;
    e.sample(
        101,
        &sample(cam(0), t + 200, vec![web.clone()]),
        now(t + 250),
    );
    e.sample(
        101,
        &sample(cam(0), t + 1200, vec![web.clone()]),
        now(t + 1250),
    );
    let d = e
        .decide(101, DecideInputs::default(), now(t + 1300))
        .unwrap();
    assert!(d.outcome.is_alert(), "{}", d.explanation);
    let f = filters_step(&d).unwrap();
    // Shown as stationary; only "parked" (which never holds back) acts.
    assert_eq!((f.1, f.3), (2, Some(FilterKind::Parked)));
    // Reset forgets it.
    assert!(e.reset_scene(cam(0)));
    assert!(e.scene().view(cam(0), t, &e.settings().filters).is_empty());
    // Turned off, nothing is held back.
    let mut s = AlertSettings::default();
    s.filters.repeat = false;
    s.filters.scene_memory = false;
    let mut e = Engine::new(s).0;
    for i in 0..8u64 {
        let t = at(23, 0) + i as Millis * 3 * MINUTE_MS;
        let d = check(
            &mut e,
            10 + i,
            cam(0),
            t,
            vec![web.clone()],
            vec![web.clone()],
        );
        assert!(d.outcome.is_alert(), "{}", d.explanation);
    }
}

#[test]
fn rechecks_teach_the_memory_and_it_survives_a_restart() {
    let mut e = Engine::new(AlertSettings::default()).0;
    let car = Detection {
        class: Class::Vehicle,
        score: 0.4,
        bbox: BBox::new(0.1, 0.5, 0.3, 0.2),
    };
    for i in 0..6 {
        let t = at(2, 0) + i * 5 * MINUTE_MS;
        assert!(e.learn(&sample(cam(3), t, vec![car.clone()]), now(t)));
    }
    assert!(e.scene_dirty());
    let bytes = e.scene().to_bytes();
    e.scene_saved();
    assert!(!e.scene_dirty());
    let mut e2 = Engine::new(AlertSettings::default()).0;
    e2.set_scene(alerts::filters::SceneMemory::from_bytes(&bytes).unwrap());
    let t = at(2, 30);
    let p = e2
        .scene()
        .place(cam(3), Class::Vehicle, &car.bbox, t, &e2.settings().filters);
    assert!(p.stationary, "{p:?}");
    // The insight layers: a grid for the class, and the thresholds.
    let ins = insights(&e2, cam(3), t, true);
    let json = serde_json::to_value(&ins).unwrap();
    assert_eq!(json["layers"][0]["id"], "scene.vehicle");
    assert_eq!(json["layers"][0]["kind"], "grid");
    assert_eq!(json["layers"][0]["rows"], 36);
    assert_eq!(json["layers"][0]["cols"], 64);
    assert_eq!(
        json["layers"][0]["values"].as_array().unwrap().len(),
        64 * 36
    );
    assert!(
        json["layers"][0]["provenance"]
            .as_str()
            .unwrap()
            .contains("1999")
    );
    let last = ins.layers.last().unwrap();
    assert_eq!(last.id, "thresholds");
    let LayerData::Scalar { items } = &last.data else {
        panic!("thresholds are scalar");
    };
    assert!(items.iter().any(|i| i.id == "floor.night"));
    // No memory for another camera: only the thresholds.
    assert_eq!(insights(&e2, cam(4), t, false).layers.len(), 1);
}

#[test]
fn night_score_floors_per_camera() {
    let mut s = AlertSettings::default();
    s.verification.night_min_score = 0.75;
    let mut c = CameraSettings::new(cam(1));
    c.night_min_score = Some(0.55);
    c.min_score = Some(0.8);
    s.cameras.push(c);
    let mut e = Engine::new(s).0;
    let walk = |x: f32, score: f32| (vec![person_at(x, score)], vec![person_at(x + 0.05, score)]);
    // At night 0.7 is below the default night floor ...
    let (a, b) = walk(0.3, 0.7);
    let d = check(&mut e, 1, cam(0), at(23, 0), a, b);
    assert!(!d.outcome.is_alert(), "{}", d.explanation);
    let floor = d.trace.find(|k| match k {
        StepKind::Sampling { min_score, .. } => *min_score,
        _ => None,
    });
    assert_eq!(floor, Some(0.75));
    // ... but above the day floor (the next day).
    let (a, b) = walk(0.3, 0.7);
    let d = check(&mut e, 2, cam(0), at(36, 0), a, b);
    assert!(d.outcome.is_alert(), "{}", d.explanation);
    // The camera's own floors: 0.6 alerts at night, not by day.
    let (a, b) = walk(0.3, 0.6);
    assert!(check(&mut e, 3, cam(1), at(23, 0), a, b).outcome.is_alert());
    let (a, b) = walk(0.5, 0.6);
    assert!(!check(&mut e, 4, cam(1), at(36, 0), a, b).outcome.is_alert());
    // Validation clamps a camera floor.
    let mut s = AlertSettings::default();
    let mut c = CameraSettings::new(cam(1));
    c.night_min_score = Some(5.0);
    s.cameras.push(c);
    let (s, issues) = s.validated();
    assert_eq!(s.score_floor(cam(1), true), 0.99);
    assert!(!issues.is_empty());
}
