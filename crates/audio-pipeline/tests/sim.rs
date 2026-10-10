//! Long-run synchronization on the simulated camera, network, and device
//! (see `common/mod.rs`). Each test prints its measurements; run with
//! `--nocapture` to see them.

mod common;

use common::*;

const HOURS: f64 = 3.0;
/// Required: every click within this of the intended latency.
const BOUND_US: f64 = 40_000.0;
/// Camera seconds after a start before the bound applies: the delivery-delay
/// estimate needs this long to see enough of the jitter.
const SETTLE_S: f64 = 60.0;

fn report(name: &str, r: &SimResult) {
    let mut errs: Vec<(f64, f64)> = r.errors().filter(|(t, _)| *t > SETTLE_S).collect();
    errs.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
    println!(
        "{name}: largest deviations (s, ms): {:?}",
        errs.iter()
            .take(6)
            .map(|(t, e)| (t.round(), (e / 100.0).round() / 10.0))
            .collect::<Vec<_>>()
    );
    let (worst, n) = r.worst_after(SETTLE_S);
    let p = r.tile.stats().playout.unwrap();
    println!(
        "{name}: worst {:.1} ms over {n} clicks ({} sent); latency {:.0} ms; jitter sd {:.1} ms, range {:.0}..{:.0} ms; rate estimate {:.2}; resyncs {}, starts {}, underruns {} ({} frames), overflow {}",
        worst / 1e3,
        r.clicks_sent,
        r.expected_latency_us / 1e3,
        r.jitter_sd_us / 1e3,
        r.jitter_range_us.0 / 1e3,
        r.jitter_range_us.1 / 1e3,
        r.tile.stats().rate_estimate.unwrap_or(0.0),
        p.resyncs,
        p.starts,
        p.underruns,
        p.underrun_frames,
        p.overflow_frames,
    );
}

/// Mean deviation per `bucket_s` of camera time.
fn bucket_means(r: &SimResult, bucket_s: f64) -> Vec<f64> {
    let mut sums: Vec<(f64, u32)> = Vec::new();
    for (t, e) in r.errors() {
        let b = (t / bucket_s) as usize;
        if sums.len() <= b {
            sums.resize(b + 1, (0.0, 0));
        }
        sums[b].0 += e;
        sums[b].1 += 1;
    }
    sums.iter()
        .filter(|s| s.1 > 0)
        .map(|s| s.0 / f64::from(s.1))
        .collect()
}

#[test]
fn measured_conditions_hold_sync_for_hours() {
    // Audio 20 ppm fast in camera time, camera and host 8 ppm apart, device
    // 20 ppm slow, jitter like the measurement.
    let cfg = SimConfig {
        duration_s: HOURS * 3600.0,
        ..SimConfig::default()
    };
    let r = run(&cfg, |_, _| {});
    report("measured", &r);
    let (worst, n) = r.worst_after(SETTLE_S);
    assert!(n as f64 > 0.97 * r.clicks_sent as f64, "{n} clicks heard");
    assert!(worst < BOUND_US, "worst {worst} µs");
    let p = r.tile.stats().playout.unwrap();
    assert!(p.underruns < 200, "{} underruns", p.underruns);
    let rate = r.tile.stats().rate_estimate.unwrap();
    assert!((rate - 16_000.33).abs() < 0.2, "rate {rate}");
}

#[test]
fn worst_case_skew_holds_sync_for_hours() {
    // 16,013 samples per camera second (800 ppm), camera and host 30 ppm
    // apart, device 50 ppm fast.
    let cfg = SimConfig {
        duration_s: HOURS * 3600.0,
        audio_rate: 16_013.0,
        camera_skew: 1.0 + 30e-6,
        device_skew: 1.0 + 50e-6,
        seed: 99,
        ..SimConfig::default()
    };
    let r = run(&cfg, |_, _| {});
    report("16,013 Hz", &r);
    let (worst, _) = r.worst_after(SETTLE_S);
    assert!(worst < BOUND_US, "worst {worst} µs");
    let rate = r.tile.stats().rate_estimate.unwrap();
    assert!((rate - 16_013.0).abs() < 0.3, "rate {rate}");
    // Only warm-up jumps, while the delay estimate settles.
    assert!(r.tile.stats().playout.unwrap().resyncs <= 3);
}

#[test]
fn controller_does_not_chase_jitter() {
    // The ratio correction stays near the true skew under heavy jitter: it
    // reacts to sustained offsets only.
    let cfg = SimConfig {
        duration_s: 1800.0,
        seed: 5,
        ..SimConfig::default()
    };
    let mut worst_corr = 0.0f64;
    run(&cfg, |t, tile| {
        if t > SETTLE_S {
            let c = tile.stats().playout.unwrap().correction;
            worst_corr = worst_corr.max(c.abs());
        }
    });
    println!(
        "largest correction after {SETTLE_S} s: {:.0} ppm",
        worst_corr * 1e6
    );
    // The true combined skew is under 50 ppm; jitter of 65 ms SD must not
    // swing the ratio by more than a fraction of the allowed 0.5%.
    assert!(worst_corr < 0.0015, "correction {worst_corr}");
}

#[test]
fn repeated_stalls_do_not_ratchet_latency() {
    // A 1.5 s stall every 5 minutes; everything sent during it arrives in a
    // burst at its end.
    let stalls: Vec<(f64, f64)> = (1..(HOURS * 12.0) as usize)
        .map(|k| (k as f64 * 300.0, 1.5))
        .collect();
    let cfg = SimConfig {
        duration_s: HOURS * 3600.0,
        stalls: stalls.clone(),
        seed: 3,
        ..SimConfig::default()
    };
    let r = run(&cfg, |_, _| {});
    report("stalls", &r);
    // Clicks well clear of a stall are on time.
    let near_stall = |t: f64| {
        stalls
            .iter()
            .any(|&(s, len)| t > s - 1.0 && t < s + len + 5.0)
    };
    let worst = r
        .errors()
        .filter(|(t, _)| *t > SETTLE_S && !near_stall(*t))
        .map(|(_, e)| e.abs())
        .fold(0.0, f64::max);
    assert!(worst < BOUND_US, "worst {worst} µs");
    // No upward trend: every 10-minute mean stays near the target.
    let means = bucket_means(&r, 600.0);
    println!(
        "10-minute mean deviations (ms): {:?}",
        means
            .iter()
            .map(|m| (m / 100.0).round() / 10.0)
            .collect::<Vec<_>>()
    );
    assert!(means.iter().all(|m| m.abs() < 15_000.0));
}

#[test]
fn resyncs_after_an_audio_gap() {
    // The camera sends no audio for 3 s while video continues.
    let cfg = SimConfig {
        duration_s: 600.0,
        audio_gaps: vec![(300.0, 3.0)],
        seed: 11,
        ..SimConfig::default()
    };
    let r = run(&cfg, |_, _| {});
    report("gap", &r);
    let s = r.tile.stats();
    assert_eq!(s.epoch, 1, "the timestamper started over");
    assert!(s.timestamps.resets >= 1);
    let worst = r
        .errors()
        .filter(|(t, _)| *t > SETTLE_S && !(299.0..310.0).contains(t))
        .map(|(_, e)| e.abs())
        .fold(0.0, f64::max);
    assert!(worst < BOUND_US, "worst {worst} µs");
    // Clicks resume after the gap.
    assert!(r.errors().any(|(t, _)| (305.0..315.0).contains(&t)));
}

#[test]
fn resyncs_after_a_long_stall() {
    // Nothing arrives for 6 s: the ring runs dry, the late audio is dropped
    // when it comes, and timing is right again right after.
    let cfg = SimConfig {
        duration_s: 300.0,
        stalls: vec![(120.0, 6.0)],
        seed: 12,
        ..SimConfig::default()
    };
    let r = run(&cfg, |_, _| {});
    report("long stall", &r);
    let worst = r
        .errors()
        .filter(|(t, _)| *t > SETTLE_S && !(119.0..128.0).contains(t))
        .map(|(_, e)| e.abs())
        .fold(0.0, f64::max);
    assert!(worst < BOUND_US, "worst {worst} µs");
}

#[test]
fn playout_delay_adapts_to_the_jitter() {
    use audio_pipeline::delay::{INITIAL_DELAY_US, MIN_DELAY_US};
    // A clean network (0–10 ms of jitter) and a much worse one than the
    // measurement (heavier and wider), 10 minutes each.
    let low = SimConfig {
        duration_s: 600.0,
        jitter: Jitter::Uniform { max_us: 10_000.0 },
        seed: 31,
        ..SimConfig::default()
    };
    let high = SimConfig {
        duration_s: 600.0,
        jitter: Jitter::Heavy {
            slow_sd_us: 90_000.0,
            slow_tau_s: 2.0,
            fast_scale_us: 30_000.0,
            clip_us: (-250_000.0, 350_000.0),
        },
        seed: 32,
        ..SimConfig::default()
    };
    for (name, cfg) in [("low jitter", &low), ("high jitter", &high)] {
        // Resyncs after the warm-up would be audible jumps.
        let mut resyncs_settled = 0;
        let r = run(cfg, |t, tile| {
            if t < 2.0 * SETTLE_S {
                resyncs_settled = tile.stats().playout.map_or(0, |p| p.resyncs);
            }
        });
        report(name, &r);
        let s = r.tile.stats();
        let p = s.playout.unwrap();
        let d = s.playout_delay_us;
        println!(
            "{name}: playout delay {:.0} ms (target {:.0} ms), {} underruns",
            d / 1e3,
            s.playout_delay_target_us.unwrap() / 1e3,
            p.underruns
        );
        if name == "low jitter" {
            // Shrinks to (nearly) the floor: record + output lead + 10 ms.
            assert!(d < INITIAL_DELAY_US - 100_000.0, "delay {d}");
            assert!(d < MIN_DELAY_US + 20_000.0, "delay {d}");
            assert_eq!(p.underruns, 0);
        } else {
            assert!(d > INITIAL_DELAY_US + 50_000.0, "delay {d}");
        }
        // Following the delay is smooth: every click stays on its (moving)
        // target and nothing jumps after the warm-up.
        let (worst, n) = r.worst_after(SETTLE_S);
        assert!(n > 500);
        assert!(worst < BOUND_US, "{name}: worst {worst} µs");
        assert_eq!(
            p.resyncs,
            resyncs_settled,
            "{name}: resyncs after {} s",
            2.0 * SETTLE_S
        );
    }
}

/// Host time that jitters around the frames rendered (a few ms per call),
/// or a timer loop whose ticks run a little longer than the frames they
/// render, must not cause resyncs or silence: only real gaps do.
#[test]
fn host_clock_jitter_causes_no_resyncs() {
    for (name, clock) in [
        ("jitter ±5 ms", HostClock::Jitter { us: 5_000.0 }),
        (
            "10 ms timer",
            HostClock::Sleep {
                period_us: 10_000.0,
                overshoot_us: 1_000.0,
            },
        ),
        // A 10 ms sleep as measured on macOS (about 12.2 ms) and a Windows
        // timer tick.
        (
            "12.2 ms timer",
            HostClock::Sleep {
                period_us: 12_000.0,
                overshoot_us: 500.0,
            },
        ),
        (
            "15.6 ms timer",
            HostClock::Sleep {
                period_us: 15_600.0,
                overshoot_us: 200.0,
            },
        ),
    ] {
        let cfg = SimConfig {
            duration_s: 120.0,
            callback_frames: 480,
            host_clock: clock,
            seed: 21,
            ..SimConfig::default()
        };
        let r = run(&cfg, |_, _| {});
        report(name, &r);
        let p = r.tile.stats().playout.unwrap();
        println!(
            "{name}: silent calls {}, longest silence {:.2} s",
            r.silent_calls, r.longest_silence_s
        );
        assert_eq!(p.resyncs, 0, "{name}: {p:?}");
        assert_eq!(p.starts, 1, "{name}: {p:?}");
        assert!(
            r.longest_silence_s < 0.5,
            "{name}: level 0 for {} s",
            r.longest_silence_s
        );
        assert!(r.tile.stats().playout.unwrap().overflow_frames == 0);
    }
}
