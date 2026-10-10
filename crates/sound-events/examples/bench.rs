//! Detection rates on the synthetic test sets, a tone-to-noise sweep, and
//! the CPU cost of the analyzers per second of audio.
//!
//! Run optimized: `cargo run --release -p sound-events --example bench`.

#[path = "../tests/common/mod.rs"]
mod common;

use common::*;
use sound_events::{AlarmConfig, AlarmDetector, LoudnessMeter, SAMPLE_RATE};
use std::hint::black_box;
use std::time::Instant;

fn report(title: &str, cases: &[Case]) {
    let mut rows: Vec<(&str, usize, usize, usize)> = Vec::new();
    for c in cases {
        let o = evaluate(c);
        let i = match rows.iter().position(|r| r.0 == c.set) {
            Some(i) => i,
            None => {
                rows.push((c.set, 0, 0, 0));
                rows.len() - 1
            }
        };
        rows[i].1 += usize::from(o.detected);
        rows[i].2 += 1;
        rows[i].3 += o.false_alarms;
    }
    println!("{title}");
    for (set, det, n, fa) in rows {
        if cases.iter().any(|c| c.set == set && c.expect.is_some()) {
            println!("  {set:<18} detected {det:>3}/{n:<3} false alarms {fa}");
        } else {
            println!("  {set:<18} cases {n:>3}     false alarms {fa}");
        }
    }
}

fn main() {
    let mut cases = clean_set();
    cases.extend(degraded_set());
    cases.extend(distractor_set());
    report("Synthetic sets", &cases);

    println!(
        "Tone-to-noise sweep (tone RMS over broadband noise RMS; 12 cases per row: T3 and T4, white and pink, 3 seeds)"
    );
    for snr in [10.0f32, 5.0, 0.0, -3.0, -5.0, -7.0, -10.0] {
        let cases = noise_set(&[snr]);
        let det = cases.iter().filter(|c| evaluate(c).detected).count();
        let fa: usize = cases.iter().map(|c| evaluate(c).false_alarms).sum();
        println!(
            "  {snr:>5} dB: detected {det:>2}/{}, false alarms {fa}",
            cases.len()
        );
    }

    // CPU cost: 1000 s of noisy audio with an alarm in it, in 1024-sample
    // blocks (the tap's 64 ms AAC frames).
    let mut rng = Rng::new(42);
    let secs_total = 1000usize;
    let mut x = pink(secs_total * SAMPLE_RATE as usize, 0.02, &mut rng);
    render(
        &mut x,
        10.0,
        &t3(20, 0.05, &mut rng),
        Voice::pure(3100.0, 0.1),
    );
    let rounds = 3;
    let mut best_alarm = f64::MAX;
    let mut best_level = f64::MAX;
    for _ in 0..rounds {
        let mut d = AlarmDetector::new(AlarmConfig::default());
        let mut events = 0usize;
        let t = Instant::now();
        for (k, block) in x.chunks(1024).enumerate() {
            d.push((k * 1024) as u64, black_box(block), |_| events += 1);
        }
        d.flush(|_| events += 1);
        best_alarm = best_alarm.min(t.elapsed().as_secs_f64());
        black_box(events);

        let mut m = LoudnessMeter::new();
        let mut seconds = 0usize;
        let t = Instant::now();
        for (k, block) in x.chunks(1024).enumerate() {
            m.push((k * 1024) as u64, black_box(block), |_| seconds += 1);
        }
        m.flush(|_| seconds += 1);
        best_level = best_level.min(t.elapsed().as_secs_f64());
        black_box(seconds);
    }
    let per_s = |total: f64| total / secs_total as f64 * 1e9;
    println!("CPU per second of 16 kHz audio (best of {rounds} runs over {secs_total} s):");
    println!(
        "  alarm detector  {:>8.0} ns  ({:.4}% of one core)",
        per_s(best_alarm),
        per_s(best_alarm) / 1e7
    );
    println!(
        "  loudness meter  {:>8.0} ns  ({:.4}% of one core)",
        per_s(best_level),
        per_s(best_level) / 1e7
    );
}
