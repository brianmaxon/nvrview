//! Export (docs/FFI-RECORDINGS.md, section 7a) called as C would call it
//! (minor 10), against an in-process mock NVR with a recording archive on a
//! simulated clock: a downloaded incident becomes trims and trimmed edit
//! lists; a range download is queued, answered again as `existing`, and
//! trimmed; stills are planned from a file and from a playback; `dest` and
//! `share_safe` apply; status codes and the out-buffer convention
//! throughout.

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

fn ms(day: u8, h: u8, m: u8, s: u8) -> i64 {
    d(day, h, m, s).to_naive_secs() * 1000
}

#[derive(Default)]
struct Ctx {
    core: AtomicPtr<NvCore>,
    json: Mutex<Vec<String>>,
}

/// The callbacks' context, never freed (see the other C interface tests).
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

/// Two channels recording all of the 8th, with a person on channel 0 and a
/// vehicle on channel 1 overlapping it.
fn mock(sim: &SimRuntime) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
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

    fn finished(&self, id: &str) {
        self.until("the download", 600, |ctx| {
            ctx.events("download")
                .iter()
                .any(|e| e["download"] == id && e["state"] == "done")
        });
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn export_through_the_c_interface() {
    let sim = SimRuntime::new();
    let nvr = mock(&sim);
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
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
    let day8 = d(8, 0, 0, 0).to_naive_secs();

    // SAFETY: valid pointers and strings throughout; the core is freed
    // once, at the end.
    unsafe {
        assert!(nv_abi_minor() >= 10);
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
        type Fun = unsafe extern "C" fn(
            *mut NvCore,
            *const c_char,
            *mut c_char,
            usize,
            *mut usize,
        ) -> NvStatus;
        // Calls that only read: size first, then fetch. Calls that queue a
        // download are made once, with room for the answer (the size probe
        // would queue it, and the second call would find it existing).
        let call = |f: Fun, json: &Value| {
            if f as usize == nv_download_range as Fun as usize
                || f as usize == nv_download_incident as Fun as usize
            {
                let mut out = vec![0 as c_char; 64 * 1024];
                let mut n = 0usize;
                let s = f(core, c(&json.to_string()).as_ptr(), out.as_mut_ptr(), out.len(), &mut n);
                if s != NvStatus::NV_OK {
                    return Err(s);
                }
                let text = CStr::from_ptr(out.as_ptr()).to_str().unwrap();
                return Ok(serde_json::from_str::<Value>(text).unwrap());
            }
            rig.answer(|o, n, l| f(core, c(&json.to_string()).as_ptr(), o, n, l))
        };

        // Before the configuration: NV_ERR_NOT_CONFIGURED.
        let range_req = |cams: &[u8], start: i64, end: i64| {
            json!({"start": start, "end": end,
                   "cameras": cams.iter().map(|&ch| json!({"camera": cam(ch), "tier": "main"}))
                       .collect::<Vec<_>>()})
        };
        let (r0, r1) = (day8 + 10 * 3600 + 5, day8 + 10 * 3600 + 25);
        assert_eq!(
            call(nv_download_range, &range_req(&[0], r0, r1)),
            Err(NvStatus::NV_ERR_NOT_CONFIGURED)
        );
        let downloads = dir.path().join("downloads");
        let cfg = c(&json!({"downloads_dir": downloads.to_str().unwrap()}).to_string());
        assert_eq!(nv_recordings_configure(core, cfg.as_ptr()), NvStatus::NV_OK);
        // A device not enabled for recordings: a camera that is not allowed.
        assert_eq!(
            call(nv_download_range, &range_req(&[0], r0, r1)),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        let enable = c(&json!({"channels": [{"channel": 0}, {"channel": 1}],
                               "utc_offset_s": OFFSET_S})
        .to_string());
        assert_eq!(
            nv_recordings_enable(core, c(&key).as_ptr(), enable.as_ptr()),
            NvStatus::NV_OK
        );

        // ---- An incident download, trimmed ------------------------------
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
        let incident = incidents["incidents"][0]["key"].as_str().unwrap().to_owned();

        // A destination that is not absolute is refused; one elsewhere
        // needs a data directory for the registry (this core has none).
        let queued = |extra: Value| {
            let mut r = json!({"incident": incident});
            r.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            call(nv_download_incident, &r)
        };
        assert_eq!(
            queued(json!({"dest": "relative/dir"})),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        assert_eq!(
            queued(json!({"dest": elsewhere.path().to_str().unwrap()})),
            Err(NvStatus::NV_ERR_NOT_CONFIGURED)
        );
        let queued = queued(json!({})).unwrap();
        let id = queued["download"].as_str().unwrap().to_owned();
        rig.finished(&id);
        let folder = queued["folder"].as_str().unwrap().to_owned();

        let (in_ms, out_ms) = (ms(8, 10, 0, 9), ms(8, 10, 0, 21));
        let trim = |extra: Value| {
            let mut r = json!({"kind": "trim", "download": id, "in_ms": in_ms, "out_ms": out_ms});
            r.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            call(nv_edit_compute, &r)
        };
        let plan = trim(json!({})).unwrap();
        assert_eq!((plan["kind"].as_str(), plan["version"].as_i64()), (Some("trim"), Some(1)));
        assert_eq!(plan["download"], id.as_str());
        let outs = plan["outputs"].as_array().unwrap();
        assert_eq!(outs.len(), 2, "{plan}");
        assert!(plan["start_ms"].as_i64().unwrap() <= in_ms);
        assert_eq!(plan["end_ms"], out_ms);
        for o in outs {
            assert_eq!(o["copy_possible"], true);
            assert!(o["source_in_ms"].as_i64().unwrap() < o["source_out_ms"].as_i64().unwrap());
            let file = o["suggested_file"].as_str().unwrap();
            assert!(file.starts_with(&folder) && file.ends_with(" trim.mp4"), "{file}");
        }
        // Exact cuts start at the point, and cannot be copied.
        let exact = trim(json!({"cut": "exact"})).unwrap();
        assert_eq!(exact["start_ms"], in_ms);
        assert_eq!(exact["outputs"][0]["copy_possible"], false);
        // A destination and share-safe names.
        let safe = trim(json!({"dest": elsewhere.path().to_str().unwrap(), "share_safe": true}))
            .unwrap();
        assert_eq!(safe["share_safe"], true);
        assert_eq!(safe["outputs"][0]["name"], "Camera A");
        assert_eq!(safe["outputs"][1]["name"], "Camera B");
        let file = safe["outputs"][0]["suggested_file"].as_str().unwrap();
        assert!(
            file.starts_with(elsewhere.path().to_str().unwrap()) && file.contains("Camera A"),
            "{file}"
        );
        // One camera only.
        let one = trim(json!({"cameras": [cam(1)]})).unwrap();
        assert_eq!(one["outputs"].as_array().unwrap().len(), 1);
        assert_eq!(one["outputs"][0]["camera"]["channel"], 1);
        // Status codes.
        assert_eq!(
            trim(json!({"in_ms": out_ms})),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        assert_eq!(
            trim(json!({"in_ms": ms(8, 11, 0, 0), "out_ms": ms(8, 11, 0, 5)})),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        assert_eq!(
            trim(json!({"dest": "relative"})),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        assert_eq!(
            call(
                nv_edit_compute,
                &json!({"kind": "trim", "download": "nope", "in_ms": 1, "out_ms": 2})
            ),
            Err(NvStatus::NV_ERR_UNKNOWN_ID)
        );

        // in_ms and out_ms on a follow cut and a grid.
        let full = call(nv_edit_compute, &json!({"kind": "follow", "download": id})).unwrap();
        let cut = call(
            nv_edit_compute,
            &json!({"kind": "follow", "download": id, "in_ms": in_ms, "out_ms": out_ms,
                    "dest": elsewhere.path().to_str().unwrap(), "share_safe": true}),
        )
        .unwrap();
        assert!(cut["start_ms"].as_i64().unwrap() >= full["start_ms"].as_i64().unwrap());
        assert!(cut["end_ms"].as_i64().unwrap() <= out_ms, "{cut}");
        assert!(cut["end_ms"].as_i64().unwrap() < full["end_ms"].as_i64().unwrap());
        assert_eq!(cut["sources"][0]["name"], "Camera A");
        assert!(
            cut["suggested_file"]
                .as_str()
                .unwrap()
                .starts_with(elsewhere.path().to_str().unwrap())
        );
        let grid = call(
            nv_edit_compute,
            &json!({"kind": "grid", "download": id, "in_ms": in_ms, "out_ms": out_ms}),
        )
        .unwrap();
        assert_eq!((grid["start_ms"].as_i64().unwrap(), grid["end_ms"].as_i64().unwrap()), (in_ms, out_ms));
        assert_eq!(
            call(
                nv_edit_compute,
                &json!({"kind": "grid", "download": id, "in_ms": out_ms, "out_ms": in_ms})
            ),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );

        // ---- A range download -------------------------------------------
        assert_eq!(
            call(nv_download_range, &range_req(&[5], r0, r1)),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        assert_eq!(
            call(nv_download_range, &range_req(&[0], r1, r0)),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        assert_eq!(
            call(nv_download_range, &range_req(&[0], r0, r0 + 25 * 3600)),
            Err(NvStatus::NV_ERR_LIMIT)
        );
        assert_eq!(
            call(nv_download_range, &range_req(&[], r0, r1)),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        let mut elsewhere_req = range_req(&[0], r0, r1);
        elsewhere_req["dest"] = json!(elsewhere.path().to_str().unwrap());
        assert_eq!(
            call(nv_download_range, &elsewhere_req),
            Err(NvStatus::NV_ERR_NOT_CONFIGURED)
        );
        let mut named = range_req(&[0, 1], r0, r1);
        named["name"] = json!("Walkway");
        let range = call(nv_download_range, &named).unwrap();
        assert_eq!(range["existing"], false);
        let rid = range["download"].as_str().unwrap().to_owned();
        assert!(rid.starts_with("r-"), "{rid}");
        let rfolder = range["folder"].as_str().unwrap().to_owned();
        assert!(rfolder.starts_with(downloads.to_str().unwrap()));
        rig.finished(&rid);
        let ev = rig
            .ctx
            .events("download")
            .into_iter()
            .rfind(|e| e["download"] == rid.as_str())
            .unwrap();
        assert!(ev["incident"].is_null(), "{ev}");
        assert_eq!(ev["range"], json!({"start": r0, "end": r1}));
        assert!(std::path::Path::new(&rfolder).join("incident.json").is_file());
        // The same request (the name is not part of it) is the same download.
        let again = call(nv_download_range, &range_req(&[1, 0], r0, r1)).unwrap();
        assert_eq!(again["existing"], true);
        assert_eq!(again["download"], rid.as_str());
        // A share-safe range of another span writes no incident.json.
        let mut safe_req = range_req(&[0], r0, r0 + 10);
        safe_req["share_safe"] = json!(true);
        let safe_range = call(nv_download_range, &safe_req).unwrap();
        let sfolder = safe_range["folder"].as_str().unwrap();
        assert!(!sfolder.contains("Walkway") && sfolder.contains("Camera A"), "{sfolder}");
        assert!(!std::path::Path::new(sfolder).join("incident.json").exists());

        // Trims work on a range download; edit lists need an incident.
        let rtrim = call(
            nv_edit_compute,
            &json!({"kind": "trim", "download": rid, "in_ms": (r0 + 5) * 1000,
                    "out_ms": (r0 + 15) * 1000, "cut": "exact"}),
        )
        .unwrap();
        assert_eq!(rtrim["outputs"].as_array().unwrap().len(), 2);
        assert_eq!(rtrim["start_ms"], (r0 + 5) * 1000);
        assert_eq!(
            call(nv_edit_compute, &json!({"kind": "follow", "download": rid})),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );

        // ---- Stills -----------------------------------------------------
        let at = (r0 + 8) * 1000 + 250;
        let still = call(
            nv_still_plan,
            &json!({"download": rid, "camera": cam(0), "at_ms": at, "exact": true,
                    "format": "jpeg"}),
        )
        .unwrap();
        let s = &still["stills"][0];
        assert_eq!(s["position_ms"], at);
        assert!(s["file"].as_str().unwrap().ends_with(".mp4"));
        assert!(s["suggested_file"].as_str().unwrap().ends_with(".jpg"));
        assert!(s["suggested_file"].as_str().unwrap().starts_with(&rfolder));
        let keyframe = call(
            nv_still_plan,
            &json!({"download": rid, "camera": cam(0), "at_ms": at, "exact": false,
                    "share_safe": true, "dest": elsewhere.path().to_str().unwrap()}),
        )
        .unwrap();
        let k = &keyframe["stills"][0];
        assert!(k["position_ms"].as_i64().unwrap() <= at);
        assert!(k["suggested_file"].as_str().unwrap().contains("Camera A"));
        assert!(k["suggested_file"].as_str().unwrap().ends_with(".png"));
        assert_eq!(
            call(
                nv_still_plan,
                &json!({"download": rid, "camera": cam(0), "at_ms": at + 3_600_000})
            ),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );
        assert_eq!(
            call(nv_still_plan, &json!({"download": "nope", "camera": cam(0), "at_ms": at})),
            Err(NvStatus::NV_ERR_UNKNOWN_ID)
        );
        assert_eq!(
            call(nv_still_plan, &json!({"playback": 12_345})),
            Err(NvStatus::NV_ERR_UNKNOWN_ID)
        );
        assert_eq!(call(nv_still_plan, &json!({})), Err(NvStatus::NV_ERR_INVALID_ARGUMENT));
        assert_eq!(
            call(nv_still_plan, &json!({"playback": 1, "download": rid})),
            Err(NvStatus::NV_ERR_INVALID_ARGUMENT)
        );

        // From a playback: the picture each tile shows, paused at p0.
        let p0 = ms(8, 10, 0, 30);
        let req = c(&json!({
            "start_ms": ms(8, 10, 0, 0), "end_ms": ms(8, 10, 10, 0), "position_ms": p0,
            "cameras": [{"camera": cam(0), "tier": "sub"}, {"camera": cam(1), "tier": "sub"}],
        })
        .to_string());
        let (mut pb, mut tiles, mut count) = (0u64, [0u64; 4], 0usize);
        assert_eq!(
            nv_playback_open(core, req.as_ptr(), &mut pb, tiles.as_mut_ptr(), 4, &mut count),
            NvStatus::NV_OK
        );
        assert_eq!(count, 2);
        rig.until("the opening seek", 20, |ctx| {
            ctx.events("playback_seek")
                .iter()
                .any(|e| e["playback"] == pb && e["state"] == "active" && e["generation"] == 1)
        });
        let stills = call(nv_still_plan, &json!({"playback": pb})).unwrap();
        let list = stills["stills"].as_array().unwrap();
        assert_eq!(list.len(), 2, "{stills}");
        for (n, s) in list.iter().enumerate() {
            assert_eq!(s["tile"], tiles[n]);
            assert_eq!(s["generation"], 1);
            let at = s["position_ms"].as_i64().unwrap();
            assert!(at >= p0 && at < p0 + 3_000, "{at}");
            let file = s["suggested_file"].as_str().unwrap();
            assert!(file.starts_with(downloads.to_str().unwrap()) && file.ends_with(".png"));
            assert!(s["label"]["name"].is_string());
        }
        let safe = call(
            nv_still_plan,
            &json!({"playback": pb, "tiles": [tiles[1]], "format": "jpeg", "share_safe": true,
                    "dest": elsewhere.path().to_str().unwrap()}),
        )
        .unwrap();
        let s = &safe["stills"][0];
        assert_eq!(s["tile"], tiles[1]);
        assert_eq!(s["name"], "Camera A");
        assert!(s["label"]["name"].is_null());
        let file = s["suggested_file"].as_str().unwrap();
        assert!(file.starts_with(elsewhere.path().to_str().unwrap()) && file.ends_with(".jpg"));
        assert_eq!(
            call(nv_still_plan, &json!({"playback": pb, "tiles": [999_999]})),
            Err(NvStatus::NV_ERR_UNKNOWN_ID)
        );
        assert_eq!(nv_playback_close(core, pb), NvStatus::NV_OK);

        // NULL arguments and a too-small buffer.
        assert_eq!(
            nv_download_range(core, ptr::null(), ptr::null_mut(), 0, &mut len),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_still_plan(ptr::null_mut(), c("{}").as_ptr(), ptr::null_mut(), 0, &mut len),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let mut small = [0 as c_char; 8];
        assert_eq!(
            nv_still_plan(
                core,
                c(&json!({"download": rid, "camera": cam(0), "at_ms": at}).to_string()).as_ptr(),
                small.as_mut_ptr(),
                small.len(),
                &mut len
            ),
            NvStatus::NV_ERR_BUFFER_TOO_SMALL
        );
        assert!(len > small.len());

        // Shutdown on the simulated clock.
        rig.sim.run_free(|| nv_core_free(core));
    }
}
