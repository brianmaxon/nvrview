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
    /// Decodes answered per generation (0 and 1).
    gen_decodes: [AtomicU64; 2],
    /// Resets per generation.
    gen_resets: [AtomicU64; 2],
}

unsafe extern "C" fn on_action(context: *mut c_void, tile: u64, action: *const NvAction) {
    // SAFETY: the test passes a pointer to a Ctx that outlives the core, and
    // the core passes a valid action for the duration of the call.
    let (ctx, a) = unsafe { (&*(context as *const Ctx), &*action) };
    let g = (a.generation as usize).min(1);
    if a.kind == NvActionKind::NV_ACTION_RESET {
        ctx.gen_resets[g].fetch_add(1, Ordering::SeqCst);
    }
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
            if unsafe { nv_tile_decoded(core, tile, a.generation, d.pts_us) } == NvStatus::NV_OK {
                ctx.answered_ok.fetch_add(1, Ordering::SeqCst);
                ctx.gen_decodes[g].fetch_add(1, Ordering::SeqCst);
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
    assert_eq!(NV_ABI_VERSION, 3);
    assert_eq!(nv_abi_minor(), NV_ABI_MINOR);
    assert_eq!(NV_ABI_MINOR, 6);
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

    // Focus: the main stream as generation 1, then generation 0 ends.
    // SAFETY: live core.
    unsafe {
        assert_eq!(
            nv_tile_set_tier(core, tile, NvStreamTier::NV_TIER_MAIN as u32),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_tile_set_tier(core, tile, 3),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_tile_set_tier(core, tile + 100, NvStreamTier::NV_TIER_MAIN as u32),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(nv_tile_retry(core, tile), NvStatus::NV_OK);
        // Again at once: nothing happens, and it is not an error.
        assert_eq!(nv_tile_retry(core, tile), NvStatus::NV_OK);
        assert_eq!(nv_tile_retry(core, tile + 100), NvStatus::NV_ERR_UNKNOWN_ID);
        assert_eq!(
            nv_tile_retry(std::ptr::null_mut(), tile),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
    }
    // (The new generation may decode pictures before it is shown, while it
    // catches up with the picture on screen.)
    wait_until("main-stream pictures and the switch", || {
        ctx.gen_decodes[1].load(Ordering::SeqCst) >= 5
            && ctx.gen_resets[0].load(Ordering::SeqCst) >= 1
    });
    assert_eq!(ctx.gen_resets[0].load(Ordering::SeqCst), 1);
    assert_eq!(ctx.configures.load(Ordering::SeqCst), 2);
    wait_until("tile_tier active", || {
        ctx.json.lock().unwrap().iter().any(|j| {
            j.contains("\"type\":\"tile_tier\"")
                && j.contains("\"state\":\"active\"")
                && j.contains("\"generation\":1")
        })
    });
    let (mut peak, mut rms) = (1.0f32, 1.0f32);
    // SAFETY: live core, valid out pointers.
    unsafe {
        assert_eq!(
            nv_tile_audio_level(core, tile, &mut peak, &mut rms),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_tile_audio_level(core, tile, ptr::null_mut(), &mut rms),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
    }
    assert_eq!((peak, rms), (0.0, 0.0), "silent tile");

    // SAFETY: live core.
    unsafe {
        assert_eq!(nv_tile_close(core, tile), NvStatus::NV_OK);
        assert_eq!(nv_tile_close(core, tile), NvStatus::NV_ERR_UNKNOWN_ID);
        assert_eq!(
            nv_tile_decoded(core, tile, 0, 0),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
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
            data_dir: ptr::null(),
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
            nv_tile_decode_failed(core, tile, 0, 7),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_tile_decode_failed(core, tile, 0, NvDecodeFailure::NV_FAIL_NO_PICTURE as u32),
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

#[test]
fn layouts_and_device_keys_through_the_c_interface() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = CString::new(dir.path().to_str().unwrap()).unwrap();
    let config = NvConfig {
        log_dir: ptr::null(),
        log_max_files: 0,
        log_max_total_bytes: 0,
        data_dir: data_dir.as_ptr(),
    };
    let ctx = Box::new(Ctx::default());
    let mut core: *mut NvCore = ptr::null_mut();
    // Nothing listens there: the device never connects, which is fine here.
    let host = CString::new("127.0.0.1").unwrap();
    let user = CString::new("example-user").unwrap();
    let password = CString::new("example-password").unwrap();
    // SAFETY: valid config, callbacks, out pointers and strings throughout;
    // the core is live until freed once at the end.
    unsafe {
        assert_eq!(
            nv_core_new(&config, callbacks(&ctx), &mut core),
            NvStatus::NV_OK
        );
        let mut dev = 0u64;
        assert_eq!(
            nv_device_add(
                core,
                host.as_ptr(),
                1,
                user.as_ptr(),
                password.as_ptr(),
                &mut dev
            ),
            NvStatus::NV_OK
        );
        let mut len = 0usize;
        assert_eq!(
            nv_device_key(core, dev, ptr::null_mut(), 0, &mut len),
            NvStatus::NV_ERR_BUFFER_TOO_SMALL
        );
        let mut buf = vec![0 as c_char; len + 1];
        assert_eq!(
            nv_device_key(core, dev, buf.as_mut_ptr(), buf.len(), &mut len),
            NvStatus::NV_OK
        );
        let key = std::ffi::CStr::from_ptr(buf.as_ptr())
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(key.len(), len);
        assert_eq!(
            nv_device_key(core, dev + 100, buf.as_mut_ptr(), buf.len(), &mut len),
            NvStatus::NV_ERR_UNKNOWN_ID
        );

        let layout = CString::new(format!(
            r#"{{"id":"one","name":"One","root":{{"camera":{{"device":"{key}","channel":0}}}}}}"#
        ))
        .unwrap();
        assert_eq!(nv_layout_put(core, layout.as_ptr()), NvStatus::NV_OK);
        let bad = CString::new(r#"{"id":"x"}"#).unwrap();
        assert_eq!(
            nv_layout_put(core, bad.as_ptr()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let mut small = [0 as c_char; 4];
        assert_eq!(
            nv_layouts_get(core, small.as_mut_ptr(), small.len(), &mut len),
            NvStatus::NV_ERR_BUFFER_TOO_SMALL
        );
        let mut buf = vec![0 as c_char; len + 1];
        assert_eq!(
            nv_layouts_get(core, buf.as_mut_ptr(), buf.len(), &mut len),
            NvStatus::NV_OK
        );
        let text = std::ffi::CStr::from_ptr(buf.as_ptr()).to_str().unwrap();
        assert!(
            text.contains("\"version\":1") && text.contains(&key),
            "{text}"
        );
        let id = CString::new("one").unwrap();
        assert_eq!(nv_layout_delete(core, id.as_ptr()), NvStatus::NV_OK);
        assert_eq!(
            nv_layout_delete(core, id.as_ptr()),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(nv_device_remove(core, dev), NvStatus::NV_OK);
        nv_core_free(core);
    }
    assert!(dir.path().join("layouts.json").exists());
    assert!(dir.path().join("devices.json").exists());
    let events = ctx.json.lock().unwrap().clone();
    assert_eq!(
        events
            .iter()
            .filter(|j| j.contains("layouts_changed"))
            .count(),
        2
    );
}

#[test]
fn talk_back_through_the_c_interface() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 2;
    cfg.talk_channels = vec![1];
    let nvr = mock_nvr::spawn(cfg).unwrap();
    let ctx = Box::new(Ctx::default());
    let mut core: *mut NvCore = ptr::null_mut();
    // SAFETY: valid callbacks and out pointer; `ctx` outlives the core.
    assert_eq!(
        unsafe { nv_core_new(ptr::null(), callbacks(&ctx), &mut core) },
        NvStatus::NV_OK
    );
    ctx.core.store(core, Ordering::SeqCst);
    let host = CString::new("127.0.0.1").unwrap();
    let user = CString::new("example-user").unwrap();
    let password = CString::new("example-password").unwrap();
    let (mut dev, mut tile) = (0u64, 0u64);
    let tone: Vec<f32> = (0..16_000).map(|i| 0.3 * (i as f32 * 0.2).sin()).collect();
    let talk_state = |state: &str| {
        let want = format!("\"state\":\"{state}\"");
        ctx.json
            .lock()
            .unwrap()
            .iter()
            .any(|j| j.starts_with("{\"type\":\"talk\"") && j.contains(&want))
    };
    // SAFETY: live core, NUL-terminated strings, valid pointers.
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
        assert_eq!(
            nv_tile_open(core, dev, 1, NvStreamTier::NV_TIER_SUB as u32, &mut tile),
            NvStatus::NV_OK
        );
        // Off by default.
        assert_eq!(nv_talk_start(core, tile), NvStatus::NV_ERR_NOT_PERMITTED);
        assert_eq!(
            nv_talk_set_permission(core, dev + 100, 1, true),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(
            nv_talk_set_permission(ptr::null_mut(), dev, 1, true),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(nv_talk_set_permission(core, dev, 1, true), NvStatus::NV_OK);
        assert_eq!(nv_talk_start(core, tile), NvStatus::NV_OK);
        assert_eq!(nv_talk_start(core, tile + 100), NvStatus::NV_ERR_UNKNOWN_ID);
    }
    wait_until("talk active", || talk_state("active"));
    // SAFETY: live core; `tone` is valid for its length.
    unsafe {
        assert_eq!(
            nv_talk_push(core, tile, ptr::null(), 10, 16_000),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_talk_push(core, tile, tone.as_ptr(), tone.len(), 4_000),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_talk_push(core, tile, ptr::null(), 0, 16_000),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_talk_push(core, tile, tone.as_ptr(), tone.len(), 16_000),
            NvStatus::NV_OK
        );
        assert_eq!(nv_talk_set_conversation(core, tile, true), NvStatus::NV_OK);
        assert_eq!(
            nv_talk_set_conversation(core, tile + 100, true),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(
            nv_talk_set_conversation(ptr::null_mut(), tile, true),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(nv_talk_stop(core, tile, true), NvStatus::NV_OK);
    }
    wait_until("talk stopped", || talk_state("stopped"));
    assert!(
        ctx.json
            .lock()
            .unwrap()
            .iter()
            .any(|j| j.contains("\"state\":\"stopped\"") && j.contains("\"reason\":\"finished\""))
    );
    wait_until("closed at the device", || {
        nvr.talk_log()
            .first()
            .is_some_and(|t| t.end == mock_nvr::talk::TalkEnd::Closed)
    });
    // 16000 samples at 16 kHz: 15 full records and one padded, then the
    // 1.5 s silent tail (24 records) before the close.
    assert_eq!(nvr.talk_log()[0].records.len(), 16 + 24);
    assert!(
        ctx.json
            .lock()
            .unwrap()
            .iter()
            .any(|j| j.contains("\"sent_ms\":1024") && j.contains("\"tail_ms\":1536"))
    );
    assert!(!ctx.bad.load(Ordering::SeqCst));
    // SAFETY: live core, freed once.
    unsafe { nv_core_free(core) };
}

#[test]
fn discovery_through_the_c_interface() {
    let ctx = Box::new(Ctx::default());
    let mut core: *mut NvCore = ptr::null_mut();
    let json = |s: &str| CString::new(s).unwrap();
    let done_events = |ctx: &Ctx| -> Vec<String> {
        ctx.json
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.contains("\"discovery_done\""))
            .cloned()
            .collect()
    };
    // SAFETY: valid callbacks, out pointers and strings throughout; the core
    // is live until freed once at the end.
    unsafe {
        assert_eq!(
            nv_core_new(ptr::null(), callbacks(&ctx), &mut core),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_discover_start(ptr::null_mut(), ptr::null()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_discover_stop(ptr::null_mut()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        for bad in ["{", "[]", "{\"seconds\":0}", "{\"interfaces\":[\"nope\"]}"] {
            assert_eq!(
                nv_discover_start(core, json(bad).as_ptr()),
                NvStatus::NV_ERR_INVALID_ARGUMENT,
                "{bad}"
            );
        }
        // Nothing running: stopping is fine and sends nothing.
        assert_eq!(nv_discover_stop(core), NvStatus::NV_OK);

        // A search on loopback only (nothing answers there), stopped at once.
        let request = json("{\"seconds\":30,\"interfaces\":[\"127.0.0.1\"],\"onvif\":false}");
        assert_eq!(nv_discover_start(core, request.as_ptr()), NvStatus::NV_OK);
        assert_eq!(nv_discover_stop(core), NvStatus::NV_OK);
        wait_until("discovery_done", || !done_events(&ctx).is_empty());
        let done = done_events(&ctx);
        assert_eq!(done.len(), 1, "{done:?}");
        assert!(done[0].contains("\"search\":1") && done[0].contains("\"stopped\":true"));

        // Defaults (NULL json); freeing the core ends it.
        assert_eq!(nv_discover_start(core, ptr::null()), NvStatus::NV_OK);
        nv_core_free(core);
    }
    assert_eq!(done_events(&ctx).len(), 2);
    assert!(!ctx.bad.load(Ordering::SeqCst));
}

/// An update request; with `staging`, a download of 0.4.0.
fn update_request(source: &str, key: &str, staging: Option<&str>) -> CString {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut r = format!(
        r#"{{"source":"{}","keys":["{}"],"current":"0.3.0","platform":"test-os""#,
        esc(source),
        esc(key)
    );
    if let Some(s) = staging {
        r.push_str(&format!(r#","version":"0.4.0","staging_dir":"{}""#, esc(s)));
    }
    r.push('}');
    CString::new(r).unwrap()
}

/// The text a successful out-buffer call wrote.
///
/// # Safety
/// `buf` holds a NUL-terminated string.
unsafe fn out_text(buf: &[c_char]) -> String {
    // SAFETY: per the caller.
    unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_str()
        .unwrap()
        .to_owned()
}

#[test]
fn updates_through_the_c_interface() {
    // A folder source with version 0.4.0 for platform "test-os", signed by
    // a key made here.
    let key = updates::SecretKey::from_seed([5; 32]);
    let src = tempfile::tempdir().unwrap();
    let file = "App-0.4.0-test-os.zip";
    let package = updates::zip::write_stored(&[("App/hello.txt", b"hello")]);
    std::fs::write(src.path().join(file), &package).unwrap();
    let (size, sha) = updates::hash_file(&src.path().join(file)).unwrap();
    let manifest = format!(
        r#"{{"format":1,"version":"0.4.0","notes":"","packages":{{"test-os":{{"file":"{file}","size":{size},"sha256":"{}"}}}}}}"#,
        updates::keys::encode_hex(&sha)
    );
    std::fs::write(src.path().join("updates.json"), &manifest).unwrap();
    std::fs::write(
        src.path().join("updates.json.sig"),
        key.sign_line(manifest.as_bytes()),
    )
    .unwrap();
    let public = key.public().to_text();
    let source = src.path().to_str().unwrap();

    let ctx = Box::new(Ctx::default());
    let mut core: *mut NvCore = ptr::null_mut();
    let cstr = |s: &str| CString::new(s).unwrap();
    let finals = |ctx: &Ctx| -> Vec<String> {
        ctx.json
            .lock()
            .unwrap()
            .iter()
            .filter(|e| {
                e.contains("\"type\":\"update\"")
                    && !e.contains("\"started\"")
                    && !e.contains("\"downloading\"")
            })
            .cloned()
            .collect()
    };
    let mut buf = vec![0 as c_char; 256];
    let mut len = 0usize;
    // SAFETY: valid strings, buffers, and out pointers throughout; the core
    // is live until freed once at the end.
    unsafe {
        // The pure helpers need no core.
        let k = cstr(&public);
        assert_eq!(
            nv_update_parse_key(k.as_ptr(), buf.as_mut_ptr(), buf.len(), &mut len),
            NvStatus::NV_OK
        );
        let info = out_text(&buf);
        assert!(
            info.contains(&key.public().id()) && info.contains(&public),
            "{info}"
        );
        assert_eq!(
            nv_update_parse_key(k.as_ptr(), buf.as_mut_ptr(), 4, &mut len),
            NvStatus::NV_ERR_BUFFER_TOO_SMALL
        );
        assert_eq!(len, info.len());
        assert_eq!(
            nv_update_parse_key(
                cstr("not a key").as_ptr(),
                buf.as_mut_ptr(),
                buf.len(),
                &mut len
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let insecure = cstr("http://updates.example.com/");
        assert_eq!(
            nv_update_parse_source(insecure.as_ptr(), buf.as_mut_ptr(), buf.len(), &mut len),
            NvStatus::NV_OK
        );
        let s = out_text(&buf);
        assert!(
            s.contains("\"ok\":false") && s.contains("insecure_source"),
            "{s}"
        );
        assert_eq!(
            nv_update_parse_source(ptr::null(), buf.as_mut_ptr(), buf.len(), &mut len),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );

        assert_eq!(
            nv_core_new(ptr::null(), callbacks(&ctx), &mut core),
            NvStatus::NV_OK
        );
        // Nothing running: cancel is fine.
        assert_eq!(nv_update_cancel(core), NvStatus::NV_OK);
        assert_eq!(
            nv_update_cancel(ptr::null_mut()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_update_check(core, ptr::null()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        for bad in ["{", r#"{"source":"/s","keys":["x"],"current":"0.3.0"}"#] {
            assert_eq!(
                nv_update_check(core, cstr(bad).as_ptr()),
                NvStatus::NV_ERR_INVALID_ARGUMENT,
                "{bad}"
            );
        }
        let check = update_request(source, &public, None);
        assert_eq!(nv_update_check(core, check.as_ptr()), NvStatus::NV_OK);
        wait_until("the check", || !finals(&ctx).is_empty());
        let f = finals(&ctx);
        assert!(
            f[0].contains("\"available\"") && f[0].contains("\"version\":\"0.4.0\""),
            "{f:?}"
        );

        let work = tempfile::tempdir().unwrap();
        let staging = work.path().join("staging");
        let download = update_request(source, &public, Some(staging.to_str().unwrap()));
        assert_eq!(nv_update_download(core, download.as_ptr()), NvStatus::NV_OK);
        wait_until("the download", || finals(&ctx).len() == 2);
        let f = finals(&ctx);
        assert!(f[1].contains("\"ready\""), "{f:?}");
        assert_eq!(
            std::fs::read(staging.join("content/App/hello.txt")).unwrap(),
            b"hello"
        );
        nv_core_free(core);
    }
    assert!(!ctx.bad.load(Ordering::SeqCst));
}
