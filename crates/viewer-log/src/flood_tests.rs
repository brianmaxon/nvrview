//! End-to-end tests: rate limiter, formatter, bounded queue, rotating writer.

use std::fs;
use std::time::Duration;

use tracing::level_filters::LevelFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;

use crate::{
    LogConfig, ManualClock, RateLimitConfig, build_pipeline_with_clock, total_bytes_on_disk,
};

fn config(dir: &std::path::Path) -> LogConfig {
    let mut c = LogConfig::new(dir, "flood");
    c.max_file_bytes = 2_000;
    c.max_files = 3;
    c.max_total_bytes = 6_000;
    c.stderr = false;
    c.queue_lines = 100_000;
    c.rate_limit = RateLimitConfig {
        burst: 5,
        max_per_interval: 20,
        interval: Duration::from_secs(1),
        ..RateLimitConfig::default()
    };
    c
}

fn read_all(dir: &std::path::Path) -> String {
    let mut out = String::new();
    for e in fs::read_dir(dir).unwrap().flatten() {
        out.push_str(&fs::read_to_string(e.path()).unwrap_or_default());
    }
    out
}

#[test]
fn million_identical_errors_stay_bounded_and_are_summarized() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let clock = ManualClock::new();
    let p = build_pipeline_with_clock(&cfg, clock.clone()).unwrap();
    let sub = tracing_subscriber::registry().with(p.limiter.clone()).with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(p.writer.clone())
            .with_filter(LevelFilter::INFO),
    );
    let cap = cfg.max_total_bytes;

    tracing::subscriber::with_default(sub, || {
        for i in 0..1_000_000u32 {
            tracing::error!("hardware decoder rejected frame");
            if i % 10_000 == 0 {
                clock.advance(Duration::from_secs(1)); // let some through so files rotate
            }
        }
        clock.advance(Duration::from_secs(60));
        tracing::error!("hardware decoder rejected frame");
    });
    p.guard.flush();

    let total = total_bytes_on_disk(dir.path(), "flood");
    assert!(total <= cap + 512, "total {total} exceeds cap {cap}");
    let all = read_all(dir.path());
    assert!(
        all.contains("similar messages from viewer_log::flood_tests"),
        "no summary in:\n{all}"
    );
    assert!(all.contains("suppressed "));
    assert_eq!(p.guard.stats().io_errors, 0);
    let files = fs::read_dir(dir.path()).unwrap().count();
    assert!(files <= 3, "{files} files");
}

#[test]
fn final_summary_flushed_on_guard_drop() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let p = build_pipeline_with_clock(&cfg, ManualClock::new()).unwrap();
    let sub = tracing_subscriber::registry().with(p.limiter.clone()).with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(p.writer.clone()),
    );
    tracing::subscriber::with_default(sub, || {
        for _ in 0..1_005 {
            tracing::error!("boom");
        }
    });
    drop(p.guard);
    let all = read_all(dir.path());
    assert!(all.contains("suppressed 1,000 similar messages"), "{all}");
}
