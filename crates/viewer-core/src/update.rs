//! Update checks and downloads as the app sees them (docs/FFI.md,
//! "Updates"; docs/UPDATES.md): one operation at a time, started by the
//! app, on a thread of its own (the `updates` crate blocks), its progress
//! and outcome delivered as `update` events.
//!
//! The core never checks on its own: the app decides when (its schedule,
//! a menu item), and nothing is applied here; the app moves the staged
//! version into place after the user agrees.

use crate::CoreError;
use crate::events::{EventSink, Priority};
use crate::json;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{debug, info};
use updates::{Cancel, CheckOutcome, Config, PublicKey, Source, UpdateError, Version};

/// How long `cancel` (and shutdown) wait for the running operation to end
/// by itself. A network read blocked in the operating system notices only
/// at its next chunk or timeout; past this wait the operation counts as
/// canceled and its thread is left to end on its own (nothing it does
/// reaches the app any more).
const CANCEL_WAIT: Duration = Duration::from_millis(500);

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
    sent: Arc<AtomicBool>,
    events: EventSink,
    op: u64,
    action: &'static str,
}

impl Final {
    /// Sends `o` unless a final event was already sent; returns whether it
    /// was sent.
    fn send(&self, o: json::Object) -> bool {
        if self.sent.swap(true, Ordering::SeqCst) {
            return false;
        }
        self.events.send(o.finish(), Priority::State);
        true
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
}

impl UpdateHub {
    pub(crate) fn new(events: EventSink) -> UpdateHub {
        UpdateHub {
            events,
            next_op: AtomicU64::new(1),
            running: Mutex::new(None),
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
        if running.as_ref().is_some_and(|r| !r.thread.is_finished()) {
            return Err(CoreError::Limit);
        }
        if let Some(r) = running.take() {
            let _ = r.thread.join();
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
            sent: Arc::new(AtomicBool::new(false)),
            events: self.events.clone(),
            op,
            action,
        };
        let worker = Worker {
            op,
            action,
            events: self.events.clone(),
            last: last.clone(),
            cancel: cancel.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("nvrview-update".into())
            .spawn(move || worker.run(request))
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

fn event(op: u64, action: &str, state: &str) -> json::Object {
    json::Object::event("update")
        .u64("op", op)
        .str("action", action)
        .str("state", state)
}

struct Worker {
    op: u64,
    action: &'static str,
    events: EventSink,
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

    fn run(self, request: Request) {
        let op = self.op;
        let source = match Source::parse(&request.source) {
            Ok(s) => s,
            Err(e) => return self.failed(&e),
        };
        let mut config = Config::new(source, request.keys, request.current.clone());
        if let Some(p) = request.platform {
            config.platform = p;
        }
        let current = request.current.to_string();
        let Some((version, staging)) = request.download else {
            match updates::check(&config, &self.cancel) {
                Ok(CheckOutcome::UpToDate { latest, no_package }) => {
                    let mut o = event(op, self.action, "up_to_date")
                        .str("current", &current)
                        .str("latest", &latest.to_string());
                    if no_package {
                        o = o.bool("no_package", true);
                    }
                    if self.last.send(o) {
                        info!(op, "update check: up to date");
                    }
                }
                Ok(CheckOutcome::Available(offer)) => {
                    if self.last.send(
                        event(op, self.action, "available")
                            .str("current", &current)
                            .str("version", &offer.version.to_string())
                            .str("notes", &offer.notes)
                            .u64("size", offer.package.size)
                            .str("key_id", &offer.key_id),
                    ) {
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
            if last_percent != Some(percent) && !self.last.sent.load(Ordering::SeqCst) {
                last_percent = Some(percent);
                self.events.send(
                    event(op, self.action, "downloading")
                        .u64("received", received)
                        .u64("total", total)
                        .finish(),
                    Priority::State,
                );
            }
        };
        match updates::download(&config, &version, &staging, &self.cancel, &mut progress) {
            Ok(staged) => {
                let sent = self.last.send(
                    event(op, self.action, "ready")
                        .str("version", &staged.version.to_string())
                        .str("path", &staged.content.to_string_lossy()),
                );
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
