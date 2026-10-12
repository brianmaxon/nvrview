//! Recorded playback (minor 6; docs/FFI-RECORDINGS.md, section 6) called as
//! C would call it, against an in-process mock NVR with a recording store,
//! on a simulated clock: open a two-camera playback, play, seek, rate 4x,
//! offsets, remove and add a camera, close; the events; the status codes;
//! and the tiles answering on_action like live ones.

// Calling the C interface from Rust needs raw pointers.
#![allow(unsafe_code)]

use bc_protocol::archive::DateTime;
use mock_nvr::archive::ArchiveConfig;
use nvrview::test_support::{TestRuntime, core_new_at, flush_events};
use nvrview::*;
use serde_json::{Value, json};
use simtime::SimRuntime;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The device's offset from UTC: an hour east.
const OFFSET_S: i64 = 3600;

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

fn ms(day: u8, h: u8, m: u8, s: u8) -> i64 {
    d(day, h, m, s).to_naive_secs() * 1000
}

#[derive(Default)]
struct Ctx {
    core: AtomicPtr<NvCore>,
    json: Mutex<Vec<String>>,
    /// Decodes answered, per tile and generation.
    decodes: Mutex<HashMap<(u64, u32), u64>>,
    /// The playback the callbacks look at (0: none yet).
    playback: AtomicU64,
    /// Statuses of calls made from inside on_event: position, play.
    inside: Mutex<Vec<(NvStatus, NvStatus)>>,
}

/// The callbacks' context, never freed. The core may call back until
/// `nv_core_free` returns, and a failing assertion unwinds past that call:
/// a context dropped then would be used after free by the core's threads.
fn leaked_ctx() -> &'static Ctx {
    Box::leak(Box::default())
}

unsafe extern "C" fn on_action(context: *mut c_void, tile: u64, action: *const NvAction) {
    // SAFETY: the context is the test's Ctx, alive until after nv_core_free;
    // the action is valid during the call.
    let (ctx, a) = unsafe { (&*(context as *const Ctx), &*action) };
    if a.kind == NvActionKind::NV_ACTION_DECODE {
        *ctx.decodes
            .lock()
            .unwrap()
            .entry((tile, a.generation))
            .or_default() += 1;
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

unsafe extern "C" fn on_event(context: *mut c_void, json: *const c_char, len: usize) {
    // SAFETY: the context is the test's Ctx and `json` is valid for `len`
    // bytes during the call.
    let (ctx, bytes) = unsafe {
        (
            &*(context as *const Ctx),
            std::slice::from_raw_parts(json.cast::<u8>(), len),
        )
    };
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let pb = ctx.playback.load(Ordering::SeqCst);
    if pb != 0 && text.contains("\"playback_state\"") {
        let core = ctx.core.load(Ordering::SeqCst);
        let (mut pos, mut state) = (0i64, NvPlaybackState::NV_PLAYBACK_FAILED);
        // SAFETY: the core is live while it calls us; valid out pointers.
        let p = unsafe { nv_playback_position(core, pb, &mut pos, &mut state) };
        // SAFETY: as above. Not allowed here: refused, nothing changes.
        let q = unsafe { nv_playback_pause(core, pb) };
        ctx.inside.lock().unwrap().push((p, q));
    }
    ctx.json.lock().unwrap().push(text);
}

impl Ctx {
    fn events(&self, kind: &str) -> Vec<Value> {
        self.json
            .lock()
            .unwrap()
            .iter()
            .map(|s| serde_json::from_str::<Value>(s).unwrap())
            .filter(|v| v["type"] == kind)
            .collect()
    }

    fn decodes(&self, tile: u64, generation: u32) -> u64 {
        self.decodes
            .lock()
            .unwrap()
            .get(&(tile, generation))
            .copied()
            .unwrap_or(0)
    }
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

/// A mock NVR with two channels recording all of the 8th.
fn mock(sim: &SimRuntime) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 2;
    let mut a = ArchiveConfig::new(vec![d(8, 0, 0, 0)]);
    a.events_per_day = 0;
    a.utc_offset_s = OFFSET_S;
    a.play_end_message = false;
    cfg.archive = Some(a);
    mock_nvr::spawn_in(cfg, sim.handle())
}

struct Rig {
    sim: SimRuntime,
    ctx: &'static Ctx,
    core: *mut NvCore,
}

impl Rig {
    fn until(&self, what: &str, secs: u64, mut cond: impl FnMut(&Ctx) -> bool) {
        // SAFETY: the core is live until the end of the test.
        let core = unsafe { &*self.core };
        self.sim.advance_until(
            what,
            Duration::from_secs(secs),
            Duration::from_millis(10),
            || {
                flush_events(core);
                cond(self.ctx)
            },
        );
    }

    fn advance(&self, d: Duration) {
        self.sim.advance(d);
        // SAFETY: the core is live until the end of the test.
        flush_events(unsafe { &*self.core });
    }

    fn position(&self, pb: u64) -> (i64, NvPlaybackState) {
        let (mut pos, mut state) = (0i64, NvPlaybackState::NV_PLAYBACK_FAILED);
        // SAFETY: live core, valid out pointers.
        let s = unsafe { nv_playback_position(self.core, pb, &mut pos, &mut state) };
        assert_eq!(s, NvStatus::NV_OK);
        (pos, state)
    }

    fn of(&self, kind: &str, pb: u64) -> Vec<Value> {
        self.ctx
            .events(kind)
            .into_iter()
            .filter(|e| e["playback"] == pb)
            .collect()
    }

    fn seek_active(&self, pb: u64, generation: u64) -> bool {
        self.of("playback_seek", pb)
            .iter()
            .any(|e| e["state"] == "active" && e["generation"] == generation)
    }

    fn status(&self, pb: u64) -> Value {
        let mut len = 0usize;
        // SAFETY: live core; a NULL out with cap 0 asks for the size.
        let first = unsafe { nv_playback_status(self.core, pb, ptr::null_mut(), 0, &mut len) };
        assert_eq!(first, NvStatus::NV_ERR_BUFFER_TOO_SMALL);
        let mut buf = vec![0 as c_char; len + 64];
        // SAFETY: `buf` holds `buf.len()` bytes.
        let s = unsafe { nv_playback_status(self.core, pb, buf.as_mut_ptr(), buf.len(), &mut len) };
        assert_eq!(s, NvStatus::NV_OK);
        // SAFETY: the core wrote a NUL-terminated string.
        let text = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap();
        serde_json::from_str(text).unwrap()
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn playback_through_the_c_interface() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let ctx = leaked_ctx();
    let cb = NvCallbacks {
        context: ctx as *const Ctx as *mut c_void,
        on_action: Some(on_action),
        on_event: Some(on_event),
        on_audio_tap: None,
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
    // "Today" is the 10th; the recordings are of the 8th.
    let today_us = d(10, 12, 0, 0).to_naive_secs() * 1_000_000;
    let core = core_new_at(runtime, cb, Some(today_us)).unwrap();
    ctx.core.store(core, Ordering::SeqCst);
    let rig = Rig { sim, ctx, core };
    let (start, end, p0) = (ms(8, 10, 0, 0), ms(8, 10, 10, 0), ms(8, 10, 0, 30));

    // SAFETY: valid pointers and strings throughout; the core is freed
    // once, at the end.
    unsafe {
        assert!(nv_abi_minor() >= 6);
        let host = c("127.0.0.1");
        let (user, password) = (c("example-user"), c("example-password"));
        let mut dev = 0u64;
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
        let mut buf = vec![0 as c_char; 256];
        let mut len = 0usize;
        assert_eq!(
            nv_device_key(core, dev, buf.as_mut_ptr(), buf.len(), &mut len),
            NvStatus::NV_OK
        );
        let key = CStr::from_ptr(buf.as_ptr()).to_str().unwrap().to_owned();
        let cams = |chans: &[u8]| -> Vec<Value> {
            chans
                .iter()
                .map(|ch| json!({"camera": {"device": key, "channel": ch}, "tier": "sub"}))
                .collect()
        };
        let request = c(&json!({
            "start_ms": start, "end_ms": end, "position_ms": p0,
            "cameras": cams(&[0, 1]),
        })
        .to_string());
        let (mut pb, mut tiles, mut count) = (0u64, [0u64; 4], 0usize);

        // Before the configuration: NV_ERR_NOT_CONFIGURED.
        assert_eq!(
            nv_playback_open(
                core,
                request.as_ptr(),
                &mut pb,
                tiles.as_mut_ptr(),
                4,
                &mut count
            ),
            NvStatus::NV_ERR_NOT_CONFIGURED
        );
        assert_eq!(
            nv_recordings_configure(core, c("{}").as_ptr()),
            NvStatus::NV_OK
        );
        // A device not enabled for recordings: NV_ERR_INVALID_ARGUMENT.
        assert_eq!(
            nv_playback_open(
                core,
                request.as_ptr(),
                &mut pb,
                tiles.as_mut_ptr(),
                4,
                &mut count
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let enable = c(&json!({"channels": [{"channel": 0}, {"channel": 1}],
                               "utc_offset_s": OFFSET_S})
        .to_string());
        assert_eq!(
            nv_recordings_enable(core, c(&key).as_ptr(), enable.as_ptr()),
            NvStatus::NV_OK
        );
        let want = c(&json!({
            "cameras": [{"device": key, "channel": 0}, {"device": key, "channel": 1}],
            "days": [d(8, 0, 0, 0).to_naive_secs()],
        })
        .to_string());
        assert_eq!(nv_recordings_want(core, want.as_ptr()), NvStatus::NV_OK);
        let searches = nvr.command_count(14);
        rig.until("the index", 30, |ctx| {
            nvr.command_count(14) >= searches + 2
                && ctx
                    .events("recordings_index")
                    .last()
                    .is_some_and(|e| e["state"] == "idle" && e["pending_days"] == 0)
        });

        // Status codes before anything opens.
        for (bad, why) in [
            ("not json", "invalid JSON"),
            (r#"{"start_ms":1,"end_ms":0,"cameras":[]}"#, "empty range"),
        ] {
            assert_eq!(
                nv_playback_open(
                    core,
                    c(bad).as_ptr(),
                    &mut pb,
                    tiles.as_mut_ptr(),
                    4,
                    &mut count
                ),
                NvStatus::NV_ERR_INVALID_ARGUMENT,
                "{why}"
            );
        }
        let rate3 = c(&json!({"start_ms": start, "end_ms": end, "rate": 3.0,
                              "cameras": cams(&[0])})
        .to_string());
        assert_eq!(
            nv_playback_open(
                core,
                rate3.as_ptr(),
                &mut pb,
                tiles.as_mut_ptr(),
                4,
                &mut count
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let day_long = c(&json!({"start_ms": start, "end_ms": start + 25 * 3_600_000,
                                 "cameras": cams(&[0])})
        .to_string());
        assert_eq!(
            nv_playback_open(
                core,
                day_long.as_ptr(),
                &mut pb,
                tiles.as_mut_ptr(),
                4,
                &mut count
            ),
            NvStatus::NV_ERR_LIMIT
        );
        let twice = c(&json!({"start_ms": start, "end_ms": end,
                              "cameras": cams(&[0, 0])})
        .to_string());
        assert_eq!(
            nv_playback_open(
                core,
                twice.as_ptr(),
                &mut pb,
                tiles.as_mut_ptr(),
                4,
                &mut count
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let unknown = c(r#"{"incident":"no-such-incident"}"#);
        assert_eq!(
            nv_playback_open(
                core,
                unknown.as_ptr(),
                &mut pb,
                tiles.as_mut_ptr(),
                4,
                &mut count
            ),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(
            nv_playback_open(
                core,
                request.as_ptr(),
                ptr::null_mut(),
                tiles.as_mut_ptr(),
                4,
                &mut count
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        // Room for one tile of two: nothing opens, the count says two.
        assert_eq!(
            nv_playback_open(
                core,
                request.as_ptr(),
                &mut pb,
                tiles.as_mut_ptr(),
                1,
                &mut count
            ),
            NvStatus::NV_ERR_BUFFER_TOO_SMALL
        );
        assert_eq!(count, 2);
        let (mut pos, mut state) = (0i64, NvPlaybackState::NV_PLAYBACK_FAILED);
        assert_eq!(
            nv_playback_position(core, 12_345, &mut pos, &mut state),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(nv_playback_play(core, 12_345), NvStatus::NV_ERR_UNKNOWN_ID);
        assert_eq!(nv_playback_close(core, 12_345), NvStatus::NV_ERR_UNKNOWN_ID);
        assert_eq!(
            nv_playback_play(ptr::null_mut(), 1),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        rig.advance(Duration::from_millis(100));
        assert!(rig.ctx.events("playback_state").is_empty());
        assert!(rig.ctx.events("playback_closed").is_empty());
        assert!(rig.ctx.events("tile_closed").is_empty());

        // Open two cameras, paused at p0 with an exact seek.
        assert_eq!(
            nv_playback_open(
                core,
                request.as_ptr(),
                &mut pb,
                tiles.as_mut_ptr(),
                tiles.len(),
                &mut count
            ),
            NvStatus::NV_OK
        );
        assert_eq!(count, 2);
        let (t0, t1) = (tiles[0], tiles[1]);
        assert!(pb != 0 && t0 != 0 && t1 != 0 && t0 != t1);
        rig.ctx.playback.store(pb, Ordering::SeqCst);
        rig.until("the opening seek", 20, |_| rig.seek_active(pb, 1));
        assert!(rig.ctx.decodes(t0, 1) > 0 && rig.ctx.decodes(t1, 1) > 0);
        assert_eq!(rig.ctx.decodes(t0, 0), 0, "generation 0 is a placeholder");
        assert_eq!(rig.position(pb), (p0, NvPlaybackState::NV_PLAYBACK_PAUSED));
        let s = rig.status(pb);
        assert_eq!(s["playback"], pb);
        assert_eq!(s["state"], "paused");
        assert_eq!(s["cameras"].as_array().unwrap().len(), 2);
        assert_eq!(s["cameras"][0]["tile"], t0);
        assert_eq!(nvr.play_log().len(), 2);

        // Playback tiles are closed through their playback.
        assert_eq!(nv_tile_close(core, t0), NvStatus::NV_ERR_INVALID_ARGUMENT);
        assert_eq!(
            nv_playback_seek(core, pb, p0, 7),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_playback_set_rate(core, pb, 3.0),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_playback_set_offset(core, pb, 999_999, 0),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(
            nv_playback_position(core, pb, ptr::null_mut(), &mut state),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );

        // Play for five seconds.
        assert_eq!(nv_playback_play(core, pb), NvStatus::NV_OK);
        rig.advance(Duration::from_secs(5));
        let (pos, state) = rig.position(pb);
        assert_eq!(state, NvPlaybackState::NV_PLAYBACK_PLAYING);
        assert!((pos - (p0 + 5_000)).abs() <= 50, "{pos}");
        assert!(
            rig.of("playback_state", pb)
                .iter()
                .any(|e| e["state"] == "playing")
        );
        // Inside a callback: the position answers, other calls are refused.
        let inside = rig.ctx.inside.lock().unwrap().clone();
        assert!(!inside.is_empty());
        for (p, q) in inside {
            assert_eq!(p, NvStatus::NV_OK);
            assert_eq!(q, NvStatus::NV_ERR_INVALID_ARGUMENT);
        }

        // An exact seek a minute on: generation 2, shown at the target.
        let target = p0 + 60_000;
        assert_eq!(
            nv_playback_seek(core, pb, target, NvSeekMode::NV_SEEK_EXACT as u32),
            NvStatus::NV_OK
        );
        rig.until("the seek", 20, |_| rig.seek_active(pb, 2));
        assert!(rig.ctx.decodes(t0, 2) > 0 && rig.ctx.decodes(t1, 2) > 0);
        let (pos, _) = rig.position(pb);
        assert!(pos >= target && pos < target + 3_000, "{pos}");

        // A keyframe seek back while playing.
        assert_eq!(
            nv_playback_seek(core, pb, p0, NvSeekMode::NV_SEEK_KEYFRAME as u32),
            NvStatus::NV_OK
        );
        rig.until("the keyframe seek", 20, |_| rig.seek_active(pb, 3));

        // Rate 4x: reported by playback_state; the clock runs four times
        // as fast.
        assert_eq!(nv_playback_set_rate(core, pb, 4.0), NvStatus::NV_OK);
        rig.until("rate 4", 10, |_| {
            rig.of("playback_state", pb)
                .iter()
                .any(|e| e["rate"] == 4.0 && e["state"] == "playing")
        });
        let (before, _) = rig.position(pb);
        rig.advance(Duration::from_secs(2));
        let (after, state) = rig.position(pb);
        assert_eq!(state, NvPlaybackState::NV_PLAYBACK_PLAYING);
        assert!((after - before - 8_000).abs() <= 200, "{before} {after}");
        assert!(nvr.play_log().iter().any(|l| l.speed == Some(4)));
        assert_eq!(nv_playback_set_rate(core, pb, 1.0), NvStatus::NV_OK);

        // An offset, then pause.
        assert_eq!(nv_playback_set_offset(core, pb, t1, 500), NvStatus::NV_OK);
        rig.advance(Duration::from_secs(2));
        assert_eq!(rig.status(pb)["cameras"][1]["offset_ms"], 500);
        assert_eq!(nv_playback_pause(core, pb), NvStatus::NV_OK);
        rig.advance(Duration::from_millis(100));
        let (held, state) = rig.position(pb);
        assert_eq!(state, NvPlaybackState::NV_PLAYBACK_PAUSED);
        rig.advance(Duration::from_secs(1));
        assert_eq!(rig.position(pb).0, held);

        // Remove a camera (its tile closes), then add it back (a new tile).
        assert_eq!(nv_playback_remove_camera(core, pb, t1), NvStatus::NV_OK);
        rig.advance(Duration::from_millis(100));
        assert!(
            rig.ctx
                .events("tile_closed")
                .iter()
                .any(|e| e["tile"] == t1 && e["reason"] == "closed")
        );
        assert_eq!(
            nv_playback_remove_camera(core, pb, t1),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        let add = c(&json!({"camera": {"device": key, "channel": 1}, "tier": "sub"}).to_string());
        let mut t2 = 0u64;
        assert_eq!(
            nv_playback_add_camera(core, pb, add.as_ptr(), ptr::null_mut()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_playback_add_camera(core, pb, add.as_ptr(), &mut t2),
            NvStatus::NV_OK
        );
        assert!(t2 != 0 && t2 != t1);
        assert_eq!(
            nv_playback_add_camera(core, pb, add.as_ptr(), &mut t2),
            NvStatus::NV_ERR_INVALID_ARGUMENT,
            "a camera already in the playback"
        );
        rig.until("the new camera's pictures", 20, |ctx| {
            ctx.decodes
                .lock()
                .unwrap()
                .iter()
                .any(|((t, _), n)| *t == t2 && *n > 0)
        });

        // Close: every tile closes, then playback_closed.
        assert_eq!(nv_playback_close(core, pb), NvStatus::NV_OK);
        rig.sim.settle();
        flush_events(&*core);
        for t in [t0, t2] {
            let closed: Vec<Value> = rig
                .ctx
                .events("tile_closed")
                .into_iter()
                .filter(|e| e["tile"] == t)
                .collect();
            assert_eq!(closed.len(), 1, "tile {t}");
            assert_eq!(closed[0]["reason"], "closed");
        }
        let closed = rig.of("playback_closed", pb);
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0]["reason"], "closed");
        assert_eq!(nvr.active_plays(), 0);
        let (mut pos, mut state) = (0i64, NvPlaybackState::NV_PLAYBACK_FAILED);
        assert_eq!(
            nv_playback_position(core, pb, &mut pos, &mut state),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(nv_playback_close(core, pb), NvStatus::NV_ERR_UNKNOWN_ID);

        rig.ctx.playback.store(0, Ordering::SeqCst);
        rig.sim.run_free(|| nv_core_free(core));
    }
}
