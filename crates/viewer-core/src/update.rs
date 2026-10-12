//! Update checks and downloads as the app sees them (docs/FFI.md,
//! "Updates"; docs/UPDATES.md): one operation at a time, started by the
//! app, on a thread of its own (the `updates` crate blocks), its progress
//! and outcome delivered as `update` events.
//!
//! The core never checks on its own: the app decides when (its schedule,
//! a menu item), and nothing is applied here; the app moves the staged
//! version into place after the user agrees.

use crate::CoreError;
use crate::clock::HostClock;
use crate::events::{EventSink, Priority};
use crate::json;
use crate::store;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use updates::{Cancel, CheckOutcome, Config, PublicKey, Source, UpdateError, Version};

/// How long `cancel` (and shutdown) wait for the running operation to end
/// by itself. A network read blocked in the operating system notices only
/// at its next chunk or timeout; past this wait the operation counts as
/// canceled and its thread is left to end on its own (nothing it does
/// reaches the app any more).
const CANCEL_WAIT: Duration = Duration::from_millis(500);

/// The file in the data directory that keeps, per source, the newest
/// `published` time of a manifest the core accepted (docs/UPDATES.md,
/// "Dates and replays").
pub const STATE_FILE: &str = "update-state.json";
/// Most sources remembered; past it the one with the oldest date goes.
const MAX_REMEMBERED_SOURCES: usize = 32;

/// The newest accepted `published` time per source (by the source's
/// normalized form), kept in [`STATE_FILE`], or in memory without a data
/// directory.
pub(crate) struct DateState {
    dir: Option<PathBuf>,
    published: BTreeMap<String, u64>,
}

impl DateState {
    pub(crate) fn open(dir: Option<PathBuf>) -> DateState {
        let mut published = BTreeMap::new();
        if let Some(v) = dir.as_deref().and_then(|d| store::read(d, STATE_FILE))
            && let Some(o) = v.get("published").and_then(Value::as_object)
        {
            for (source, t) in o {
                if let Some(t) = t.as_str().and_then(updates::time::parse) {
                    published.insert(source.clone(), t);
                }
            }
        }
        DateState { dir, published }
    }

    fn get(&self, source: &str) -> Option<u64> {
        self.published.get(source).copied()
    }

    /// Remembers `t` for `source` if it is newer than what is kept.
    fn record(&mut self, source: &str, t: u64) {
        if self.get(source).is_some_and(|seen| seen >= t) {
            return;
        }
        self.published.insert(source.to_string(), t);
        while self.published.len() > MAX_REMEMBERED_SOURCES {
            let oldest = self
                .published
                .iter()
                .min_by_key(|(_, t)| **t)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                self.published.remove(&k);
            }
        }
        let Some(dir) = &self.dir else { return };
        let map: serde_json::Map<String, Value> = self
            .published
            .iter()
            .map(|(k, t)| (k.clone(), Value::String(updates::time::format(*t))))
            .collect();
        let value = serde_json::json!({"format": 1, "published": map});
        if let Err(e) = store::write_atomic(dir, STATE_FILE, &value) {
            warn!(error = %e, "cannot write the update state");
        }
    }
}

/// What the app asked for, parsed.
#[derive(Debug)]
pub struct Request {
    /// The source as typed, parsed by the worker (a bad source is a
    /// `failed` event with its reason, not a refused call).
    pub source: String,
    pub keys: Vec<PublicKey>,
    pub current: Version,
    pub platform: Option<String>,
    /// `nv_update_download` only.
    pub download: Option<(Version, PathBuf)>,
    /// Refuse a manifest without a `published` time.
    pub require_published: bool,
}

/// Parses `{"source":"...","keys":["..."],"current":"0.3.0",
/// "platform":"macos-arm64"}`, and for a download also
/// `"version":"0.4.0","staging_dir":"/abs/path"`.
///
/// # Errors
/// [`CoreError::InvalidArgument`] for JSON that is not an object, a
/// missing or mistyped member, a key that is not an update public key,
/// more than [`updates::keys::MAX_TRUSTED_KEYS`] keys, or a version that is
/// not a semantic version. The source is checked by the operation.
pub fn parse_request(text: &str, download: bool) -> Result<Request, CoreError> {
    let bad = CoreError::InvalidArgument;
    let value: Value = serde_json::from_str(text.trim()).map_err(|_| bad)?;
    let o = value.as_object().ok_or(bad)?;
    let string = |name: &str| o.get(name).and_then(Value::as_str);
    let source = string("source").ok_or(bad)?.to_string();
    let keys = o.get("keys").and_then(Value::as_array).ok_or(bad)?;
    if keys.len() > updates::keys::MAX_TRUSTED_KEYS {
        return Err(bad);
    }
    let keys = keys
        .iter()
        .map(|k| k.as_str().and_then(PublicKey::parse).ok_or(bad))
        .collect::<Result<Vec<_>, _>>()?;
    let current = string("current").and_then(Version::parse).ok_or(bad)?;
    let platform = match o.get("platform") {
        None | Some(Value::Null) => None,
        Some(Value::String(p)) if !p.is_empty() && p.len() <= 32 => Some(p.clone()),
        Some(_) => return Err(bad),
    };
    let require_published = match o.get("require_published") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad),
    };
    let download = if download {
        let version = string("version").and_then(Version::parse).ok_or(bad)?;
        let staging = PathBuf::from(string("staging_dir").filter(|s| !s.is_empty()).ok_or(bad)?);
        Some((version, staging))
    } else {
        None
    };
    Ok(Request {
        source,
        keys,
        current,
        platform,
        download,
        require_published,
    })
}

/// `nv_update_parse_key`: the canonical key text and its ID.
pub fn key_info(text: &str) -> Option<String> {
    let key = PublicKey::parse(text)?;
    Some(
        json::Object::new()
            .str("key", &key.to_text())
            .str("id", &key.id())
            .finish(),
    )
}

/// `nv_update_parse_source`: whether the source is acceptable, its kind,
/// and how to show it; or the reason it is not, with a sentence.
pub fn source_info(text: &str) -> String {
    match Source::parse(text) {
        Ok(s) => json::Object::new()
            .bool("ok", true)
            .str("kind", s.kind())
            .str("display", &s.display())
            .finish(),
        Err(e) => json::Object::new()
            .bool("ok", false)
            .str("reason", e.reason.as_str())
            .str("message", &e.detail)
            .finish(),
    }
}

/// The one final event of an operation (`up_to_date`, `available`,
/// `ready`, `failed`, or `canceled`) is sent once: by the worker when it
/// finishes, or by `cancel` when the worker is still blocked.
#[derive(Clone)]
struct Final {
    /// Whether the final event went out. Held while an event of the
    /// operation is queued, so a progress event can never follow the final
    /// one (the worker's progress and `cancel` run on different threads).
    sent: Arc<Mutex<bool>>,
    events: EventSink,
    op: u64,
    action: &'static str,
}

impl Final {
    /// Sends `o` unless a final event was already sent; returns whether it
    /// was sent.
    fn send(&self, o: json::Object) -> bool {
        let mut sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        if *sent {
            return false;
        }
        *sent = true;
        self.events.send(o.finish(), Priority::State);
        true
    }

    /// Sends a progress event unless the final event was already sent.
    fn progress(&self, o: json::Object) {
        let sent = self.sent.lock().unwrap_or_else(|e| e.into_inner());
        if !*sent {
            self.events.send(o.finish(), Priority::State);
        }
    }

    /// Whether the final event was sent (the operation no longer holds the
    /// core's update slot).
    fn is_sent(&self) -> bool {
        *self.sent.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn canceled(&self) -> bool {
        self.send(event(self.op, self.action, "canceled"))
    }
}

struct Running {
    op: u64,
    cancel: Cancel,
    last: Final,
    thread: JoinHandle<()>,
}

/// The core's one update operation.
pub(crate) struct UpdateHub {
    events: EventSink,
    next_op: AtomicU64,
    running: Mutex<Option<Running>>,
    dates: Arc<Mutex<DateState>>,
    clock: HostClock,
}

impl UpdateHub {
    pub(crate) fn new(events: EventSink, data_dir: Option<PathBuf>, clock: HostClock) -> UpdateHub {
        UpdateHub {
            events,
            next_op: AtomicU64::new(1),
            running: Mutex::new(None),
            dates: Arc::new(Mutex::new(DateState::open(data_dir))),
            clock,
        }
    }

    /// Starts a check, or a download when the request has one. Returns the
    /// operation's ID.
    ///
    /// # Errors
    /// [`CoreError::Limit`] while another operation runs (cancel it
    /// first), [`CoreError::Internal`] if the thread cannot start.
    pub(crate) fn start(&self, request: Request) -> Result<u64, CoreError> {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        // The slot is free once the final event went out, even while the
        // worker thread is still returning: the app may start the next
        // operation from that event.
        if running
            .as_ref()
            .is_some_and(|r| !r.thread.is_finished() && !r.last.is_sent())
        {
            return Err(CoreError::Limit);
        }
        if let Some(r) = running.take() {
            // Join only a thread that already ended; one past its final
            // event is just returning, and needs no waiting for.
            if r.thread.is_finished() {
                let _ = r.thread.join();
            }
        }
        let op = self.next_op.fetch_add(1, Ordering::Relaxed);
        let cancel = Cancel::new();
        let action = if request.download.is_some() {
            "download"
        } else {
            "check"
        };
        // Before anything else, so the app can tell this operation's events
        // from an earlier one's.
        self.events
            .send(event(op, action, "started").finish(), Priority::State);
        let last = Final {
            sent: Arc::new(Mutex::new(false)),
            events: self.events.clone(),
            op,
            action,
        };
        let worker = Worker {
            op,
            action,
            dates: self.dates.clone(),
            now: u64::try_from(self.clock.wall_now_us() / 1_000_000).unwrap_or(0),
            last: last.clone(),
            cancel: cancel.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("nvrview-update".into())
            .spawn(move || {
                worker.run(request);
                #[cfg(test)]
                tests::exit_hold();
            })
            .map_err(|_| CoreError::Internal)?;
        info!(op, action, "update operation started");
        *running = Some(Running {
            op,
            cancel,
            last,
            thread,
        });
        Ok(op)
    }

    /// Cancels the running operation, if any. Waits up to
    /// [`CANCEL_WAIT`] for it to end by itself; either way its final event
    /// (`canceled`, or the outcome it reached first) is queued before this
    /// returns, and nothing more of it follows. A worker still blocked in a
    /// network read ends at its next chunk or timeout and removes what it
    /// created.
    pub(crate) fn cancel(&self) {
        let taken = self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let Some(r) = taken else { return };
        r.cancel.cancel();
        let deadline = Instant::now() + CANCEL_WAIT;
        while !r.thread.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if r.thread.is_finished() {
            let _ = r.thread.join();
        } else {
            info!(
                op = r.op,
                "update operation blocked; canceled without waiting for it"
            );
        }
        if r.last.canceled() {
            info!(op = r.op, "update operation canceled");
        }
    }
}

/// Adds the manifest's `published` time, or `"warning":"unpublished"` for
/// an undated manifest.
fn dated(o: json::Object, published: Option<u64>) -> json::Object {
    match published {
        Some(t) => o.str("published", &updates::time::format(t)),
        None => o.str("warning", "unpublished"),
    }
}

fn event(op: u64, action: &str, state: &str) -> json::Object {
    json::Object::event("update")
        .u64("op", op)
        .str("action", action)
        .str("state", state)
}

struct Worker {
    op: u64,
    action: &'static str,
    dates: Arc<Mutex<DateState>>,
    /// Wall-clock now, seconds since the epoch, from the core's clock.
    now: u64,
    last: Final,
    cancel: Cancel,
}

impl Worker {
    fn failed(&self, e: &UpdateError) {
        let op = self.op;
        if e.reason == updates::Reason::Canceled {
            if self.last.canceled() {
                info!(op, "update operation canceled");
            }
            return;
        }
        // The detail can name the source's host or folder: debug only.
        if self.last.send(
            event(op, self.action, "failed")
                .str("reason", e.reason.as_str())
                .str("message", &e.detail),
        ) {
            info!(
                op,
                action = self.action,
                reason = e.reason.as_str(),
                "update operation failed"
            );
            debug!(op, detail = %e.detail, "update failure detail");
        }
    }

    /// A verified manifest was accepted: remember its date for this
    /// source, or log that it had none.
    fn accepted(&self, source_key: &str, published: Option<u64>) {
        match published {
            Some(t) => self
                .dates
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record(source_key, t),
            None => warn!(
                op = self.op,
                "the update manifest has no published time; replays of it cannot be detected"
            ),
        }
    }

    fn run(self, request: Request) {
        let op = self.op;
        let source = match Source::parse(&request.source) {
            Ok(s) => s,
            Err(e) => return self.failed(&e),
        };
        let source_key = source.display();
        let mut config = Config::new(source, request.keys, request.current.clone());
        if let Some(p) = request.platform {
            config.platform = p;
        }
        config.now = self.now;
        config.require_published = request.require_published;
        config.accepted_published = self
            .dates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&source_key);
        let current = request.current.to_string();
        let Some((version, staging)) = request.download else {
            match updates::check(&config, &self.cancel) {
                Ok(CheckOutcome::UpToDate {
                    latest,
                    no_package,
                    published,
                }) => {
                    self.accepted(&source_key, published);
                    let mut o = dated(
                        event(op, self.action, "up_to_date")
                            .str("current", &current)
                            .str("latest", &latest.to_string()),
                        published,
                    );
                    if no_package {
                        o = o.bool("no_package", true);
                    }
                    if self.last.send(o) {
                        info!(op, "update check: up to date");
                    }
                }
                Ok(CheckOutcome::Available(offer)) => {
                    self.accepted(&source_key, offer.published);
                    if self.last.send(dated(
                        event(op, self.action, "available")
                            .str("current", &current)
                            .str("version", &offer.version.to_string())
                            .str("notes", &offer.notes)
                            .u64("size", offer.package.size)
                            .str("key_id", &offer.key_id),
                        offer.published,
                    )) {
                        info!(op, version = %offer.version, "update check: newer version available");
                    }
                }
                Err(e) => self.failed(&e),
            }
            return;
        };
        // At most one progress event per percent (and the first and last),
        // so a fast local copy does not flood the queue; none once a final
        // event went out.
        let mut last_percent = None;
        let mut progress = |received: u64, total: u64| {
            let percent = received
                .saturating_mul(100)
                .checked_div(total)
                .unwrap_or(100);
            if last_percent != Some(percent) {
                last_percent = Some(percent);
                self.last.progress(
                    event(op, self.action, "downloading")
                        .u64("received", received)
                        .u64("total", total),
                );
            }
        };
        match updates::download(&config, &version, &staging, &self.cancel, &mut progress) {
            Ok(staged) => {
                self.accepted(&source_key, staged.published);
                let sent = self.last.send(dated(
                    event(op, self.action, "ready")
                        .str("version", &staged.version.to_string())
                        .str("path", &staged.content.to_string_lossy()),
                    staged.published,
                ));
                if sent {
                    info!(op, version = %staged.version, "update downloaded and verified");
                } else {
                    // Canceled while finishing: nobody will use it.
                    let _ = std::fs::remove_dir_all(&staging);
                }
            }
            Err(e) => self.failed(&e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Held by a test to keep every update worker thread alive after its
    /// final event.
    static EXIT_HOLD: Mutex<()> = Mutex::new(());

    pub(super) fn exit_hold() {
        drop(EXIT_HOLD.lock().unwrap_or_else(|e| e.into_inner()));
    }

    fn key() -> String {
        updates::SecretKey::from_seed([3; 32]).public().to_text()
    }

    #[test]
    fn requests() {
        let k = key();
        let r = parse_request(
            &format!(r#"{{"source":"/srv/u","keys":["{k}"],"current":"0.3.0","later":1}}"#),
            false,
        )
        .unwrap();
        assert_eq!(r.current.to_string(), "0.3.0");
        assert!(r.download.is_none() && r.platform.is_none());
        let r = parse_request(
            &format!(
                r#"{{"source":"/srv/u","keys":["{k}"],"current":"0.3.0","platform":"win-x64","version":"0.4.0","staging_dir":"/tmp/s"}}"#
            ),
            true,
        )
        .unwrap();
        assert_eq!(r.download.unwrap().0.to_string(), "0.4.0");
        assert_eq!(r.platform.as_deref(), Some("win-x64"));
        for bad in [
            "".to_string(),
            "[]".into(),
            format!(r#"{{"keys":["{k}"],"current":"0.3.0"}}"#),
            format!(r#"{{"source":"/s","keys":"{k}","current":"0.3.0"}}"#),
            r#"{"source":"/s","keys":["nope"],"current":"0.3.0"}"#.into(),
            format!(r#"{{"source":"/s","keys":["{k}"],"current":"0.3"}}"#),
            format!(r#"{{"source":"/s","keys":["{k}"],"current":"0.3.0","platform":1}}"#),
            format!(
                r#"{{"source":"/s","keys":["{k}"],"current":"0.3.0","require_published":"yes"}}"#
            ),
        ] {
            assert_eq!(
                parse_request(&bad, false).unwrap_err(),
                CoreError::InvalidArgument,
                "{bad}"
            );
        }
        // A download needs the version and the staging folder.
        let check_only = format!(r#"{{"source":"/s","keys":["{k}"],"current":"0.3.0"}}"#);
        assert!(parse_request(&check_only, true).is_err());
        // No keys is a valid request; the operation reports it.
        assert!(parse_request(r#"{"source":"/s","keys":[],"current":"0.3.0"}"#, false).is_ok());
    }

    #[test]
    fn no_progress_after_the_final_event() {
        struct Collect(Mutex<Vec<String>>);
        impl crate::Callbacks for Collect {
            fn on_action(&self, _: u64, _: u32, _: &crate::Action<'_>) {}
            fn on_event(&self, json: &str) {
                self.0.lock().unwrap().push(json.to_string());
            }
        }
        let seen = Arc::new(Collect(Mutex::new(Vec::new())));
        let mut thread = crate::events::EventThread::spawn(seen.clone()).unwrap();
        let last = Final {
            sent: Arc::new(Mutex::new(false)),
            events: thread.sink(),
            op: 7,
            action: "download",
        };
        let progress = || event(7, "download", "downloading").u64("received", 1);
        last.progress(progress());
        assert!(last.canceled());
        // What a worker still running after `cancel` would send.
        last.progress(progress());
        assert!(!last.send(event(7, "download", "ready")));
        thread.stop();
        let states: Vec<String> = seen
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|j| serde_json::from_str::<Value>(j).unwrap()["state"].to_string())
            .collect();
        assert_eq!(states, [r#""downloading""#, r#""canceled""#]);
    }

    /// A new operation is accepted as soon as the previous one's final
    /// event is out, while its worker thread has not returned yet.
    #[test]
    fn slot_is_free_at_the_final_event() {
        struct Collect(Mutex<std::sync::mpsc::Sender<Value>>);
        impl crate::Callbacks for Collect {
            fn on_action(&self, _: u64, _: u32, _: &crate::Action<'_>) {}
            fn on_event(&self, json: &str) {
                let _ = self
                    .0
                    .lock()
                    .unwrap()
                    .send(serde_json::from_str(json).unwrap());
            }
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let mut thread =
            crate::events::EventThread::spawn(Arc::new(Collect(Mutex::new(tx)))).unwrap();
        let hub = UpdateHub::new(thread.sink(), None, HostClock::Host);
        // No keys: the check fails at once.
        let request =
            || parse_request(r#"{"source":"/srv/u","keys":[],"current":"0.3.0"}"#, false).unwrap();
        let hold = EXIT_HOLD.lock().unwrap();
        let first = hub.start(request()).unwrap();
        loop {
            let e = rx.recv().unwrap();
            if e["op"] == first && e["state"] != "started" {
                assert_eq!(e["state"], "failed");
                break;
            }
        }
        // The first worker is parked past its final event.
        let second = hub
            .start(request())
            .expect("slot free after the final event");
        assert_ne!(first, second);
        drop(hold);
        thread.stop();
    }

    #[test]
    fn key_and_source_info() {
        let info: Value = serde_json::from_str(&key_info(&key()).unwrap()).unwrap();
        assert_eq!(info["key"], key());
        assert_eq!(info["id"].as_str().unwrap().len(), 16);
        assert!(key_info("nope").is_none());
        let ok: Value =
            serde_json::from_str(&source_info("https://updates.example.com/u")).unwrap();
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["kind"], "https");
        assert_eq!(ok["display"], "https://updates.example.com/u/");
        let no: Value = serde_json::from_str(&source_info("http://updates.example.com/u")).unwrap();
        assert_eq!(no["ok"], false);
        assert_eq!(no["reason"], "insecure_source");
        assert!(no["message"].as_str().unwrap().contains("local network"));
    }
}
