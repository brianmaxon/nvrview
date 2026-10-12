//! Capture (docs/FFI.md, "Capture"; minor 9) called as C would call it,
//! against an in-process mock NVR with AAC audio and a doorbell, on a
//! simulated clock: a live tile recorded to a finalized MP4, a call
//! recording whose Conversation and You tracks go through an
//! `NvAudioEncoder` of C functions (answering from inside its own
//! callbacks), the limit answer, a too-small answer buffer, and the status
//! codes.

// Calling the C interface from Rust needs raw pointers.
#![allow(unsafe_code)]

#[path = "../../mp4/tests/common/mod.rs"]
mod mp4_common;

use nvrview::test_support::{TestRuntime, core_new_on, flush_events};
use nvrview::*;
use serde_json::{Value, json};
use simtime::SimRuntime;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The doorbell's channel (it has a speaker); channel 0 is a plain camera.
const BELL: u8 = 1;
/// A silent AAC-LC mono access unit (what the fake encoder answers).
const SILENT_AU: [u8; 4] = [0, 0, 0, 7];

#[derive(Default)]
struct Ctx {
    core: AtomicPtr<NvCore>,
    json: Mutex<Vec<String>>,
}

/// The callbacks' context, never freed (the core may call back until
/// `nv_core_free` returns, and a failing assertion unwinds past that call).
fn leaked<T: Default>() -> &'static T {
    Box::leak(Box::default())
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
            );
        }
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
    ctx.json
        .lock()
        .unwrap()
        .push(String::from_utf8(bytes.to_vec()).unwrap());
}

impl Ctx {
    fn recording(&self, id: u64) -> Vec<Value> {
        self.json
            .lock()
            .unwrap()
            .iter()
            .map(|s| serde_json::from_str::<Value>(s).unwrap())
            .filter(|v| v["type"] == "recording" && v["recording"] == id)
            .collect()
    }

    fn has_state(&self, id: u64, state: &str) -> bool {
        self.recording(id).iter().any(|e| e["state"] == state)
    }
}

/// (recording, track, rate, channels, bit rate) of an `open`.
type Opened = (u64, u32, u32, u32, u32);

/// The encoder's context: what the core asked of it.
#[derive(Default)]
struct Enc {
    core: AtomicPtr<NvCore>,
    opened: Mutex<Vec<Opened>>,
    closed: Mutex<Vec<(u64, u32, bool)>>,
    /// Frames encoded per (recording, track).
    frames: Mutex<Vec<(u64, u32)>>,
    /// nv_record_audio_encoded calls that did not answer NV_OK.
    refused: AtomicU64,
}

impl Enc {
    fn answer(&self, recording: u64, track: u32) {
        // SAFETY: the core is live while it calls the encoder; the unit is
        // valid for its length.
        let s = unsafe {
            nv_record_audio_encoded(
                self.core.load(Ordering::SeqCst),
                recording,
                track,
                SILENT_AU.as_ptr(),
                SILENT_AU.len(),
            )
        };
        if s != NvStatus::NV_OK {
            self.refused.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn frames(&self, recording: u64, track: u32) -> usize {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|f| **f == (recording, track))
            .count()
    }
}

unsafe extern "C" fn enc_open(
    context: *mut c_void,
    recording: u64,
    track: u32,
    sample_rate: u32,
    channels: u32,
    bit_rate: u32,
    asc: *mut u8,
    asc_cap: usize,
    asc_len: *mut usize,
    priming_frames: *mut u32,
) -> bool {
    // SAFETY: the context is the test's Enc; the out pointers are valid for
    // the call, `asc` for `asc_cap` bytes.
    unsafe {
        let e = &*(context as *const Enc);
        e.opened
            .lock()
            .unwrap()
            .push((recording, track, sample_rate, channels, bit_rate));
        if asc_cap < 2 {
            return false;
        }
        // AAC-LC, 48 kHz, mono.
        asc.write(0x11);
        asc.add(1).write(0x88);
        asc_len.write(2);
        priming_frames.write(2112);
    }
    true
}

unsafe extern "C" fn enc_encode(
    context: *mut c_void,
    recording: u64,
    track: u32,
    pcm: *const f32,
    frames: usize,
) {
    // SAFETY: the context is the test's Enc.
    let e = unsafe { &*(context as *const Enc) };
    if pcm.is_null() || frames != 1024 {
        e.refused.fetch_add(1, Ordering::SeqCst);
    }
    e.frames.lock().unwrap().push((recording, track));
    // Answered from inside the encoder's own callback.
    e.answer(recording, track);
}

unsafe extern "C" fn enc_close(context: *mut c_void, recording: u64, track: u32, flush: bool) {
    // SAFETY: the context is the test's Enc.
    let e = unsafe { &*(context as *const Enc) };
    if flush {
        // The encoder's delay comes out at the end.
        e.answer(recording, track);
        e.answer(recording, track);
    }
    e.closed.lock().unwrap().push((recording, track, flush));
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name)
}

/// Two channels with a 16 kHz AAC tone; talk-back on BELL.
fn mock(sim: &SimRuntime) -> mock_nvr::MockNvr {
    let mut cfg = mock_nvr::Config::new(
        mock_nvr::fixture::Fixture::load(&fixture("h264-basic.h264")).unwrap(),
    );
    cfg.channels = 2;
    cfg.talk_channels = vec![BELL];
    cfg.doorbell_channels = vec![BELL];
    cfg.audio = Some(Arc::new(mock_nvr::audio::AudioConfig::new(
        mock_nvr::audio::AudioFixture::load(&fixture("aac-tone.aac")).unwrap(),
    )));
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

    fn until_state(&self, id: u64, state: &str, secs: u64) {
        self.until(state, secs, |ctx| ctx.has_state(id, state));
    }

    fn until_ended(&self, id: u64) -> Value {
        self.until("the end of the recording", 10, |ctx| {
            ctx.has_state(id, "finished") || ctx.has_state(id, "failed")
        });
        self.ctx.recording(id).pop().unwrap()
    }

    /// nv_record_start with a roomy buffer: the status and the answer.
    fn start(&self, request: &str) -> (NvStatus, Option<Value>) {
        let mut out = vec![0 as c_char; 4096];
        let mut len = 0usize;
        let req = c(request);
        // SAFETY: valid pointers; the core is live.
        let s = unsafe {
            nv_record_start(
                self.core,
                req.as_ptr(),
                out.as_mut_ptr(),
                out.len(),
                &mut len,
            )
        };
        let answer = (len > 0).then(|| {
            // SAFETY: the core wrote a NUL-terminated string.
            let text = unsafe { CStr::from_ptr(out.as_ptr()) }.to_str().unwrap();
            assert_eq!(text.len(), len);
            serde_json::from_str(text).unwrap()
        });
        (s, answer)
    }

    /// A start that succeeds: the recording's ID and file.
    fn started(&self, request: &str) -> (u64, PathBuf) {
        let (s, a) = self.start(request);
        assert_eq!(s, NvStatus::NV_OK, "{request}");
        let a = a.unwrap();
        assert_eq!(a["state"], "waiting_keyframe");
        (
            a["recording"].as_u64().unwrap(),
            PathBuf::from(a["file"].as_str().unwrap()),
        )
    }

    fn status(&self) -> Value {
        let mut out = vec![0 as c_char; 64 * 1024];
        let mut len = 0usize;
        // SAFETY: valid pointers; the core is live.
        let s = unsafe { nv_record_status(self.core, out.as_mut_ptr(), out.len(), &mut len) };
        assert_eq!(s, NvStatus::NV_OK);
        // SAFETY: the core wrote a NUL-terminated string.
        serde_json::from_str(unsafe { CStr::from_ptr(out.as_ptr()) }.to_str().unwrap()).unwrap()
    }
}

/// A finished, standard (not fragmented) MP4: its tracks' tables.
fn read_mp4(path: &Path) -> Vec<mp4_common::TrackTables> {
    let d = std::fs::read(path).unwrap();
    let kinds: Vec<String> = mp4_common::top(&d).into_iter().map(|b| b.ty).collect();
    assert!(kinds.contains(&"moov".into()), "{kinds:?}");
    assert!(
        !kinds.contains(&"moof".into()),
        "still fragmented: {kinds:?}"
    );
    mp4_common::tracks(&d)
}

fn seconds(t: &mp4_common::TrackTables) -> f64 {
    t.media_duration as f64 / f64::from(t.timescale)
}

#[test]
#[allow(clippy::too_many_lines)]
fn capture_through_the_c_interface() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let live = tempfile::tempdir().unwrap();
    let ctx: &'static Ctx = leaked();
    let enc: &'static Enc = leaked();
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
    let core = core_new_on(runtime, cb).unwrap();
    ctx.core.store(core, Ordering::SeqCst);
    enc.core.store(core, Ordering::SeqCst);
    let rig = Rig { sim, ctx, core };
    let dir = serde_json::to_string(&live.path().to_string_lossy()).unwrap();

    // SAFETY: valid pointers and strings throughout; the core is freed
    // once, at the end.
    unsafe {
        assert!(nv_abi_minor() >= 9);
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
        let mut tile = 0u64;
        assert_eq!(
            nv_tile_open(core, dev, 0, NvStreamTier::NV_TIER_SUB as u32, &mut tile),
            NvStatus::NV_OK
        );
        let live_req = json!({"kind": "live", "tile": tile, "name": "Camera 1",
                              "utc_offset_s": 3600})
        .to_string();

        // Before nv_capture_configure: not configured, no answer.
        assert_eq!(
            rig.start(&live_req),
            (NvStatus::NV_ERR_NOT_CONFIGURED, None)
        );
        for bad in [
            "{",
            r#"{"live_dir":"relative/dir"}"#,
            r#"{"live_dir":"/x","quality":"best"}"#,
        ] {
            assert_eq!(
                nv_capture_configure(core, c(bad).as_ptr()),
                NvStatus::NV_ERR_INVALID_ARGUMENT,
                "{bad}"
            );
        }
        assert_eq!(
            nv_capture_configure(core, ptr::null()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_capture_configure(ptr::null_mut(), c("{}").as_ptr()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        // Free space is the computer's own (no C hook): a threshold no disk
        // meets refuses with the disk_low limit and its answer.
        let cfg = |extra: &str| c(&format!(r#"{{"live_dir":{dir}{extra}}}"#));
        assert_eq!(
            nv_capture_configure(
                core,
                cfg(r#","min_free_bytes":1000000000000000000"#).as_ptr()
            ),
            NvStatus::NV_OK
        );
        assert_eq!(
            rig.start(&live_req),
            (NvStatus::NV_ERR_LIMIT, Some(json!({"limit": "disk_low"})))
        );
        // The limit is reported even when its answer does not fit.
        let mut len = 0usize;
        let req = c(&live_req);
        assert_eq!(
            nv_record_start(core, req.as_ptr(), ptr::null_mut(), 0, &mut len),
            NvStatus::NV_ERR_LIMIT
        );
        assert_eq!(len, r#"{"limit":"disk_low"}"#.len());
        assert_eq!(
            nv_record_start(core, req.as_ptr(), ptr::null_mut(), 0, ptr::null_mut()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let settings = r#","min_free_bytes":0,"stop_free_bytes":0,"max_live":1"#;
        assert_eq!(
            nv_capture_configure(core, cfg(settings).as_ptr()),
            NvStatus::NV_OK
        );

        // Refusals that start nothing.
        assert_eq!(
            rig.start(&json!({"kind": "live", "tile": tile + 999}).to_string()),
            (NvStatus::NV_ERR_UNKNOWN_ID, None)
        );
        assert_eq!(
            rig.start(r#"{"kind":"live"}"#),
            (NvStatus::NV_ERR_INVALID_ARGUMENT, None)
        );
        assert_eq!(nv_record_stop(core, 999), NvStatus::NV_ERR_UNKNOWN_ID);
        assert_eq!(rig.status(), json!({"recordings": []}));

        // An answer that does not fit: the recording is taken back.
        let mut small = vec![0 as c_char; 8];
        assert_eq!(
            nv_record_start(
                core,
                req.as_ptr(),
                small.as_mut_ptr(),
                small.len(),
                &mut len
            ),
            NvStatus::NV_ERR_BUFFER_TOO_SMALL
        );
        assert!(len > small.len());
        rig.until("the taken-back recording ends", 10, |ctx| {
            ctx.json
                .lock()
                .unwrap()
                .iter()
                .any(|e| e.contains(r#""type":"recording""#) && e.contains(r#""reason":"stopped""#))
        });
        assert_eq!(rig.status(), json!({"recordings": []}));

        // ---- A live tile to a finalized MP4 -----------------------------
        let (id, file) = rig.started(&live_req);
        assert_eq!(file.parent(), Some(live.path()));
        assert!(
            file.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(" Camera 1.mp4"),
            "{file:?}"
        );
        // max_live is 1.
        assert_eq!(
            rig.start(&live_req),
            (NvStatus::NV_ERR_LIMIT, Some(json!({"limit": "live"})))
        );
        rig.until_state(id, "recording", 10);
        rig.until("3 s recorded", 20, |ctx| {
            ctx.recording(id)
                .last()
                .and_then(|e| e["duration_ms"].as_u64())
                .unwrap_or(0)
                >= 3000
        });
        let status = rig.status();
        assert_eq!(status["recordings"][0]["recording"], id);
        assert_eq!(status["recordings"][0]["state"], "recording");
        assert_eq!(nv_record_stop(core, id), NvStatus::NV_OK);
        let end = rig.until_ended(id);
        assert_eq!(end["state"], "finished");
        assert_eq!(end["reason"], "stopped");
        assert_eq!(end["audio"], "passthrough");
        assert_eq!(end["tracks"], json!(["video", "audio"]));
        let t = read_mp4(&file);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].sync.as_ref().unwrap()[0], 1, "starts at a keyframe");
        let dur = end["duration_ms"].as_u64().unwrap() as f64 / 1000.0;
        assert!((seconds(&t[0]) - dur).abs() < 0.2, "{} s", seconds(&t[0]));
        assert!((seconds(&t[1]) - seconds(&t[0])).abs() < 0.5);

        // ---- A call recording through the C encoder ----------------------
        let mut bell = 0u64;
        assert_eq!(
            nv_tile_open(
                core,
                dev,
                BELL,
                NvStreamTier::NV_TIER_MAIN as u32,
                &mut bell
            ),
            NvStatus::NV_OK
        );
        let call = live.path().join("2026-10-11 17.02.03 Doorbell.mp4");
        let call_req = json!({"kind": "call", "tile": bell, "file": call}).to_string();
        // Without an encoder a call cannot be recorded.
        assert_eq!(
            rig.start(&call_req),
            (NvStatus::NV_ERR_NOT_CONFIGURED, None)
        );
        let mut encoder = NvAudioEncoder {
            context: enc as *const Enc as *mut c_void,
            open: Some(enc_open),
            encode: Some(enc_encode),
            close: None,
        };
        assert_eq!(
            nv_capture_set_encoder(core, &encoder),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        encoder.close = Some(enc_close);
        assert_eq!(nv_capture_set_encoder(core, &encoder), NvStatus::NV_OK);
        assert_eq!(
            nv_talk_set_permission(core, dev, BELL, true),
            NvStatus::NV_OK
        );
        assert_eq!(nv_talk_set_conversation(core, bell, true), NvStatus::NV_OK);
        let (call_id, call_file) = rig.started(&call_req);
        assert_eq!(call_file, call);
        // Removing the encoder now leaves the running call with its own.
        assert_eq!(nv_capture_set_encoder(core, ptr::null()), NvStatus::NV_OK);
        rig.until_state(call_id, "recording", 10);
        rig.until("2 s recorded", 20, |ctx| {
            ctx.recording(call_id)
                .last()
                .and_then(|e| e["duration_ms"].as_u64())
                .unwrap_or(0)
                >= 2000
        });
        // The call ends: so does its recording.
        assert_eq!(nv_talk_set_conversation(core, bell, false), NvStatus::NV_OK);
        let end = rig.until_ended(call_id);
        assert_eq!(end["state"], "finished", "{end}");
        assert_eq!(end["kind"], "call");
        assert_eq!(
            end["tracks"],
            json!(["video", "Conversation", "Visitor", "You"])
        );
        assert_eq!(end["encode_dropped_ms"], 0);
        assert_eq!(enc.refused.load(Ordering::SeqCst), 0);
        assert_eq!(
            *enc.opened.lock().unwrap(),
            [
                (call_id, 0, 48_000, 1, 96_000),
                (call_id, 2, 48_000, 1, 96_000)
            ]
        );
        assert_eq!(
            *enc.closed.lock().unwrap(),
            [(call_id, 0, true), (call_id, 2, true)]
        );
        assert_eq!(
            nv_record_audio_encoded(core, call_id, 0, ptr::null(), 4),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let t = read_mp4(&call_file);
        assert_eq!(t.len(), 4);
        // Every frame given to the encoder came back as a unit (plus the
        // two of the flush), and the priming is an edit.
        let conversation = enc.frames(call_id, 0);
        assert!(conversation > 50, "{conversation} frames");
        assert_eq!(t[1].stsz.len(), conversation + 2);
        assert_eq!(t[3].stsz.len(), enc.frames(call_id, 2) + 2);
        assert_eq!(t[1].edits.last().unwrap().1, 2112);
        assert!((seconds(&t[1]) - seconds(&t[0])).abs() < 0.5);

        // Shutdown on the simulated clock.
        rig.sim.run_free(|| nv_core_free(core));
    }
}
