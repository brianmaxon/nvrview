//! The alarm detector on synthetic audio: detection on clean, noisy, and
//! degraded alarms, and no reports on distractors. Run with `--nocapture`
//! to see the per-set table.

mod common;

use common::*;
use sound_events::{AlarmConfig, AlarmDetector, AlarmEvent, AlarmKind};

/// Evaluates cases and prints one line per set; returns (detected, cases,
/// false alarms) per set, in first-seen order.
fn table(cases: &[Case]) -> Vec<(&'static str, usize, usize, usize)> {
    let mut rows: Vec<(&'static str, usize, usize, usize)> = Vec::new();
    for c in cases {
        let o = evaluate(c);
        if c.expect.is_some() && !o.detected || o.false_alarms > 0 {
            println!(
                "  miss or false alarm: {} / {} (false alarms {})",
                c.set, c.name, o.false_alarms
            );
        }
        let row = match rows.iter_mut().find(|r| r.0 == c.set) {
            Some(r) => r,
            None => {
                rows.push((c.set, 0, 0, 0));
                rows.last_mut().unwrap()
            }
        };
        row.1 += usize::from(o.detected);
        row.2 += 1;
        row.3 += o.false_alarms;
    }
    for (set, det, n, fa) in &rows {
        println!("{set:>18}: detected {det}/{n}, false alarms {fa}");
    }
    rows
}

#[test]
fn clean_alarms_are_all_detected() {
    let rows = table(&clean_set());
    assert_eq!(rows[0].1, rows[0].2);
    assert_eq!(rows[0].3, 0);
}

#[test]
fn noisy_alarms_down_to_0_db_snr() {
    // Asserted down to 0 dB; lower ratios are reported only.
    let rows = table(&noise_set(&[20.0, 10.0, 5.0, 0.0]));
    for r in &rows {
        assert_eq!(r.1, r.2, "{}", r.0);
        assert_eq!(r.3, 0, "{}", r.0);
    }
    println!("below 0 dB (reported, not asserted):");
    let rows = table(&noise_set(&[-5.0, -10.0]));
    for r in &rows {
        assert_eq!(r.3, 0, "{}", r.0);
    }
}

#[test]
fn degraded_alarms_are_detected() {
    let rows = table(&degraded_set());
    for r in &rows {
        assert_eq!(r.1, r.2, "{}", r.0);
        assert_eq!(r.3, 0, "{}", r.0);
    }
}

#[test]
fn distractors_never_report() {
    let rows = table(&distractor_set());
    assert_eq!(rows[0].3, 0);
}

#[test]
fn start_and_end_times_match_the_alarm() {
    let mut rng = Rng::new(5);
    let r = t3(5, 0.0, &mut rng);
    let mut x = vec![0.0; samples(30.0)];
    render(&mut x, 2.0, &r, Voice::pure(3100.0, 0.3));
    let ev = detect(&x);
    assert_eq!(ev.len(), 2, "{ev:?}");
    let AlarmEvent::Started {
        kind,
        start,
        cycles,
    } = ev[0]
    else {
        panic!()
    };
    assert_eq!((kind, cycles), (AlarmKind::Smoke, 2));
    assert!(
        (secs(start as usize) - 2.0).abs() < 0.05,
        "{}",
        secs(start as usize)
    );
    let AlarmEvent::Ended {
        start: s2,
        end,
        cycles,
        ..
    } = ev[1]
    else {
        panic!()
    };
    assert_eq!((s2, cycles), (start, 5));
    // Last pulse ends at 2 + 4 * 4 + 2.5 = 20.5 s.
    assert!(
        (secs(end as usize) - 20.5).abs() < 0.05,
        "{}",
        secs(end as usize)
    );
}

#[test]
fn block_size_and_sample_format_do_not_matter() {
    let mut rng = Rng::new(9);
    let mut r = t3(3, 0.05, &mut rng);
    r.push((0.0, 3.0));
    r.extend(t4(3, 0.05, &mut rng));
    let mut x = vec![0.0; samples(length(&r) + 8.0)];
    render(&mut x, 1.0, &r, Voice::pure(3200.0, 0.2));
    let noise = white(x.len(), 0.01, &mut rng);
    add(&mut x, &noise);
    let reference = detect_with(AlarmConfig::default(), &x, 256);
    assert_eq!(started(&reference), vec![AlarmKind::Smoke, AlarmKind::Co]);
    for block in [1usize, 7, 255, 1000, 16_000, x.len()] {
        assert_eq!(
            detect_with(AlarmConfig::default(), &x, block),
            reference,
            "block {block}"
        );
    }
    // i16 input gives the same events as its f32 equivalent.
    let ints: Vec<i16> = x
        .iter()
        .map(|s| (s * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
        .collect();
    let floats: Vec<f32> = ints.iter().map(|&s| f32::from(s) / 32768.0).collect();
    let mut d = AlarmDetector::new(AlarmConfig::default());
    let mut a = Vec::new();
    d.push_i16(0, &ints, |e| a.push(e));
    d.flush(|e| a.push(e));
    assert_eq!(a, detect_with(AlarmConfig::default(), &floats, 4096));
}

#[test]
fn detection_can_be_limited_to_one_kind_and_retuned() {
    let mut rng = Rng::new(11);
    let mut x = vec![0.0; samples(30.0)];
    render(&mut x, 1.0, &t3(5, 0.0, &mut rng), Voice::pure(520.0, 0.3));
    assert!(started(&detect(&x)).is_empty());
    let low = AlarmConfig {
        band_low_hz: 450.0,
        band_high_hz: 600.0,
        co: false,
        ..AlarmConfig::default()
    };
    assert_eq!(started(&detect_with(low, &x, 512)), vec![AlarmKind::Smoke]);
    let mut x = vec![0.0; samples(30.0)];
    render(&mut x, 1.0, &t4(4, 0.0, &mut rng), Voice::pure(3100.0, 0.3));
    let no_co = AlarmConfig {
        co: false,
        ..AlarmConfig::default()
    };
    assert!(started(&detect_with(no_co, &x, 512)).is_empty());
    let three = AlarmConfig {
        min_cycles: 3,
        ..AlarmConfig::default()
    };
    let ev = detect_with(three, &x, 512);
    assert!(
        matches!(
            ev[0],
            AlarmEvent::Started {
                kind: AlarmKind::Co,
                cycles: 3,
                ..
            }
        ),
        "{ev:?}"
    );
}

#[test]
fn a_gap_in_the_samples_breaks_the_rhythm() {
    // A T3 whose second group falls in a gap of the stream: the groups
    // around the gap do not link.
    let mut rng = Rng::new(13);
    let mut x = vec![0.0; samples(12.0)];
    render(&mut x, 1.0, &t3(3, 0.0, &mut rng), Voice::pure(3100.0, 0.3));
    let mut d = AlarmDetector::new(AlarmConfig::default());
    let mut ev = Vec::new();
    let cut = (samples(4.5), samples(8.0));
    d.push(0, &x[..cut.0], |e| ev.push(e));
    d.push(cut.1 as u64, &x[cut.1..], |e| ev.push(e));
    d.flush(|e| ev.push(e));
    assert!(ev.is_empty(), "{ev:?}");
}

#[test]
fn an_episode_survives_one_masked_cycle_and_ends_after_two() {
    let mut rng = Rng::new(17);
    let r = t3(8, 0.0, &mut rng);
    let mut x = vec![0.0; samples(length(&r) + 12.0)];
    render(&mut x, 1.0, &r, Voice::pure(3100.0, 0.1));
    // Loud broadband noise masks the fourth cycle.
    let noise = white(samples(4.0), 0.5, &mut rng);
    let a = samples(13.0);
    add(&mut x[a..a + noise.len()], &noise);
    let ev = detect(&x);
    assert_eq!(started(&ev).len(), 1, "{ev:?}");
    assert!(
        matches!(ev.last(), Some(AlarmEvent::Ended { cycles: 7, .. })),
        "{ev:?}"
    );
}
