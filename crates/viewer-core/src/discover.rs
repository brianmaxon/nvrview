//! LAN device discovery as the app sees it (docs/FFI.md, "Device
//! discovery"): one search at a time, started and stopped by the app, its
//! answers delivered as `discovered` events and its end as one
//! `discovery_done`.
//!
//! The network work is the `discovery` crate's runner. This module adds the
//! per-search device keys, the event shapes, and the start, restart, and
//! stop rules. Device IDs and MAC addresses stay inside the `discovery`
//! types; nothing here puts them in an event or a log, and addresses are
//! logged at debug level only.

use crate::CoreError;
use crate::events::{EventSink, Priority};
use crate::json;
use discovery::{DiscoveredDevice, Options};
use serde_json::Value;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{debug, info};

/// Shortest and longest search `seconds` accepts.
pub const SECONDS: std::ops::RangeInclusive<f64> = 0.5..=30.0;
/// The search length when `seconds` is not given.
pub const DEFAULT_SECONDS: f64 = 3.0;
/// Most interfaces a request may name.
pub const MAX_INTERFACES: usize = 32;

/// Turns the app's request (`{"seconds":3,"interfaces":["192.0.2.5"],
/// "onvif":true}`; every member optional, unknown members ignored) into the
/// runner's options. `None` or an empty string means all defaults.
///
/// # Errors
/// [`CoreError::InvalidArgument`] for JSON that is not an object, a
/// `seconds` outside [`SECONDS`], an interface that is not an IPv4
/// address, more than [`MAX_INTERFACES`] of them, or a member of the wrong
/// type.
pub fn parse_options(request: Option<&str>) -> Result<Options, CoreError> {
    let mut options = Options {
        duration: Duration::from_secs_f64(DEFAULT_SECONDS),
        ..Options::default()
    };
    let text = request.map(str::trim).unwrap_or_default();
    if text.is_empty() {
        return Ok(options);
    }
    let value: Value = serde_json::from_str(text).map_err(|_| CoreError::InvalidArgument)?;
    let object = match value {
        Value::Object(o) => o,
        Value::Null => return Ok(options),
        _ => return Err(CoreError::InvalidArgument),
    };
    if let Some(v) = object.get("seconds").filter(|v| !v.is_null()) {
        let s = v
            .as_f64()
            .filter(|s| SECONDS.contains(s))
            .ok_or(CoreError::InvalidArgument)?;
        options.duration = Duration::from_secs_f64(s);
    }
    if let Some(v) = object.get("interfaces").filter(|v| !v.is_null()) {
        let list = v.as_array().ok_or(CoreError::InvalidArgument)?;
        if list.len() > MAX_INTERFACES {
            return Err(CoreError::InvalidArgument);
        }
        for item in list {
            let addr: Ipv4Addr = item
                .as_str()
                .and_then(|s| s.parse().ok())
                .ok_or(CoreError::InvalidArgument)?;
            options.interfaces.push(addr);
        }
    }
    if let Some(v) = object.get("onvif").filter(|v| !v.is_null()) {
        options.onvif = v.as_bool().ok_or(CoreError::InvalidArgument)?;
    }
    Ok(options)
}

/// The `discovered` event for a device (new, or with more known than
/// before). Carries no UID and no MAC address.
fn discovered_event(search: u64, key: &str, d: &DiscoveredDevice) -> String {
    let mut o = json::Object::event("discovered")
        .u64("search", search)
        .str("key", key)
        .str("kind", d.kind.as_str());
    if let Some(name) = &d.name {
        o = o.str("name", name);
    }
    if let Some(model) = &d.model {
        o = o.str("model", model);
    }
    o = o.str("address", &d.address.to_string());
    if let Some(port) = d.native_port() {
        o = o.u64("port", u64::from(port));
    }
    if let Some(channels) = d.channels {
        o = o.u64("channels", u64::from(channels));
    }
    if let Some(battery) = d.battery_percent {
        o = o.u64("battery_percent", u64::from(battery));
    }
    let services = d.services.iter().map(|s| {
        let mut so = json::Object::new().str("kind", s.kind.as_str());
        if let Some(port) = s.port {
            so = so.u64("port", u64::from(port));
        }
        if let Some(url) = &s.url {
            so = so.str("url", url);
        }
        so
    });
    o.objects("services", services).finish()
}

fn done_event(search: u64, count: usize, stopped: bool, errors: &[String]) -> String {
    json::Object::event("discovery_done")
        .u64("search", search)
        .u64("count", count as u64)
        .bool("stopped", stopped)
        .strs("errors", errors.iter().map(String::as_str))
        .finish()
}

/// Per-search device keys: one per device, by the address the merged
/// device keeps from its first answer.
#[derive(Default)]
struct Keys {
    by_address: HashMap<IpAddr, String>,
}

impl Keys {
    fn key(&mut self, address: IpAddr) -> String {
        let next = self.by_address.len() + 1;
        self.by_address
            .entry(address)
            .or_insert_with(|| format!("d{next}"))
            .clone()
    }

    fn count(&self) -> usize {
        self.by_address.len()
    }
}

struct Running {
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

/// The core's one search.
pub(crate) struct DiscoverHub {
    events: EventSink,
    handle: Handle,
    next_search: AtomicU64,
    running: Mutex<Option<Running>>,
}

impl DiscoverHub {
    pub(crate) fn new(events: EventSink, handle: Handle) -> DiscoverHub {
        DiscoverHub {
            events,
            handle,
            next_search: AtomicU64::new(1),
            running: Mutex::new(None),
        }
    }

    /// Stops the running search (if any; its `discovery_done` is sent with
    /// `stopped` true before this returns) and starts a new one. Returns
    /// the new search's ID.
    pub(crate) fn start(&self, options: Options) -> u64 {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        self.stop_locked(&mut running);
        let search = self.next_search.fetch_add(1, Ordering::Relaxed);
        let (stop, stop_rx) = oneshot::channel();
        // Before any answer, so an app that starts searches in quick
        // succession can tell which events belong to the last one.
        self.events.send(
            json::Object::event("discovery_started")
                .u64("search", search)
                .finish(),
            Priority::State,
        );
        let task = self
            .handle
            .spawn(run(search, options, self.events.clone(), stop_rx));
        info!(search, "discovery started");
        *running = Some(Running {
            stop: Some(stop),
            task,
        });
        search
    }

    /// Stops the running search, if any.
    pub(crate) fn stop(&self) {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        self.stop_locked(&mut running);
    }

    fn stop_locked(&self, running: &mut Option<Running>) {
        if let Some(mut r) = running.take() {
            if let Some(stop) = r.stop.take() {
                // Fails when the search already ended on its own.
                let _ = stop.send(());
            }
            // The task closes its sockets and sends its one
            // `discovery_done`, then ends; that is quick and does not wait
            // on the app.
            let _ = self.handle.block_on(r.task);
        }
    }
}

async fn run(search: u64, options: Options, events: EventSink, stop: oneshot::Receiver<()>) {
    let mut keys = Keys::default();
    let outcome = {
        let search_future = discovery::discover_with(&options, |d| {
            let key = keys.key(d.address);
            debug!(search, key, address = %d.address, "discovery answer");
            events.send(discovered_event(search, &key, d), Priority::State);
        });
        tokio::pin!(search_future);
        tokio::select! {
            r = &mut search_future => Some(r),
            // A signal, or the hub going away: either way, stop.
            _ = stop => None,
        }
    };
    let (stopped, errors) = match outcome {
        None => (true, Vec::new()),
        Some(Ok(report)) => (false, report.errors),
        Some(Err(e)) => (false, vec![e.to_string()]),
    };
    info!(
        search,
        count = keys.count(),
        stopped,
        problems = errors.len(),
        "discovery done"
    );
    for e in &errors {
        // Interface names and addresses: debug level only.
        debug!(search, error = %e, "discovery problem");
    }
    events.send(
        done_event(search, keys.count(), stopped, &errors),
        Priority::State,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_members() {
        let d = parse_options(None).unwrap();
        assert_eq!(d.duration, Duration::from_secs(3));
        assert!(d.onvif && d.interfaces.is_empty());
        assert_eq!(parse_options(Some("  ")).unwrap().duration, d.duration);
        assert_eq!(parse_options(Some("{}")).unwrap().duration, d.duration);
        let o = parse_options(Some(
            r#"{"seconds":1.5,"interfaces":["192.0.2.5"],"onvif":false,"later":1}"#,
        ))
        .unwrap();
        assert_eq!(o.duration, Duration::from_millis(1500));
        assert_eq!(o.interfaces, vec![Ipv4Addr::new(192, 0, 2, 5)]);
        assert!(!o.onvif);
    }

    #[test]
    fn bad_requests_are_refused() {
        for bad in [
            "[]",
            "3",
            "{",
            r#"{"seconds":0.1}"#,
            r#"{"seconds":31}"#,
            r#"{"seconds":"3"}"#,
            r#"{"interfaces":"192.0.2.5"}"#,
            r#"{"interfaces":["nope"]}"#,
            r#"{"interfaces":["::1"]}"#,
            r#"{"onvif":1}"#,
        ] {
            assert_eq!(
                parse_options(Some(bad)).unwrap_err(),
                CoreError::InvalidArgument,
                "{bad}"
            );
        }
        let many: Vec<String> = (0..=MAX_INTERFACES).map(|_| "192.0.2.5".into()).collect();
        let req = serde_json::json!({ "interfaces": many }).to_string();
        assert!(parse_options(Some(&req)).is_err());
    }

    #[test]
    fn events_carry_no_identifiers() {
        let mut d = DiscoveredDevice::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)));
        d.kind = discovery::DeviceKind::Nvr;
        d.name = Some("NVR \"1\"".into());
        d.channels = Some(16);
        d.mac = Some("00:00:5e:00:53:10".into());
        d.device_id = Some("UID-EXAMPLE".into());
        d.services.push(discovery::Service {
            kind: discovery::ServiceKind::Native,
            port: Some(9000),
            url: None,
        });
        let text = discovered_event(1, "d1", &d);
        assert!(!text.contains("UID-EXAMPLE") && !text.contains("00:00:5e"));
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["type"], "discovered");
        assert_eq!(v["key"], "d1");
        assert_eq!(v["name"], "NVR \"1\"");
        assert_eq!(v["port"], 9000);
        assert_eq!(v["channels"], 16);
        assert_eq!(v["services"][0]["kind"], "native");
        assert!(v.get("model").is_none());
    }

    #[test]
    fn keys_are_per_device() {
        let mut k = Keys::default();
        let a = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
        let b = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11));
        assert_eq!(k.key(a), "d1");
        assert_eq!(k.key(b), "d2");
        assert_eq!(k.key(a), "d1");
        assert_eq!(k.count(), 2);
    }
}
