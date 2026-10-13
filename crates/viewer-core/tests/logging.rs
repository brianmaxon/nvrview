//! File logging through viewer-log. Its own test binary: the subscriber is
//! global to the process.

use crate::common::*;
use std::time::Duration;
use viewer_core::{CoreConfig, LogSettings, StreamTier};

#[test]
fn logs_to_capped_files_without_credentials_or_addresses() {
    let dir = tempfile::tempdir().unwrap();
    let nvr = mock(1);
    let (core, rec) = start(CoreConfig {
        log: Some(LogSettings {
            dir: dir.path().to_path_buf(),
            max_files: 2,
            max_total_bytes: 64 * 1024,
        }),
        ..fast_config()
    });
    let dev = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, PASSWORD)
        .unwrap();
    let t = core.open_tile(dev, 0, StreamTier::Sub).unwrap();
    wait_until("decodes", 10, || rec.decodes(t) > 5);
    // A refused login logs a warning too.
    let bad = core
        .add_device("127.0.0.1", nvr.addr().port(), USER, "wrong-password")
        .unwrap();
    wait_until("login_failed", 10, || {
        rec.events_of("device_state")
            .iter()
            .any(|e| e["device"] == bad && e["state"] == "login_failed")
    });
    std::thread::sleep(Duration::from_millis(100));
    core.shutdown();

    let mut text = String::new();
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        text.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    assert!(text.contains("tile opened"), "{text}");
    assert!(text.contains("login refused"), "{text}");
    assert!(!text.contains(PASSWORD) && !text.contains("wrong-password"));
    assert!(!text.contains("127.0.0.1"), "addresses only at debug level");
    assert!(viewer_log::total_bytes_on_disk(dir.path(), "nvrview-core") <= 64 * 1024);
}
