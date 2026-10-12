#![allow(dead_code)]

use alerts::time::{Date, HOUR_MS, MINUTE_MS, Millis};
use alerts::trigger::{ExternalBody, from_detection, from_external};
use alerts::zone::BBox;
use alerts::{
    CameraId, Class, DecideInputs, Decision, Detection, Engine, Next, Now, Sample, Source, Trigger,
    TriggerState,
};

pub fn cam(channel: u16) -> CameraId {
    CameraId::new(1, channel, 0)
}

/// 2024-06-20 (a Thursday), 00:00 UTC.
pub fn day0() -> Millis {
    Date::new(2024, 6, 20).utc_midnight_ms()
}

pub fn at(h: i64, m: i64) -> Millis {
    day0() + h * HOUR_MS + m * MINUTE_MS
}

pub fn now(ms: Millis) -> Now {
    Now::new(ms, 0)
}

pub fn device(id: u64, camera: CameraId, kind: &str, at_ms: Millis) -> Trigger {
    from_detection(id, Source::Device, camera, kind, TriggerState::Start, at_ms).unwrap()
}

pub fn audio(id: u64, camera: CameraId, kind: &str, at_ms: Millis) -> Trigger {
    from_detection(
        id,
        Source::LocalAudio,
        camera,
        kind,
        TriggerState::Start,
        at_ms,
    )
    .unwrap()
}

pub fn sensor(id: u64, key: &str, at_ms: Millis) -> Trigger {
    let body = ExternalBody {
        kind: "sensor_motion".into(),
        state: TriggerState::Pulse,
        at_ms: Some(at_ms),
    };
    from_external(id, key, &body, at_ms).unwrap()
}

pub fn det(class: Class, score: f32) -> Detection {
    Detection {
        class,
        score,
        bbox: BBox::new(0.45, 0.4, 0.1, 0.3),
    }
}

pub fn sample(camera: CameraId, at_ms: Millis, d: Vec<Detection>) -> Sample {
    Sample {
        camera,
        at_ms,
        detections: d,
    }
}

/// Runs a trigger through intake, feeds one picture per entry of `frames`
/// (camera, offset from the trigger, detections), and decides.
pub fn run(
    e: &mut Engine,
    t: Trigger,
    frames: Vec<(CameraId, Millis, Vec<Detection>)>,
    inputs: DecideInputs,
) -> Decision {
    let t0 = t.received_ms;
    let intake = e.intake(t, now(t0));
    match intake.next {
        Next::Done(d) => *d,
        Next::DecideNow => e.decide(intake.trigger_id, inputs, now(t0)).unwrap(),
        Next::Sample { .. } => {
            let mut last = t0;
            for (c, off, d) in frames {
                last = t0 + off;
                e.sample(intake.trigger_id, &sample(c, t0 + off, d), now(last + 50));
            }
            e.decide(intake.trigger_id, inputs, now(last + 100))
                .unwrap()
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// Two pictures of a person on `c`, 1 s apart.
pub fn seen(c: CameraId, score: f32) -> Vec<(CameraId, Millis, Vec<Detection>)> {
    vec![
        (c, 200, vec![det(Class::Person, score)]),
        (c, 1200, vec![det(Class::Person, score - 0.05)]),
    ]
}

/// Two pictures with nothing in them.
pub fn empty(c: CameraId) -> Vec<(CameraId, Millis, Vec<Detection>)> {
    vec![(c, 200, vec![]), (c, 1200, vec![])]
}
