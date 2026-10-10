//! Listening through the C interface, as an app's real-time audio thread
//! would: `nv_audio_render` from its own thread against a mock NVR sending
//! AAC clicks, under a counting allocator armed on that thread around every
//! call. Meanwhile the app changes the output format, volume, audibility,
//! and taps, so the render path is checked through playback, resyncs, slot
//! changes, and renderer replacement (whose old renderer must be freed on
//! the control thread, never the real-time one).
//!
//! Lock freedom is by construction (viewer-core's `RenderHandoff` and
//! audio-pipeline's renderer touch shared state only through atomics); this
//! test checks the other half, no allocation or deallocation.

// Calling the C interface and writing a global allocator need `unsafe`.
#![allow(unsafe_code)]

use nvrview::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::{CString, c_char, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::time::{Duration, Instant};

struct Counting;

thread_local! {
    /// Armed only on the render thread, around each render call.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}
static EVENTS: AtomicU64 = AtomicU64::new(0);

fn count() {
    if ARMED.with(Cell::get) {
        EVENTS.fetch_add(1, Ordering::Relaxed);
    }
}

// SAFETY: forwards every call unchanged to the system allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: forwarded unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count();
        // SAFETY: forwarded unchanged.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: forwarded unchanged.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[derive(Default)]
struct Ctx {
    core: AtomicPtr<NvCore>,
    taps: AtomicU64,
    tap_samples: AtomicU64,
    bad_tap: AtomicBool,
}

unsafe extern "C" fn on_action(context: *mut c_void, tile: u64, action: *const NvAction) {
    // SAFETY: the context is the test's Ctx, alive until after nv_core_free;
    // the action is valid during the call.
    let (ctx, a) = unsafe { (&*(context as *const Ctx), &*action) };
    if a.kind == NvActionKind::NV_ACTION_DECODE {
        // SAFETY: the core is live while it calls us.
        unsafe {
            nv_tile_decoded(
                ctx.core.load(Ordering::SeqCst),
                tile,
                a.generation,
                a.decode.pts_us,
            )
        };
    }
}

unsafe extern "C" fn on_event(_: *mut c_void, _: *const c_char, _: usize) {}

unsafe extern "C" fn on_audio_tap(context: *mut c_void, _tile: u64, block: *const NvAudioBlock) {
    // SAFETY: as above; the block and its samples are valid during the call.
    let (ctx, b) = unsafe { (&*(context as *const Ctx), &*block) };
    // SAFETY: `sample_count` floats at `samples`.
    let s = unsafe { std::slice::from_raw_parts(b.samples, b.sample_count) };
    if b.sample_rate != 16_000 || s.len() != 1024 || s.iter().any(|x| !x.is_finite()) {
        ctx.bad_tap.store(true, Ordering::SeqCst);
    }
    ctx.taps.fetch_add(1, Ordering::SeqCst);
    ctx.tap_samples
        .fetch_add(b.sample_count as u64, Ordering::SeqCst);
}

fn mock() -> mock_nvr::MockNvr {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic");
    let mut cfg = mock_nvr::Config::new(
        mock_nvr::fixture::Fixture::load(&dir.join("h264-basic.h264")).unwrap(),
    );
    cfg.channels = 1;
    let mut a = mock_nvr::audio::AudioConfig::new(
        mock_nvr::audio::AudioFixture::load(&dir.join("aac-clicks.aac")).unwrap(),
    );
    a.jitter_us = 20_000;
    cfg.audio = Some(Arc::new(a));
    mock_nvr::spawn(cfg).unwrap()
}

/// The core handle, shared with the render thread.
struct CorePtr(*mut NvCore);
// SAFETY: the C interface is callable from any thread; the main thread
// joins the render thread before nv_core_free.
unsafe impl Send for CorePtr {}

#[test]
fn render_through_the_c_interface_never_allocates() {
    let nvr = mock();
    let ctx = Box::new(Ctx::default());
    let cb = NvCallbacks {
        context: &*ctx as *const Ctx as *mut c_void,
        on_action: Some(on_action),
        on_event: Some(on_event),
        on_audio_tap: Some(on_audio_tap),
    };
    let mut core: *mut NvCore = ptr::null_mut();
    let user = CString::new("example-user").unwrap();
    let pass = CString::new("example-password").unwrap();
    let host = CString::new("127.0.0.1").unwrap();
    let (mut dev, mut tile) = (0u64, 0u64);
    // SAFETY: valid pointers throughout; the core is freed once, at the end.
    unsafe {
        assert_eq!(nv_core_new(ptr::null(), cb, &mut core), NvStatus::NV_OK);
        ctx.core.store(core, Ordering::SeqCst);
        assert_eq!(nv_audio_set_output(core, 48_000, 2), NvStatus::NV_OK);
        assert_eq!(
            nv_device_add(
                core,
                host.as_ptr(),
                nvr.addr().port(),
                user.as_ptr(),
                pass.as_ptr(),
                &mut dev
            ),
            NvStatus::NV_OK
        );
        assert_eq!(nv_tile_open(core, dev, 0, 0, &mut tile), NvStatus::NV_OK);
        assert_eq!(nv_tile_audio_set(core, tile, true, 1.0), NvStatus::NV_OK);
        assert_eq!(nv_tile_audio_tap(core, tile, true), NvStatus::NV_OK);
    }

    // The app's real-time thread: 10 ms callbacks on the core's clock,
    // each 10 ms ahead of when it is heard.
    let stop = Arc::new(AtomicBool::new(false));
    let render = {
        let stop = stop.clone();
        let c = CorePtr(core);
        std::thread::spawn(move || {
            let c = c;
            let mut buf = vec![0.0f32; 480 * 2];
            let (mut ok, mut other, mut loud, mut calls) = (0u64, 0u64, 0u64, 0u64);
            let start = nv_clock_now_ns() + 10_000_000;
            let mut k = 0u64;
            while !stop.load(Ordering::SeqCst) {
                let host = start + k * 10_000_000;
                let now = nv_clock_now_ns();
                if now + 10_000_000 < host {
                    std::thread::sleep(Duration::from_nanos(host - 10_000_000 - now));
                }
                ARMED.with(|a| a.set(true));
                // SAFETY: the core outlives this thread; `buf` holds
                // 480 frames of 2 channels.
                let st = unsafe { nv_audio_render(c.0, buf.as_mut_ptr(), 480, 2, host) };
                ARMED.with(|a| a.set(false));
                calls += 1;
                if st == NvStatus::NV_OK {
                    ok += 1;
                } else {
                    other += 1;
                }
                if buf.iter().any(|s| s.abs() > 0.3) {
                    loud += 1;
                }
                k += 1;
            }
            (calls, ok, other, loud)
        })
    };

    // The app changes things while audio plays.
    let t0 = Instant::now();
    let at = |s: f64| {
        let d = Duration::from_secs_f64(s).saturating_sub(t0.elapsed());
        std::thread::sleep(d);
    };
    // SAFETY: the core is live.
    unsafe {
        at(4.0);
        assert_eq!(nv_tile_audio_set(core, tile, true, 0.5), NvStatus::NV_OK);
        at(5.0);
        // A device change: same channels, new rate (a new renderer).
        assert_eq!(nv_audio_set_output(core, 44_100, 2), NvStatus::NV_OK);
        at(7.0);
        assert_eq!(nv_tile_audio_set(core, tile, false, 1.0), NvStatus::NV_OK);
        at(7.5);
        assert_eq!(nv_tile_audio_set(core, tile, true, 1.0), NvStatus::NV_OK);
        at(8.0);
        // A change to mono: the render thread still renders stereo, so it
        // gets silence and NV_ERR_INVALID_ARGUMENT, safely.
        assert_eq!(nv_audio_set_output(core, 48_000, 1), NvStatus::NV_OK);
        at(8.5);
        assert_eq!(nv_audio_set_output(core, 48_000, 2), NvStatus::NV_OK);
        at(11.0);
        assert_eq!(nv_tile_audio_tap(core, tile, false), NvStatus::NV_OK);
    }
    stop.store(true, Ordering::SeqCst);
    let (calls, ok, other, loud) = render.join().unwrap();
    let taps = ctx.taps.load(Ordering::SeqCst);
    println!(
        "{calls} render calls: {ok} ok, {other} silent (no output yet or wrong channel count), {loud} with clicks; {taps} tap blocks; allocation events while rendering: {}",
        EVENTS.load(Ordering::SeqCst)
    );
    // SAFETY: rendering has stopped; freed once.
    unsafe { nv_core_free(core) };
    nvr.shutdown();

    assert_eq!(
        EVENTS.load(Ordering::SeqCst),
        0,
        "allocation in nv_audio_render"
    );
    assert!(calls > 900 && ok > 800, "{calls} calls, {ok} ok");
    assert!(other > 10, "the mono period was rendered as silence");
    // Clicks every 0.5 s, minus the start, the silent half second, and the
    // resyncs after each format change.
    assert!(loud >= 8, "{loud} calls with clicks");
    assert!(taps > 100, "{taps} tap blocks");
    assert!(!ctx.bad_tap.load(Ordering::SeqCst));
    // The counter does count.
    ARMED.with(|a| a.set(true));
    drop(std::hint::black_box(vec![1u8; 100]));
    ARMED.with(|a| a.set(false));
    assert!(EVENTS.load(Ordering::SeqCst) >= 2);
}
