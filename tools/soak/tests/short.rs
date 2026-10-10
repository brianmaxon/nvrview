//! A short soak as a normal test: a synthetic stream for ten simulated
//! minutes at 60x (ten seconds of wall time), checking nothing was lost to
//! failures or resets and that the audio stays in sync.

use soak::{SoakConfig, run, synth};
use std::time::Duration;

#[test]
fn ten_simulated_minutes_at_60x() {
    let replay = synth::recording(&synth::fixtures_dir(), 120, 15).unwrap();
    let mut cfg = SoakConfig::new(replay, 60.0, Duration::from_secs(600));
    cfg.sample_every = Duration::from_millis(500);
    let r = run(&cfg).unwrap();
    println!("{}", r.to_markdown());

    assert!(r.decode_actions > 1_000, "{} pictures", r.decode_actions);
    assert_eq!(r.video.failures, 0);
    assert_eq!(r.resets_seen, 0);
    assert!(r.video.audio_frames > 100);
    // Memory is sampled only where `soak::memory::current_bytes` supports it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert!(!r.memory.is_empty());
    if let Some(t) = &r.trend {
        assert!(t.stable, "{t:?}");
    }

    let a = r.audio.as_ref().unwrap();
    assert!(a.records > 8_000, "{} audio records", a.records);
    assert_eq!(a.rejected, 0);
    assert!(a.offset_samples > 1_000);
    assert!(
        a.offset_min_us.abs() < 40_000.0 && a.offset_max_us.abs() < 40_000.0,
        "{a:?}"
    );
}
