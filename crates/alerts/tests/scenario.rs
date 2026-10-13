//! A scripted day of triggers on a made-up installation: four cameras, one
//! external motion sensor covering two of them, sunrise and sunset from a
//! coarse location, and a reconnect gap at night.

use crate::common::*;
use alerts::decision::Outcome;
use alerts::model::Exposure;
use alerts::settings::{CameraSettings, Location, SourceCamera, SourceMapping};
use alerts::time::{Millis, SECOND_MS};
use alerts::trace::StepKind;
use alerts::{AlertSettings, Class, DecideInputs, Decision, Detection, Engine, Next, Now, Trigger};

/// UTC+1, as a summer offset would be.
const OFFSET: i32 = 60;

fn settings() -> AlertSettings {
    let mut s = AlertSettings {
        location: Some(Location {
            lat: 51.5,
            lon: 0.0,
        }),
        ..AlertSettings::default()
    };
    let mut entrance = CameraSettings::new(cam(0));
    entrance.entrance = true;
    let mut perimeter = CameraSettings::new(cam(1));
    perimeter.exposure = Exposure::Perimeter;
    let mut inside = CameraSettings::new(cam(2));
    inside.exposure = Exposure::Inside;
    let doorbell = CameraSettings::new(cam(3));
    s.cameras = vec![entrance, perimeter, inside, doorbell];
    s.sources.push(SourceMapping::new(
        "sensor-1",
        vec![
            SourceCamera {
                camera: cam(1),
                roi: Default::default(),
                weight: 1.0,
            },
            SourceCamera {
                camera: cam(0),
                roi: Default::default(),
                weight: 1.0,
            },
        ],
    ));
    s
}

/// What happened, compactly.
fn label(d: &Decision) -> String {
    match &d.outcome {
        Outcome::Alert {
            severity,
            rev,
            late,
            ..
        } => format!(
            "alert {severity}{}{}",
            if *rev > 1 { " update" } else { "" },
            if *late { " late" } else { "" }
        ),
        Outcome::Quiet { reason } => format!("quiet {reason:?}"),
        Outcome::Suppress { reason } => format!("suppress {reason:?}"),
    }
}

struct Day {
    e: Engine,
    log: Vec<(Millis, String, String)>,
}

impl Day {
    fn now(t: Millis) -> Now {
        Now::new(t, OFFSET)
    }

    fn record(&mut self, d: Decision) {
        // Every decision carries its trace, ending in the decision step.
        assert!(matches!(
            d.trace.steps.last().unwrap().kind,
            StepKind::Decision { .. }
        ));
        assert!(!d.explanation.is_empty());
        self.log
            .push((d.trace.steps[0].at_ms, label(&d), d.explanation));
    }

    /// One trigger; `frames` are (camera, offset from the trigger,
    /// detections).
    fn trigger(&mut self, t: Trigger, frames: Vec<(alerts::CameraId, Millis, Vec<Detection>)>) {
        let t0 = t.received_ms;
        // The host ticks the engine regularly; here, before each trigger.
        let tick = self.e.tick(Self::now(t0));
        assert!(tick.decided.is_empty());
        let intake = self.e.intake(t, Self::now(t0));
        match intake.next {
            Next::Done(d) => self.record(*d),
            Next::DecideNow => {
                let d = self
                    .e
                    .decide(intake.trigger_id, DecideInputs::default(), Self::now(t0))
                    .unwrap();
                self.record(d);
            }
            Next::Sample { .. } => {
                let mut last = t0;
                for (c, off, det) in frames {
                    last = t0 + off;
                    let p = self
                        .e
                        .sample(intake.trigger_id, &sample(c, last, det), Self::now(last))
                        .unwrap();
                    if p.reached {
                        break;
                    }
                }
                let d = self
                    .e
                    .decide(
                        intake.trigger_id,
                        DecideInputs::default(),
                        Self::now(last + 100),
                    )
                    .unwrap();
                self.record(d);
            }
            Next::Extended { .. } | Next::Ignored => {}
        }
    }
}

#[test]
fn a_day_of_triggers() {
    let mut day = Day {
        e: Engine::new(settings()).0,
        log: Vec::new(),
    };
    let person = |s| vec![det(Class::Person, s)];

    // Morning: a person at the entrance, then the same person again inside
    // the cooldown.
    day.trigger(device(1, cam(0), "people", at(7, 30)), seen(cam(0), 0.8));
    day.trigger(
        device(2, cam(0), "people", at(7, 30) + 20 * SECOND_MS),
        vec![],
    );
    // Someone at home walks past the inside camera.
    day.trigger(device(3, cam(2), "people", at(9, 0)), seen(cam(2), 0.9));
    // A doorbell press in the day.
    day.trigger(device(4, cam(3), "visitor", at(12, 0)), vec![]);
    // The sensor fires for nothing (wind, a bird).
    day.trigger(
        sensor(5, "sensor-1", at(13, 0)),
        vec![
            (cam(1), 300, vec![]),
            (cam(0), 300, vec![]),
            (cam(1), 1300, vec![]),
        ],
    );
    // The sensor fires and both cameras see a person.
    day.trigger(
        sensor(6, "sensor-1", at(14, 0)),
        vec![(cam(1), 300, person(0.7)), (cam(0), 400, person(0.6))],
    );
    // The smoke alarm pattern on the inside camera's microphone.
    day.trigger(audio(7, cam(2), "smoke_alarm", at(16, 0)), vec![]);
    // Night (sunset about 20:21 UTC; night from about 20:51): a person on
    // the perimeter camera, and again 30 s later.
    day.trigger(device(8, cam(1), "people", at(21, 30)), seen(cam(1), 0.75));
    day.trigger(
        device(9, cam(1), "people", at(21, 30) + 30 * SECOND_MS),
        seen(cam(1), 0.75),
    );
    // The camera's AI reports a person our check does not see.
    day.trigger(device(10, cam(0), "people", at(22, 0)), empty(cam(0)));

    // The connection drops 22:30 to 23:10; a catch-up search finds a
    // vehicle and a person from the gap.
    let mut late_vehicle = device(11, cam(0), "vehicle", at(23, 10));
    late_vehicle.at_ms = at(22, 40);
    late_vehicle.catch_up = true;
    day.trigger(
        late_vehicle,
        vec![
            (cam(0), 200, vec![det(Class::Vehicle, 0.8)]),
            (cam(0), 1200, vec![det(Class::Vehicle, 0.8)]),
        ],
    );
    let mut late_person = device(12, cam(1), "people", at(23, 10));
    late_person.at_ms = at(22, 50);
    late_person.catch_up = true;
    day.trigger(late_person, vec![]);
    // A doorbell press at night.
    day.trigger(device(13, cam(3), "visitor", at(23, 15)), vec![]);

    let settled = day.e.tick(Day::now(at(23, 59)));
    assert!(settled.decided.is_empty());

    let labels: Vec<(String, &str)> = day
        .log
        .iter()
        .map(|(t, l, _)| {
            let lt = alerts::time::local(*t, OFFSET);
            (alerts::time::hhmm(lt.minute_of_day), l.as_str())
        })
        .collect();
    let expected = [
        ("08:30", "alert normal"),
        (
            "08:30",
            "suppress Cooldown { left_ms: 41300, since_ms: 18700 }",
        ),
        ("10:00", "quiet LowSeverity { severity: Low }"),
        ("13:00", "alert high"),
        ("14:00", "quiet NothingSeen { hits: 0, n: 3 }"),
        ("15:00", "alert normal"),
        ("17:00", "alert critical"),
        ("22:30", "alert critical"),
        ("22:30", "alert critical update"),
        ("23:00", "quiet NotConfirmed"),
        ("00:10", "quiet Late { age_ms: 1800000 }"),
        ("00:10", "alert critical late"),
        ("00:15", "alert critical"),
    ];
    let got: Vec<(&str, &str)> = labels.iter().map(|(t, l)| (t.as_str(), *l)).collect();
    assert_eq!(got, expected);

    // A few explanations in full.
    let ex = |i: usize| day.log[i].2.as_str();
    assert_eq!(
        ex(0),
        "Person on Camera 1 at 08:30 (day, outside camera, entrance). Verified in 2 of 2 pictures (0.80). The camera's AI agreed."
    );
    assert_eq!(
        ex(5),
        "Person on Camera 2 at 15:00 (day, perimeter camera), after Sensor 1 fired. Verified in 2 of 2 pictures on 2 cameras (0.70)."
    );
    assert_eq!(
        ex(7),
        "Person on Camera 2 at 22:30 (night, perimeter camera). Verified in 2 of 2 pictures (0.75). The camera's AI agreed. Always alerts: a person on an outside camera at night."
    );
    assert_eq!(
        ex(10),
        "Vehicle on Camera 1 at 23:40 (night, outside camera, entrance). Verified in 2 of 2 pictures (0.80). The camera's AI agreed. Not alerted: reported 30 min late."
    );
    assert_eq!(
        ex(11),
        "Person on Camera 2 at 23:50 (night, perimeter camera). Always alerts: a person on an outside camera at night. No picture could be checked; alerted unverified. Reported 20 min late."
    );
    let h = day.e.source_health("sensor-1");
    assert_eq!((h.fired, h.confirmed, h.nothing_seen), (2, 1, 1));
}
