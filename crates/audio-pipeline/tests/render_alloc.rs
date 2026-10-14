//! The render path never allocates or frees: checked with a counting global
//! allocator around every `render` call, across normal playback, volume
//! changes, skips (resyncs), underruns, and slots being released and
//! reclaimed.
//!
//! Lock freedom is by construction (see the `output` module documentation):
//! the renderer touches shared state only through atomics.
//!
//! Only the test thread's allocations count: under `cargo test` the other
//! tests of this binary run on other threads of the same process.

// A global allocator can only be written with `unsafe`; this test-only one
// forwards to the system allocator and counts.
#![allow(unsafe_code)]

use audio_pipeline::{OutputConfig, OutputFormat, PcmFormat, TileAudio, TileConfig, output};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

struct Counting;

thread_local! {
    // `const` with no destructor: reading it never allocates.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}
static EVENTS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.with(Cell::get) {
            EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.with(Cell::get) {
            EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.with(Cell::get) {
            EVENTS.fetch_add(1, Ordering::Relaxed);
        }
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn render_never_allocates() {
    let (out, mut renderer) = output(
        OutputFormat {
            sample_rate: 48_000,
            channels: 2,
        },
        OutputConfig {
            max_tiles: 4,
            buffer_ms: 500,
        },
    );
    let mut tiles: Vec<TileAudio> = (0..4)
        .map(|_| {
            let mut t = TileAudio::new(TileConfig::default());
            t.set_output(Some(out.clone()));
            t.set_audible(true);
            t
        })
        .collect();
    let fmt = PcmFormat {
        sample_rate: 16_000,
        channels: 1,
    };
    let pcm: Vec<f32> = (0..1024).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
    let mut buf = vec![0.0f32; 2 * 480];
    let mut host_us = 10_000_000.0f64;
    let mut pts = 0u64;
    let mut calls = 0u64;
    let mut renders = 0u64;
    let mut sample_clock = 0.0f64;
    for step in 0..20_000u64 {
        // 10 ms per render call; one record every 64 ms per tile, with
        // occasional holes (underruns), a jump in the video time (resync),
        // volume changes, and a slot released and reclaimed.
        host_us += 10_000.0;
        sample_clock += 160.0;
        while sample_clock >= 1024.0 {
            sample_clock -= 1024.0;
            pts += 64_000;
            let hole = (step / 500) % 7 == 3;
            for (i, t) in tiles.iter_mut().enumerate() {
                if hole && i == 1 {
                    continue;
                }
                let p = if step > 12_000 && i == 2 {
                    pts + 5_000_000
                } else {
                    pts
                };
                t.push_pcm(fmt, &pcm, Some(p), host_us as u64, None);
                calls += 1;
            }
        }
        if step % 777 == 0 {
            tiles[0].set_volume((step % 3) as f32 * 0.5);
            tiles[3].set_muted(step % 2 == 0);
        }
        if step == 9_000 {
            tiles[3].set_audible(false);
        }
        if step == 9_500 {
            tiles[3].set_audible(true);
        }
        ARMED.with(|a| a.set(true));
        renderer.render(&mut buf, ((host_us + 5_000.0) * 1e3) as u64);
        ARMED.with(|a| a.set(false));
        renders += 1;
    }
    let stats: Vec<_> = tiles.iter().map(|t| t.stats().playout).collect();
    println!("{renders} renders, {calls} records; {stats:?}");
    // The scenario did exercise the paths it is meant to.
    let p1 = stats[1].unwrap();
    assert!(p1.underruns > 0, "underruns exercised");
    let p2 = stats[2].unwrap();
    assert!(p2.starts + p2.resyncs > 1, "skips exercised");
    assert_eq!(EVENTS.load(Ordering::SeqCst), 0, "allocations in render");
    // And the counter does count.
    ARMED.with(|a| a.set(true));
    let v = std::hint::black_box(vec![1u8; 100]);
    drop(v);
    ARMED.with(|a| a.set(false));
    assert!(EVENTS.load(Ordering::SeqCst) >= 2);
}
