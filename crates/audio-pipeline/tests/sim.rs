//! Synchronization on the simulated camera, network, and device (see
//! `common/mod.rs`). Each test prints its measurements; run with
//! `--nocapture` to see them.
//!
//! Long-run behavior is proven directly, in minutes of simulated time, not by
//! simulating hours:
//!
//! - **Drift does not accumulate.** The estimators land on exact, known clock
//!   ratios (`timestamper_lands_on_exact_drift`,
//!   `presentation_estimator_lands_on_exact_clock_ratio`). With exactly
//!   linear clocks and no jitter, the whole loop settles and is then
//!   stationary: every click within 5 ms of its target, the rate estimate
//!   right, and the resampling correction equal to the true skew (a
//!   correction equal to the true skew means playout runs exactly as fast as
//!   the source, so the error cannot grow however long it runs). With jitter
//!   like the measurement, every click stays within 40 ms, the rate estimate
//!   stays right, and jitter does not swing the correction. Both for the
//!   measured clocks and for the worst case (800 ppm audio, 30 ppm camera to
//!   host, 50 ppm device).
//! - **Stalls do not ratchet latency.** After one stall, and after several
//!   in a row, the latency equals that of the same run without the stalls.
//! - **Long-running numerics.** The same checks with every clock and counter
//!   (host time, camera timestamps, audio sample count, output ring) started
//!   as if it had run for 3 hours and for a week.
//! - **The adaptive playout delay** settles where the jitter puts it and
//!   then stays on its target.
//!
//! The simulation is event-stepped: it jumps from one arrival to the next
//! and renders the device callbacks due in between (512 frames each). The
//! renderer is part of the loop under test (it publishes which ring frame
//! plays when, and the controller acts on that), so its samples are still
//! produced; what makes the tests fast is running minutes, not hours.
//!
//! The original 3-hour runs are kept as one ignored test, run by hand when a
//! sync problem is suspected (never scheduled):
//!
//! ```text
//! cargo nextest run -p audio-pipeline --test sim --run-ignored only --no-capture
//! ```

mod common;

use audio_pipeline::presentation::PresentationEstimator;
use audio_pipeline::timestamp::Timestamper;
use common::*;

/// Required: every click within this of the intended latency.
const BOUND_US: f64 = 40_000.0;
/// Camera seconds after a start before the bound applies: the delivery-delay
/// estimate needs this long to see enough of the jitter.
const SETTLE_S: f64 = 60.0;
/// Length of the runs with exact clock drift and no jitter, camera seconds.
const EXACT_RUN_S: f64 = 420.0;
/// Length of the runs with jitter, camera seconds.
const JITTER_RUN_S: f64 = 300.0;
/// Without jitter, every click after [`SETTLE_S`] is within this of its
/// target. Drift that accumulated at even a few µs per second would leave
/// it within the run.
const EXACT_BOUND_US: f64 = 5_000.0;
/// From here on (camera seconds) the correction of a run without jitter is
/// stationary.
const CORRECTION_SETTLE_S: f64 = 180.0;
/// Window over which the correction is averaged, seconds.
const CORRECTION_WINDOW_S: f64 = 60.0;
/// Without jitter, each window's mean correction is within this of the
/// true skew (the presentation estimator's slope is still converging)...
const CORRECTION_WINDOW_TOL: f64 = 15e-6;
/// ...and the mean over all of them is within this: a lasting difference
/// from the true skew is drift.
const CORRECTION_TOL: f64 = 3e-6;
/// Jitter must not swing the correction (the ratio) by more than this
/// around the true skew: a fraction of the allowed 0.5%.
const CORRECTION_SWING: f64 = 0.0015;
/// The rate estimate stays within this of the true rate once its 60 s
/// window is full: at 7.5 fps it wanders by a few tenths of a hertz as the
/// picture grid beats against the records (see
/// [`timestamper_lands_on_exact_drift`]).
const RATE_TOL_HZ: f64 = 0.5;
/// After a stall, the latency is within this of the same run without it,
/// without jitter...
const EXACT_STALL_TOL_US: f64 = 2_000.0;
/// ...and with it.
const STALL_TOL_US: f64 = 15_000.0;
/// Seconds per day, for the long-numerics runs.
const DAY_S: f64 = 86_400.0;

/// The tile's state after one record.
#[derive(Clone, Copy, Debug)]
struct Sample {
    t: f64,
    correction: f64,
    rate: Option<f64>,
    delay_us: f64,
    target_us: Option<f64>,
    resyncs: u64,
}

fn run_traced(cfg: &SimConfig) -> (SimResult, Vec<Sample>) {
    let mut trace = Vec::new();
    let r = run(cfg, |t, tile| {
        let s = tile.stats();
        let p = s.playout.unwrap_or_default();
        trace.push(Sample {
            t,
            correction: p.correction,
            rate: s.rate_estimate,
            delay_us: s.playout_delay_us,
            target_us: s.playout_delay_target_us,
            resyncs: p.resyncs,
        });
    });
    (r, trace)
}

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

/// Mean correction per whole window of [`CORRECTION_WINDOW_S`] from
/// `from_s` on.
fn correction_means(trace: &[Sample], from_s: f64) -> Vec<f64> {
    let mut sums: Vec<(f64, u32)> = Vec::new();
    let end = trace.last().map_or(0.0, |s| s.t);
    let windows = ((end - from_s) / CORRECTION_WINDOW_S).floor().max(0.0) as usize;
    sums.resize(windows, (0.0, 0));
    for s in trace.iter().filter(|s| s.t >= from_s) {
        let b = ((s.t - from_s) / CORRECTION_WINDOW_S) as usize;
        if let Some(w) = sums.get_mut(b) {
            w.0 += s.correction;
            w.1 += 1;
        }
    }
    sums.iter()
        .filter(|s| s.1 > 0)
        .map(|s| s.0 / f64::from(s.1))
        .collect()
}

/// Mean latency (host time heard minus host time captured) of the clicks
/// whose source sample lies in `from_s..to_s`.
fn mean_latency(r: &SimResult, from_s: f64, to_s: f64) -> f64 {
    let v: Vec<f64> = r
        .heard
        .iter()
        .filter(|h| (from_s..to_s).contains(&h.camera_s))
        .map(|h| h.latency_us)
        .collect();
    assert!(v.len() > 10, "{} clicks in {from_s}..{to_s} s", v.len());
    v.iter().sum::<f64>() / v.len() as f64
}

/// Checks common to every settled run: the rate estimate right, no resyncs
/// after the warm-up.
fn check_estimates(name: &str, cfg: &SimConfig, trace: &[Sample]) {
    let rate_off = trace
        .iter()
        .filter(|s| s.t > 2.0 * SETTLE_S)
        .map(|s| (s.rate.unwrap() - cfg.audio_rate).abs())
        .fold(0.0, f64::max);
    println!(
        "{name}: rate estimate within {rate_off:.3} Hz after {} s",
        2.0 * SETTLE_S
    );
    assert!(rate_off < RATE_TOL_HZ, "{name}: rate off by {rate_off} Hz");
    let at_settle = trace.iter().find(|s| s.t > SETTLE_S).unwrap().resyncs;
    let at_end = trace.last().unwrap().resyncs;
    assert_eq!(at_settle, at_end, "{name}: resyncs after {SETTLE_S} s");
}

/// The clocks drift exactly linearly and nothing jitters: once settled, the
/// loop is stationary. Every click is within a few ms of its target, the
/// rate estimate is right, and the correction equals the true combined skew
/// (per minute within the rate estimate's wander, on average within a few
/// ppm). A correction equal to the true skew means playout runs exactly as
/// fast as the source, so the error cannot grow however long it runs.
fn exact_drift_is_stationary(name: &str, cfg: SimConfig) {
    let cfg = SimConfig {
        duration_s: EXACT_RUN_S,
        ..exact(cfg)
    };
    let (r, trace) = run_traced(&cfg);
    report(name, &r);
    let (worst, n) = r.worst_after(SETTLE_S);
    let expected_clicks = (cfg.duration_s - SETTLE_S) * cfg.audio_rate / cfg.click_every as f64;
    assert!(
        n as f64 > 0.99 * expected_clicks,
        "{name}: {n} clicks heard of {expected_clicks:.0}"
    );
    assert!(worst < EXACT_BOUND_US, "{name}: worst {worst} µs");
    check_estimates(name, &cfg, &trace);

    let truth = cfg.true_correction();
    let means = correction_means(&trace, CORRECTION_SETTLE_S);
    let mean = means.iter().sum::<f64>() / means.len() as f64;
    println!(
        "{name}: true skew {:.1} ppm; correction per {CORRECTION_WINDOW_S} s {:?} ppm, mean {:.1} ppm",
        truth * 1e6,
        means
            .iter()
            .map(|m| (m * 1e7).round() / 10.0)
            .collect::<Vec<_>>(),
        mean * 1e6
    );
    assert!(means.len() >= 3, "{name}: {} windows", means.len());
    for m in &means {
        assert!(
            (m - truth).abs() < CORRECTION_WINDOW_TOL,
            "{name}: correction {:.1} ppm over a window, true {:.1} ppm",
            m * 1e6,
            truth * 1e6
        );
    }
    assert!(
        (mean - truth).abs() < CORRECTION_TOL,
        "{name}: mean correction {:.1} ppm, true {:.1} ppm",
        mean * 1e6,
        truth * 1e6
    );
}

/// The same clocks with jitter like the measurement: once settled, every
/// click is within the bound, the rate estimate is right, jitter does not
/// swing the correction, and nothing jumps.
fn jitter_holds(name: &str, cfg: SimConfig) -> SimResult {
    let cfg = SimConfig {
        duration_s: JITTER_RUN_S,
        ..cfg
    };
    let (r, trace) = run_traced(&cfg);
    report(name, &r);
    let (worst, n) = r.worst_after(SETTLE_S);
    let expected_clicks = (cfg.duration_s - SETTLE_S) * cfg.audio_rate / cfg.click_every as f64;
    assert!(
        n as f64 > 0.97 * expected_clicks,
        "{name}: {n} clicks heard of {expected_clicks:.0}"
    );
    assert!(worst < BOUND_US, "{name}: worst {worst} µs");
    check_estimates(name, &cfg, &trace);
    let truth = cfg.true_correction();
    let swing = trace
        .iter()
        .filter(|s| s.t > SETTLE_S)
        .map(|s| (s.correction - truth).abs())
        .fold(0.0, f64::max);
    println!(
        "{name}: largest correction swing after {SETTLE_S} s: {:.0} ppm",
        swing * 1e6
    );
    assert!(swing < CORRECTION_SWING, "{name}: swing {swing}");
    let p = r.tile.stats().playout.unwrap();
    assert!(p.underruns < 10, "{name}: {} underruns", p.underruns);
    r
}

/// `cfg` with exactly linear clocks: no jitter, and pictures every 8 ms so
/// that the picture grid does not quantize the timestamps either (its
/// effect at 7.5 fps is the rate estimate's wander, bounded in
/// [`timestamper_lands_on_exact_drift`]).
fn exact(cfg: SimConfig) -> SimConfig {
    SimConfig {
        jitter: Jitter::Uniform { max_us: 0.0 },
        fps: 125.0,
        ..cfg
    }
}

/// Audio 20 ppm fast in camera time, camera and host 8 ppm apart, device
/// 20 ppm slow, jitter like the measurement.
fn measured() -> SimConfig {
    SimConfig {
        seed: 5,
        ..SimConfig::default()
    }
}

/// 16,013 samples per camera second (800 ppm), camera and host 30 ppm
/// apart, device 50 ppm fast.
fn worst_case() -> SimConfig {
    SimConfig {
        audio_rate: 16_013.0,
        camera_skew: 1.0 + 30e-6,
        device_skew: 1.0 + 50e-6,
        seed: 99,
        ..SimConfig::default()
    }
}

/// The timestamper's rate estimate lands on the known rate when the clocks
/// drift exactly linearly: with a picture at every record's send time (no
/// quantization at all) it is exact; with pictures on a 7.5 fps grid it
/// wanders by a few tenths of a hertz around it (the grid beats against the
/// records) and averages out to it. Also from a sample count and camera
/// time a week in.
#[test]
fn timestamper_lands_on_exact_drift() {
    for rate in [16_000.33, 16_013.0, 15_987.0] {
        for base in [0u64, (7.0 * DAY_S * 16_000.0) as u64] {
            for fps in [None, Some(7.5)] {
                let mut ts = Timestamper::with_base(16_000, base);
                let t0 = PTS0_US + if base > 0 { 7.0 * DAY_S * 1e6 } else { 0.0 };
                let (mut worst, mut sum, mut n) = (0.0f64, 0.0, 0u32);
                for k in 0..(3.0 * 3600.0 * rate / 1024.0) as u64 {
                    let t_end = t0 + (k + 1) as f64 * 1024.0 * 1e6 / rate;
                    let pts = match fps {
                        None => t_end,
                        Some(f) => t0 + ((t_end - t0) * f / 1e6).floor() * 1e6 / f,
                    };
                    let s = ts.push(1024, Some(pts.round() as u64));
                    if k as f64 * 1024.0 / rate > 2.0 * SETTLE_S {
                        let err = s.timeline.unwrap().rate() - rate;
                        worst = worst.max(err.abs());
                        sum += err;
                        n += 1;
                    }
                }
                let mean = sum / f64::from(n);
                println!(
                    "rate {rate}, base {base}, fps {fps:?}: worst {worst:.4} Hz, mean {mean:.4} Hz"
                );
                let (tol, mean_tol) = match fps {
                    None => (0.005, 0.001),
                    Some(_) => (RATE_TOL_HZ, 0.05),
                };
                assert!(
                    worst < tol,
                    "rate {rate}, base {base}, fps {fps:?}: {worst} Hz"
                );
                assert!(
                    mean.abs() < mean_tol,
                    "rate {rate}, base {base}, fps {fps:?}: mean {mean} Hz"
                );
            }
        }
    }
}

/// The presentation estimator's clock ratio lands on the known camera-to-host
/// skew when arrivals drift exactly linearly. Its slope prior (1.0) fades as
/// its 30-minute window fills, so this feeds it two hours of arrivals (just
/// the estimator: under a second), after which the prior's pull is under
/// 0.5% of the skew.
fn presentation_estimator_lands_on(skew_ppm: f64, start_s: f64) {
    let skew = 1.0 + skew_ppm * 1e-6;
    let mut e = PresentationEstimator::new();
    let host0 = HOST0_US + start_s * 1e6;
    let pts0 = PTS0_US + start_s * 1e6;
    // About 16 records per second, as the audio delivers them.
    let step_us = 1024.0 / 16_000.0 * 1e6;
    let n = (2.0 * 3600.0 * 1e6 / step_us) as u64;
    for k in 0..n {
        let c = k as f64 * step_us;
        e.observe(pts0 + c, host0 + c * skew + 200_000.0);
    }
    let m = e.map().unwrap();
    let got_ppm = (m.host_per_camera - 1.0) * 1e6;
    let c = n as f64 * step_us;
    let err = m.host_at(pts0 + c) - (host0 + c * skew + 200_000.0);
    println!(
        "skew {skew_ppm} ppm, start {start_s} s: estimate {got_ppm:.3} ppm, offset {err:.1} µs"
    );
    assert!(
        (got_ppm - skew_ppm).abs() < 0.005 * skew_ppm.abs() + 0.05,
        "skew {skew_ppm} ppm: estimate {got_ppm} ppm"
    );
    assert!(err.abs() < 100.0, "offset {err} µs");
}

#[test]
fn presentation_estimator_lands_on_exact_clock_ratio() {
    presentation_estimator_lands_on(8.0, 0.0);
}

/// A large skew, with clocks a week in.
#[test]
fn presentation_estimator_lands_on_a_large_clock_ratio() {
    presentation_estimator_lands_on(-150.0, 7.0 * DAY_S);
}

#[test]
fn measured_clocks_settle_and_stay_stationary() {
    exact_drift_is_stationary("measured, exact", measured());
}

#[test]
fn measured_conditions_hold_sync_under_jitter() {
    jitter_holds("measured", measured());
}

#[test]
fn worst_case_skew_settles_and_stays_stationary() {
    exact_drift_is_stationary("16,013 Hz, exact", worst_case());
}

#[test]
fn worst_case_skew_holds_sync_under_jitter() {
    jitter_holds("16,013 Hz", worst_case());
}

/// Every clock and counter starts as if it had run for 3 hours, and for a
/// week: host time, camera timestamps, the audio sample count, the output
/// ring. Timestamp precision, counter widths and rounding that grow with
/// magnitude would show against the tight bounds of the runs without
/// jitter.
#[test]
fn numerics_hold_three_hours_in() {
    exact_drift_is_stationary("measured, 3 h in", measured().started_after(3.0 * 3600.0));
}

#[test]
fn numerics_hold_a_week_in() {
    exact_drift_is_stationary(
        "16,013 Hz, 7 days in",
        worst_case().started_after(7.0 * DAY_S),
    );
}

#[test]
fn numerics_hold_a_week_in_under_jitter() {
    jitter_holds("measured, 7 days in", measured().started_after(7.0 * DAY_S));
}

/// Stalls (start, length in camera seconds): nothing arrives, then
/// everything sent during it in a burst. After recovery the latency (host time heard minus host time
/// captured) equals that of the same run without the stalls (same seed),
/// within `tol_us`: the stall left nothing behind, in the ring or in the
/// presentation reference. The playout delay is fixed here; that a rare
/// stall does not raise the adaptive one is a unit test in `delay.rs`
/// (`rare_stalls_do_not_move_the_target`). Clicks clear of the stalls stay
/// on their target.
fn stalls_keep_latency(name: &str, base: SimConfig, stalls: &[(f64, f64)], tol_us: f64) {
    let last = stalls.last().unwrap().0;
    let clean = SimConfig {
        duration_s: last + 75.0,
        playout_delay_us: Some(280_000.0),
        ..base
    };
    let cfg = SimConfig {
        stalls: stalls.to_vec(),
        ..clean.clone()
    };
    let r = run(&cfg, |_, _| {});
    let r0 = run(&clean, |_, _| {});
    report(name, &r);
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
    assert!(worst < BOUND_US, "{name}: worst {worst} µs");
    // Before the first stall the runs are the same.
    let first = stalls[0].0;
    let before =
        mean_latency(&r, first - 60.0, first - 1.0) - mean_latency(&r0, first - 60.0, first - 1.0);
    assert!(before.abs() < 100.0, "{name}: {before} µs apart before");
    // From 10 s after each stall to the next one (or to the end).
    let mut extra = Vec::new();
    for (i, &(s, len)) in stalls.iter().enumerate() {
        let from = s + len + 10.0;
        let to = stalls.get(i + 1).map_or(cfg.duration_s, |&(n, _)| n - 1.0);
        extra.push(mean_latency(&r, from, to) - mean_latency(&r0, from, to));
    }
    println!(
        "{name}: latency added after each stall {:?} ms",
        extra
            .iter()
            .map(|a| (a / 100.0).round() / 10.0)
            .collect::<Vec<_>>(),
    );
    for a in &extra {
        assert!(
            a.abs() < tol_us,
            "{name}: latency {:.1} ms higher after a stall",
            a / 1e3
        );
    }
}

#[test]
fn a_stall_does_not_raise_latency() {
    stalls_keep_latency(
        "one stall",
        exact(SimConfig::default()),
        &[(150.0, 1.5)],
        EXACT_STALL_TOL_US,
    );
}

/// Several stalls in a row: 1.5 s ones (the ring runs dry and playout
/// jumps back onto its target) and a 0.5 s one (the ring runs dry for less
/// than the resync threshold, so only the renderer's dropping of late
/// frames keeps the schedule), without jitter and with jitter like the
/// measurement (where the run without stalls wanders a few ms differently
/// after each resync, so the tolerance is wider; a ratchet adds hundreds).
const STALLS: [(f64, f64); 3] = [(150.0, 1.5), (200.0, 0.5), (250.0, 1.5)];

#[test]
fn repeated_stalls_do_not_ratchet_latency() {
    stalls_keep_latency(
        "three stalls",
        exact(SimConfig::default()),
        &STALLS,
        EXACT_STALL_TOL_US,
    );
}

#[test]
fn repeated_stalls_do_not_ratchet_latency_under_jitter() {
    stalls_keep_latency(
        "three stalls, jitter",
        SimConfig {
            seed: 4,
            ..SimConfig::default()
        },
        &STALLS,
        STALL_TOL_US,
    );
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

/// The adaptive playout delay settles where the jitter puts it and then
/// stays there: near the floor on a clean network, well above the initial
/// delay on a much worse one than the measurement. Once settled, the delay
/// in use equals its target and is stationary, every click stays on its
/// (moving) target, and nothing jumps after the warm-up.
#[test]
fn playout_delay_adapts_to_low_jitter() {
    playout_delay_adapts_to("low jitter");
}

#[test]
fn playout_delay_adapts_to_high_jitter() {
    playout_delay_adapts_to("high jitter");
}

fn playout_delay_adapts_to(which: &str) {
    use audio_pipeline::delay::{INITIAL_DELAY_US, MIN_DELAY_US};
    let run_s = 300.0;
    // Settled: the warm-up allowance has faded (120 s) and the delay has had
    // time to slew to the target (2 ms/s) and approach it (10 s).
    let settled_s = 240.0;
    let low = SimConfig {
        duration_s: run_s,
        jitter: Jitter::Uniform { max_us: 10_000.0 },
        seed: 31,
        ..SimConfig::default()
    };
    let high = SimConfig {
        duration_s: run_s,
        jitter: Jitter::Heavy {
            slow_sd_us: 90_000.0,
            slow_tau_s: 2.0,
            fast_scale_us: 30_000.0,
            clip_us: (-250_000.0, 350_000.0),
        },
        seed: 32,
        ..SimConfig::default()
    };
    for (name, cfg) in [("low jitter", &low), ("high jitter", &high)]
        .into_iter()
        .filter(|(n, _)| *n == which)
    {
        let (r, trace) = run_traced(cfg);
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
        // Stationary once settled: the delay in use stays within a few ms of
        // its target and moves less than that.
        let settled: Vec<&Sample> = trace.iter().filter(|s| s.t > settled_s).collect();
        let (lo, hi) = settled.iter().fold((f64::MAX, f64::MIN), |(lo, hi), s| {
            (lo.min(s.delay_us), hi.max(s.delay_us))
        });
        let off = settled
            .iter()
            .map(|s| (s.delay_us - s.target_us.unwrap()).abs())
            .fold(0.0, f64::max);
        println!(
            "{name}: after {settled_s} s the delay spans {:.1} ms, at most {:.1} ms off its target",
            (hi - lo) / 1e3,
            off / 1e3
        );
        assert!(hi - lo < 10_000.0, "{name}: delay spans {} µs", hi - lo);
        assert!(off < 5_000.0, "{name}: delay {off} µs off its target");
        // Following the delay is smooth: every click stays on its (moving)
        // target and nothing jumps after the warm-up.
        let (worst, n) = r.worst_after(SETTLE_S);
        assert!(n > 200);
        assert!(worst < BOUND_US, "{name}: worst {worst} µs");
        let at = trace.iter().find(|s| s.t > 2.0 * SETTLE_S).unwrap().resyncs;
        assert_eq!(p.resyncs, at, "{name}: resyncs after {} s", 2.0 * SETTLE_S);
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

/// The original 3-hour runs, kept for when a sync problem is suspected:
/// the measured conditions, the worst-case skew, and a 1.5 s stall every
/// five minutes, each for 3 hours of simulated time (about 20 s each on a
/// fast machine, a minute or more on a slow one; they run in parallel).
/// Not run by `scripts/check.sh`. Run it by hand:
///
/// ```text
/// cargo nextest run -p audio-pipeline --test sim --run-ignored only --no-capture
/// ```
#[test]
#[ignore = "3 hours of simulated audio; run by hand when a sync problem is suspected"]
fn three_hour_runs_hold_sync() {
    const HOURS: f64 = 3.0;
    let long = HOURS * 3600.0;
    std::thread::scope(|s| {
        s.spawn(|| {
            let cfg = SimConfig {
                duration_s: long,
                ..SimConfig::default()
            };
            let r = run(&cfg, |_, _| {});
            report("measured, 3 h", &r);
            let (worst, n) = r.worst_after(SETTLE_S);
            assert!(n as f64 > 0.97 * r.clicks_sent as f64, "{n} clicks heard");
            assert!(worst < BOUND_US, "worst {worst} µs");
            let p = r.tile.stats().playout.unwrap();
            assert!(p.underruns < 200, "{} underruns", p.underruns);
            let rate = r.tile.stats().rate_estimate.unwrap();
            assert!((rate - 16_000.33).abs() < 0.2, "rate {rate}");
        });
        s.spawn(|| {
            let cfg = SimConfig {
                duration_s: long,
                ..worst_case()
            };
            let r = run(&cfg, |_, _| {});
            report("16,013 Hz, 3 h", &r);
            let (worst, _) = r.worst_after(SETTLE_S);
            assert!(worst < BOUND_US, "worst {worst} µs");
            let rate = r.tile.stats().rate_estimate.unwrap();
            assert!((rate - 16_013.0).abs() < 0.3, "rate {rate}");
            assert!(r.tile.stats().playout.unwrap().resyncs <= 3);
        });
        s.spawn(|| {
            // A 1.5 s stall every 5 minutes; everything sent during it
            // arrives in a burst at its end.
            let stalls: Vec<(f64, f64)> = (1..(HOURS * 12.0) as usize)
                .map(|k| (k as f64 * 300.0, 1.5))
                .collect();
            let cfg = SimConfig {
                duration_s: long,
                stalls: stalls.clone(),
                seed: 3,
                ..SimConfig::default()
            };
            let r = run(&cfg, |_, _| {});
            report("stalls, 3 h", &r);
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
            assert!(means.iter().all(|m| m.abs() < 15_000.0), "{means:?}");
        });
    });
}
