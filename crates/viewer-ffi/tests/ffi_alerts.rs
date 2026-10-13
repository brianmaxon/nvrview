//! Smart alerts (docs/FFI.md, "Smart alerts"; minor 12) called as C would
//! call them, against an in-process mock NVR on a simulated clock: a
//! trigger from the app, a verification request answered through
//! nv_alert_frames_get and nv_alert_detections_put, the alert in the log,
//! a snapshot, feedback, a mute, and the status codes.

// Calling the C interface from Rust needs raw pointers.
#![allow(unsafe_code)]

use nvrview::test_support::{TestRuntime, core_new_on, flush_events};
use nvrview::*;
use serde_json::{Value, json};
use simtime::SimRuntime;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Ctx {
    json: Mutex<Vec<String>>,
}

fn leaked<T: Default>() -> &'static T {
    Box::leak(Box::default())
}

unsafe extern "C" fn on_action(_context: *mut c_void, _tile: u64, _action: *const NvAction) {}

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
    fn events_of(&self, kind: &str) -> Vec<Value> {
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

#[test]
#[allow(clippy::too_many_lines)]
fn smart_alerts_through_the_c_interface() {
    let sim = SimRuntime::new();
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 1;
    let nvr = mock_nvr::spawn_in(cfg, sim.handle());
    let ctx: &'static Ctx = leaked();
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
    let until = |what: &str, cond: &mut dyn FnMut() -> bool| {
        // SAFETY: the core is live until the end of the test.
        let r = unsafe { &*core };
        sim.advance_until(
            what,
            Duration::from_secs(20),
            Duration::from_millis(10),
            || {
                flush_events(r);
                cond()
            },
        );
    };

    // SAFETY: valid pointers and strings throughout; the core is freed
    // once, at the end.
    unsafe {
        assert!(nv_abi_minor() >= 13);
        let (host, user, password) = (c("127.0.0.1"), c("example-user"), c("example-password"));
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
        let cam = json!({"device": dev, "channel": 0}).to_string();
        let trigger = c(
            &json!({"source": "device", "camera": {"device": dev, "channel": 0}, "kind": "people"})
                .to_string(),
        );
        let mut id = 0u64;

        // Before configuring: off.
        assert_eq!(
            nv_alerts_trigger_put(core, trigger.as_ptr(), &mut id),
            NvStatus::NV_ERR_NOT_CONFIGURED
        );
        assert_eq!(
            nv_alert_mute(core, c(&cam).as_ptr(), 60),
            NvStatus::NV_ERR_NOT_CONFIGURED
        );
        // Sun times need no configuration. 2024-06-21 12:00 UTC at a
        // made-up place near the equator (UTC-3): the sun rises about
        // 08:00 UTC and sets about 20:00 UTC; the margins move the night.
        let sun = |req: Value| -> (NvStatus, Value) {
            let mut buf = vec![0u8; 1024];
            let mut len = 0usize;
            let s = nv_alerts_sun_times(
                core,
                c(&req.to_string()).as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut len,
            );
            let v = if s == NvStatus::NV_OK {
                serde_json::from_slice(&buf[..len]).unwrap()
            } else {
                Value::Null
            };
            (s, v)
        };
        let noon = 1_718_971_200_000i64;
        let (s, v) = sun(json!({"lat": 12.3, "lon": -45.6, "at_ms": noon,
            "utc_offset_min": -180, "after_sunset_min": 30, "before_sunrise_min": 20}));
        assert_eq!(s, NvStatus::NV_OK);
        assert_eq!(v["date"], "2024-06-21");
        assert_eq!(v["kind"], "normal");
        let ms = |k: &str| v[k].as_i64().unwrap();
        let min = 60_000i64;
        // Longitude -45.6: solar noon at 15:02 UTC, day length about 12.9 h.
        assert!((ms("sunrise_ms") - (noon + (8 * 60 + 36 - 12 * 60) * min)).abs() <= 3 * min);
        assert!((ms("sunset_ms") - (noon + (21 * 60 + 29 - 12 * 60) * min)).abs() <= 3 * min);
        assert_eq!(ms("night_start_ms"), ms("sunset_ms") + 30 * min);
        assert!(ms("night_end_ms") > ms("night_start_ms"));
        // Polar day and night, with no times; a location out of range.
        let (s, v) = sun(json!({"lat": 80.1, "lon": 10.2, "at_ms": noon}));
        assert_eq!(
            (s, v["kind"].as_str()),
            (NvStatus::NV_OK, Some("polar_day"))
        );
        assert!(v["sunrise_ms"].is_null() && v["night_start_ms"].is_null());
        let winter = 1_734_782_400_000i64; // 2024-12-21 12:00 UTC
        let (_, v) = sun(json!({"lat": 80.1, "lon": 10.2, "at_ms": winter}));
        assert_eq!(v["kind"], "polar_night");
        for bad in [
            json!({"lat": 91.0, "lon": 0.0, "at_ms": noon}),
            json!({"lat": 0.0, "lon": 181.0, "at_ms": noon}),
            json!({"lat": 0.0, "lon": 0.0}),
            json!({"lat": "x", "lon": 0.0, "at_ms": noon}),
        ] {
            assert_eq!(
                sun(bad.clone()).0,
                NvStatus::NV_ERR_INVALID_ARGUMENT,
                "{bad}"
            );
        }
        for bad in ["{", "[]", r#"{"enabled":"yes"}"#] {
            assert_eq!(
                nv_alerts_configure(core, c(bad).as_ptr()),
                NvStatus::NV_ERR_INVALID_ARGUMENT,
                "{bad}"
            );
        }
        assert_eq!(
            nv_alerts_configure(core, ptr::null()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let settings = json!({"cameras": [{"camera": {"device": dev, "channel": 0}, "exposure": "outside"}],
                              "utc_offset_min": 0, "acquisition": {"recheck": false}});
        assert_eq!(
            nv_alerts_configure(core, c(&settings.to_string()).as_ptr()),
            NvStatus::NV_OK
        );
        until("connected and configured", &mut || {
            !ctx.events_of("alerts_configured").is_empty()
                && ctx
                    .events_of("device_state")
                    .iter()
                    .any(|e| e["state"] == "connected")
        });

        assert_eq!(
            nv_alerts_trigger_put(core, trigger.as_ptr(), &mut id),
            NvStatus::NV_OK
        );
        assert!(id > 0);
        assert_eq!(
            nv_alerts_trigger_put(core, c(r#"{"source":"device"}"#).as_ptr(), ptr::null_mut()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );

        // Answer every request: frames first (too small, then whole).
        let mut answered = std::collections::HashSet::new();
        until("a decision", &mut || {
            for r in ctx.events_of("verify_request") {
                let key = r["request"].as_str().unwrap().to_owned();
                if !answered.insert(key.clone()) {
                    continue;
                }
                let k = c(&key);
                let mut info = NvThumbnail {
                    kind: 0,
                    width: 0,
                    height: 0,
                    pts_us: 0,
                };
                let mut len = 0usize;
                assert_eq!(
                    nv_alert_frames_get(core, k.as_ptr(), &mut info, ptr::null_mut(), 0, &mut len),
                    NvStatus::NV_ERR_BUFFER_TOO_SMALL
                );
                assert_eq!(len, r["bytes"].as_u64().unwrap() as usize);
                let mut buf = vec![0u8; len];
                assert_eq!(
                    nv_alert_frames_get(
                        core,
                        k.as_ptr(),
                        &mut info,
                        buf.as_mut_ptr(),
                        buf.len(),
                        &mut len
                    ),
                    NvStatus::NV_OK
                );
                assert_eq!(info.kind, NvThumbKind::NV_THUMB_H264_KEYFRAME as u32);
                assert_eq!(info.pts_us as u64, r["pts_us"].as_u64().unwrap());
                assert!(buf.starts_with(&[0, 0, 0, 1]) || buf.starts_with(&[0, 0, 1]));
                assert_eq!(
                    nv_alert_detections_put(core, k.as_ptr(), c("{").as_ptr()),
                    NvStatus::NV_ERR_INVALID_ARGUMENT
                );
                let boxes = json!({"detections": [{"class": "person", "score": 0.9,
                                   "box": {"x": 0.4, "y": 0.2, "w": 0.2, "h": 0.5}}]});
                assert_eq!(
                    nv_alert_detections_put(core, k.as_ptr(), c(&boxes.to_string()).as_ptr()),
                    NvStatus::NV_OK
                );
                // Answered: gone.
                assert_eq!(
                    nv_alert_detections_put(core, k.as_ptr(), c(&boxes.to_string()).as_ptr()),
                    NvStatus::NV_ERR_UNKNOWN_ID
                );
                assert_eq!(
                    nv_alert_frames_get(
                        core,
                        k.as_ptr(),
                        &mut info,
                        buf.as_mut_ptr(),
                        buf.len(),
                        &mut len
                    ),
                    NvStatus::NV_ERR_UNKNOWN_ID
                );
            }
            ctx.events_of("alert")
                .iter()
                .any(|e| e["decision"]["trigger_id"] == id)
        });
        let alert = ctx
            .events_of("alert")
            .into_iter()
            .find(|e| e["decision"]["trigger_id"] == id)
            .unwrap();
        assert_eq!(alert["state"], "new");
        assert_eq!(alert["decision"]["decision"], "alert");
        let alert_id = alert["decision"]["alert_id"].as_u64().unwrap();

        // Snapshot round trip.
        let jpeg = b"\xff\xd8example\xff\xd9";
        assert_eq!(
            nv_alert_snapshot_put(core, alert_id, jpeg.as_ptr(), jpeg.len()),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_alert_snapshot_put(core, alert_id, ptr::null(), 0),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_alert_snapshot_put(core, 999, jpeg.as_ptr(), jpeg.len()),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        let mut len = 0usize;
        assert_eq!(
            nv_alert_snapshot_get(core, alert_id, ptr::null_mut(), 0, &mut len),
            NvStatus::NV_ERR_BUFFER_TOO_SMALL
        );
        assert_eq!(len, jpeg.len());
        let mut out = vec![0u8; len];
        assert_eq!(
            nv_alert_snapshot_get(core, alert_id, out.as_mut_ptr(), out.len(), &mut len),
            NvStatus::NV_OK
        );
        assert_eq!(&out, jpeg);

        // Feedback and mute.
        let fb = c(
            &json!({"alert_id": alert_id, "vote": "down", "reason": "nothing_there"}).to_string(),
        );
        assert_eq!(nv_alert_feedback(core, fb.as_ptr()), NvStatus::NV_OK);
        assert_eq!(
            nv_alert_feedback(core, c(r#"{"alert_id":999,"vote":"up"}"#).as_ptr()),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(nv_alert_mute(core, c(&cam).as_ptr(), 600), NvStatus::NV_OK);

        // The log.
        let mut out = vec![0 as c_char; 256 * 1024];
        let mut len = 0usize;
        assert_eq!(
            nv_alerts_get(core, ptr::null(), out.as_mut_ptr(), out.len(), &mut len),
            NvStatus::NV_OK
        );
        let log: Value =
            serde_json::from_str(CStr::from_ptr(out.as_ptr()).to_str().unwrap()).unwrap();
        let e = log["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["decision"]["alert_id"] == alert_id)
            .unwrap();
        assert_eq!(e["snapshot"], true);
        assert_eq!(e["feedback"]["vote"], "down");
        assert_eq!(log["status"]["muted"].as_array().unwrap().len(), 1);
        assert_eq!(
            nv_alerts_get(core, c("[").as_ptr(), out.as_mut_ptr(), out.len(), &mut len),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let mut tiny = [0 as c_char; 4];
        assert_eq!(
            nv_alerts_get(core, ptr::null(), tiny.as_mut_ptr(), tiny.len(), &mut len),
            NvStatus::NV_ERR_BUFFER_TOO_SMALL
        );

        sim.run_free(|| nv_core_free(core));
    }
}
