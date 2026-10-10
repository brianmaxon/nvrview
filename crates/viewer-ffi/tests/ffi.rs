//! The extern "C" functions called as C would call them, against the
//! in-process mock NVR.

// Calling the C interface from Rust needs raw pointers.
#![allow(unsafe_code)]

use nvrview::*;
use std::ffi::{CString, c_char, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Coded width and height, parameter set count, matrix, and the luma gain.
type ConfigureSeen = (u32, u32, usize, NvColorMatrix, f32);

#[derive(Default)]
struct Ctx {
    core: AtomicPtr<NvCore>,
    configures: AtomicU64,
    decodes: AtomicU64,
    answered_ok: AtomicU64,
    events: AtomicU64,
    bad: AtomicBool,
    last_configure: Mutex<Option<ConfigureSeen>>,
    json: Mutex<Vec<String>>,
}

unsafe extern "C" fn on_action(context: *mut c_void, tile: u64, action: *const NvAction) {
    // SAFETY: the test passes a pointer to a Ctx that outlives the core, and
    // the core passes a valid action for the duration of the call.
    let (ctx, a) = unsafe { (&*(context as *const Ctx), &*action) };
    match a.kind {
        NvActionKind::NV_ACTION_CONFIGURE => {
            ctx.configures.fetch_add(1, Ordering::SeqCst);
            let c = &a.configure;
            // SAFETY: the core passes `parameter_set_count` valid NvBytes.
            let sets =
                unsafe { std::slice::from_raw_parts(c.parameter_sets, c.parameter_set_count) };
            if sets.iter().any(|b| b.data.is_null() || b.len == 0) {
                ctx.bad.store(true, Ordering::SeqCst);
            }
            *ctx.last_configure.lock().unwrap() = Some((
                c.coded_size.width,
                c.coded_size.height,
                sets.len(),
                c.color.matrix,
                c.color.ycbcr_to_rgb[0],
            ));
        }
        NvActionKind::NV_ACTION_DECODE => {
            ctx.decodes.fetch_add(1, Ordering::SeqCst);
            let d = &a.decode;
            if d.access_unit.data.is_null() || d.access_unit.len < 5 {
                ctx.bad.store(true, Ordering::SeqCst);
            } else {
                // SAFETY: valid for `len` bytes during the call.
                let au =
                    unsafe { std::slice::from_raw_parts(d.access_unit.data, d.access_unit.len) };
                if au[..3] != [0, 0, 0] && au[..3] != [0, 0, 1] {
                    ctx.bad.store(true, Ordering::SeqCst);
                }
            }
            // Answer from inside the callback, as allowed.
            let core = ctx.core.load(Ordering::SeqCst);
            // SAFETY: the core is live while it calls us.
            if unsafe { nv_tile_decoded(core, tile, d.pts_us) } == NvStatus::NV_OK {
                ctx.answered_ok.fetch_add(1, Ordering::SeqCst);
            }
        }
        _ => {}
    }
}

unsafe extern "C" fn on_event(context: *mut c_void, json: *const c_char, len: usize) {
    // SAFETY: as above; `json` is valid for `len` bytes during the call.
    let (ctx, bytes) = unsafe {
        (
            &*(context as *const Ctx),
            std::slice::from_raw_parts(json.cast::<u8>(), len),
        )
    };
    ctx.events.fetch_add(1, Ordering::SeqCst);
    match std::str::from_utf8(bytes) {
        Ok(s) if s.starts_with("{\"type\":") && s.ends_with('}') => {
            ctx.json.lock().unwrap().push(s.to_owned());
        }
        _ => ctx.bad.store(true, Ordering::SeqCst),
    }
}

fn callbacks(ctx: &Ctx) -> NvCallbacks {
    NvCallbacks {
        context: ctx as *const Ctx as *mut c_void,
        on_action: Some(on_action),
        on_event: Some(on_event),
        on_audio_tap: None,
    }
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn mock() -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 2;
    mock_nvr::spawn(cfg).unwrap()
}

#[test]
fn abi_version() {
    assert_eq!(nv_abi_version(), NV_ABI_VERSION);
    assert_eq!(NV_ABI_VERSION, 2);
}

#[test]
fn a_tile_plays_through_the_c_interface() {
    let nvr = mock();
    let ctx = Box::new(Ctx::default());
    let mut core: *mut NvCore = ptr::null_mut();
    // SAFETY: valid callbacks and out pointer; `ctx` outlives the core.
    assert_eq!(
        unsafe { nv_core_new(ptr::null(), callbacks(&ctx), &mut core) },
        NvStatus::NV_OK
    );
    assert!(!core.is_null());
    ctx.core.store(core, Ordering::SeqCst);

    let host = CString::new("127.0.0.1").unwrap();
    let user = CString::new("example-user").unwrap();
    let password = CString::new("example-password").unwrap();
    let (mut dev, mut tile) = (0u64, 0u64);
    // SAFETY: live core, NUL-terminated strings, valid out pointers.
    unsafe {
        assert_eq!(
            nv_device_add(
                core,
                host.as_ptr(),
                nvr.addr().port(),
                user.as_ptr(),
                password.as_ptr(),
                &mut dev
            ),
            NvStatus::NV_OK
        );
        assert_ne!(dev, 0);
        assert_eq!(
            nv_tile_open(core, dev, 0, NvStreamTier::NV_TIER_SUB as u32, &mut tile),
            NvStatus::NV_OK
        );
        assert_ne!(tile, 0);
    }
    wait_until("decodes", || ctx.answered_ok.load(Ordering::SeqCst) >= 25);
    assert!(!ctx.bad.load(Ordering::SeqCst));
    assert_eq!(ctx.configures.load(Ordering::SeqCst), 1);
    let (w, h, sets, matrix, y_gain) = ctx.last_configure.lock().unwrap().unwrap();
    assert_eq!((w, h), (320, 240));
    assert_eq!(sets, 2, "H.264: SPS and PPS");
    assert_eq!(matrix, NvColorMatrix::NV_MATRIX_BT601);
    assert!(
        (y_gain - 255.0 / 219.0).abs() < 1e-4,
        "video-range luma gain"
    );
    wait_until("channels event", || {
        ctx.json
            .lock()
            .unwrap()
            .iter()
            .any(|j| j.contains("\"type\":\"channels\"") && j.contains("Channel 1"))
    });

    // SAFETY: live core.
    unsafe {
        assert_eq!(nv_tile_close(core, tile), NvStatus::NV_OK);
        assert_eq!(nv_tile_close(core, tile), NvStatus::NV_ERR_UNKNOWN_ID);
        assert_eq!(nv_tile_decoded(core, tile, 0), NvStatus::NV_ERR_UNKNOWN_ID);
        assert_eq!(nv_device_remove(core, dev), NvStatus::NV_OK);
        assert_eq!(nv_device_remove(core, dev), NvStatus::NV_ERR_UNKNOWN_ID);
    }
    wait_until("logout", || nvr.active_logins() == 0);
    let events = ctx.events.load(Ordering::SeqCst);
    // SAFETY: frees the core exactly once; no other call is running.
    unsafe { nv_core_free(core) };
    std::thread::sleep(Duration::from_millis(100));
    assert!(ctx.events.load(Ordering::SeqCst) >= events);
    let after = ctx.events.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        ctx.events.load(Ordering::SeqCst),
        after,
        "no callback after free"
    );
}

#[test]
fn invalid_arguments_are_refused() {
    let ctx = Box::new(Ctx::default());
    let mut core: *mut NvCore = ptr::null_mut();
    let no_action = NvCallbacks {
        on_action: None,
        ..callbacks(&ctx)
    };
    let bad_utf8 = [0xffu8, 0xfe, 0];
    // SAFETY: every pointer is null or valid; out pointers are valid.
    unsafe {
        assert_eq!(
            nv_core_new(ptr::null(), callbacks(&ctx), ptr::null_mut()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_core_new(ptr::null(), no_action, &mut core),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let config = NvConfig {
            log_dir: bad_utf8.as_ptr().cast(),
            log_max_files: 0,
            log_max_total_bytes: 0,
        };
        assert_eq!(
            nv_core_new(&config, callbacks(&ctx), &mut core),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert!(core.is_null());
        assert_eq!(
            nv_core_new(ptr::null(), callbacks(&ctx), &mut core),
            NvStatus::NV_OK
        );

        let user = CString::new("example-user").unwrap();
        let mut id = 0u64;
        assert_eq!(
            nv_device_add(
                core,
                ptr::null(),
                9000,
                user.as_ptr(),
                user.as_ptr(),
                &mut id
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_device_add(
                core,
                bad_utf8.as_ptr().cast(),
                9000,
                user.as_ptr(),
                user.as_ptr(),
                &mut id
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_device_add(
                ptr::null_mut(),
                user.as_ptr(),
                9000,
                user.as_ptr(),
                user.as_ptr(),
                &mut id
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let host = CString::new("127.0.0.1").unwrap();
        assert_eq!(
            nv_device_add(
                core,
                host.as_ptr(),
                9,
                user.as_ptr(),
                user.as_ptr(),
                ptr::null_mut()
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_device_add(
                core,
                host.as_ptr(),
                9,
                user.as_ptr(),
                user.as_ptr(),
                &mut id
            ),
            NvStatus::NV_OK
        );
        let mut tile = 0u64;
        assert_eq!(
            nv_tile_open(core, id, 0, 3, &mut tile),
            NvStatus::NV_ERR_INVALID_ARGUMENT,
            "tier 3 does not exist"
        );
        assert_eq!(
            nv_tile_open(core, id + 1000, 0, 1, &mut tile),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(nv_tile_open(core, id, 0, 1, &mut tile), NvStatus::NV_OK);
        assert_eq!(
            nv_tile_decode_failed(core, tile, 7),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_tile_decode_failed(core, tile, NvDecodeFailure::NV_FAIL_NO_PICTURE as u32),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_tile_close(ptr::null_mut(), tile),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );

        // Audio.
        let mut buf = [1.0f32; 64];
        assert_eq!(
            nv_audio_render(core, buf.as_mut_ptr(), 32, 2, 0),
            NvStatus::NV_ERR_CLOSED,
            "no output yet"
        );
        assert!(buf.iter().all(|&x| x == 0.0), "silence written");
        assert_eq!(
            nv_audio_set_output(core, 1000, 2),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_audio_set_output(core, 48_000, 0),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(nv_audio_set_output(core, 48_000, 2), NvStatus::NV_OK);
        assert_eq!(
            nv_audio_render(core, ptr::null_mut(), 32, 2, 0),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_audio_render(core, buf.as_mut_ptr(), 64, 1, 0),
            NvStatus::NV_ERR_INVALID_ARGUMENT,
            "channel count differs from the output's"
        );
        assert_eq!(
            nv_audio_render(core, buf.as_mut_ptr(), 32, 2, 0),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_tile_audio_set(core, tile, true, -0.5),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_tile_audio_set(core, tile + 1000, true, 1.0),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(nv_tile_audio_set(core, tile, true, 0.5), NvStatus::NV_OK);
        assert_eq!(
            nv_tile_audio_tap(core, tile, true),
            NvStatus::NV_ERR_INVALID_ARGUMENT,
            "no on_audio_tap callback"
        );
        assert_eq!(nv_tile_audio_tap(core, tile, false), NvStatus::NV_OK);
        let a = nv_clock_now_ns();
        assert!(nv_clock_now_ns() >= a && a > 0);
        nv_core_free(core);
        nv_core_free(ptr::null_mut());
    }
}

#[test]
fn grid_layout_matches_the_pipeline() {
    use viewer_core::layout::{FitMode, Size, grid_layout};
    let sizes = [
        NvSize {
            width: 1920,
            height: 1080,
        },
        NvSize::default(),
        NvSize {
            width: 640,
            height: 480,
        },
    ];
    let viewport = NvSize {
        width: 1000,
        height: 700,
    };
    for fill in [false, true] {
        let mut out = [NvTileLayout::default(); 3];
        // SAFETY: three sizes in, three layouts out.
        let r = unsafe { nv_grid_layout(3, viewport, 4, sizes.as_ptr(), fill, out.as_mut_ptr()) };
        assert_eq!(r, NvStatus::NV_OK);
        let mode = if fill { FitMode::Fill } else { FitMode::Fit };
        let expect = grid_layout(
            3,
            Size::new(1000, 700),
            4,
            &sizes.map(|s| Size::new(s.width, s.height)),
            mode,
        );
        for (o, e) in out.iter().zip(&expect) {
            assert_eq!(
                (o.dest.x, o.dest.y, o.dest.width),
                (e.dest.x, e.dest.y, e.dest.width)
            );
            assert_eq!(
                (o.source.width, o.source.height),
                (e.source.width, e.source.height)
            );
            assert_eq!(o.cell.height, e.cell.height);
        }
        assert_eq!(out[1].source.width, 0, "unknown picture: nothing to sample");
    }
    // SAFETY: zero tiles touch no pointer; null out is refused.
    unsafe {
        assert_eq!(
            nv_grid_layout(0, viewport, 0, ptr::null(), false, ptr::null_mut()),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_grid_layout(2, viewport, 0, ptr::null(), false, ptr::null_mut()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let mut out = [NvTileLayout::default(); 2];
        assert_eq!(
            nv_grid_layout(2, viewport, 0, ptr::null(), false, out.as_mut_ptr()),
            NvStatus::NV_OK
        );
        assert!(out[0].cell.width > 0);
    }
}
