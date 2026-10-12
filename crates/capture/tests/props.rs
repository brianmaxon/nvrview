//! Property tests: the invariants that hold for any input.

use capture::epoch::{AudioPlacer, Timeline, TimelineConfig};
use capture::mixer::{LIMITER_CEILING, soft_limit};
use capture::partials::Registry;
use proptest::prelude::*;

proptest! {
    #[test]
    fn the_limiter_never_exceeds_its_ceiling(x in any::<f32>()) {
        let y = soft_limit(x);
        prop_assert!(y.is_finite());
        prop_assert!(y.abs() <= LIMITER_CEILING);
        if x.is_finite() && x.abs() <= 0.8 {
            prop_assert_eq!(y, x);
        }
    }

    #[test]
    fn the_file_clock_never_runs_backwards_across_epochs(
        steps in proptest::collection::vec((any::<u32>(), 0i64..20_000_000), 1..200),
        start in any::<u32>(),
    ) {
        let mut t = Timeline::new(TimelineConfig::default());
        let mut raw = u64::from(start);
        let mut host = 0i64;
        let mut prev: Option<i64> = None;
        for (jump, dhost) in steps {
            // Mostly small steps, sometimes anything.
            raw = if jump % 7 == 0 {
                u64::from(jump)
            } else {
                (raw + u64::from(jump % 80_000)) % (1 << 32)
            };
            host += dhost;
            let p = t.push_video(raw, host);
            if let Some(prev) = prev {
                if p.new_epoch {
                    prop_assert!(p.file_us > prev);
                } else {
                    // Only a small reorder step may go back.
                    prop_assert!(p.file_us >= prev - 1_000_000);
                }
            }
            prev = Some(p.file_us);
        }
    }

    #[test]
    fn placed_audio_never_overlaps(
        chunks in proptest::collection::vec((-1_000_000i64..60_000_000, 1usize..4096), 1..100),
    ) {
        let mut p = AudioPlacer::new(16_000, 20_000);
        let mut end = 0i64;
        for (t, n) in chunks {
            if let Some(pl) = p.place(t, n) {
                prop_assert!(pl.pos >= end, "pos {} < end {}", pl.pos, end);
                prop_assert!(pl.skip < n);
                prop_assert!(pl.pos >= 0);
                end = pl.pos + (n - pl.skip) as i64;
                prop_assert_eq!(p.next_pos(), end);
            }
        }
    }

    #[test]
    fn registry_bytes_never_panic_and_only_yield_valid_entries(
        bytes in proptest::collection::vec(any::<u8>(), 0..400),
    ) {
        let l = Registry::from_bytes(&bytes);
        prop_assert!(l.registry.entries().iter().all(|e| e.is_valid()));
        prop_assert!(l.registry.entries().len() <= capture::partials::MAX_ENTRIES);
    }
}
