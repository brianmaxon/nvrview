//! Properties over random settings and trigger sequences.

mod common;

use alerts::decision::Outcome;
use alerts::model::{Exposure, Mode, Severity};
use alerts::settings::{CameraSettings, DailyWindow, Schedule};
use alerts::time::{Millis, SECOND_MS};
use alerts::trace::StepKind;
use alerts::{AlertSettings, Class, DecideInputs, Engine, Rarity};
use common::*;
use proptest::prelude::*;

fn arb_settings() -> impl Strategy<Value = AlertSettings> {
    (
        prop::collection::vec(
            (
                0u8..3,
                any::<bool>(),
                prop::collection::vec(0usize..7, 0..7),
                0u8..3,
                0u16..1440,
                0u16..1440,
            ),
            4,
        ),
        0u8..3,
        0u8..4,
        10u32..400,
        10u32..900,
    )
        .prop_map(|(cams, mode, min_sev, c1, c2)| {
            let all = [
                Class::Person,
                Class::Vehicle,
                Class::Animal,
                Class::Package,
                Class::Visitor,
                Class::Smoke,
                Class::Co,
            ];
            let mut s = AlertSettings::default();
            for (i, (exp, trust, classes, sched, a, b)) in cams.into_iter().enumerate() {
                let mut c = CameraSettings::new(cam(i as u16));
                c.exposure =
                    [Exposure::Outside, Exposure::Perimeter, Exposure::Inside][exp as usize];
                c.trust_camera_ai = trust;
                c.classes = classes.into_iter().map(|k| all[k]).collect();
                c.schedule = match sched {
                    0 => Schedule::Always,
                    1 => Schedule::Night,
                    _ => Schedule::Windows {
                        windows: vec![DailyWindow {
                            days: 0x55,
                            start_min: a,
                            end_min: b,
                        }],
                    },
                };
                s.cameras.push(c);
            }
            s.mode = [Mode::Home, Mode::Away, Mode::Night][mode as usize];
            s.min_alert_severity = [
                Severity::Low,
                Severity::Normal,
                Severity::High,
                Severity::Critical,
            ][min_sev as usize];
            s.timing.initial_cooldown_s = c1;
            s.timing.escalated_cooldown_s = c2;
            s
        })
}

#[derive(Clone, Debug)]
struct Ev {
    gap_s: u32,
    camera: u16,
    kind: u8,
    frames: u8,
    late: bool,
    rarity: Option<u32>,
}

fn arb_events() -> impl Strategy<Value = Vec<Ev>> {
    prop::collection::vec(
        (
            0u32..900,
            0u16..4,
            0u8..7,
            0u8..3,
            prop::bool::weighted(0.1),
            prop::option::of(1u32..100),
        )
            .prop_map(|(gap_s, camera, kind, frames, late, rarity)| Ev {
                gap_s,
                camera,
                kind,
                frames,
                late,
                rarity,
            }),
        1..60,
    )
}

const KINDS: [&str; 7] = [
    "people",
    "vehicle",
    "dog_cat",
    "visitor",
    "md",
    "smoke_alarm",
    "co_alarm",
];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Whatever the settings, schedules, classes, modes, cooldowns, learned
    /// rarity, lateness, and earlier events: an always-alert outcome is an
    /// alert, smoke and CO always alert, and nothing but a mute stops them.
    #[test]
    fn always_alert_is_never_suppressed(s in arb_settings(), evs in arb_events(), start_h in 0i64..24) {
        let mut e = Engine::new(s).0;
        let mut t = at(start_h, 0);
        for (i, ev) in evs.iter().enumerate() {
            t += Millis::from(ev.gap_s) * SECOND_MS;
            e.tick(now(t));
            let kind = KINDS[ev.kind as usize];
            let mut trig = if kind.ends_with("_alarm") {
                audio(i as u64 + 1, cam(ev.camera), kind, t)
            } else {
                device(i as u64 + 1, cam(ev.camera), kind, t)
            };
            if ev.late {
                trig.catch_up = true;
            }
            let class = trig.kind.class();
            let frames = match ev.frames {
                0 => vec![],
                1 => empty(cam(ev.camera)),
                _ => seen(cam(ev.camera), 0.8)
                    .into_iter()
                    .map(|(c, o, _)| (c, o, vec![det(if class.is_detector_class() { class } else { Class::Person }, 0.8)]))
                    .collect(),
            };
            let inputs = DecideInputs { rarity: ev.rarity.map(|one_in| Rarity { one_in }), incident: None };
            let d = run(&mut e, trig, frames, inputs);
            let always = d.trace.find(|k| match k {
                StepKind::Rules { always, .. } => Some(*always),
                _ => None,
            }).flatten();
            if always.is_some() {
                prop_assert!(d.outcome.is_alert(), "{}", d.explanation);
            }
            if matches!(class, Class::Smoke | Class::Co) {
                prop_assert!(
                    matches!(d.outcome, Outcome::Alert { severity: Severity::Critical, .. }),
                    "{}", d.explanation
                );
            }
        }
    }

    /// Within an episode the cooldown never shrinks, and two alerts of the
    /// same episode with the same severity and no always-alert rule are at
    /// least the cooldown apart.
    #[test]
    fn cooldown_is_monotonic(gaps in prop::collection::vec(1u32..400, 1..80), c1 in 10u32..200, c2 in 10u32..600) {
        let mut s = AlertSettings::default();
        s.timing.initial_cooldown_s = c1;
        s.timing.escalated_cooldown_s = c2;
        s.timing.settle_after_s = 3600;
        let mut e = Engine::new(s).0;
        let mut t = at(10, 0);
        let mut last_alert: Option<(u64, Millis, Millis)> = None; // alert id, time, cooldown
        let mut cooldown_seen: Millis = 0;
        for (i, g) in gaps.iter().enumerate() {
            t += Millis::from(*g) * SECOND_MS;
            e.tick(now(t));
            let d = run(&mut e, device(i as u64 + 1, cam(0), "people", t), seen(cam(0), 0.8), DecideInputs::default());
            let cd = e.cooldown_ms(cam(0), Class::Person);
            if let Outcome::Alert { alert_id, .. } = d.outcome {
                if let Some((id, when, cool)) = last_alert
                    && id == alert_id
                {
                    prop_assert!(d.decided_ms - when >= cool, "alerts {} ms apart, cooldown {cool}", d.decided_ms - when);
                    prop_assert!(cd >= cooldown_seen);
                }
                last_alert = Some((alert_id, d.decided_ms, cd));
                cooldown_seen = cd;
            }
            prop_assert!(cd <= Millis::from(c1.max(c2).min(3600)) * SECOND_MS);
        }
    }
}
