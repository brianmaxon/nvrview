//! A short soak as a normal test: a synthetic stream at 60x on a simulated
//! clock (two seconds of device time, run as fast as the CPU allows). The
//! audio settles for its warm-up minute and then has to stay in sync over
//! the next minute; the video loses nothing to failures or resets. Long
//! runs, and memory growth over them, are the `soak` tool's job.

use soak::audio_sim::WARMUP_US;
use soak::{SoakConfig, run, synth};
use std::time::Duration;

#[test]
fn two_simulated_minutes_at_60x_stay_steady_after_settling() {
    // One pass of a two-minute recording: no loop, so no second warm-up.
    let replay = synth::recording(&synth::fixtures_dir(), 120, 15).unwrap();
    let mut cfg = SoakConfig::new(replay, 60.0, Duration::from_secs(120));
    cfg.sample_every = Duration::from_millis(200);
    cfg.simulated_clock = true;
    let r = run(&cfg).unwrap();
    println!("{}", r.to_markdown());

    // Nearly every picture the stream offered reached the decoder.
    assert!(
        r.decode_actions as f64 > 0.9 * r.offered_per_tile,
        "{} pictures of {}",
        r.decode_actions,
        r.offered_per_tile
    );
    assert_eq!(r.video.failures, 0);
    assert_eq!(r.resets_seen, 0);
    assert!(r.video.audio_frames > 100);
    // Memory is sampled only where `soak::memory::current_bytes` supports it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert!(!r.memory.is_empty());

    // The audio settled within its warm-up, then held sync for the minute
    // after it, without a restart.
    let a = r.audio.as_ref().unwrap();
    assert_eq!(a.restarts, 0);
    assert_eq!(a.rejected, 0);
    assert!(a.records > 1_800, "{} audio records", a.records);
    // A sample per record (one every 64 ms) after the warm-up.
    let steady_ms = (120_000_000 - WARMUP_US) / 1000;
    assert!(a.offset_samples >= steady_ms / 64 - 10, "{a:?}");
    assert!(
        a.offset_min_us.abs() < 40_000.0 && a.offset_max_us.abs() < 40_000.0,
        "{a:?}"
    );
}
