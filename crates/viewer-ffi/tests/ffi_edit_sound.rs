//! Edit lists (docs/FFI-RECORDINGS.md, section 7) and the rest of sound
//! (section 2a) called as C would call them (minor 8), against an in-process
//! mock NVR with a recording archive and a smoke alarm on its audio, on a
//! simulated clock: a downloaded incident becomes a follow cut and a grid;
//! a watched camera fills the loudness lane and produces an alarm label;
//! the app's labels are merged; a recorded day is analyzed (and fails for
//! want of a source); a live tile's loudness is read; status codes and the
//! out-buffer convention throughout.

// Calling the C interface from Rust needs raw pointers.
#![allow(unsafe_code)]

use bc_protocol::archive::DateTime;
use mock_nvr::archive::{ArchiveConfig, MockEvent};
use nvrview::test_support::{TestRuntime, core_new_at, flush_events};
use nvrview::*;
use serde_json::{Value, json};
use simtime::SimRuntime;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The device's offset from UTC: an hour east.
const OFFSET_S: i64 = 3600;

fn d(day: u8, h: u8, m: u8, s: u8) -> DateTime {
    DateTime::new(2026, 10, day, h, m, s).unwrap()
}

#[derive(Default)]
struct Ctx {
    core: AtomicPtr<NvCore>,
    json: Mutex<Vec<String>>,
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
    fn events(&self, kind: &str) -> Vec<Value> {
        self.json
            .lock()
            .unwrap()
            .iter()
            .map(|s| serde_json::from_str::<Value>(s).unwrap())
            .filter(|v| v["type"] == kind)
            .collect()
    }
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name)
}

/// Two channels recording all of the 8th, with a person on channel 0 and a
/// vehicle on channel 1 overlapping it, and a smoke alarm on every
/// channel's live audio.
fn mock(sim: &SimRuntime) -> mock_nvr::MockNvr {
    let mut cfg = mock_nvr::Config::new(
        mock_nvr::fixture::Fixture::load(&fixture("h264-basic.h264")).unwrap(),
    );
    cfg.channels = 2;
    let mut a = ArchiveConfig::new(vec![d(8, 0, 0, 0)]);
    a.events_per_day = 0;
    a.utc_offset_s = OFFSET_S;
    a.play_end_message = false;
    a.events = vec![
        MockEvent {
            channel: 0,
            kind: "people".into(),
            start: d(8, 10, 0, 0),
            end: d(8, 10, 0, 20),
        },
        MockEvent {
            channel: 1,
            kind: "vehicle".into(),
            start: d(8, 10, 0, 12),
            end: d(8, 10, 0, 30),
        },
    ];
    cfg.archive = Some(a);
    let mut audio = mock_nvr::audio::AudioConfig::new(
        mock_nvr::audio::AudioFixture::load(&fixture("aac-smoke-t3.aac")).unwrap(),
    );
    audio.jitter_us = 30_000;
    cfg.audio = Some(Arc::new(audio));
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

    /// An out-buffer call the way the bindings make it: ask for the size,
    /// then fetch with room for the NUL, looping while the answer grows.
    fn answer(
        &self,
        mut f: impl FnMut(*mut c_char, usize, *mut usize) -> NvStatus,
    ) -> Result<Value, NvStatus> {
        let mut len = 0usize;
        let first = f(ptr::null_mut(), 0, &mut len);
        if first != NvStatus::NV_ERR_BUFFER_TOO_SMALL {
            return Err(first);
        }
        loop {
            let mut buf = vec![0 as c_char; len + 1];
            match f(buf.as_mut_ptr(), buf.len(), &mut len) {
                NvStatus::NV_OK => {
                    // SAFETY: the core wrote a NUL-terminated string.
                    let text = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap();
                    assert_eq!(text.len(), len);
                    return Ok(serde_json::from_str(text).unwrap());
                }
                NvStatus::NV_ERR_BUFFER_TOO_SMALL => continue,
                s => return Err(s),
            }
        }
    }
}

fn db(lane: &Value, member: &str) -> Vec<Option<i64>> {
    lane[member]
        .as_array()
        .unwrap()
        .iter()
        .map(Value::as_i64)
        .collect()
}

#[test]
#[allow(clippy::too_many_lines)]
fn edit_lists_and_sound_through_the_c_interface() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let dir = tempfile::tempdir().unwrap();
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
    // "Today" is the 10th at noon UTC; the recordings are of the 8th.
    let today = d(10, 12, 0, 0).to_naive_secs();
    let core = core_new_at(runtime, cb, Some(today * 1_000_000)).unwrap();
    ctx.core.store(core, Ordering::SeqCst);
    let rig = Rig { sim, ctx, core };
    let local0 = today + OFFSET_S;
    let day8 = d(8, 0, 0, 0).to_naive_secs();

    // SAFETY: valid pointers and strings throughout; the core is freed
    // once, at the end.
    unsafe {
        assert!(nv_abi_minor() >= 8);
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
        let cam = |ch: u8| json!({"device": key, "channel": ch, "lens": 0});

        // Before the configuration: NV_ERR_NOT_CONFIGURED where the call
        // needs the recordings owner.
        assert_eq!(
            nv_sound_watch(core, c(&cam(0).to_string()).as_ptr(), true),
            NvStatus::NV_ERR_NOT_CONFIGURED
        );
        assert_eq!(
            rig.answer(|o, n, l| nv_edit_compute(
                core,
                c(r#"{"kind":"follow","download":"x"}"#).as_ptr(),
                o,
                n,
                l
            )),
            Err(NvStatus::NV_ERR_NOT_CONFIGURED)
        );
        let downloads = dir.path().join("downloads");
        let cfg = c(&json!({"downloads_dir": downloads.to_str().unwrap()}).to_string());
        assert_eq!(nv_recordings_configure(core, cfg.as_ptr()), NvStatus::NV_OK);
        let enable = c(&json!({"channels": [{"channel": 0}, {"channel": 1}],
                               "utc_offset_s": OFFSET_S})
        .to_string());
        assert_eq!(
            nv_recordings_enable(core, c(&key).as_ptr(), enable.as_ptr()),
            NvStatus::NV_OK
        );

        // ---- Edit lists -------------------------------------------------
        let want = c(&json!({"cameras": [cam(0), cam(1)], "days": [day8]}).to_string());
        assert_eq!(nv_recordings_want(core, want.as_ptr()), NvStatus::NV_OK);
        let query = c(&json!({"start": day8, "end": day8 + 86_399}).to_string());
        rig.until("the incident", 60, |_| {
            rig.answer(|o, n, l| nv_incidents_get(core, query.as_ptr(), o, n, l))
                .is_ok_and(|v| {
                    v["incidents"].as_array().is_some_and(|i| {
                        i.len() == 1 && i[0]["cameras"].as_array().unwrap().len() == 2
                    })
                })
        });
        let incidents = rig
            .answer(|o, n, l| nv_incidents_get(core, query.as_ptr(), o, n, l))
            .unwrap();
        let incident = incidents["incidents"][0]["key"]
            .as_str()
            .unwrap()
            .to_owned();

        // Nothing is downloaded yet: the download is unknown.
        let follow = |download: &str| {
            c(
                &json!({"kind": "follow", "download": download, "min_hold_ms": 2000,
                      "pre_roll_ms": 1000, "cut": "keyframe"})
                .to_string(),
            )
        };
        assert_eq!(
            rig.answer(|o, n, l| nv_edit_compute(
                core,
                follow("no-such-download").as_ptr(),
                o,
                n,
                l
            )),
            Err(NvStatus::NV_ERR_UNKNOWN_ID)
        );

        let request = c(&json!({"incident": incident}).to_string());
        let mut out = vec![0 as c_char; 64 * 1024];
        assert_eq!(
            nv_download_incident(
                core,
                request.as_ptr(),
                out.as_mut_ptr(),
                out.len(),
                &mut len
            ),
            NvStatus::NV_OK
        );
        let queued: Value =
            serde_json::from_str(CStr::from_ptr(out.as_ptr()).to_str().unwrap()).unwrap();
        let id = queued["download"].as_str().unwrap().to_owned();
        rig.until("the download", 600, |ctx| {
            ctx.events("download")
                .iter()
                .any(|e| e["download"] == id.as_str() && e["state"] == "done")
        });

        // The follow cut: the person first, then the vehicle.
        let plan = rig
            .answer(|o, n, l| nv_edit_compute(core, follow(&id).as_ptr(), o, n, l))
            .unwrap();
        assert_eq!(plan["version"], 1);
        assert_eq!(plan["kind"], "follow");
        assert_eq!(plan["download"], id.as_str());
        assert_eq!(plan["sources"].as_array().unwrap().len(), 2, "{plan}");
        assert_eq!(plan["sources"][0]["codec"], "h264");
        assert!(plan["sources"][0]["width"].as_u64().unwrap() > 0);
        let segs = plan["segments"].as_array().unwrap();
        assert!(segs.len() >= 2, "{plan}");
        assert_eq!(segs[0]["source"], 0);
        assert_eq!(segs.last().unwrap()["source"], 1);
        assert!(plan["start_ms"].as_i64().unwrap() < plan["end_ms"].as_i64().unwrap());
        // A frame cut and a priority are accepted; an unknown member is
        // ignored.
        let frame = c(&json!({"kind": "follow", "download": id, "cut": "frame",
                              "priority": [cam(1)], "future": 1})
        .to_string());
        let plan = rig
            .answer(|o, n, l| nv_edit_compute(core, frame.as_ptr(), o, n, l))
            .unwrap();
        assert_eq!(plan["copy_possible"], false);

        // A 2-up grid: two cells that tile the canvas.
        let grid = c(&json!({"kind": "grid", "download": id, "layout": "2up",
                             "canvas": {"width": 1920, "height": 1080}, "fit": "fit",
                             "offsets_ms": [{"camera": cam(1), "offset_ms": -400}]})
        .to_string());
        let plan = rig
            .answer(|o, n, l| nv_edit_compute(core, grid.as_ptr(), o, n, l))
            .unwrap();
        assert_eq!(plan["kind"], "grid");
        assert_eq!(plan["layout"], "2up");
        let cells = plan["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0]["rect"]["width"], 960);
        assert_eq!(cells[1]["rect"]["x"], 960);
        assert!(!cells[0]["present"].as_array().unwrap().is_empty());

        // Status codes.
        let bad =
            |json: &str| rig.answer(|o, n, l| nv_edit_compute(core, c(json).as_ptr(), o, n, l));
        assert_eq!(bad("{"), Err(NvStatus::NV_ERR_INVALID_ARGUMENT));
        assert_eq!(
            bad(&json!({"kind": "nonsense", "download": id}).to_string()),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        assert_eq!(
            bad(&json!({"kind": "follow"}).to_string()),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        assert_eq!(
            bad(&json!({"kind": "grid", "download": id,
                        "canvas": {"width": 0, "height": 10}})
            .to_string()),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        // Trimming and the trim kind belong to export.
        assert_eq!(
            bad(&json!({"kind": "follow", "download": id, "in_ms": 1}).to_string()),
            Err(NvStatus::NV_ERR_UNSUPPORTED)
        );
        assert_eq!(
            bad(&json!({"kind": "trim", "download": id}).to_string()),
            Err(NvStatus::NV_ERR_UNSUPPORTED)
        );
        // A NULL length, request, and core.
        assert_eq!(
            nv_edit_compute(
                core,
                follow(&id).as_ptr(),
                ptr::null_mut(),
                0,
                ptr::null_mut()
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_edit_compute(core, ptr::null(), ptr::null_mut(), 0, &mut len),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_edit_compute(
                ptr::null_mut(),
                follow(&id).as_ptr(),
                ptr::null_mut(),
                0,
                &mut len
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        // Too small a buffer reports the size and fills nothing.
        let mut small = [0 as c_char; 8];
        assert_eq!(
            nv_edit_compute(
                core,
                follow(&id).as_ptr(),
                small.as_mut_ptr(),
                small.len(),
                &mut len
            ),
            NvStatus::NV_ERR_BUFFER_TOO_SMALL
        );
        assert!(len > small.len());

        // ---- Sound: watching --------------------------------------------
        assert_eq!(
            nv_sound_watch(core, c("{").as_ptr(), true),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_sound_watch(ptr::null_mut(), c(&cam(1).to_string()).as_ptr(), true),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        // Nothing is heard before a watch.
        let lane_query = |ch: u8, bucket: u32| {
            c(
                &json!({"start": local0, "end": local0 + 59, "bucket_s": bucket,
                      "cameras": [{"device": key, "channel": ch}]})
                .to_string(),
            )
        };
        let lane = |ch: u8, bucket: u32| {
            let q = lane_query(ch, bucket);
            rig.answer(|o, n, l| nv_recordings_loudness(core, q.as_ptr(), o, n, l))
                .unwrap()["lanes"][0]
                .clone()
        };
        assert!(db(&lane(1, 1), "peak_db").iter().all(Option::is_none));

        let watch = c(&cam(1).to_string());
        assert_eq!(nv_sound_watch(core, watch.as_ptr(), true), NvStatus::NV_OK);
        let alarms = c(
            &json!({"start": day_of(local0), "end": day_of(local0) + 86_399,
                               "kinds": ["smoke_alarm"]})
            .to_string(),
        );
        rig.until("an alarm label", 120, |_| {
            rig.answer(|o, n, l| nv_recordings_events(core, alarms.as_ptr(), o, n, l))
                .is_ok_and(|v| !v["events"].as_array().unwrap().is_empty())
        });
        let found = rig
            .answer(|o, n, l| nv_recordings_events(core, alarms.as_ptr(), o, n, l))
            .unwrap();
        assert_eq!(found["events"][0]["source"], "local_audio");
        assert_eq!(found["events"][0]["analyzer"], "pattern-v1");
        assert_eq!(found["events"][0]["camera"]["channel"], 1);
        let l = lane(1, 1);
        assert_eq!(l["bucket_s"], 1);
        assert_eq!(l["start"], local0);
        let peaks = db(&l, "peak_db");
        assert!(
            peaks.iter().flatten().any(|v| (-13..=-7).contains(v)),
            "{peaks:?}"
        );
        // Channel 0 was not listened to.
        assert!(db(&lane(0, 1), "peak_db").iter().all(Option::is_none));
        let states: Vec<String> = rig
            .ctx
            .events("sound_watch")
            .iter()
            .map(|e| e["state"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(states, ["watching"]);

        // Loudness query errors.
        let loud = |json: &str| {
            rig.answer(|o, n, l| nv_recordings_loudness(core, c(json).as_ptr(), o, n, l))
        };
        assert_eq!(loud("{"), Err(NvStatus::NV_ERR_INVALID_ARGUMENT));
        assert_eq!(
            loud(&json!({"start": 0, "end": 10, "bucket_s": 0}).to_string()),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        assert_eq!(
            loud(
                &json!({"start": 0, "end": 200_000, "bucket_s": 1,
                         "cameras": [cam(0)]})
                .to_string()
            ),
            Err(NvStatus::NV_ERR_LIMIT)
        );

        // ---- Sound: a live tile's loudness ------------------------------
        let mut tile = 0u64;
        assert_eq!(
            nv_tile_open(core, dev, 0, NvStreamTier::NV_TIER_SUB as _, &mut tile),
            NvStatus::NV_OK
        );
        rig.advance(Duration::from_secs(8));
        let (mut peak, mut rms) = ([0u8; 16], [0u8; 16]);
        let (mut start_s, mut n) = (0i64, 0usize);
        assert_eq!(
            nv_tile_loudness(
                core,
                tile,
                peak.as_mut_ptr(),
                rms.as_mut_ptr(),
                peak.len(),
                &mut start_s,
                &mut n
            ),
            NvStatus::NV_OK
        );
        assert!((1..=16).contains(&n), "{n}");
        assert!(peak[..n].iter().any(|&b| b != 0), "{:?}", &peak[..n]);
        assert!(start_s >= today - 5, "{start_s}");
        // Asking for fewer gives the newest ones, starting later.
        let (mut s2, mut n2) = (0i64, 0usize);
        let (mut p2, mut r2) = ([0u8; 2], [0u8; 2]);
        assert_eq!(
            nv_tile_loudness(
                core,
                tile,
                p2.as_mut_ptr(),
                r2.as_mut_ptr(),
                2,
                &mut s2,
                &mut n2
            ),
            NvStatus::NV_OK
        );
        assert_eq!(n2, 2);
        assert!(s2 >= start_s);
        // Only the count.
        assert_eq!(
            nv_tile_loudness(
                core,
                tile,
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                &mut s2,
                &mut n2
            ),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_tile_loudness(
                core,
                987_654,
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                &mut s2,
                &mut n2
            ),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(
            nv_tile_loudness(
                core,
                tile,
                ptr::null_mut(),
                rms.as_mut_ptr(),
                4,
                &mut s2,
                &mut n2
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_tile_loudness(
                core,
                tile,
                ptr::null_mut(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                &mut n2
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(nv_tile_close(core, tile), NvStatus::NV_OK);

        // Stopping the watch.
        assert_eq!(nv_sound_watch(core, watch.as_ptr(), false), NvStatus::NV_OK);
        rig.advance(Duration::from_millis(500));
        let states: Vec<String> = rig
            .ctx
            .events("sound_watch")
            .iter()
            .map(|e| e["state"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(states, ["watching", "stopped"]);

        // ---- Sound: labels from the app ---------------------------------
        let label_day = local0 - 86_400;
        let put = |analyzer: &str, kind: &str| {
            c(&json!({"camera": cam(0), "analyzer": analyzer,
                      "start": label_day, "end": label_day + 600,
                      "labels": [{"kind": kind, "start": label_day + 125,
                                  "end": label_day + 131, "confidence": 0.91,
                                  "label": "dog_bark"}]})
            .to_string())
        };
        assert_eq!(
            nv_sound_labels_put(core, put("soundanalysis-v1", "dog_bark").as_ptr()),
            NvStatus::NV_OK
        );
        let barks = c(
            &json!({"start": day_of(label_day), "end": day_of(label_day) + 86_399,
                              "kinds": ["dog_bark"]})
            .to_string(),
        );
        let found = rig
            .answer(|o, n, l| nv_recordings_events(core, barks.as_ptr(), o, n, l))
            .unwrap();
        let e = &found["events"][0];
        assert_eq!(e["source"], "local_audio");
        assert_eq!(e["analyzer"], "soundanalysis-v1");
        assert_eq!(e["label"], "dog_bark");
        assert!((e["confidence"].as_f64().unwrap() - 0.91).abs() < 1e-6);
        // The core's own analyzer name is reserved; a bad kind and a device
        // that is unknown are refused; a NULL string is invalid.
        assert_eq!(
            nv_sound_labels_put(core, put("pattern-v1", "dog_bark").as_ptr()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_sound_labels_put(core, put("soundanalysis-v1", "Dog Bark").as_ptr()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let wide = c(&json!({"camera": cam(0), "analyzer": "soundanalysis-v1",
                             "start": 0, "end": 40 * 86_400, "labels": []})
        .to_string());
        assert_eq!(
            nv_sound_labels_put(core, wide.as_ptr()),
            NvStatus::NV_ERR_LIMIT
        );
        assert_eq!(
            nv_sound_labels_put(core, ptr::null()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );

        // ---- Sound: analyzing a recorded day ----------------------------
        let analyze = c(&json!({"camera": cam(0), "day": day8 + 5}).to_string());
        // A call that changes state is made once with room for the answer
        // (a size probe would queue the request and the second call would
        // find it, perhaps already running).
        let mut room = vec![0 as c_char; 4096];
        assert_eq!(
            nv_sound_analyze_day(
                core,
                analyze.as_ptr(),
                room.as_mut_ptr(),
                room.len(),
                &mut len
            ),
            NvStatus::NV_OK
        );
        let queued: Value =
            serde_json::from_str(CStr::from_ptr(room.as_ptr()).to_str().unwrap()).unwrap();
        assert_eq!(queued["state"], "queued");
        // Asking again while it waits or runs returns the same request.
        assert_eq!(
            nv_sound_analyze_day(
                core,
                analyze.as_ptr(),
                room.as_mut_ptr(),
                room.len(),
                &mut len
            ),
            NvStatus::NV_OK
        );
        let again: Value =
            serde_json::from_str(CStr::from_ptr(room.as_ptr()).to_str().unwrap()).unwrap();
        assert_eq!(again["request"], queued["request"]);
        let request = queued["request"].as_u64().unwrap();
        // With no audio source (none exists until downloads or playback
        // provide one) the request fails; events say so.
        rig.until("the analysis to end", 60, |ctx| {
            ctx.events("sound_analyze")
                .iter()
                .any(|e| e["request"] == request && e["state"] == "failed")
        });
        let last = rig.ctx.events("sound_analyze").pop().unwrap();
        assert_eq!(last["reason"], "no_source");
        assert_eq!(last["camera"]["channel"], 0);
        assert_eq!(
            nv_sound_analyze_cancel(core, request),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(
            nv_sound_analyze_cancel(core, 99),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(
            rig.answer(|o, n, l| nv_sound_analyze_day(core, c("{").as_ptr(), o, n, l)),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        let other = c(&json!({"camera": {"device": key, "channel": 9}, "day": day8}).to_string());
        assert_eq!(
            rig.answer(|o, n, l| nv_sound_analyze_day(core, other.as_ptr(), o, n, l)),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        assert_eq!(
            nv_sound_analyze_day(core, analyze.as_ptr(), ptr::null_mut(), 0, ptr::null_mut()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_sound_analyze_cancel(ptr::null_mut(), 1),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );

        // Shutdown on the simulated clock.
        rig.sim.run_free(|| nv_core_free(core));
    }
}

/// The start of the device-local day holding `local` seconds.
fn day_of(local: i64) -> i64 {
    local - local.rem_euclid(86_400)
}
