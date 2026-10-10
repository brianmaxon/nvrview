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
//!
//! The core and the mock run on a simulated clock (`simtime`): the render
//! thread renders one callback per 10 ms step of it, in lockstep with the
//! test thread, which makes the app's changes at their simulated times. The
//! schedule is the same on every run and takes no real time to wait out.

// Calling the C interface and writing a global allocator need `unsafe`.
#![allow(unsafe_code)]

use nvrview::test_support::{TestRuntime, clock_now_ns, core_new_on};
use nvrview::*;
use simtime::SimRuntime;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::ffi::{CString, c_char, c_void};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
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

fn mock(sim: &SimRuntime) -> mock_nvr::MockNvr {
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
    mock_nvr::spawn_in(cfg, sim.handle())
}

/// The core handle, shared with the render thread.
struct CorePtr(*mut NvCore);
// SAFETY: the C interface is callable from any thread; the main thread
// joins the render thread before nv_core_free.
unsafe impl Send for CorePtr {}

#[test]
fn render_through_the_c_interface_never_allocates() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let ctx = Box::new(Ctx::default());
    let cb = NvCallbacks {
        context: &*ctx as *const Ctx as *mut c_void,
        on_action: Some(on_action),
        on_event: Some(on_event),
        on_audio_tap: Some(on_audio_tap),
    };
    let runtime = TestRuntime {
        runtime: sim.handle().clone(),
        connect: Arc::new(|host: &str, port: u16| {
            let ip = host
                .parse()
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
            mock_nvr::connect(std::net::SocketAddr::new(ip, port))
        }),
    };
    let core: *mut NvCore = core_new_on(runtime, cb).unwrap();
    let user = CString::new("example-user").unwrap();
    let pass = CString::new("example-password").unwrap();
    let host = CString::new("127.0.0.1").unwrap();
    let (mut dev, mut tile) = (0u64, 0u64);
    // SAFETY: valid pointers throughout; the core is freed once, at the end.
    unsafe {
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
    // SAFETY: the core is live until nv_core_free below.
    let now_ns = || clock_now_ns(unsafe { &*core });

    // The app's real-time thread: 10 ms callbacks on the core's clock,
    // each 10 ms ahead of when it is heard. It renders when told to, so the
    // test can step the simulated clock between callbacks.
    let (go_tx, go_rx) = sync_channel::<u64>(0);
    let (done_tx, done_rx) = sync_channel::<()>(0);
    let render = {
        let c = CorePtr(core);
        std::thread::spawn(move || {
            let c = c;
            let mut buf = vec![0.0f32; 480 * 2];
            let (mut ok, mut other, mut loud, mut calls) = (0u64, 0u64, 0u64, 0u64);
            while let Ok(host) = go_rx.recv() {
                ARMED.with(|a| a.set(true));
                // SAFETY: the core outlives this thread; `buf` holds
                // 480 frames of 2 channels.
                let st = unsafe { nv_audio_render(c.0, buf.as_mut_ptr(), 480, 2, host) };
                // A meter read from the same thread: no allocation either.
                let (mut peak, mut rms) = (0.0f32, 0.0f32);
                // SAFETY: the core outlives this thread; valid out pointers.
                let lv = unsafe { nv_tile_audio_level(c.0, tile, &mut peak, &mut rms) };
                ARMED.with(|a| a.set(false));
                assert_eq!(lv, NvStatus::NV_OK);
                calls += 1;
                if st == NvStatus::NV_OK {
                    ok += 1;
                } else {
                    other += 1;
                }
                if buf.iter().any(|s| s.abs() > 0.3) {
                    loud += 1;
                }
                let _ = done_tx.send(());
            }
            (calls, ok, other, loud)
        })
    };

    // The app changes things while audio plays, at these simulated times
    // (seconds after the first callback).
    type Change = fn(*mut NvCore, u64);
    let changes: [(f64, Change); 7] = [
        // SAFETY (each): the core is live.
        (4.0, |c, t| {
            assert_eq!(
                unsafe { nv_tile_audio_set(c, t, true, 0.5) },
                NvStatus::NV_OK
            )
        }),
        // A device change: same channels, new rate (a new renderer).
        (5.0, |c, _| {
            assert_eq!(
                unsafe { nv_audio_set_output(c, 44_100, 2) },
                NvStatus::NV_OK
            )
        }),
        (7.0, |c, t| {
            assert_eq!(
                unsafe { nv_tile_audio_set(c, t, false, 1.0) },
                NvStatus::NV_OK
            )
        }),
        (7.5, |c, t| {
            assert_eq!(
                unsafe { nv_tile_audio_set(c, t, true, 1.0) },
                NvStatus::NV_OK
            )
        }),
        // A change to mono: the render thread still renders stereo, so it
        // gets silence and NV_ERR_INVALID_ARGUMENT, safely.
        (8.0, |c, _| {
            assert_eq!(
                unsafe { nv_audio_set_output(c, 48_000, 1) },
                NvStatus::NV_OK
            )
        }),
        (8.5, |c, _| {
            assert_eq!(
                unsafe { nv_audio_set_output(c, 48_000, 2) },
                NvStatus::NV_OK
            )
        }),
        (11.0, |c, t| {
            assert_eq!(unsafe { nv_tile_audio_tap(c, t, false) }, NvStatus::NV_OK)
        }),
    ];
    let start = now_ns() + 10_000_000;
    let mut next = 0;
    for k in 0..1100u64 {
        let host = start + k * 10_000_000;
        let now = now_ns();
        if now + 10_000_000 < host {
            sim.advance(Duration::from_nanos(host - 10_000_000 - now));
        }
        while next < changes.len() && changes[next].0 * 1e9 <= (k * 10_000_000) as f64 {
            (changes[next].1)(core, tile);
            next += 1;
        }
        go_tx.send(host).unwrap();
        done_rx.recv().unwrap();
    }
    while next < changes.len() {
        (changes[next].1)(core, tile);
        next += 1;
    }
    drop(go_tx);
    let (calls, ok, other, loud) = render.join().unwrap();
    // Tap blocks reach the app on the core's tap thread, in real time.
    let t0 = Instant::now();
    while ctx.taps.load(Ordering::SeqCst) <= 100 && t0.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let taps = ctx.taps.load(Ordering::SeqCst);
    println!(
        "{calls} render calls: {ok} ok, {other} silent (no output yet or wrong channel count), {loud} with clicks; {taps} tap blocks; allocation events while rendering: {}",
        EVENTS.load(Ordering::SeqCst)
    );
    // SAFETY: rendering has stopped; freed once.
    sim.run_free(|| unsafe { nv_core_free(core) });
    nvr.shutdown();

    assert_eq!(
        EVENTS.load(Ordering::SeqCst),
        0,
        "allocation in nv_audio_render or nv_tile_audio_level"
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
