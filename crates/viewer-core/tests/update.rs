//! Update checks and downloads through the core (docs/FFI.md, "Updates"):
//! the `update` events, one operation at a time, cancel, and shutdown with
//! an operation running. Sources are local folders and a loopback socket;
//! the signing key is made by the test.

mod common;

use common::*;
use std::fs;
use std::io::Read;
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use viewer_core::update::parse_request;
use viewer_core::{CoreConfig, CoreError};

/// A stored zip holding `App/hello.txt`.
fn tiny_zip() -> Vec<u8> {
    updates::zip::write_stored(&[("App/hello.txt", b"hello")])
}

/// Publishes version 0.4.0 for platform `test-os` in `dir`; returns the
/// public key text.
fn publish(dir: &Path) -> String {
    let key = updates::SecretKey::from_seed([9; 32]);
    let package = tiny_zip();
    let file = "App-0.4.0-test-os.zip";
    fs::write(dir.join(file), &package).unwrap();
    let (size, sha) = updates::hash_file(&dir.join(file)).unwrap();
    let manifest = format!(
        r#"{{"format":1,"version":"0.4.0","notes":"Fixes.","packages":{{"test-os":{{"file":"{file}","size":{size},"sha256":"{}"}}}}}}"#,
        updates::keys::encode_hex(&sha)
    );
    fs::write(dir.join("updates.json"), &manifest).unwrap();
    fs::write(
        dir.join("updates.json.sig"),
        key.sign_line(manifest.as_bytes()),
    )
    .unwrap();
    key.public().to_text()
}

fn request(source: &str, key: &str, current: &str) -> String {
    serde_json::json!({"source": source, "keys": [key], "current": current, "platform": "test-os"})
        .to_string()
}

fn updates_of(rec: &Recorder, op: u64) -> Vec<serde_json::Value> {
    rec.events_of("update")
        .into_iter()
        .filter(|e| e["op"] == op)
        .collect()
}

fn last_state(rec: &Recorder, op: u64) -> Option<String> {
    updates_of(rec, op)
        .last()
        .and_then(|e| e["state"].as_str().map(str::to_owned))
}

fn finished(rec: &Recorder, op: u64) -> bool {
    last_state(rec, op).is_some_and(|s| !matches!(s.as_str(), "started" | "downloading"))
}

#[test]
fn check_then_download_through_the_core() {
    let src = tempfile::tempdir().unwrap();
    let key = publish(src.path());
    let source = src.path().to_str().unwrap();
    let (core, rec) = start(CoreConfig::default());

    let op = core
        .update_start(parse_request(&request(source, &key, "0.3.0"), false).unwrap())
        .unwrap();
    wait_until("the check", 10, || finished(&rec, op));
    core.flush_events();
    let ev = updates_of(&rec, op);
    assert_eq!(ev[0]["state"], "started");
    assert_eq!(ev[0]["action"], "check");
    let last = ev.last().unwrap();
    assert_eq!(last["state"], "available");
    assert_eq!(last["version"], "0.4.0");
    assert_eq!(last["current"], "0.3.0");
    assert_eq!(last["notes"], "Fixes.");
    assert_eq!(last["size"], tiny_zip().len() as u64);
    assert_eq!(last["key_id"].as_str().unwrap().len(), 16);

    let op = core
        .update_start(parse_request(&request(source, &key, "0.4.0"), false).unwrap())
        .unwrap();
    wait_until("the second check", 10, || finished(&rec, op));
    core.flush_events();
    let last = updates_of(&rec, op).pop().unwrap();
    assert_eq!(last["state"], "up_to_date");
    assert_eq!(last["latest"], "0.4.0");

    let work = tempfile::tempdir().unwrap();
    let staging = work.path().join(".App-update").join("staging-0.4.0");
    let mut req: serde_json::Value = serde_json::from_str(&request(source, &key, "0.3.0")).unwrap();
    req["version"] = "0.4.0".into();
    req["staging_dir"] = staging.to_str().unwrap().into();
    let op = core
        .update_start(parse_request(&req.to_string(), true).unwrap())
        .unwrap();
    wait_until("the download", 10, || finished(&rec, op));
    core.flush_events();
    let ev = updates_of(&rec, op);
    assert_eq!(ev[0]["action"], "download");
    let progress: Vec<_> = ev.iter().filter(|e| e["state"] == "downloading").collect();
    assert_eq!(progress.first().unwrap()["received"], 0);
    assert_eq!(
        progress.last().unwrap()["received"],
        progress.last().unwrap()["total"]
    );
    let ready = ev.last().unwrap();
    assert_eq!(ready["state"], "ready");
    assert_eq!(ready["version"], "0.4.0");
    let path = Path::new(ready["path"].as_str().unwrap());
    assert_eq!(path, staging.join("content"));
    assert_eq!(fs::read(path.join("App/hello.txt")).unwrap(), b"hello");
    core.shutdown();
}

#[test]
fn failures_carry_a_reason() {
    let src = tempfile::tempdir().unwrap();
    let key = publish(src.path());
    let other = updates::SecretKey::from_seed([1; 32]).public().to_text();
    let (core, rec) = start(CoreConfig::default());
    let source = src.path().to_str().unwrap();
    for (req, reason) in [
        (request(source, &other, "0.3.0"), "bad_signature"),
        (
            request("http://updates.example.com/u/", &key, "0.3.0"),
            "insecure_source",
        ),
        (request("relative/folder", &key, "0.3.0"), "source_invalid"),
        (
            serde_json::json!({"source": source, "keys": [], "current": "0.3.0"}).to_string(),
            "no_trusted_keys",
        ),
    ] {
        let op = core
            .update_start(parse_request(&req, false).unwrap())
            .unwrap();
        wait_until("the check", 10, || finished(&rec, op));
        core.flush_events();
        let last = updates_of(&rec, op).pop().unwrap();
        assert_eq!(last["state"], "failed", "{req}");
        assert_eq!(last["reason"], reason, "{req}");
        assert!(!last["message"].as_str().unwrap().is_empty());
    }
    core.shutdown();
}

/// A server that reads each request and never answers. Returns its port
/// and the number of requests it has read.
fn silent_server() -> (u16, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let counter = requests.clone();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for mut s in l.incoming().flatten() {
            let mut buf = [0u8; 1024];
            if s.read(&mut buf).is_ok_and(|n| n > 0) {
                counter.fetch_add(1, Ordering::SeqCst);
            }
            held.push(s);
        }
    });
    (port, requests)
}

#[test]
fn one_operation_at_a_time_and_cancel() {
    let (port, requests) = silent_server();
    let key = updates::SecretKey::from_seed([9; 32]).public().to_text();
    let (core, rec) = start(CoreConfig::default());
    let source = format!("http://127.0.0.1:{port}/u/");
    let req = || parse_request(&request(&source, &key, "0.3.0"), false).unwrap();
    let first = core.update_start(req()).unwrap();
    // Busy: a second start is refused, not queued.
    assert_eq!(core.update_start(req()).unwrap_err(), CoreError::Limit);
    // Cancel while the check is blocked waiting for the server's answer
    // (20 s before its timeout): it returns after its short wait, with the
    // final event already queued.
    wait_until("the request to reach the server", 10, || {
        requests.load(Ordering::SeqCst) == 1
    });
    let t = Instant::now();
    core.update_cancel().unwrap();
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    core.flush_events();
    let ev = updates_of(&rec, first);
    assert_eq!(ev.last().unwrap()["state"], "canceled");
    // Nothing running now: a new operation may start, and cancel with
    // nothing running is fine.
    let second = core.update_start(req()).unwrap();
    assert!(second > first);
    wait_until("the second request", 10, || {
        requests.load(Ordering::SeqCst) == 2
    });
    core.update_cancel().unwrap();
    core.update_cancel().unwrap();
    // Shutdown with an operation blocked does not wait for its timeout.
    let third = core.update_start(req()).unwrap();
    wait_until("the third request", 10, || {
        requests.load(Ordering::SeqCst) == 3
    });
    let t = Instant::now();
    core.shutdown();
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    // Each operation ended with exactly one final event, canceled.
    for op in [first, second, third] {
        let ev = updates_of(&rec, op);
        let finals: Vec<_> = ev.iter().filter(|e| e["state"] != "started").collect();
        assert_eq!(finals.len(), 1, "{ev:?}");
        assert_eq!(finals[0]["state"], "canceled");
    }
}

/// Publishes 0.4.0 in `dir` with an optional `published` time (dates long
/// past, so no test depends on the real clock); returns the key text.
fn publish_dated(dir: &Path, published: Option<&str>) -> String {
    publish(dir);
    let key = updates::SecretKey::from_seed([9; 32]);
    let text = fs::read_to_string(dir.join("updates.json")).unwrap();
    let text = match published {
        Some(p) => text.replacen("{", &format!(r#"{{"published":"{p}","#), 1),
        None => text,
    };
    fs::write(dir.join("updates.json"), &text).unwrap();
    fs::write(dir.join("updates.json.sig"), key.sign_line(text.as_bytes())).unwrap();
    key.public().to_text()
}

fn check_once(core: &viewer_core::Core, rec: &Recorder, req: &str) -> serde_json::Value {
    let op = core
        .update_start(parse_request(req, false).unwrap())
        .unwrap();
    wait_until("the check", 10, || finished(rec, op));
    core.flush_events();
    updates_of(rec, op).pop().unwrap()
}

#[test]
fn an_older_manifest_is_a_replay_even_after_a_restart() {
    let data = tempfile::tempdir().unwrap();
    let src = tempfile::tempdir().unwrap();
    let source = src.path().to_str().unwrap();
    let config = || CoreConfig {
        data_dir: Some(data.path().to_path_buf()),
        ..CoreConfig::default()
    };

    let key = publish_dated(src.path(), Some("2020-02-01T00:00:00Z"));
    let (core, rec) = start(config());
    let last = check_once(&core, &rec, &request(source, &key, "0.3.0"));
    assert_eq!(last["state"], "available");
    assert_eq!(last["published"], "2020-02-01T00:00:00Z");
    assert!(last.get("warning").is_none());
    core.shutdown();
    let stored = fs::read_to_string(data.path().join(viewer_core::update::STATE_FILE)).unwrap();
    assert!(stored.contains("2020-02-01T00:00:00Z"), "{stored}");

    // A new core reads what was accepted: an older signed manifest, or
    // an undated one, from the same source is refused.
    let (core, rec) = start(config());
    publish_dated(src.path(), Some("2020-01-01T00:00:00Z"));
    let last = check_once(&core, &rec, &request(source, &key, "0.3.0"));
    assert_eq!(last["state"], "failed");
    assert_eq!(last["reason"], "stale_manifest");
    publish_dated(src.path(), None);
    let last = check_once(&core, &rec, &request(source, &key, "0.3.0"));
    assert_eq!(last["reason"], "stale_manifest");
    // The same date again, and a newer one, are fine.
    publish_dated(src.path(), Some("2020-02-01T00:00:00Z"));
    let last = check_once(&core, &rec, &request(source, &key, "0.3.0"));
    assert_eq!(last["state"], "available");

    // Another source has its own history: undated is accepted with a
    // warning, unless the request requires a date.
    let other = tempfile::tempdir().unwrap();
    let other_source = other.path().to_str().unwrap();
    publish_dated(other.path(), None);
    let last = check_once(&core, &rec, &request(other_source, &key, "0.3.0"));
    assert_eq!(last["state"], "available");
    assert_eq!(last["warning"], "unpublished");
    let mut req: serde_json::Value =
        serde_json::from_str(&request(other_source, &key, "0.3.0")).unwrap();
    req["require_published"] = true.into();
    let last = check_once(&core, &rec, &req.to_string());
    assert_eq!(last["reason"], "unpublished");
    core.shutdown();
}
