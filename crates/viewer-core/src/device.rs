//! One device: its session and the task that turns session state into
//! control events.

use crate::events::{EventSink, Priority};
use crate::json::Object;
use bc_protocol::session::Command;
use bc_protocol::stream::StreamTier;
use bc_protocol::xml::{self, ChannelInfo, VersionInfo};
use bc_session::{ConnState, DeviceSession};
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tracing::{debug, warn};

/// Connects by host name or address, resolving again on every attempt (a
/// device's address may change while the app runs).
pub(crate) struct HostConnector {
    pub(crate) host: String,
    pub(crate) port: u16,
}

impl bc_session::Connector for HostConnector {
    type Stream = TcpStream;

    fn connect(&self) -> Pin<Box<dyn Future<Output = io::Result<TcpStream>> + Send + '_>> {
        Box::pin(async move {
            let s = TcpStream::connect((self.host.as_str(), self.port)).await?;
            s.set_nodelay(true)?;
            Ok(s)
        })
    }
}

/// How long closing a session may take before the core stops waiting. The
/// session's own write timeout bounds the graceful part; this only guards
/// against a stuck runtime.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(15);

/// The core's handle on a device.
pub(crate) struct DeviceHandle {
    session: Option<Arc<DeviceSession>>,
    stop: Option<oneshot::Sender<()>>,
    watcher: Option<JoinHandle<()>>,
}

impl DeviceHandle {
    /// Starts watching `session`; must be called inside the runtime.
    pub(crate) fn start(id: u64, session: DeviceSession, events: EventSink) -> DeviceHandle {
        let session = Arc::new(session);
        let (stop_tx, stop_rx) = oneshot::channel();
        let watcher = tokio::spawn(watch(id, session.clone(), events, stop_rx));
        DeviceHandle {
            session: Some(session),
            stop: Some(stop_tx),
            watcher: Some(watcher),
        }
    }

    pub(crate) fn session(&self) -> Option<Arc<DeviceSession>> {
        self.session.clone()
    }

    /// Stops the watcher and its queries. After this the core holds the only
    /// reference to the session.
    pub(crate) async fn stop_watching(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
        if let Some(w) = self.watcher.take() {
            let _ = w.await;
        }
    }

    /// Stops the watcher, stops every preview, and closes the socket.
    pub(crate) async fn close(mut self) {
        self.stop_watching().await;
        let Some(session) = self.session.take() else {
            return;
        };
        match Arc::try_unwrap(session) {
            Ok(s) => {
                if tokio::time::timeout(CLOSE_TIMEOUT, s.close())
                    .await
                    .is_err()
                {
                    warn!("device session did not close in time");
                }
            }
            Err(shared) => {
                // Someone still holds the session (not expected): dropping
                // our reference starts the close; wait for the supervisor to
                // finish it.
                let mut state = shared.watch_state();
                drop(shared);
                let done = state
                    .wait_for(|s| matches!(s, ConnState::Closed | ConnState::LoginFailed { .. }));
                if tokio::time::timeout(CLOSE_TIMEOUT, done).await.is_err() {
                    warn!("device session did not close in time");
                }
            }
        }
    }

    /// [`DeviceHandle::close`] from a thread outside the runtime.
    pub(crate) fn close_blocking(self, runtime: &Handle) {
        runtime.block_on(self.close());
    }
}

pub(crate) fn tier_name(t: StreamTier) -> &'static str {
    match t {
        StreamTier::Main => "main",
        StreamTier::Sub => "sub",
        StreamTier::Mobile => "mobile",
        StreamTier::Extern => "extern",
    }
}

fn state_event(id: u64, s: &ConnState) -> String {
    let o = Object::event("device_state").u64("device", id);
    match s {
        ConnState::Connecting => o.str("state", "connecting"),
        ConnState::Connected => o.str("state", "connected"),
        ConnState::Backoff { attempt, delay } => o
            .str("state", "backoff")
            .u64("attempt", u64::from(*attempt))
            .u64("retry_ms", delay.as_millis() as u64),
        ConnState::LoginFailed {
            status,
            remain_times,
        } => {
            let o = o
                .str("state", "login_failed")
                .u64("status", u64::from(*status));
            match remain_times {
                Some(r) => o.u64("remain_times", u64::from(*r)),
                None => o,
            }
        }
        ConnState::Closed => o.str("state", "closed"),
    }
    .finish()
}

/// The `device_state` event for a device the app removed.
pub(crate) fn closed_event(id: u64) -> String {
    state_event(id, &ConnState::Closed)
}

fn channels_event(id: u64, slots: &[ChannelInfo], names: &HashMap<u8, VersionInfo>) -> String {
    let channels = slots.iter().map(|c| {
        let o = Object::new()
            .u64("channel", u64::from(c.channel))
            .bool("connected", c.connected)
            .strs("tiers", c.tiers.iter().map(|t| tier_name(*t)));
        match names.get(&c.channel) {
            Some(v) => o.str("name", &v.name).str("model", &v.model),
            None => o,
        }
    });
    Object::event("channels")
        .u64("device", id)
        .objects("channels", channels)
        .finish()
}

enum Info {
    Device(VersionInfo),
    Channels(Vec<VersionInfo>),
}

async fn device_info(session: Arc<DeviceSession>) -> Option<Info> {
    let r = session.query(Command::VersionInfo, None).await.ok()?;
    if r.status != 200 {
        return None;
    }
    xml::parse_version_info(&r.xml).ok().map(Info::Device)
}

/// Names and models of the given channels (command 318), one query at a
/// time; channels whose query fails are left out.
async fn channel_names(session: Arc<DeviceSession>, channels: Vec<u8>) -> Option<Info> {
    let mut found = Vec::new();
    for ch in channels {
        match session.query(Command::ChannelVersionInfo, Some(ch)).await {
            Ok(r) if r.status == 200 => {
                if let Ok(mut v) = xml::parse_version_info(&r.xml) {
                    v.channel = Some(ch);
                    found.push(v);
                }
            }
            Ok(r) => debug!(channel = ch, status = r.status, "channel info refused"),
            Err(e) => debug!(channel = ch, error = %e, "channel info failed"),
        }
    }
    Some(Info::Channels(found))
}

/// Turns session state into events until stopped: `device_state` on every
/// change, `device_info` after each login, and `channels` whenever the
/// device pushes its list, again once the channel names are known.
async fn watch(
    id: u64,
    session: Arc<DeviceSession>,
    events: EventSink,
    mut stop: oneshot::Receiver<()>,
) {
    let mut state_rx = session.watch_state();
    let mut channels_rx = session.watch_channels();
    let mut tasks: JoinSet<Option<Info>> = JoinSet::new();
    let mut slots: Option<Vec<ChannelInfo>> = None;
    let mut names: HashMap<u8, VersionInfo> = HashMap::new();
    let (mut state_open, mut channels_open) = (true, true);

    // The session may publish the same state twice in a row; report changes.
    let mut last: Option<ConnState> = None;
    let mut on_state = |s: &ConnState, tasks: &mut JoinSet<Option<Info>>| {
        if last.as_ref() == Some(s) {
            return;
        }
        last = Some(s.clone());
        events.send(state_event(id, s), Priority::State);
        if *s == ConnState::Connected {
            tasks.spawn(device_info(session.clone()));
        }
    };
    // Every device starts by connecting; report that even when the session
    // has already moved on by the time this task runs, so apps always see
    // `connecting` first.
    let first = state_rx.borrow_and_update().clone();
    if first != ConnState::Connecting {
        on_state(&ConnState::Connecting, &mut tasks);
    }
    on_state(&first, &mut tasks);

    loop {
        tokio::select! {
            _ = &mut stop => break,
            r = state_rx.changed(), if state_open => match r {
                Ok(()) => {
                    let s = state_rx.borrow_and_update().clone();
                    on_state(&s, &mut tasks);
                }
                Err(_) => state_open = false,
            },
            r = channels_rx.changed(), if channels_open => match r {
                Ok(()) => {
                    let list = channels_rx.borrow_and_update().clone();
                    if let Some(list) = list {
                        events.send(channels_event(id, &list, &names), Priority::State);
                        let wanted = list.iter().filter(|c| c.connected).map(|c| c.channel).collect();
                        tasks.spawn(channel_names(session.clone(), wanted));
                        slots = Some(list);
                    }
                }
                Err(_) => channels_open = false,
            },
            Some(done) = tasks.join_next(), if !tasks.is_empty() => match done {
                Ok(Some(Info::Device(v))) => {
                    let mut o = Object::event("device_info")
                        .u64("device", id)
                        .str("name", &v.name)
                        .str("model", &v.model);
                    if let Some(f) = &v.firmware {
                        o = o.str("firmware", f);
                    }
                    events.send(o.finish(), Priority::State);
                }
                Ok(Some(Info::Channels(found))) => {
                    let mut changed = false;
                    for v in found {
                        let Some(ch) = v.channel else { continue };
                        if names.get(&ch) != Some(&v) {
                            names.insert(ch, v);
                            changed = true;
                        }
                    }
                    if let (true, Some(list)) = (changed, &slots) {
                        events.send(channels_event(id, list, &names), Priority::State);
                    }
                }
                Ok(None) | Err(_) => {}
            },
        }
    }
    tasks.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_events() {
        let parse = |s: &ConnState| -> serde_json::Value {
            serde_json::from_str(&state_event(3, s)).unwrap()
        };
        let v = parse(&ConnState::Backoff {
            attempt: 3,
            delay: Duration::from_millis(8000),
        });
        assert_eq!(v["type"], "device_state");
        assert_eq!(v["device"], 3);
        assert_eq!(v["state"], "backoff");
        assert_eq!(v["attempt"], 3);
        assert_eq!(v["retry_ms"], 8000);
        let v = parse(&ConnState::LoginFailed {
            status: 401,
            remain_times: Some(2),
        });
        assert_eq!(v["state"], "login_failed");
        assert_eq!(v["status"], 401);
        assert_eq!(v["remain_times"], 2);
        let v = parse(&ConnState::LoginFailed {
            status: 401,
            remain_times: None,
        });
        assert!(v.get("remain_times").is_none());
    }

    #[test]
    fn channels_event_lists_slots_and_known_names() {
        let slots = vec![
            ChannelInfo {
                channel: 0,
                connected: true,
                tiers: vec![StreamTier::Main, StreamTier::Sub, StreamTier::Extern],
            },
            ChannelInfo {
                channel: 1,
                connected: false,
                tiers: vec![],
            },
        ];
        let mut names = HashMap::new();
        names.insert(
            0,
            VersionInfo {
                channel: Some(0),
                name: "Channel \"1\"".into(),
                model: "EXAMPLE-CAM".into(),
                firmware: None,
            },
        );
        let v: serde_json::Value =
            serde_json::from_str(&channels_event(1, &slots, &names)).unwrap();
        assert_eq!(v["type"], "channels");
        let c = &v["channels"];
        assert_eq!(c[0]["tiers"], serde_json::json!(["main", "sub", "extern"]));
        assert_eq!(c[0]["name"], "Channel \"1\"");
        assert_eq!(c[0]["model"], "EXAMPLE-CAM");
        assert_eq!(c[1]["connected"], false);
        assert!(c[1].get("name").is_none());
    }
}
