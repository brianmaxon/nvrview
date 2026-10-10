//! One device: its session and the task that turns session state into
//! control events.

use crate::events::{EventSink, Priority};
use crate::json::Object;
use crate::lens::{self, Camera, LensContext, LensProbes, ProbeJob, TileStarts};
use bc_protocol::detection::{self, AlarmState};
use bc_protocol::session::Command;
use bc_protocol::stream::StreamTier;
use bc_protocol::xml::{self, ChannelInfo, VersionInfo};
use bc_session::{AlarmPush, ConnState, DeviceSession, LensProbe};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::runtime::Handle;
use tokio::sync::{broadcast, oneshot};
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
    tiles: Arc<TileStarts>,
}

impl DeviceHandle {
    /// Starts watching `session`; must be called inside the runtime. No
    /// event is sent until `gate` resolves (or its sender is dropped), so
    /// none names the device before the app has its ID.
    pub(crate) fn start(
        id: u64,
        key: String,
        session: DeviceSession,
        events: EventSink,
        gate: oneshot::Receiver<()>,
        lens: LensContext,
    ) -> DeviceHandle {
        let session = Arc::new(session);
        let tiles = Arc::new(TileStarts::default());
        let (stop_tx, stop_rx) = oneshot::channel();
        let ctx = WatchCtx {
            id,
            key,
            session: session.clone(),
            events,
            lens,
            tiles: tiles.clone(),
        };
        let watcher = tokio::spawn(watch(ctx, stop_rx, gate));
        DeviceHandle {
            session: Some(session),
            stop: Some(stop_tx),
            watcher: Some(watcher),
            tiles,
        }
    }

    pub(crate) fn session(&self) -> Option<Arc<DeviceSession>> {
        self.session.clone()
    }

    /// A tile is starting a stream on `channel` (opened, or switching
    /// tiers): a second-lens probe of that channel waits a little.
    pub(crate) fn tile_starting(&self, channel: u8) {
        self.tiles.note(channel);
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
            unlock_secs,
        } => {
            let o = o
                .str("state", "login_failed")
                .u64("status", u64::from(*status))
                .str("reason", bc_protocol::xml::login_status_meaning(*status));
            let o = match remain_times {
                Some(r) => o.u64("remain_times", u64::from(*r)),
                None => o,
            };
            match unlock_secs {
                Some(s) => o.u64("unlock_s", u64::from(*s)),
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

/// What one channel detects (from 199).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ChannelDetects {
    /// Every kind the camera advertises: the item's own `aitype` and its
    /// lenses' together.
    kinds: Vec<&'static str>,
    /// Per lens, for a camera with two or more lenses (`subItem`s); empty
    /// for a single-lens camera.
    lenses: Vec<Vec<&'static str>>,
}

impl ChannelDetects {
    fn from_ai(c: &detection::ChannelAi) -> ChannelDetects {
        let all = c.lenses.iter().fold(c.aitype, |a, l| a | l.aitype);
        ChannelDetects {
            kinds: detection::ai_kinds(all),
            lenses: if c.lenses.len() >= 2 {
                c.lenses
                    .iter()
                    .map(|l| detection::ai_kinds(l.aitype))
                    .collect()
            } else {
                Vec::new()
            },
        }
    }
}

/// What each channel detects (from 199), once known.
type Detects = HashMap<u8, ChannelDetects>;

fn channels_event(
    id: u64,
    slots: &[ChannelInfo],
    names: &HashMap<u8, VersionInfo>,
    detects: Option<&Detects>,
    probed: &HashMap<u8, u8>,
) -> String {
    let channels = slots.iter().map(|c| {
        let o = Object::new()
            .u64("channel", u64::from(c.channel))
            .bool("connected", c.connected)
            .strs("tiers", c.tiers.iter().map(|t| tier_name(*t)));
        let o = match names.get(&c.channel) {
            Some(v) => o.str("name", &v.name).str("model", &v.model),
            None => o,
        };
        let cd = detects.and_then(|d| d.get(&c.channel));
        let o = match detects {
            Some(_) => o.strs(
                "detects",
                cd.into_iter().flat_map(|x| x.kinds.iter().copied()),
            ),
            None => o,
        };
        match (cd, probed.get(&c.channel)) {
            (Some(x), _) if x.lenses.len() >= 2 => o
                .u64("lenses", x.lenses.len() as u64)
                .str_arrays("lens_detects", x.lenses.iter().map(|l| l.iter().copied())),
            // Found by a probe: what each lens detects is unknown, so
            // `lens_detects` is left out.
            (_, Some(n)) if *n >= 2 => o.u64("lenses", u64::from(*n)),
            _ => o,
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
    /// The capabilities, or `None` when they could not be read.
    Detects(Option<Detects>),
    Probe(ProbeJob, LensProbe),
}

async fn device_info(session: Arc<DeviceSession>) -> Option<Info> {
    let r = session.query(Command::VersionInfo, None).await.ok()?;
    if r.status != 200 {
        return None;
    }
    xml::parse_version_info(&r.xml).ok().map(Info::Device)
}

/// What each channel (and each lens of a multi-lens camera) detects, from
/// the `aitype` bits of the capabilities (command 199).
async fn channel_detects(session: Arc<DeviceSession>) -> Option<Info> {
    Some(Info::Detects(read_detects(session).await))
}

async fn read_detects(session: Arc<DeviceSession>) -> Option<Detects> {
    let r = session.query(Command::Support, None).await.ok()?;
    if r.status != 200 && r.status != 300 {
        debug!(status = r.status, "capabilities refused");
        return None;
    }
    let ai = detection::parse_channel_ai(&r.xml).ok()?;
    Some(
        ai.into_iter()
            .map(|c| (c.channel, ChannelDetects::from_ai(&c)))
            .collect(),
    )
}

/// The channels that need a second-lens probe (docs/protocol.md, "Second
/// lenses"): connected, not listing `externStream`, and not already shown
/// as multi-lens by the capabilities; each with its camera's identity.
fn lens_candidates(
    slots: &[ChannelInfo],
    names: &HashMap<u8, VersionInfo>,
    detects: Option<&Detects>,
    uids: &HashMap<u8, String>,
) -> Vec<ProbeJob> {
    slots
        .iter()
        .filter(|c| c.connected && !c.tiers.contains(&StreamTier::Extern))
        .filter(|c| {
            !detects
                .and_then(|d| d.get(&c.channel))
                .is_some_and(|d| d.lenses.len() >= 2)
        })
        .map(|c| ProbeJob {
            channel: c.channel,
            camera: Camera {
                uid: uids.get(&c.channel).filter(|u| !u.is_empty()).cloned(),
                model: names.get(&c.channel).map(|v| v.model.clone()),
                name: names.get(&c.channel).map(|v| v.name.clone()),
            },
        })
        .collect()
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

/// One detection-related event (docs/FFI.md, "Events").
fn detection_event(
    device: u64,
    slot: (u8, u8),
    what: Change<'_>,
    start: bool,
    at_ns: u64,
) -> String {
    let (kind_type, kind) = match what {
        Change::Detection(k) => ("detection", Some(k)),
        Change::Motion => ("motion", None),
        Change::Alarm(k) => ("alarm", Some(k)),
    };
    let mut o = Object::event(kind_type)
        .u64("device", device)
        .u64("channel", u64::from(slot.0));
    if slot.1 != 0 {
        o = o.u64("lens", u64::from(slot.1));
    }
    if let Some(k) = kind {
        o = o.str("kind", k);
    }
    o = o.str("state", if start { "start" } else { "stop" });
    if matches!(what, Change::Detection(_)) {
        o = o.str("source", "device");
    }
    o.u64("at_ns", at_ns).finish()
}

#[derive(Clone, Copy)]
enum Change<'a> {
    /// An `AItype` name (lowercase).
    Detection(&'a str),
    /// `MD` in `status`.
    Motion,
    /// Another `status` name, as the device wrote it.
    Alarm(&'a str),
}

/// What is active on one slot, normalized for comparison.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SlotActive {
    detections: Vec<String>,
    motion: bool,
    alarms: Vec<String>,
}

impl SlotActive {
    fn from_state(s: &AlarmState) -> SlotActive {
        SlotActive {
            detections: s.ai_types.iter().map(|k| k.to_ascii_lowercase()).collect(),
            motion: s.motion(),
            alarms: s
                .status
                .iter()
                .filter(|n| !n.eq_ignore_ascii_case("MD"))
                .cloned()
                .collect(),
        }
    }

    fn is_idle(&self) -> bool {
        *self == SlotActive::default()
    }
}

fn contains(list: &[String], name: &str) -> bool {
    list.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// Turns the device's alarm-state snapshots (command 33) into start and stop
/// events (docs/protocol.md, "How nvrview reads the pushes").
#[derive(Default)]
struct Detections {
    connection: Option<u64>,
    active: BTreeMap<(u8, u8), SlotActive>,
}

impl Detections {
    /// Events for the differences between the last snapshot and `p`.
    fn push(&mut self, device: u64, p: &AlarmPush, at_ns: u64) -> Vec<String> {
        let mut out = Vec::new();
        if self.connection != Some(p.connection) {
            // A new connection's snapshot starts afresh.
            out = self.stop_all(device, at_ns);
            self.connection = Some(p.connection);
        }
        for st in &p.states {
            let slot = (st.channel, st.sub_channel);
            let new = SlotActive::from_state(st);
            let old = self.active.get(&slot).cloned().unwrap_or_default();
            if new == old {
                continue;
            }
            Self::diff(device, slot, &old, &new, at_ns, &mut out);
            if new.is_idle() {
                self.active.remove(&slot);
            } else {
                self.active.insert(slot, new);
            }
        }
        out
    }

    /// Stops everything active (the session dropped).
    fn stop_all(&mut self, device: u64, at_ns: u64) -> Vec<String> {
        let mut out = Vec::new();
        for (slot, old) in std::mem::take(&mut self.active) {
            Self::diff(device, slot, &old, &SlotActive::default(), at_ns, &mut out);
        }
        out
    }

    /// Stops first, then starts.
    fn diff(
        device: u64,
        slot: (u8, u8),
        old: &SlotActive,
        new: &SlotActive,
        at_ns: u64,
        out: &mut Vec<String>,
    ) {
        let mut ev = |what, start| out.push(detection_event(device, slot, what, start, at_ns));
        for k in old
            .detections
            .iter()
            .filter(|k| !contains(&new.detections, k))
        {
            ev(Change::Detection(k), false);
        }
        if old.motion && !new.motion {
            ev(Change::Motion, false);
        }
        for k in old.alarms.iter().filter(|k| !contains(&new.alarms, k)) {
            ev(Change::Alarm(k), false);
        }
        for k in new
            .detections
            .iter()
            .filter(|k| !contains(&old.detections, k))
        {
            ev(Change::Detection(k), true);
        }
        if new.motion && !old.motion {
            ev(Change::Motion, true);
        }
        for k in new.alarms.iter().filter(|k| !contains(&old.alarms, k)) {
            ev(Change::Alarm(k), true);
        }
    }
}

/// What a device's watcher works with.
struct WatchCtx {
    id: u64,
    key: String,
    session: Arc<DeviceSession>,
    events: EventSink,
    lens: LensContext,
    tiles: Arc<TileStarts>,
}

/// Turns session state into events until stopped: `device_state` on every
/// change, `device_info` after each login, and `channels` whenever the
/// device pushes its list, again once the channel names are known, and
/// again when a second-lens probe finds a lens. Probes run one at a time,
/// once the channel list, names, and capabilities are in.
async fn watch(ctx: WatchCtx, mut stop: oneshot::Receiver<()>, gate: oneshot::Receiver<()>) {
    tokio::select! {
        biased;
        _ = &mut stop => return,
        _ = gate => {}
    }
    let WatchCtx {
        id,
        key,
        session,
        events,
        lens,
        tiles,
    } = ctx;
    let mut state_rx = session.watch_state();
    let mut channels_rx = session.watch_channels();
    let mut alarms_rx = session.alarms();
    let mut tasks: JoinSet<Option<Info>> = JoinSet::new();
    let mut slots: Option<Vec<ChannelInfo>> = None;
    let mut names: HashMap<u8, VersionInfo> = HashMap::new();
    let mut detects: Option<Detects> = None;
    let mut detections = Detections::default();
    let timing = lens.timing;
    let mut probes = LensProbes::new(key.clone(), lens);
    // Name and capability queries in flight; probes wait for them.
    let (names_pending, detects_pending) = (AtomicU32::new(0), AtomicU32::new(0));
    let add = |c: &AtomicU32| c.fetch_add(1, Ordering::Relaxed);
    // Only this task changes them; atomics only because the future must be
    // `Send`.
    let finished = |c: &AtomicU32| {
        c.store(
            c.load(Ordering::Relaxed).saturating_sub(1),
            Ordering::Relaxed,
        )
    };
    let (mut state_open, mut channels_open, mut alarms_open) = (true, true, true);
    let send_all = |list: Vec<String>| {
        for e in list {
            events.send(e, Priority::State);
        }
    };

    // The session may publish the same state twice in a row; report changes.
    let mut last: Option<ConnState> = None;
    let mut on_state =
        |s: &ConnState, tasks: &mut JoinSet<Option<Info>>, detections: &mut Detections| {
            if last.as_ref() == Some(s) {
                return false;
            }
            last = Some(s.clone());
            if *s != ConnState::Connected {
                // Whatever was active is unknown now; the next connection's
                // first push is a full snapshot.
                send_all(detections.stop_all(id, crate::clock::now_ns()));
            }
            events.send(state_event(id, s), Priority::State);
            if *s == ConnState::Connected {
                tasks.spawn(device_info(session.clone()));
                add(&detects_pending);
                tasks.spawn(channel_detects(session.clone()));
            }
            true
        };
    // Every device starts by connecting; report that even when the session
    // has already moved on by the time this task runs, so apps always see
    // `connecting` first.
    let first = state_rx.borrow_and_update().clone();
    if first != ConnState::Connecting {
        on_state(&ConnState::Connecting, &mut tasks, &mut detections);
    }
    on_state(&first, &mut tasks, &mut detections);

    // Plans and starts second-lens probes once everything they depend on is
    // in; sends `channels` when what the app should see changed.
    let advance = |probes: &mut LensProbes,
                   tasks: &mut JoinSet<Option<Info>>,
                   slots: &Option<Vec<ChannelInfo>>,
                   names: &HashMap<u8, VersionInfo>,
                   detects: &Option<Detects>| {
        let Some(list) = slots else { return };
        if names_pending.load(Ordering::Relaxed) > 0
            || detects_pending.load(Ordering::Relaxed) > 0
            || session.state() != ConnState::Connected
        {
            return;
        }
        let candidates = lens_candidates(list, names, detects.as_ref(), &session.channel_uids());
        if probes.plan(candidates) {
            events.send(
                channels_event(id, list, names, detects.as_ref(), probes.known()),
                Priority::State,
            );
        }
        if let Some(job) = probes.next() {
            let (s, t) = (session.clone(), tiles.clone());
            tasks.spawn(async move {
                let (job, r) = lens::run_probe(s, t, timing, job).await;
                Some(Info::Probe(job, r))
            });
        }
    };

    loop {
        tokio::select! {
            _ = &mut stop => {
                // The device is being removed: nothing stays active.
                send_all(detections.stop_all(id, crate::clock::now_ns()));
                break;
            }
            r = state_rx.changed(), if state_open => match r {
                Ok(()) => {
                    let s = state_rx.borrow_and_update().clone();
                    if on_state(&s, &mut tasks, &mut detections) {
                        probes.reset_connection();
                    }
                }
                Err(_) => state_open = false,
            },
            r = channels_rx.changed(), if channels_open => match r {
                Ok(()) => {
                    let list = channels_rx.borrow_and_update().clone();
                    if let Some(list) = list {
                        events.send(
                            channels_event(id, &list, &names, detects.as_ref(), probes.known()),
                            Priority::State,
                        );
                        let wanted = list.iter().filter(|c| c.connected).map(|c| c.channel).collect();
                        add(&names_pending);
                        tasks.spawn(channel_names(session.clone(), wanted));
                        // Cameras may have changed: read their abilities again.
                        if slots.is_some() {
                            add(&detects_pending);
                            tasks.spawn(channel_detects(session.clone()));
                        }
                        slots = Some(list);
                    }
                }
                Err(_) => channels_open = false,
            },
            r = alarms_rx.recv(), if alarms_open => match r {
                Ok(push) => send_all(detections.push(id, &push, crate::clock::now_ns())),
                // Missed some pushes: the next one is a full snapshot.
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    debug!(device = id, missed = n, "alarm pushes skipped");
                }
                Err(broadcast::error::RecvError::Closed) => alarms_open = false,
            },
            Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                match done {
                    Ok(Some(Info::Device(v))) => {
                        let mut o = Object::event("device_info")
                            .u64("device", id)
                            .str("key", &key)
                            .str("name", &v.name)
                            .str("model", &v.model);
                        if let Some(f) = &v.firmware {
                            o = o.str("firmware", f);
                        }
                        events.send(o.finish(), Priority::State);
                    }
                    Ok(Some(Info::Channels(found))) => {
                        finished(&names_pending);
                        let mut changed = false;
                        for v in found {
                            let Some(ch) = v.channel else { continue };
                            if names.get(&ch) != Some(&v) {
                                names.insert(ch, v);
                                changed = true;
                            }
                        }
                        if let (true, Some(list)) = (changed, &slots) {
                            events.send(
                                channels_event(id, list, &names, detects.as_ref(), probes.known()),
                                Priority::State,
                            );
                        }
                    }
                    Ok(Some(Info::Detects(d))) => {
                        finished(&detects_pending);
                        if let Some(d) = d
                            && detects.as_ref() != Some(&d)
                        {
                            detects = Some(d);
                            if let Some(list) = &slots {
                                events.send(
                                    channels_event(id, list, &names, detects.as_ref(), probes.known()),
                                    Priority::State,
                                );
                            }
                        }
                    }
                    Ok(Some(Info::Probe(job, r))) => probes.finish(&job, r),
                    Ok(None) | Err(_) => {}
                }
                advance(&mut probes, &mut tasks, &slots, &names, &detects);
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
            unlock_secs: None,
        });
        assert_eq!(v["state"], "login_failed");
        assert_eq!(v["status"], 401);
        assert_eq!(v["reason"], "wrong user name or password");
        assert_eq!(v["remain_times"], 2);
        assert!(v.get("unlock_s").is_none());
        let v = parse(&ConnState::LoginFailed {
            status: 407,
            remain_times: None,
            unlock_secs: Some(300),
        });
        assert!(v.get("remain_times").is_none());
        assert_eq!(v["reason"], "account locked");
        assert_eq!(v["unlock_s"], 300);
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
            serde_json::from_str(&channels_event(1, &slots, &names, None, &HashMap::new()))
                .unwrap();
        assert_eq!(v["type"], "channels");
        let c = &v["channels"];
        assert_eq!(c[0]["tiers"], serde_json::json!(["main", "sub", "extern"]));
        assert_eq!(c[0]["name"], "Channel \"1\"");
        assert_eq!(c[0]["model"], "EXAMPLE-CAM");
        assert_eq!(c[1]["connected"], false);
        assert!(c[1].get("name").is_none());
        assert!(c[0].get("detects").is_none());
        let mut detects = HashMap::new();
        detects.insert(
            0,
            ChannelDetects {
                kinds: vec!["people", "vehicle"],
                lenses: Vec::new(),
            },
        );
        let v: serde_json::Value = serde_json::from_str(&channels_event(
            1,
            &slots,
            &names,
            Some(&detects),
            &HashMap::new(),
        ))
        .unwrap();
        let c = &v["channels"];
        assert_eq!(c[0]["detects"], serde_json::json!(["people", "vehicle"]));
        assert!(c[0].get("lenses").is_none() && c[0].get("lens_detects").is_none());
        assert_eq!(c[1]["detects"], serde_json::json!([]));

        // A multi-lens camera: every kind of the camera, and per lens.
        let ai = detection::ChannelAi {
            channel: 1,
            aitype: 1 | 2,
            lenses: vec![
                detection::LensAi {
                    lens: 0,
                    aitype: 1 | 2 | 4,
                },
                detection::LensAi {
                    lens: 1,
                    aitype: 1 | 16,
                },
            ],
        };
        detects.insert(1, ChannelDetects::from_ai(&ai));
        let v: serde_json::Value = serde_json::from_str(&channels_event(
            1,
            &slots,
            &names,
            Some(&detects),
            &HashMap::new(),
        ))
        .unwrap();
        let c = &v["channels"];
        assert_eq!(c[1]["lenses"], 2);
        assert_eq!(
            c[1]["detects"],
            serde_json::json!(["people", "vehicle", "dog_cat"])
        );
        assert_eq!(
            c[1]["lens_detects"],
            serde_json::json!([["people", "vehicle"], ["dog_cat"]])
        );
        // One sub-item only: not a multi-lens camera.
        let one = detection::ChannelAi {
            lenses: ai.lenses[..1].to_vec(),
            ..ai.clone()
        };
        assert!(ChannelDetects::from_ai(&one).lenses.is_empty());
    }

    fn push(connection: u64, states: Vec<(u8, u8, &str, &str)>) -> AlarmPush {
        let text = |v: &str| {
            v.split(',')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        };
        AlarmPush {
            connection,
            states: states
                .into_iter()
                .map(|(channel, sub_channel, status, ai)| AlarmState {
                    channel,
                    sub_channel,
                    status: text(status),
                    ai_types: text(ai),
                })
                .collect(),
        }
    }

    fn parsed(list: Vec<String>) -> Vec<serde_json::Value> {
        list.iter()
            .map(|e| serde_json::from_str(e).unwrap())
            .collect()
    }

    #[test]
    fn snapshots_become_starts_and_stops() {
        let mut d = Detections::default();
        assert!(
            d.push(1, &push(1, vec![(0, 0, "", ""), (1, 0, "", "")]), 5)
                .is_empty()
        );
        // Motion, then a person while motion continues (an observed order).
        let e = parsed(d.push(1, &push(1, vec![(0, 0, "", ""), (1, 0, "MD", "")]), 10));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0]["type"], "motion");
        assert_eq!(e[0]["device"], 1);
        assert_eq!(e[0]["channel"], 1);
        assert_eq!(e[0]["state"], "start");
        assert_eq!(e[0]["at_ns"], 10);
        assert!(e[0].get("lens").is_none());
        let e = parsed(d.push(1, &push(1, vec![(1, 0, "MD", "People")]), 20));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0]["type"], "detection");
        assert_eq!(e[0]["kind"], "people");
        assert_eq!(e[0]["source"], "device");
        assert_eq!(e[0]["state"], "start");
        // A slot left out of a push keeps its state.
        assert!(d.push(1, &push(1, vec![(0, 0, "", "")]), 25).is_empty());
        // Motion ends but the person stays; then the person goes.
        let e = parsed(d.push(1, &push(1, vec![(1, 0, "", "people")]), 30));
        assert_eq!(
            (e.len(), &e[0]["type"], &e[0]["state"]),
            (1, &"motion".into(), &"stop".into())
        );
        let e = parsed(d.push(1, &push(1, vec![(1, 0, "", "")]), 40));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0]["kind"], "people");
        assert_eq!(e[0]["state"], "stop");
        // Second lens, two kinds and a doorbell press at once.
        let e = parsed(d.push(1, &push(1, vec![(2, 1, "visitor", "dog_cat,people")]), 50));
        assert_eq!(e.len(), 3);
        assert!(e.iter().all(|v| v["lens"] == 1 && v["state"] == "start"));
        assert_eq!(e[2]["type"], "alarm");
        assert_eq!(e[2]["kind"], "visitor");
        // A new connection: what was active stops, then the new snapshot.
        let e = parsed(d.push(1, &push(2, vec![(2, 1, "", "people")]), 60));
        let stops = e.iter().filter(|v| v["state"] == "stop").count();
        let starts = e.iter().filter(|v| v["state"] == "start").count();
        assert_eq!((stops, starts), (3, 1));
        let e = parsed(d.stop_all(1, 70));
        assert_eq!(e.len(), 1);
        assert_eq!(e[0]["kind"], "people");
        assert_eq!(e[0]["state"], "stop");
        assert!(d.stop_all(1, 80).is_empty());
    }
}
