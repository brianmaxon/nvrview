//! Sound analysis allocates nothing per record once a stream is running:
//! the 16 kHz mono form (with and without resampling) and the analyzers,
//! checked with a counting global allocator. One test only, so no other
//! test thread allocates while it is armed.

// A global allocator can only be written with `unsafe`; this test-only one
// forwards to the system allocator and counts.
#![allow(unsafe_code)]

use audio_pipeline::{PcmFormat, TileAudio, TileConfig};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use viewer_core::sound::{SoundOutput, SoundSettings, TileSound};

struct Counting;

static ARMED: AtomicBool = AtomicBool::new(false);
static EVENTS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.load(Ordering::Relaxed) {
            EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations while the mono form and the analyzers handle records
/// `warm..total` of a T3 alarm in `format` (the tile's own pipeline runs
/// outside the count).
fn count(format: PcmFormat, total: usize, warm: usize) -> (u64, usize) {
    let mut tile = TileAudio::new(TileConfig::default());
    tile.set_mono_output(true);
    let mut sound = TileSound::new(&SoundSettings::default()).unwrap();
    let rate = format.sample_rate as usize;
    let frames = 1024 * rate / 16_000;
    let mut pcm = vec![0.0f32; frames * format.channels];
    let mut alarms = 0;
    let mut events = 0;
    let base = EVENTS.load(Ordering::SeqCst);
    for k in 0..total {
        for (i, f) in pcm.chunks_mut(format.channels).enumerate() {
            let t = (k * frames + i) as f64 / rate as f64;
            let c = t % 4.0;
            let v = if c < 3.0 && c % 1.0 < 0.5 {
                0.3 * (2.0 * std::f64::consts::PI * 3100.0 * t).sin() as f32
            } else {
                0.0
            };
            f.fill(v);
        }
        let t_us = (k * 64_000) as u64;
        tile.push_pcm(
            format,
            &pcm,
            Some(t_us),
            1_000_000 + t_us,
            Some(1_700_000_000_000_000 + t_us as i64),
        );
        if k >= warm {
            ARMED.store(true, Ordering::SeqCst);
        }
        // Only the analysis is counted here; `count_conversion` checks the
        // conversion inside `push_pcm`.
        if let Some(b) = tile.mono_block() {
            sound.push(&b, 0, |o| {
                if let SoundOutput::Alarm { .. } = o {
                    alarms += 1;
                }
            });
        }
        ARMED.store(false, Ordering::SeqCst);
        if k >= warm {
            events = EVENTS.load(Ordering::SeqCst) - base;
        }
    }
    (events, alarms)
}

/// The conversion itself, under the count: a tile pushing records with
/// only the mono form on, after warming up.
fn count_conversion(format: PcmFormat) -> u64 {
    let mut tile = TileAudio::new(TileConfig::default());
    tile.set_mono_output(true);
    let frames = 1024 * format.sample_rate as usize / 16_000;
    let pcm: Vec<f32> = (0..frames * format.channels)
        .map(|i| (i as f32 * 0.01).sin() * 0.2)
        .collect();
    let mut with = 0;
    let mut without = 0;
    // Same pushes with the conversion off and on: any difference is the
    // conversion's.
    for (mono, total) in [(false, &mut without), (true, &mut with)] {
        tile.set_mono_output(mono);
        for k in 0..400u64 {
            let t_us = k * 64_000;
            let before = EVENTS.load(Ordering::SeqCst);
            ARMED.store(k >= 200, Ordering::SeqCst);
            tile.push_pcm(
                format,
                &pcm,
                Some(t_us),
                1_000_000 + t_us,
                Some(t_us as i64),
            );
            ARMED.store(false, Ordering::SeqCst);
            *total += EVENTS.load(Ordering::SeqCst) - before;
        }
    }
    with.saturating_sub(without)
}

#[test]
fn sound_analysis_does_not_allocate_per_record() {
    for format in [
        PcmFormat {
            sample_rate: 16_000,
            channels: 1,
        },
        PcmFormat {
            sample_rate: 48_000,
            channels: 2,
        },
    ] {
        let (events, alarms) = count(format, 400, 100);
        assert_eq!(events, 0, "{format:?}: allocations in the analyzers");
        assert_eq!(alarms, 1, "{format:?}: the alarm was found");
        assert_eq!(
            count_conversion(format),
            0,
            "{format:?}: allocations in the conversion"
        );
    }
}
