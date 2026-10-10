//! Two devices at once, detection events, device keys, and layouts, against
//! two in-process mock NVRs.

mod common;

use common::*;
use std::path::PathBuf;
use viewer_core::{CoreConfig, CoreError, StreamTier};

fn mock_with_ai(channels: u8, aitypes: Vec<u32>) -> mock_nvr::MockNvr {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/h264-basic.h264");
    let mut cfg = mock_nvr::Config::new(mock_nvr::fixture::Fixture::load(&path).unwrap());
    cfg.channels = channels;
    cfg.aitypes = aitypes;
    mock_nvr::spawn(cfg).unwrap()
}

fn add(core: &viewer_core::Core, nvr: &mock_nvr::MockNvr) -> u64 {
    let addr = nvr.addr();
    core.add_device(&addr.ip().to_string(), addr.port(), USER, PASSWORD)
        .unwrap()
}

fn of_device(rec: &Recorder, kind: &str, dev: u64) -> Vec<serde_json::Value> {
    rec.events_of(kind)
        .into_iter()
        .filter(|e| e["device"] == dev)
        .collect()
}

#[test]
fn two_devices_each_with_one_session_detections_and_closing() {
    let a_nvr = mock_with_ai(2, vec![7, 0]);
    let b_nvr = mock_with_ai(3, vec![0, 0, 1 | (1 << 1) | (1 << 4)]);
    let (core, rec) = start(fast_config());
    let a = add(&core, &a_nvr);
    let b = add(&core, &b_nvr);
    let tiles = [
        core.open_tile(a, 0, StreamTier::Sub).unwrap(),
        core.open_tile(a, 1, StreamTier::Sub).unwrap(),
        core.open_tile(b, 2, StreamTier::Sub).unwrap(),
        core.open_tile(b, 2, StreamTier::Main).unwrap(),
    ];
    for t in tiles {
        wait_until("pictures on every tile", 10, || rec.decodes(t) >= 5);
    }
    assert_eq!(a_nvr.active_logins(), 1);
    assert_eq!(b_nvr.active_logins(), 1);

    // Capabilities in the channel list.
    wait_until("detects", 10, || {
        of_device(&rec, "channels", b)
            .iter()
            .any(|e| e["channels"][2].get("detects").is_some())
            && of_device(&rec, "channels", a)
                .iter()
                .any(|e| e["channels"][0].get("detects").is_some())
    });
    let last = |dev| of_device(&rec, "channels", dev).pop().unwrap();
    assert_eq!(
        last(a)["channels"][0]["detects"],
        serde_json::json!(["people", "vehicle"])
    );
    assert_eq!(last(a)["channels"][1]["detects"], serde_json::json!([]));
    assert_eq!(
        last(b)["channels"][2]["detects"],
        serde_json::json!(["people", "dog_cat"])
    );

    // Detections, each named by its own device.
    wait_until("subscribed", 10, || {
        a_nvr.alarm_subscribers() == 1 && b_nvr.alarm_subscribers() == 1
    });
    b_nvr.set_alarm(2, "MD", "");
    b_nvr.set_alarm(2, "MD", "dog_cat");
    a_nvr.set_alarm(0, "", "people");
    wait_until("detection starts", 5, || {
        of_device(&rec, "detection", a).len() == 1 && of_device(&rec, "detection", b).len() == 1
    });
    let d = &of_device(&rec, "detection", b)[0];
    assert_eq!(
        (&d["channel"], &d["kind"], &d["state"], &d["source"]),
        (
            &2.into(),
            &"dog_cat".into(),
            &"start".into(),
            &"device".into()
        )
    );
    assert!(d["at_ns"].as_u64().unwrap() > 0);
    let m = &of_device(&rec, "motion", b)[0];
    assert_eq!((&m["channel"], &m["state"]), (&2.into(), &"start".into()));
    assert!(of_device(&rec, "motion", a).is_empty());
    b_nvr.set_alarm(2, "", "");
    wait_until("detection stop", 5, || {
        of_device(&rec, "detection", b).len() == 2 && of_device(&rec, "motion", b).len() == 2
    });
    assert_eq!(of_device(&rec, "detection", b)[1]["state"], "stop");

    // Removing a device stops what was active on it, closes its tiles and
    // its session, and leaves the other device alone.
    core.remove_device(a).unwrap();
    wait_until("device a logged out", 5, || a_nvr.active_logins() == 0);
    let stops: Vec<_> = of_device(&rec, "detection", a);
    assert_eq!(stops.len(), 2, "{stops:?}");
    assert_eq!(stops[1]["state"], "stop");
    assert_eq!(b_nvr.active_logins(), 1);
    let before = rec.decodes(tiles[2]);
    wait_until("device b still plays", 5, || {
        rec.decodes(tiles[2]) > before + 3
    });
    wait_until("tile_closed", 5, || rec.events_of("tile_closed").len() == 2);
    assert_eq!(
        rec.check_closed_once(&tiles[..2]),
        ["device_removed", "device_removed"]
    );
    core.shutdown();
    wait_until("device b logged out", 5, || b_nvr.active_logins() == 0);
    assert_eq!(
        rec.check_closed_once(&tiles[2..]),
        ["core_shutdown", "core_shutdown"]
    );
}

#[test]
fn device_keys_and_layouts_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let nvr = mock(1);
    let config = || CoreConfig {
        data_dir: Some(dir.path().to_owned()),
        ..fast_config()
    };
    let (core, rec) = start(config());
    let dev = add(&core, &nvr);
    let key = core.device_key(dev).unwrap();
    assert!(key.starts_with("d-"));
    assert_eq!(core.device_key(dev + 100), Err(CoreError::UnknownId));
    wait_until("device_info", 10, || {
        !rec.events_of("device_info").is_empty()
    });
    assert_eq!(rec.events_of("device_info")[0]["key"], key.as_str());

    let layout = format!(
        r#"{{"id":"entrances","name":"Entrances","root":{{"split":"row","ratio":0.62,
            "a":{{"camera":{{"device":"{key}","channel":0}}}},"b":{{"camera":null}}}}}}"#
    );
    assert_eq!(core.put_layout(&layout), Ok(1));
    assert_eq!(core.put_layout(&layout), Ok(2));
    assert_eq!(core.put_layout("{}"), Err(CoreError::InvalidArgument));
    assert_eq!(core.delete_layout("nope"), Err(CoreError::UnknownId));
    wait_until("layouts_changed", 5, || {
        rec.events_of("layouts_changed").len() == 2
    });
    assert_eq!(rec.events_of("layouts_changed")[1]["revision"], 2);
    core.shutdown();
    drop(core);

    let (core, _rec) = start(config());
    let again = add(&core, &nvr);
    assert_eq!(
        core.device_key(again).unwrap(),
        key,
        "same address, same key"
    );
    let v: serde_json::Value = serde_json::from_str(&core.layouts_json()).unwrap();
    assert_eq!(v["revision"], 2);
    assert_eq!(v["layouts"][0]["version"], 2);
    assert_eq!(
        v["layouts"][0]["root"]["a"]["camera"]["device"],
        key.as_str()
    );
    assert_eq!(core.delete_layout("entrances"), Ok(()));
    core.shutdown();
}
