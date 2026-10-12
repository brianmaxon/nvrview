//! The recordings functions (minor 5; docs/FFI-RECORDINGS.md, sections 1
//! to 5 and 8) called as C would call them, against an in-process mock NVR
//! with a recording store: status codes, the out-buffer convention, JSON in
//! and out, thumbnails from pending to ready, a download, a lock, the
//! Trash, and the events.

// Calling the C interface from Rust needs raw pointers.
#![allow(unsafe_code)]

use bc_protocol::archive::DateTime;
use mock_nvr::archive::{ArchiveConfig, MockEvent};
use nvrview::*;
use serde_json::Value;
use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct Ctx {
    json: Mutex<Vec<String>>,
}

/// The callbacks' context, never freed. The core may call back until
/// `nv_core_free` returns, and a failing assertion unwinds past that call:
/// a context dropped then would be used after free by the core's threads.
fn leaked_ctx() -> &'static Ctx {
    Box::leak(Box::default())
}

unsafe extern "C" fn on_action(_: *mut c_void, _: u64, _: *const NvAction) {}

unsafe extern "C" fn on_event(context: *mut c_void, json: *const c_char, len: usize) {
    // SAFETY: the test passes a pointer to a Ctx that outlives the core, and
    // `json` is valid for `len` bytes during the call.
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

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !cond() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The start of yesterday (UTC; the device's offset is 0 here), so no
/// incident in it is still growing.
fn yesterday() -> i64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    (now / 86_400 - 1) * 86_400
}

fn at(secs: i64) -> DateTime {
    DateTime::from_naive_secs(secs).unwrap()
}

/// Two channels recorded all of yesterday: a person on channel 0 at 10:00
/// and a vehicle on channel 1 at 14:00.
fn mock(day: i64) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = 2;
    let mut a = ArchiveConfig::new(vec![at(day)]);
    a.events_per_day = 0;
    a.events = vec![
        MockEvent {
            channel: 0,
            kind: "people".into(),
            start: at(day + 10 * 3600),
            end: at(day + 10 * 3600 + 15),
        },
        MockEvent {
            channel: 1,
            kind: "vehicle".into(),
            start: at(day + 14 * 3600),
            end: at(day + 14 * 3600 + 20),
        },
    ];
    cfg.archive = Some(a);
    mock_nvr::spawn(cfg).unwrap()
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

/// Calls an out-buffer function the way the bindings do: ask for the size,
/// then call with a buffer one larger, looping while the answer grows.
fn answer(
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

/// Calls a function that changes state once, with a buffer large enough
/// for its answer.
fn once(f: impl FnOnce(*mut c_char, usize, *mut usize) -> NvStatus) -> Result<Value, NvStatus> {
    let mut buf = vec![0 as c_char; 64 * 1024];
    let mut len = 0usize;
    match f(buf.as_mut_ptr(), buf.len(), &mut len) {
        NvStatus::NV_OK => {
            // SAFETY: the core wrote a NUL-terminated string.
            let text = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_str().unwrap();
            Ok(serde_json::from_str(text).unwrap())
        }
        s => Err(s),
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn recordings_through_the_c_interface() {
    let day = yesterday();
    let nvr = mock(day);
    let dir = tempfile::tempdir().unwrap();
    let data_dir = c(dir.path().join("data").to_str().unwrap());
    let downloads = dir.path().join("downloads");
    let config = NvConfig {
        log_dir: ptr::null(),
        log_max_files: 0,
        log_max_total_bytes: 0,
        data_dir: data_dir.as_ptr(),
    };
    let ctx = leaked_ctx();
    let callbacks = NvCallbacks {
        context: ctx as *const Ctx as *mut c_void,
        on_action: Some(on_action),
        on_event: Some(on_event),
        on_audio_tap: None,
    };
    let mut core: *mut NvCore = ptr::null_mut();
    // SAFETY: valid config, callbacks, out pointers and strings throughout;
    // the core is live until freed once at the end.
    unsafe {
        assert_eq!(nv_core_new(&config, callbacks, &mut core), NvStatus::NV_OK);
        let host = c(&nvr.addr().ip().to_string());
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
        let ckey = c(&key);
        let enable = c(r#"{"channels":[{"channel":0},{"channel":1}]}"#);

        // Before the configuration: NV_ERR_NOT_CONFIGURED.
        assert_eq!(
            nv_recordings_enable(core, ckey.as_ptr(), enable.as_ptr()),
            NvStatus::NV_ERR_NOT_CONFIGURED
        );
        assert_eq!(
            answer(|o, n, l| nv_recordings_status(core, o, n, l)),
            Err(NvStatus::NV_ERR_NOT_CONFIGURED)
        );
        // Invalid arguments.
        assert_eq!(
            nv_recordings_configure(core, ptr::null()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let cfg = c(&format!(
            r#"{{"downloads_dir":{}}}"#,
            Value::from(downloads.to_str().unwrap())
        ));
        assert_eq!(
            nv_recordings_configure(ptr::null_mut(), cfg.as_ptr()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_recordings_configure(core, c("{").as_ptr()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let bad_utf8 = [0xffu8 as c_char, 0];
        assert_eq!(
            nv_recordings_configure(core, bad_utf8.as_ptr()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(nv_recordings_configure(core, cfg.as_ptr()), NvStatus::NV_OK);
        assert_eq!(
            nv_recordings_enable(core, ckey.as_ptr(), enable.as_ptr()),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_sound_configure(core, c(r#"{"alarms":true}"#).as_ptr()),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_sound_configure(core, c(r#"{"loudness_history_s":-1}"#).as_ptr()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );

        // The device reports what it can do with recordings.
        wait_until("device_info", || !ctx.events("device_info").is_empty());
        assert_eq!(
            ctx.events("device_info")[0]["recordings"],
            serde_json::json!({"search": true, "playback": true, "download": true, "thumbnails": true})
        );

        // Asking for a day: limits and allowed cameras first.
        let cams =
            format!(r#"[{{"device":"{key}","channel":0}},{{"device":"{key}","channel":1}}]"#);
        let many: Vec<String> = (0..32).map(|i| (day - i * 86_400).to_string()).collect();
        assert_eq!(
            nv_recordings_want(
                core,
                c(&format!(
                    r#"{{"cameras":{cams},"days":[{}]}}"#,
                    many.join(",")
                ))
                .as_ptr()
            ),
            NvStatus::NV_ERR_LIMIT
        );
        assert_eq!(
            nv_recordings_want(
                core,
                c(&format!(
                    r#"{{"cameras":[{{"device":"{key}","channel":5}}],"days":[{day}]}}"#
                ))
                .as_ptr()
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let want = c(&format!(r#"{{"cameras":{cams},"days":[{day}]}}"#));
        assert_eq!(nv_recordings_want(core, want.as_ptr()), NvStatus::NV_OK);
        let query = c(&format!(r#"{{"start":{day},"end":{}}}"#, day + 86_399));
        wait_until("both events indexed", || {
            answer(|o, n, l| nv_recordings_events(core, query.as_ptr(), o, n, l))
                .is_ok_and(|v| v["events"].as_array().unwrap().len() == 2)
        });
        wait_until("recordings_changed", || {
            ctx.events("recordings_changed")
                .iter()
                .any(|e| e["device"] == key.as_str())
        });
        assert!(
            ctx.events("recordings_index")
                .iter()
                .any(|e| e["device"] == key.as_str())
        );
        let events = answer(|o, n, l| nv_recordings_events(core, query.as_ptr(), o, n, l)).unwrap();
        assert_eq!(events["events"][0]["kind"], "people");
        assert_eq!(events["events"][0]["source"], "device");
        assert_eq!(events["truncated"], false);

        // The buffer convention: too small reports the size, then it fits.
        let mut small = [0 as c_char; 8];
        assert_eq!(
            nv_recordings_calendar(
                core,
                query.as_ptr(),
                small.as_mut_ptr(),
                small.len(),
                &mut len
            ),
            NvStatus::NV_ERR_BUFFER_TOO_SMALL
        );
        assert!(len > small.len());
        let calendar =
            answer(|o, n, l| nv_recordings_calendar(core, query.as_ptr(), o, n, l)).unwrap();
        assert_eq!(calendar["days"].as_array().unwrap().len(), 2);
        assert_eq!(
            nv_recordings_calendar(
                core,
                query.as_ptr(),
                small.as_mut_ptr(),
                small.len(),
                ptr::null_mut()
            ),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        let spans = answer(|o, n, l| nv_recordings_spans(core, query.as_ptr(), o, n, l)).unwrap();
        assert!(!spans["spans"][0]["spans"].as_array().unwrap().is_empty());
        let status = answer(|o, n, l| nv_recordings_status(core, o, n, l)).unwrap();
        assert_eq!(status["devices"][0]["device"], key.as_str());

        // Today stays fresh while held; hold 0 is refused.
        let hold = c(&format!(r#"{{"cameras":{cams}}}"#));
        assert_eq!(
            nv_recordings_hold_today(core, 7, hold.as_ptr()),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_recordings_hold_today(core, 0, hold.as_ptr()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_recordings_hold_today(core, 7, ptr::null()),
            NvStatus::NV_OK
        );

        // Incidents, one by key, and an edit.
        let incidents = answer(|o, n, l| nv_incidents_get(core, query.as_ptr(), o, n, l)).unwrap();
        let list = incidents["incidents"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        // Newest first: the vehicle at 14:00, then the person at 10:00.
        let vehicle = list[0]["key"].as_str().unwrap().to_owned();
        let person = list[1]["key"].as_str().unwrap().to_owned();
        assert_eq!(list[1]["kinds"], serde_json::json!(["people"]));
        let cperson = c(&person);
        let one = answer(|o, n, l| nv_incident_get(core, cperson.as_ptr(), o, n, l)).unwrap();
        assert_eq!(one["incident"]["key"], person.as_str());
        assert_eq!(
            answer(|o, n, l| nv_incident_get(core, c("i-unknown").as_ptr(), o, n, l)),
            Err(NvStatus::NV_ERR_UNKNOWN_ID)
        );
        let rename = c(&format!(
            r#"{{"op":"rename","key":"{person}","name":"Delivery"}}"#
        ));
        let edited = once(|o, n, l| nv_incident_edit(core, rename.as_ptr(), o, n, l)).unwrap();
        assert_eq!(edited["keys"], serde_json::json!([person.as_str()]));
        assert_eq!(
            once(|o, n, l| nv_incident_edit(
                core,
                c(r#"{"op":"rename","key":"i-unknown","name":"x"}"#).as_ptr(),
                o,
                n,
                l
            )),
            Err(NvStatus::NV_ERR_UNKNOWN_ID)
        );
        wait_until("incidents_changed", || {
            !ctx.events("incidents_changed").is_empty()
        });

        // Camera groups.
        let groups = c(&format!(
            r#"{{"groups":[],"camera_names":[{{"camera":{{"device":"{key}","channel":0}},"name":"Front"}}]}}"#
        ));
        assert_eq!(nv_camera_groups_put(core, groups.as_ptr()), NvStatus::NV_OK);
        let got = answer(|o, n, l| nv_camera_groups_get(core, o, n, l)).unwrap();
        assert_eq!(got["camera_names"][0]["name"], "Front");
        wait_until("camera_groups_changed", || {
            !ctx.events("camera_groups_changed").is_empty()
        });

        // A thumbnail: pending, then ready after its event.
        let thumb_key = c(&format!(
            r#"{{"camera":{{"device":"{key}","channel":0}},"tier":"sub","start":{},"end":{}}}"#,
            day + 10 * 3600,
            day + 10 * 3600 + 15
        ));
        let mut info = NvThumbnail {
            kind: 0,
            width: 0,
            height: 0,
            pts_us: 0,
        };
        assert_eq!(
            nv_thumbnail_get(
                core,
                thumb_key.as_ptr(),
                &mut info,
                ptr::null_mut(),
                0,
                &mut len
            ),
            NvStatus::NV_PENDING
        );
        assert_eq!(len, 0);
        wait_until("thumbnail ready", || {
            ctx.events("thumbnail")
                .iter()
                .any(|e| e["state"] == "ready")
        });
        assert_eq!(
            nv_thumbnail_get(
                core,
                thumb_key.as_ptr(),
                &mut info,
                ptr::null_mut(),
                0,
                &mut len
            ),
            NvStatus::NV_ERR_BUFFER_TOO_SMALL
        );
        assert!(len > 4);
        assert_eq!(info.kind, NvThumbKind::NV_THUMB_H264_KEYFRAME as u32);
        let mut bytes = vec![0u8; len];
        assert_eq!(
            nv_thumbnail_get(
                core,
                thumb_key.as_ptr(),
                &mut info,
                bytes.as_mut_ptr(),
                bytes.len(),
                &mut len
            ),
            NvStatus::NV_OK
        );
        assert!(bytes.starts_with(&[0, 0, 0, 1]) || bytes.starts_with(&[0, 0, 1]));
        let image = NvThumbnail {
            kind: NvThumbKind::NV_THUMB_IMAGE as u32,
            width: 64,
            height: 36,
            pts_us: 0,
        };
        let jpeg = [0xffu8, 0xd8, 0xff, 0xd9];
        assert_eq!(
            nv_thumbnail_put(core, thumb_key.as_ptr(), &image, jpeg.as_ptr(), jpeg.len()),
            NvStatus::NV_OK
        );
        let mut back = [0u8; 16];
        assert_eq!(
            nv_thumbnail_get(
                core,
                thumb_key.as_ptr(),
                &mut info,
                back.as_mut_ptr(),
                back.len(),
                &mut len
            ),
            NvStatus::NV_OK
        );
        assert_eq!((info.kind, info.width, &back[..len]), (3, 64, &jpeg[..]));
        let bad = NvThumbnail { kind: 9, ..image };
        assert_eq!(
            nv_thumbnail_put(core, thumb_key.as_ptr(), &bad, jpeg.as_ptr(), jpeg.len()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(
            nv_thumbnail_put(core, thumb_key.as_ptr(), &image, ptr::null(), 4),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(nv_thumbnail_cancel(core, ptr::null()), NvStatus::NV_OK);
        assert_eq!(
            nv_thumbnail_cancel(core, thumb_key.as_ptr()),
            NvStatus::NV_OK
        );

        // A download, to the end.
        let request = c(&format!(r#"{{"incident":"{person}"}}"#));
        let queued = once(|o, n, l| nv_download_incident(core, request.as_ptr(), o, n, l)).unwrap();
        let id = queued["download"].as_str().unwrap().to_owned();
        let folder = PathBuf::from(queued["folder"].as_str().unwrap());
        assert_eq!(queued["existing"], false);
        assert!(folder.starts_with(&downloads));
        wait_until("download done", || {
            ctx.events("download")
                .iter()
                .any(|e| e["download"] == id.as_str() && e["state"] == "done")
        });
        let all = answer(|o, n, l| nv_downloads_get(core, o, n, l)).unwrap();
        let mine = &all["downloads"].as_array().unwrap()[0];
        assert_eq!(mine["download"], id.as_str());
        let output = PathBuf::from(mine["cameras"][0]["output"].as_str().unwrap());
        assert!(output.starts_with(&folder) && output.exists(), "{output:?}");

        // Locks: the downloaded incident's download is returned; the other
        // one is queued; unlocking answers the keys.
        let locked =
            once(|o, n, l| nv_incident_lock(core, cperson.as_ptr(), true, o, n, l)).unwrap();
        assert_eq!(locked["download_status"], "existing");
        assert_eq!(locked["download"], id.as_str());
        let cvehicle = c(&vehicle);
        let second =
            once(|o, n, l| nv_incident_lock(core, cvehicle.as_ptr(), true, o, n, l)).unwrap();
        assert_eq!(second["download_status"], "queued");
        let unlocked =
            once(|o, n, l| nv_incident_lock(core, cvehicle.as_ptr(), false, o, n, l)).unwrap();
        assert_eq!(unlocked["keys"], serde_json::json!([vehicle.as_str()]));
        assert!(unlocked.get("download").is_none());
        assert_eq!(
            once(|o, n, l| nv_incident_lock(core, c("i-unknown").as_ptr(), true, o, n, l)),
            Err(NvStatus::NV_ERR_UNKNOWN_ID)
        );
        let after = answer(|o, n, l| nv_incident_get(core, cperson.as_ptr(), o, n, l)).unwrap();
        assert_eq!(after["incident"]["locked"], true);

        // Download control on unknown IDs and cameras.
        let unknown = c("no-such-download");
        assert_eq!(
            nv_download_pause(core, unknown.as_ptr(), ptr::null(), true),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(
            nv_download_retry(core, unknown.as_ptr(), ptr::null()),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(
            nv_download_forget(core, unknown.as_ptr(), false),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        let cid = c(&id);
        assert_eq!(
            nv_download_pause(
                core,
                cid.as_ptr(),
                c(&format!(r#"{{"device":"{key}","channel":1}}"#)).as_ptr(),
                true
            ),
            NvStatus::NV_ERR_UNKNOWN_ID
        );
        assert_eq!(
            nv_download_retry(core, cid.as_ptr(), ptr::null()),
            NvStatus::NV_OK
        );

        // The Trash: the folder comes back, then download_removed.
        let trashed = once(|o, n, l| nv_download_trash(core, cid.as_ptr(), o, n, l)).unwrap();
        assert_eq!(PathBuf::from(trashed["folder"].as_str().unwrap()), folder);
        wait_until("download_removed", || {
            ctx.events("download_removed")
                .iter()
                .any(|e| e["download"] == id.as_str() && e["trash"] == true)
        });
        assert_eq!(
            once(|o, n, l| nv_download_trash(core, cid.as_ptr(), o, n, l)),
            Err(NvStatus::NV_ERR_UNKNOWN_ID)
        );
        // The core deleted nothing.
        assert!(output.exists());

        // Stopping the device's indexing; enabling again works.
        assert_eq!(
            nv_recordings_enable(core, ckey.as_ptr(), ptr::null()),
            NvStatus::NV_OK
        );
        assert_eq!(
            nv_recordings_want(core, want.as_ptr()),
            NvStatus::NV_ERR_INVALID_ARGUMENT
        );
        assert_eq!(nv_device_remove(core, dev), NvStatus::NV_OK);
        nv_core_free(core);
    }
    assert!(
        dir.path()
            .join("data/recordings/incident-edits.json")
            .exists()
    );
    assert!(
        dir.path()
            .join("data/recordings/incident-locks.json")
            .exists()
    );
}
