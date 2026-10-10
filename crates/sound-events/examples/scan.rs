//! Runs the analyzers over raw 16 kHz mono f32 little-endian audio and
//! prints aggregates only: alarm episodes found and the loudness
//! distribution. Optionally mixes synthetic alarms into the audio first, to
//! measure detection on top of a real background.
//!
//! ```sh
//! ffmpeg -i input.aac -f f32le -ar 16000 -ac 1 audio.raw
//! cargo run --release -p sound-events --example scan -- audio.raw [mix dBFS ...]
//! ```
//!
//! With mix levels, each level adds a 6-cycle T3 and a 5-cycle T4 (tone
//! peak amplitude at that dBFS, 3.1 kHz) at three places, and reports how
//! many of the six were found and anything else reported.

#[path = "../tests/common/mod.rs"]
mod common;

use common::*;
use sound_events::{AlarmConfig, AlarmDetector, AlarmEvent, AlarmKind, Level, LoudnessMeter};

fn read_raw(path: &str) -> Vec<f32> {
    let bytes = std::fs::read(path).expect("readable file");
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

fn run(x: &[f32]) -> Vec<AlarmEvent> {
    run_with(x, AlarmConfig::default())
}

fn run_with(x: &[f32], cfg: AlarmConfig) -> Vec<AlarmEvent> {
    let mut d = AlarmDetector::new(cfg);
    let mut ev = Vec::new();
    for (k, block) in x.chunks(1024).enumerate() {
        d.push((k * 1024) as u64, block, |e| ev.push(e));
    }
    d.flush(|e| ev.push(e));
    ev
}

fn percentile(sorted: &[i8], p: f32) -> i8 {
    sorted[((sorted.len() - 1) as f32 * p).round() as usize]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: scan <audio.raw> [mix dBFS ...]");
        std::process::exit(2);
    };
    let x = read_raw(path);
    let dur = secs(x.len());
    println!("audio: {dur:.1} s");

    let mut m = LoudnessMeter::new();
    let mut peaks = Vec::new();
    let mut rmss = Vec::new();
    for (k, block) in x.chunks(1024).enumerate() {
        m.push((k * 1024) as u64, block, |s| {
            peaks.push(s.peak);
            rmss.push(s.rms);
        });
    }
    m.flush(|s| {
        peaks.push(s.peak);
        rmss.push(s.rms);
    });
    for (name, levels) in [("peak", &peaks), ("rms", &rmss)] {
        let mut v: Vec<i8> = levels.iter().filter_map(|l: &Level| l.db()).collect();
        v.sort_unstable();
        if v.is_empty() {
            continue;
        }
        println!(
            "{name} dBFS per second: min {} p10 {} median {} p90 {} max {} ({} seconds)",
            v[0],
            percentile(&v, 0.1),
            percentile(&v, 0.5),
            percentile(&v, 0.9),
            v[v.len() - 1],
            v.len()
        );
        let mut hist = String::new();
        for lo in (-130..0).step_by(10) {
            let n = v
                .iter()
                .filter(|&&d| i32::from(d) >= lo && (i32::from(d) < lo + 10 || lo == -10))
                .count();
            if n > 0 {
                let close = if lo == -10 { "]" } else { ")" };
                hist.push_str(&format!(" [{lo},{}{close}:{n}", lo + 10));
            }
        }
        println!("  histogram:{hist}");
    }

    let ev = run(&x);
    let count = |k: AlarmKind| {
        ev.iter()
            .filter(|e| matches!(e, AlarmEvent::Started { kind, .. } if *kind == k))
            .count()
    };
    println!(
        "unmixed: smoke episodes {}, co episodes {}",
        count(AlarmKind::Smoke),
        count(AlarmKind::Co)
    );

    // A margin indicator: single cycles (half the evidence needed).
    let single = run_with(
        &x,
        AlarmConfig {
            min_cycles: 1,
            ..AlarmConfig::default()
        },
    );
    let singles = single
        .iter()
        .filter(|e| matches!(e, AlarmEvent::Started { .. }))
        .count();
    println!("unmixed, one cycle enough: {singles} episodes");

    let places = [0.1f32, 0.45, 0.8];
    for level in args[1..]
        .iter()
        .map(|s| s.parse::<f32>().expect("a dBFS number"))
    {
        let mut y = x.clone();
        let mut rng = Rng::new(level.to_bits() as u64);
        let mut expected = Vec::new();
        for (i, p) in places.iter().enumerate() {
            let at = dur * p;
            let t3r = t3(6, 0.05, &mut rng);
            render(
                &mut y,
                at,
                &t3r,
                Voice {
                    harmonic: 0.2,
                    ..Voice::pure(3100.0, dbfs_amp(level))
                },
            );
            expected.push((AlarmKind::Smoke, at, at + length(&t3r)));
            let at4 = at + length(&t3r) + 5.0;
            let t4r = t4(5, 0.05, &mut rng);
            render(
                &mut y,
                at4,
                &t4r,
                Voice {
                    harmonic: 0.2,
                    ..Voice::pure(3150.0 + 50.0 * i as f32, dbfs_amp(level))
                },
            );
            expected.push((AlarmKind::Co, at4, at4 + length(&t4r)));
        }
        let ev = run(&y);
        let starts: Vec<(AlarmKind, f32)> = ev
            .iter()
            .filter_map(|e| match e {
                AlarmEvent::Started { kind, start, .. } => Some((*kind, secs(*start as usize))),
                _ => None,
            })
            .collect();
        let hit = |k: &AlarmKind, a: f32, b: f32, t: f32, kind: &AlarmKind| {
            k == kind && t >= a - 1.0 && t <= b
        };
        let found = expected
            .iter()
            .filter(|(k, a, b)| starts.iter().any(|(kind, t)| hit(k, *a, *b, *t, kind)))
            .count();
        let other = starts
            .iter()
            .filter(|(kind, t)| !expected.iter().any(|(k, a, b)| hit(k, *a, *b, *t, kind)))
            .count();
        let episodes = starts.len() - other;
        println!(
            "mixed at {level} dBFS peak: found {found}/{} (in {episodes} episodes), other reports {other}",
            expected.len()
        );
    }
}
