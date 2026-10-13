//! Smart alerts in the core (docs/NOTIFICATIONS.md, docs/FFI.md "Smart
//! alerts"): the service that owns a [`alerts::Engine`], takes in
//! triggers, wakes cameras and collects pictures, asks the app to check
//! them, and records, delivers, and reports the decisions.
//!
//! - **Triggers**: the core's own `detection`, `motion`, and `alarm` events
//!   (device AI and status, local sound alarms) through a tap on the event
//!   queue; external sources through [`ExternalTriggers`] (MQTT, a webhook);
//!   and the app ([`crate::Core::alerts_trigger_put`]).
//! - **Pictures**: a check's camera is woken by subscribing to its main
//!   stream on the existing session (viewing tier only; a tile already
//!   showing that stream shares it). The first keyframe and a picture about
//!   1 s later are kept as access units and announced in `verify_request`
//!   events; the app fetches them ([`crate::Core::alert_frames_get`]),
//!   decodes, runs its detector, and answers with boxes
//!   ([`crate::Core::alert_detections_put`]). A request with no answer in
//!   time counts as "could not look" (fail open for always-alert cases).
//! - **Bounds**: cameras woken at once per device (settings, default 4) and
//!   per process; more wait, ordered by priority. Requests, bytes, the GOP
//!   per stream, the log, and the snapshot cache are capped. A woken
//!   stream stops when its checks end and, after an alert, when the
//!   episode settles (or its re-check limit passes).
//! - **Lifecycle**: starts with the core, idle until configured; survives
//!   device reconnects (subscriptions restart with the session); stops
//!   every stream, input, and output on shutdown.

mod api;
mod diag;
mod evidence;
mod frames;
mod modules;
mod seams;
mod store;

pub use modules::{home_assistant_factory, mqtt_factory};
pub use seams::{
    AlertError, AlertFrames, AlertInput, AlertNotice, AlertOutput, DeliveryDone, ExternalTrigger,
    ExternalTriggers, InputContext, InputFactory, NoticeState, OutputContext, OutputFactory,
    RecordingOutput, Secret, TestDone, TestResult, TriggerRejected,
};

use crate::clock::HostClock;
use crate::events::{EventSink, Priority};
use alerts::decision::{Decision, Outcome};
use alerts::engine::{DecideInputs, Engine, Next, Now};
use alerts::episode::{EpisodeKey, EpisodeState};
use alerts::model::{CameraId, Class, Severity, Source};
use alerts::settings::OutputKind;
use alerts::trace::{GateResult, Step, StepKind};
use alerts::trigger::{ExternalBody, Trigger, TriggerState, from_detection, from_external};
use alerts::verify::{Detection, Sample};
use alerts::zone::BBox;
use bc_protocol::stream::StreamTier;
use bc_session::DeviceSession;
use evidence::EvidenceStore;
use frames::{Bundle, Note, Want};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use store::{AlertLog, FeedbackStore, SnapshotCache};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{debug, info, warn};

/// Triggers waiting for the service.
pub(crate) const INTAKE_CAPACITY: usize = 256;
/// Verification requests waiting for an answer at once.
pub(crate) const MAX_REQUESTS: usize = 64;
/// Bytes of frames held for requests at once.
pub(crate) const MAX_REQUEST_BYTES: usize = 64 << 20;
/// Longest an output may take to report a delivery.
pub(crate) const DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest a notice waits for the app's snapshot when an output wants one.
pub(crate) const SNAPSHOT_WAIT: Duration = Duration::from_secs(2);
/// Most notices waiting for delivery or a result at once.
const MAX_DELIVERIES: usize = 256;
/// External triggers per source: a burst of this many ...
const RATE_BURST: f64 = 20.0;
/// ... refilled at 10 a minute.
const RATE_PER_MS: f64 = 10.0 / 60_000.0;
/// How often the engine settles episodes and decides abandoned checks.
const TICK: Duration = Duration::from_secs(1);
/// Most detections taken from one answer.
const MAX_DETECTIONS: usize = 256;
/// Longest result line an output may add to a `delivery` step.
const SUMMARY_MAX: usize = 200;
/// Longest summary of a quiet or suppressed decision in an `alert` event.
const EVENT_SUMMARY_MAX: usize = 200;
/// Most trigger inputs and outputs configured.
const MAX_PLUGINS: usize = 16;
/// Simulated triggers remembered as tests.
pub(crate) const MAX_TESTS: usize = 256;

/// Host-side tunables of picture acquisition (`acquisition` in the
/// settings document).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Acquisition {
    /// How long the app has to answer a verification request.
    pub(crate) answer_timeout: Duration,
    /// Without a first picture this long after waking, a cover image of
    /// the last seconds is asked of the recording instead.
    pub(crate) first_picture_timeout: Duration,
    /// The second picture's distance from the first.
    pub(crate) second_picture: Duration,
    /// Later pictures' distance (external checks).
    pub(crate) picture_interval: Duration,
    /// Pictures per camera for a camera's own trigger.
    pub(crate) camera_pictures: u8,
    /// Most pictures per camera for an external trigger.
    pub(crate) sensor_pictures: u8,
    /// Cameras woken at once across all devices.
    pub(crate) max_wakes: usize,
    /// Fall back to a cover image when the stream gives no picture.
    pub(crate) cover_fallback: bool,
    /// Re-check alerted episodes ("still present") while they last.
    pub(crate) recheck: bool,
}

impl Default for Acquisition {
    fn default() -> Self {
        Acquisition {
            answer_timeout: Duration::from_secs(5),
            first_picture_timeout: Duration::from_secs(3),
            second_picture: Duration::from_millis(1000),
            picture_interval: Duration::from_millis(2000),
            camera_pictures: 2,
            sensor_pictures: 4,
            max_wakes: 8,
            cover_fallback: true,
            recheck: true,
        }
    }
}

/// A trigger on its way to the service.
pub(crate) enum Intake {
    /// From the core's own events.
    Camera {
        source: Source,
        camera: CameraId,
        kind: String,
        state: TriggerState,
        received_ms: i64,
    },
    /// From an input module.
    External {
        trigger: ExternalTrigger,
        received_ms: i64,
    },
    /// From the app, already normalized and numbered.
    Full(Trigger),
}

/// One check being sampled.
struct CheckRun {
    classes: Vec<Class>,
    min_score: f32,
    /// The engine's sampling window end (UTC ms) and its Instant.
    until_ms: i64,
    /// Pictures still wanted, per camera.
    want: BTreeMap<CameraId, u8>,
    /// Requests sent and not answered.
    outstanding: BTreeSet<String>,
    /// Decided at the latest by then.
    deadline: Instant,
    reached: bool,
    /// A region's bounding box per camera (a crop for the detector).
    roi: BTreeMap<CameraId, [f32; 4]>,
}

/// A "still present" re-check of an alerted camera.
struct Hold {
    classes: BTreeSet<Class>,
    until: Instant,
    next: Instant,
    pending: Option<String>,
}

/// One woken camera.
struct WakeRun {
    generation: u64,
    task: JoinHandle<()>,
    cover: Option<JoinHandle<()>>,
    started: Instant,
    first_picture: Option<Instant>,
    failed: bool,
    cover_tried: bool,
    checks: BTreeSet<u64>,
    hold: Option<Hold>,
    /// The stream this wake reads: the camera's main stream for checks,
    /// the low-quality one for the re-checks of an alerted camera.
    tier: StreamTier,
    /// The low-quality stream was refused: re-checks stay on the main one.
    low_failed: bool,
    want: Want,
    last_pts: Option<u64>,
    pictures: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Check,
    Recheck,
}

/// A verification request waiting for the app's boxes.
struct Request {
    camera: CameraId,
    purpose: Purpose,
    checks: Vec<u64>,
    at_ms: i64,
    deadline: Instant,
    bundle: Bundle,
}

/// One configured output.
struct OutputSlot {
    index: u32,
    kind: OutputKind,
    raw: Value,
    secret: Option<Secret>,
    output: Arc<dyn AlertOutput>,
}

/// One configured input.
struct InputSlot {
    raw: Value,
    secret: Option<Secret>,
    input: Box<dyn AlertInput>,
}

/// A notice waiting for its snapshot or for an output's result.
struct Delivery {
    trigger_id: u64,
    slot: usize,
    notice: AlertNotice,
    /// Delivered at this time (after the snapshot wait).
    due: Instant,
    sent: bool,
    done: Arc<AtomicBool>,
    timeout: Instant,
}

#[derive(Clone, Copy)]
struct Alerted {
    alert_id: u64,
    rev: u32,
    severity: Severity,
    trigger_id: u64,
}

struct Bucket {
    tokens: f64,
    at_ms: i64,
}

#[derive(Default)]
struct Factories {
    inputs: HashMap<String, InputFactory>,
    outputs: HashMap<String, OutputFactory>,
}

struct State {
    enabled: bool,
    engine: Option<Engine>,
    offset_min: i32,
    acq: Acquisition,
    sessions: HashMap<u64, (Arc<DeviceSession>, String)>,
    checks: BTreeMap<u64, CheckRun>,
    wakes: BTreeMap<CameraId, WakeRun>,
    /// Cameras waiting for a free slot: (priority, order, camera, check).
    waiting: Vec<(u8, u64, CameraId, u64)>,
    requests: HashMap<String, Request>,
    request_bytes: usize,
    next_request: u64,
    next_generation: u64,
    next_order: u64,
    alerted: BTreeMap<EpisodeKey, Alerted>,
    deliveries: Vec<Delivery>,
    outputs: Vec<OutputSlot>,
    inputs: Vec<InputSlot>,
    buckets: HashMap<String, Bucket>,
    mutes: BTreeMap<CameraId, i64>,
    log: AlertLog,
    snapshots: SnapshotCache,
    evidence: EvidenceStore,
    feedback: FeedbackStore,
    /// Triggers started by "simulate trigger" (the newest [`MAX_TESTS`]),
    /// with whether a test may also go to Home Assistant.
    tests: BTreeMap<u64, bool>,
    /// The alerts those triggers raised, with the same flag.
    test_alerts: BTreeMap<u64, bool>,
    next_alert_id: u64,
    last_tick: Option<Instant>,
}

/// The shared part of the hub.
pub(crate) struct Inner {
    /// This hub, for tasks it starts.
    me: std::sync::Weak<Inner>,
    runtime: tokio::runtime::Handle,
    events: EventSink,
    clock: HostClock,
    state: Mutex<State>,
    wake: Notify,
    intake: mpsc::Sender<Intake>,
    next_trigger: AtomicU64,
    closed: AtomicBool,
    factories: Mutex<Factories>,
}

/// Owned by the core.
pub(crate) struct AlertHub {
    inner: Arc<Inner>,
    service: Mutex<Option<(oneshot::Sender<()>, JoinHandle<()>)>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Work done after the state lock is released (it may call into modules).
enum Effect {
    Deliver(Arc<dyn AlertOutput>, AlertNotice, DeliveryDone),
}

/// A result line from an output, cleaned for the trace: control characters
/// removed, at most [`SUMMARY_MAX`] characters.
fn clean_summary(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(SUMMARY_MAX)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn camera_json(c: CameraId) -> Value {
    json!({"device": c.device, "channel": c.channel, "lens": c.lens})
}

fn class_name(c: Class) -> String {
    serde_json::to_value(c)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Maps a detector's label (COCO-style or ours) to an alert class; labels
/// the engine has no class for are dropped.
pub(crate) fn class_of_label(label: &str) -> Option<Class> {
    Some(match label.trim().to_ascii_lowercase().as_str() {
        "person" | "people" => Class::Person,
        "vehicle" | "car" | "truck" | "bus" | "motorcycle" => Class::Vehicle,
        "animal" | "dog" | "cat" | "dog_cat" => Class::Animal,
        _ => return None,
    })
}

impl AlertHub {
    pub(crate) fn new(
        runtime: tokio::runtime::Handle,
        events: EventSink,
        clock: HostClock,
        data_dir: Option<PathBuf>,
    ) -> AlertHub {
        let dir = data_dir.map(|d| d.join("alerts"));
        let now_ms = clock.wall_now_us().div_euclid(1000);
        let log = AlertLog::open(dir.clone());
        let (high_trigger, high_alert) = log.highest_ids();
        let (tx, rx) = mpsc::channel(INTAKE_CAPACITY);
        let inner = Arc::new_cyclic(|me| Inner {
            me: me.clone(),
            runtime: runtime.clone(),
            events,
            clock,
            state: Mutex::new(State {
                enabled: false,
                engine: None,
                offset_min: 0,
                acq: Acquisition::default(),
                sessions: HashMap::new(),
                checks: BTreeMap::new(),
                wakes: BTreeMap::new(),
                waiting: Vec::new(),
                requests: HashMap::new(),
                request_bytes: 0,
                next_request: 1,
                next_generation: 1,
                next_order: 0,
                alerted: BTreeMap::new(),
                deliveries: Vec::new(),
                outputs: Vec::new(),
                inputs: Vec::new(),
                buckets: HashMap::new(),
                mutes: BTreeMap::new(),
                log,
                snapshots: SnapshotCache::open(dir.clone(), now_ms),
                evidence: EvidenceStore::open(dir.clone(), now_ms),
                tests: BTreeMap::new(),
                test_alerts: BTreeMap::new(),
                feedback: FeedbackStore::open(dir),
                next_alert_id: high_alert + 1,
                last_tick: None,
            }),
            wake: Notify::new(),
            intake: tx,
            next_trigger: AtomicU64::new(high_trigger + 1),
            closed: AtomicBool::new(false),
            factories: Mutex::new(modules::builtin()),
        });
        let (stop_tx, stop_rx) = oneshot::channel();
        let join = runtime.spawn(service(inner.clone(), rx, stop_rx));
        AlertHub {
            inner,
            service: Mutex::new(Some((stop_tx, join))),
        }
    }

    /// The function the event queue calls with every event: detections,
    /// motion, and alarms become triggers. Never blocks.
    pub(crate) fn tap(&self) -> Arc<dyn Fn(&str) + Send + Sync> {
        let tx = self.inner.intake.clone();
        let clock = self.inner.clock.clone();
        Arc::new(move |json: &str| {
            if !(json.starts_with(r#"{"type":"detection""#)
                || json.starts_with(r#"{"type":"motion""#)
                || json.starts_with(r#"{"type":"alarm""#))
            {
                return;
            }
            if let Some(i) = intake_of_event(json, clock.wall_now_us().div_euclid(1000))
                && tx.try_send(i).is_err()
            {
                debug!("alert intake full; a trigger was dropped");
            }
        })
    }

    pub(crate) fn device_added(&self, id: u64, key: &str, session: Arc<DeviceSession>) {
        lock(&self.inner.state)
            .sessions
            .insert(id, (session, key.to_owned()));
    }

    /// Lets go of a device's streams before its session closes.
    pub(crate) fn device_removed(&self, id: u64) {
        let mut st = lock(&self.inner.state);
        st.sessions.remove(&id);
        let cams: Vec<CameraId> = st
            .wakes
            .keys()
            .filter(|c| u64::from(c.device) == id)
            .copied()
            .collect();
        for c in cams {
            self.inner.stop_wake(&mut st, c);
        }
        drop(st);
        self.inner.wake.notify_one();
    }

    /// Stops the service: every stream, request, input, and output. Waits
    /// for the service task (which lets go of every subscription).
    pub(crate) fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        let Some((stop, join)) = lock(&self.service).take() else {
            return;
        };
        let _ = stop.send(());
        let _ = self.inner.runtime.block_on(join);
        // The sessions close next: let go of them.
        lock(&self.inner.state).sessions.clear();
    }
}

/// A trigger from one of the core's own events.
fn intake_of_event(json: &str, now_ms: i64) -> Option<Intake> {
    let v: Value = serde_json::from_str(json).ok()?;
    let ty = v["type"].as_str()?;
    let device = u32::try_from(v["device"].as_u64()?).ok()?;
    let channel = u16::try_from(v["channel"].as_u64()?).ok()?;
    let lens = u8::try_from(v["lens"].as_u64().unwrap_or(0)).ok()?;
    let kind = match ty {
        "motion" => "md".to_owned(),
        "detection" | "alarm" => v["kind"].as_str()?.to_owned(),
        _ => return None,
    };
    let state = match v["state"].as_str()? {
        "start" => TriggerState::Start,
        "stop" => TriggerState::Stop,
        _ => return None,
    };
    let source = match v["source"].as_str() {
        Some("local_audio") => Source::LocalAudio,
        _ => Source::Device,
    };
    Some(Intake::Camera {
        source,
        camera: CameraId::new(device, channel, lens),
        kind,
        state,
        received_ms: now_ms,
    })
}

/// The service task: triggers in, timers, and the pump.
async fn service(
    inner: Arc<Inner>,
    mut rx: mpsc::Receiver<Intake>,
    mut stop: oneshot::Receiver<()>,
) {
    info!("alert service started");
    loop {
        let next = inner.next_deadline();
        tokio::select! {
            biased;
            _ = &mut stop => break,
            m = rx.recv() => match m {
                Some(m) => inner.take(m),
                None => break,
            },
            () = inner.wake.notified() => {}
            () = tokio::time::sleep_until(next) => {}
        }
        // Drain what else is queued before pumping.
        while let Ok(m) = rx.try_recv() {
            inner.take(m);
        }
        inner.pump();
    }
    rx.close();
    inner.stop_all().await;
    info!("alert service stopped");
}

impl Inner {
    fn now(&self, st: &State) -> (Instant, Now) {
        let ms = self.clock.wall_now_us().div_euclid(1000);
        (Instant::now(), Now::new(ms, st.offset_min))
    }

    fn send(&self, v: Value) {
        self.events.send(v.to_string(), Priority::State);
    }

    fn step_event(&self, trigger_id: u64, step: &Step) {
        if let Ok(s) = serde_json::to_value(step) {
            self.send(json!({"type": "verify_step", "trigger_id": trigger_id, "step": s}));
        }
    }

    fn rejected(&self, source: &str, origin: Option<&str>, reason: &str) {
        let mut v = json!({"type": "trigger_rejected", "source": source, "reason": reason});
        if let Some(o) = origin {
            v["origin"] = json!(o);
        }
        self.send(v);
    }

    pub(crate) fn new_trigger_id(&self) -> u64 {
        self.next_trigger.fetch_add(1, Ordering::Relaxed)
    }

    /// One trigger from the intake queue.
    fn take(&self, m: Intake) {
        let mut st = lock(&self.state);
        if !st.enabled || st.engine.is_none() {
            return;
        }
        let trigger = match m {
            Intake::Camera {
                source,
                camera,
                kind,
                state,
                received_ms,
            } => {
                let id = self.new_trigger_id();
                let Some(t) = from_detection(id, source, camera, &kind, state, received_ms) else {
                    return;
                };
                t
            }
            Intake::External {
                trigger,
                received_ms,
            } => {
                // Rate limit per source: bursts of 20, then 10 a minute.
                let b = st.buckets.entry(trigger.source.clone()).or_insert(Bucket {
                    tokens: RATE_BURST,
                    at_ms: received_ms,
                });
                let gained = (received_ms - b.at_ms).max(0) as f64 * RATE_PER_MS;
                b.tokens = (b.tokens + gained).min(RATE_BURST);
                b.at_ms = received_ms;
                if b.tokens < 1.0 {
                    drop(st);
                    self.rejected("external", None, "rate_limit");
                    return;
                }
                b.tokens -= 1.0;
                if st.buckets.len() > 256 {
                    let oldest = st
                        .buckets
                        .iter()
                        .min_by_key(|(_, b)| b.at_ms)
                        .map(|(k, _)| k.clone());
                    if let Some(k) = oldest {
                        st.buckets.remove(&k);
                    }
                }
                let body = ExternalBody {
                    kind: trigger.kind.clone(),
                    state: trigger.state,
                    at_ms: trigger.at_ms,
                };
                let id = self.new_trigger_id();
                match from_external(id, &trigger.source, &body, received_ms) {
                    Ok(t) => t,
                    Err(e) => {
                        drop(st);
                        let reason = match e {
                            alerts::trigger::ExternalError::BadSource => "bad_source",
                            alerts::trigger::ExternalError::NoKind => "no_kind",
                        };
                        self.rejected("external", None, reason);
                        return;
                    }
                }
            }
            Intake::Full(t) => t,
        };
        self.process(&mut st, trigger);
    }

    /// Runs one trigger through the engine's intake.
    fn process(&self, st: &mut State, trigger: Trigger) {
        let (instant, now) = self.now(st);
        let source = trigger.source;
        let origin = trigger.origin.clone();
        let Some(engine) = st.engine.as_mut() else {
            return;
        };
        let intake = engine.intake(trigger.clone(), now);
        for s in &intake.steps {
            self.step_event(intake.trigger_id, s);
        }
        let always = intake.steps.iter().any(|s| {
            matches!(
                s.kind,
                StepKind::Gate {
                    result: GateResult::PassAlways { .. }
                }
            )
        });
        match intake.next {
            Next::Ignored => {}
            Next::Done(d) => {
                let mut fx = Vec::new();
                self.record(st, *d, false, &mut fx);
                self.run_later(fx);
            }
            Next::DecideNow => {
                let decided = st
                    .engine
                    .as_mut()
                    .and_then(|e| e.decide(intake.trigger_id, DecideInputs::default(), now));
                if let Some(d) = decided {
                    let mut fx = Vec::new();
                    self.record(st, d, true, &mut fx);
                    self.run_later(fx);
                }
            }
            Next::Extended {
                trigger_id,
                until_ms,
            } => {
                let acq = st.acq;
                if let Some(c) = st.checks.get_mut(&trigger_id) {
                    c.until_ms = c.until_ms.max(until_ms);
                    let left = Duration::from_millis(u64::try_from(until_ms - now.ms).unwrap_or(0));
                    c.deadline = c.deadline.max(instant + left + acq.answer_timeout + TICK);
                    for w in c.want.values_mut() {
                        *w = (*w).max(1);
                    }
                }
            }
            Next::Sample {
                cameras,
                from_ms: _,
                until_ms,
                classes,
            } => {
                let acq = st.acq;
                let external = source == Source::External;
                let per_camera = if external {
                    acq.sensor_pictures
                } else {
                    acq.camera_pictures
                };
                let settings = st.engine.as_ref().map(|e| e.settings().clone());
                let mapping = origin
                    .as_deref()
                    .and_then(|o| settings.as_ref().and_then(|s| s.source(o)))
                    .map(|(_, m)| m.clone());
                let min_score = mapping.as_ref().map_or_else(
                    || settings.as_ref().map_or(0.5, |s| s.verification.min_score),
                    |m| m.min_score,
                );
                let mut roi = BTreeMap::new();
                if let Some(m) = &mapping {
                    for c in &m.cameras {
                        if let Some(b) = polygon_box(&c.roi.0) {
                            roi.insert(c.camera, b);
                        }
                    }
                }
                let priority = if always {
                    3
                } else if external || classes.contains(&Class::Person) {
                    2
                } else {
                    1
                };
                let left = Duration::from_millis(u64::try_from(until_ms - now.ms).unwrap_or(0));
                let id = intake.trigger_id;
                st.checks.insert(
                    id,
                    CheckRun {
                        classes,
                        min_score,
                        until_ms,
                        want: cameras.iter().map(|&c| (c, per_camera)).collect(),
                        outstanding: BTreeSet::new(),
                        deadline: instant + left + acq.answer_timeout + TICK,
                        reached: false,
                        roi,
                    },
                );
                for cam in cameras {
                    self.attach(st, cam, id, priority, now);
                }
            }
        }
    }

    /// Gives a check a camera: an existing wake, a new one, or a place in
    /// the queue.
    fn attach(&self, st: &mut State, cam: CameraId, check: u64, priority: u8, now: Now) {
        if !st.sessions.contains_key(&u64::from(cam.device)) {
            // Nothing to look with: this angle gives no pictures.
            if let Some(c) = st.checks.get_mut(&check) {
                c.want.insert(cam, 0);
            }
            return;
        }
        // A check needs the main stream's pictures, not a re-check's.
        if st
            .wakes
            .get(&cam)
            .is_some_and(|w| w.tier != frames::tier_of(cam))
        {
            self.restart_wake(st, cam, frames::tier_of(cam));
        }
        if let Some(w) = st.wakes.get_mut(&cam) {
            w.checks.insert(check);
            if let Some(t) = w.first_picture {
                let ms = u32::try_from(t.duration_since(w.started).as_millis()).ok();
                self.woken_step(st, check, cam, ms, now);
            }
            return;
        }
        if !self.room_for(st, cam.device) {
            let order = st.next_order;
            st.next_order += 1;
            st.waiting.push((priority, order, cam, check));
            return;
        }
        self.start_wake(st, cam, check);
    }

    fn woken_step(&self, st: &mut State, check: u64, cam: CameraId, ms: Option<u32>, now: Now) {
        if let Some(step) = st
            .engine
            .as_mut()
            .and_then(|e| e.camera_woken(check, cam, ms, now))
        {
            self.step_event(check, &step);
        }
    }

    /// Whether another camera of `device` may be woken now, making room by
    /// ending a re-check hold if needed.
    fn room_for(&self, st: &mut State, device: u32) -> bool {
        let per_device = st
            .engine
            .as_ref()
            .map_or(4, |e| usize::from(e.settings().standby.max_wake_per_device));
        loop {
            let on_device = st.wakes.keys().filter(|c| c.device == device).count();
            let total = st.wakes.len();
            if on_device < per_device && total < st.acq.max_wakes {
                return true;
            }
            // A wake kept only for re-checks gives way to a new check.
            let victim = st
                .wakes
                .iter()
                .filter(|(c, w)| {
                    w.checks.is_empty() && (on_device < per_device || c.device == device)
                })
                .min_by_key(|(_, w)| w.started)
                .map(|(c, _)| *c);
            match victim {
                Some(c) => self.stop_wake(st, c),
                None => return false,
            }
        }
    }

    fn start_wake(&self, st: &mut State, cam: CameraId, check: u64) {
        let Some((session, _)) = st.sessions.get(&u64::from(cam.device)).cloned() else {
            return;
        };
        let generation = st.next_generation;
        st.next_generation += 1;
        let inner = self.self_arc();
        let task = self.runtime.spawn(frames::run_wake(
            inner,
            session,
            cam,
            generation,
            frames::tier_of(cam),
        ));
        debug!(
            device = cam.device,
            channel = cam.channel,
            lens = cam.lens,
            "camera woken for a check"
        );
        st.wakes.insert(
            cam,
            WakeRun {
                generation,
                task,
                cover: None,
                started: Instant::now(),
                first_picture: None,
                failed: false,
                cover_tried: false,
                checks: BTreeSet::from([check]),
                hold: None,
                tier: frames::tier_of(cam),
                low_failed: false,
                want: Want::Keyframe,
                last_pts: None,
                pictures: 0,
            },
        );
    }

    /// Reads a woken camera's other stream from now on: the old task ends
    /// (dropping its subscription), a new one starts on `tier`, and the next
    /// picture comes from its first keyframe.
    fn restart_wake(&self, st: &mut State, cam: CameraId, tier: StreamTier) {
        let Some((session, _)) = st.sessions.get(&u64::from(cam.device)).cloned() else {
            return;
        };
        let generation = st.next_generation;
        st.next_generation += 1;
        let inner = self.self_arc();
        let Some(w) = st.wakes.get_mut(&cam) else {
            return;
        };
        w.task.abort();
        if let Some(c) = w.cover.take() {
            c.abort();
        }
        w.task = self
            .runtime
            .spawn(frames::run_wake(inner, session, cam, generation, tier));
        w.generation = generation;
        w.tier = tier;
        w.started = Instant::now();
        w.first_picture = None;
        w.failed = false;
        w.cover_tried = false;
        w.last_pts = None;
        w.pictures = 0;
        w.want = Want::Keyframe;
        debug!(
            device = cam.device,
            channel = cam.channel,
            lens = cam.lens,
            low = tier != frames::tier_of(cam),
            "woken camera changed stream"
        );
    }

    /// Stops a woken camera: its task ends and drops the subscription.
    fn stop_wake(&self, st: &mut State, cam: CameraId) {
        let Some(w) = st.wakes.remove(&cam) else {
            return;
        };
        w.task.abort();
        if let Some(c) = w.cover {
            c.abort();
        }
        if let Some(Hold {
            pending: Some(k), ..
        }) = &w.hold
        {
            self.drop_request(st, k, "canceled");
        }
        for id in &w.checks {
            if let Some(c) = st.checks.get_mut(id) {
                c.want.insert(cam, 0);
            }
        }
        debug!(
            device = cam.device,
            channel = cam.channel,
            lens = cam.lens,
            "woken camera stopped"
        );
    }

    fn self_arc(&self) -> Arc<Inner> {
        self.me.upgrade().expect("the hub outlives its own calls")
    }

    // ---- From the wake tasks ----

    pub(crate) fn wake_want(&self, cam: CameraId, generation: u64) -> Want {
        let st = lock(&self.state);
        match st.wakes.get(&cam) {
            Some(w) if w.generation == generation => w.want,
            _ => Want::Nothing,
        }
    }

    pub(crate) fn wake_note(&self, cam: CameraId, generation: u64, note: Note) {
        let mut st = lock(&self.state);
        let (_, now) = self.now(&st);
        let Some(w) = st
            .wakes
            .get_mut(&cam)
            .filter(|w| w.generation == generation)
        else {
            return;
        };
        match note {
            Note::FirstVideo => {
                if w.first_picture.is_none() {
                    let t = Instant::now();
                    w.first_picture = Some(t);
                    let ms = u32::try_from(t.duration_since(w.started).as_millis()).ok();
                    let checks: Vec<u64> = w.checks.iter().copied().collect();
                    for id in checks {
                        self.woken_step(&mut st, id, cam, ms, now);
                    }
                }
            }
            Note::Refused | Note::Ended => {
                w.failed = true;
                w.want = Want::Nothing;
            }
        }
        drop(st);
        self.wake.notify_one();
    }

    pub(crate) fn wake_picture(&self, cam: CameraId, generation: u64, bundle: Bundle) {
        let mut st = lock(&self.state);
        let (instant, now) = self.now(&st);
        let Some(w) = st
            .wakes
            .get_mut(&cam)
            .filter(|w| w.generation == generation)
        else {
            return;
        };
        if !bundle.from_cover {
            w.last_pts = Some(bundle.pts_us());
        }
        w.pictures += 1;
        w.want = Want::Nothing;
        if w.first_picture.is_none() {
            w.first_picture = Some(instant);
        }
        let attached: Vec<u64> = w.checks.iter().copied().collect();
        let hold_wants = w
            .hold
            .as_ref()
            .is_some_and(|h| h.pending.is_none() && instant >= h.next);
        let mut takers = Vec::new();
        for id in attached {
            if let Some(c) = st.checks.get_mut(&id)
                && !c.reached
                && now.ms <= c.until_ms
                && c.want.get(&cam).copied().unwrap_or(0) > 0
            {
                if let Some(n) = c.want.get_mut(&cam) {
                    *n -= 1;
                }
                takers.push(id);
            }
        }
        let purpose = if !takers.is_empty() {
            Purpose::Check
        } else if hold_wants {
            Purpose::Recheck
        } else {
            return;
        };
        let key = self.new_request(
            &mut st,
            cam,
            purpose,
            takers.clone(),
            bundle,
            now.ms,
            instant,
        );
        match (key, purpose) {
            (Some(k), Purpose::Check) => {
                for id in takers {
                    if let Some(c) = st.checks.get_mut(&id) {
                        c.outstanding.insert(k.clone());
                    }
                }
            }
            (Some(k), Purpose::Recheck) => {
                if let Some(h) = st.wakes.get_mut(&cam).and_then(|w| w.hold.as_mut()) {
                    h.pending = Some(k);
                }
            }
            (None, Purpose::Recheck) => {
                let period = recheck_period(&st);
                if let Some(h) = st.wakes.get_mut(&cam).and_then(|w| w.hold.as_mut()) {
                    h.next = instant + period;
                }
            }
            (None, Purpose::Check) => {}
        }
        drop(st);
        self.wake.notify_one();
    }

    /// Holds a picture for the app and announces it.
    #[allow(clippy::too_many_arguments)]
    fn new_request(
        &self,
        st: &mut State,
        cam: CameraId,
        purpose: Purpose,
        checks: Vec<u64>,
        bundle: Bundle,
        at_ms: i64,
        instant: Instant,
    ) -> Option<String> {
        let bytes = bundle.bytes();
        if st.requests.len() >= MAX_REQUESTS || st.request_bytes + bytes > MAX_REQUEST_BYTES {
            warn!("verification requests at their limit; a picture was skipped");
            return None;
        }
        let key = format!("v{}", st.next_request);
        st.next_request += 1;
        let deadline = instant + st.acq.answer_timeout;
        let deadline_ms = at_ms + i64::try_from(st.acq.answer_timeout.as_millis()).unwrap_or(0);
        // Classes, score floor, and crops for the detector.
        let mut classes: BTreeSet<Class> = BTreeSet::new();
        let mut min_score = 1.0f32;
        let mut region = None;
        for id in &checks {
            if let Some(c) = st.checks.get(id) {
                classes.extend(c.classes.iter().copied());
                min_score = min_score.min(c.min_score);
                region = region.or(c.roi.get(&cam).copied());
            }
        }
        if purpose == Purpose::Recheck {
            if let Some(h) = st.wakes.get(&cam).and_then(|w| w.hold.as_ref()) {
                classes.extend(h.classes.iter().copied());
            }
            min_score = st
                .engine
                .as_ref()
                .map_or(0.5, |e| e.settings().verification.min_score);
        }
        let mut crops = vec![json!({"what": "frame", "x": 0.0, "y": 0.0, "w": 1.0, "h": 1.0})];
        for (x, y) in [(0.0, 0.0), (0.45, 0.0), (0.0, 0.45), (0.45, 0.45)] {
            crops.push(json!({"what": "tile", "x": x, "y": y, "w": 0.55, "h": 0.55}));
        }
        if let Some([x, y, w, h]) = region {
            crops.push(json!({"what": "region", "x": x, "y": y, "w": w, "h": h}));
        }
        let frames: Vec<Value> = {
            let f = bundle.frames();
            bundle
                .aus
                .iter()
                .zip(f.sizes.iter())
                .map(|(a, &n)| json!({"pts_us": a.pts_us, "bytes": n, "keyframe": a.keyframe}))
                .collect()
        };
        let ev = json!({
            "type": "verify_request",
            "request": key,
            "purpose": if purpose == Purpose::Check { "check" } else { "recheck" },
            "camera": camera_json(cam),
            "trigger_ids": checks,
            "codec": if bundle.codec == crate::Codec::H265 { "h265" } else { "h264" },
            "width": bundle.width,
            "height": bundle.height,
            "pts_us": bundle.pts_us(),
            "bytes": bytes,
            "frames": frames,
            "from_cover": bundle.from_cover,
            "at_ms": at_ms,
            "deadline_ms": deadline_ms,
            "classes": classes.iter().map(|&c| class_name(c)).collect::<Vec<_>>(),
            "min_score": min_score,
            "crops": crops,
        });
        st.request_bytes += bytes;
        st.requests.insert(
            key.clone(),
            Request {
                camera: cam,
                purpose,
                checks,
                at_ms,
                deadline,
                bundle,
            },
        );
        self.send(ev);
        Some(key)
    }

    fn drop_request(&self, st: &mut State, key: &str, result: &str) {
        if let Some(r) = st.requests.remove(key) {
            st.request_bytes = st.request_bytes.saturating_sub(r.bundle.bytes());
            for id in &r.checks {
                if let Some(c) = st.checks.get_mut(id) {
                    c.outstanding.remove(key);
                }
            }
            let period = recheck_period(st);
            if r.purpose == Purpose::Recheck
                && let Some(h) = st.wakes.get_mut(&r.camera).and_then(|w| w.hold.as_mut())
                && h.pending.as_deref() == Some(key)
            {
                // Unanswered or canceled: the next re-check waits a period
                // like an answered one (not the very next keyframe).
                h.pending = None;
                h.next = h.next.max(Instant::now() + period);
            }
            self.send(json!({"type": "verify_request_done", "request": key, "result": result}));
        }
    }

    // ---- Decisions ----

    /// Logs a decision, reports it, and starts deliveries and re-checks.
    fn record(&self, st: &mut State, d: Decision, emit_tail: bool, fx: &mut Vec<Effect>) {
        let (instant, now) = self.now(st);
        if emit_tail {
            let mut tail: Vec<&Step> = d
                .trace
                .steps
                .iter()
                .rev()
                .take_while(|s| {
                    matches!(
                        s.kind,
                        StepKind::Checked { .. }
                            | StepKind::Rules { .. }
                            | StepKind::Rarity { .. }
                            | StepKind::Timeliness { .. }
                            | StepKind::Decision { .. }
                    )
                })
                .collect();
            tail.reverse();
            for s in tail {
                self.step_event(d.trigger_id, s);
            }
        }
        let source = d.trace.steps.iter().find_map(|s| match &s.kind {
            StepKind::TriggerReceived { source, .. } => Some(*source),
            _ => None,
        });
        let source_name = source
            .and_then(|s| serde_json::to_value(s).ok())
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let device_key = d
            .camera
            .and_then(|c| st.sessions.get(&u64::from(c.device)))
            .map(|(_, k)| k.clone());
        let decision = serde_json::to_value(&d).unwrap_or(Value::Null);
        let state = match &d.outcome {
            Outcome::Alert { rev: 1, .. } => "new",
            Outcome::Alert { .. } => "update",
            _ => "logged",
        };
        let test_flag = st.tests.get(&d.trigger_id).copied();
        // The configured sensor that triggered it, by its key.
        let sensor_key = st.engine.as_ref().map(Engine::settings).and_then(|s| {
            d.trace.steps.iter().find_map(|step| match &step.kind {
                StepKind::TriggerReceived {
                    sensor: Some(i), ..
                } => s.sources.get(*i as usize).map(|m| m.key.clone()),
                _ => None,
            })
        });
        let mut line = json!({"logged_ms": now.ms, "source": source_name, "decision": decision});
        if let Some(k) = &sensor_key {
            line["sensor"] = json!(k);
        }
        if test_flag.is_some() {
            line["test"] = json!(true);
        }
        if st.evidence.has(d.trigger_id) {
            let mut summary = decision.clone();
            if let Some(o) = summary.as_object_mut() {
                o.remove("trace");
            }
            let mut patch = json!({"decision": summary, "source": source_name});
            // What triggered the check: the camera AI's claim (a device
            // source) or the sensor's kind.
            if let Some(t) = d.trace.steps.iter().find_map(|s| match &s.kind {
                StepKind::TriggerReceived {
                    source,
                    kind,
                    class,
                    ..
                } => Some(json!({"source": source, "kind": kind, "class": class})),
                _ => None,
            }) {
                patch["trigger"] = t;
            }
            if test_flag.is_some() {
                patch["test"] = json!(true);
            }
            st.evidence.merge_doc(d.trigger_id, &patch, now.ms);
        }
        if let Some(k) = &device_key {
            line["device_key"] = json!(k);
        }
        st.log.append(&line.to_string(), now.ms);
        // Only alerts carry the full trace in events (it stays in the log and
        // in `alerts_get`); quiet and suppressed decisions carry a summary.
        let mut event_decision = decision.clone();
        if state == "logged"
            && let Some(o) = event_decision.as_object_mut()
        {
            o.remove("trace");
            let summary: String = d.explanation.chars().take(EVENT_SUMMARY_MAX).collect();
            o.insert("summary".to_owned(), json!(summary));
        }
        let mut ev = json!({"type": "alert", "state": state, "source": source_name,
                            "decision": event_decision});
        if test_flag.is_some() {
            ev["test"] = json!(true);
        }
        if let Some(k) = &sensor_key {
            ev["sensor"] = json!(k);
        }
        if let Some(k) = &device_key {
            ev["device_key"] = json!(k);
        }
        self.send(ev);
        info!(
            trigger = d.trigger_id,
            decision = d.outcome.label(),
            "smart alert decision"
        );
        if let Outcome::Alert {
            severity,
            alert_id,
            rev,
            late,
            unverified,
            always,
        } = &d.outcome
        {
            st.next_alert_id = st.next_alert_id.max(alert_id + 1);
            if let Some(to_ha) = test_flag {
                st.test_alerts.insert(*alert_id, to_ha);
                while st.test_alerts.len() > MAX_TESTS {
                    st.test_alerts.pop_first();
                }
            }
            if let Some(cam) = d.camera {
                st.alerted.insert(
                    EpisodeKey {
                        camera: cam,
                        class: d.class,
                    },
                    Alerted {
                        alert_id: *alert_id,
                        rev: *rev,
                        severity: *severity,
                        trigger_id: d.trigger_id,
                    },
                );
                if st.acq.recheck && d.class.is_detector_class() {
                    self.hold(st, cam, d.class, instant);
                }
            }
            let (camera_name, source_key, offset_min) = {
                let settings = st.engine.as_ref().map(Engine::settings);
                let name = match (settings, d.camera) {
                    (Some(s), Some(c)) => s.camera(c).display_name(),
                    _ => String::new(),
                };
                let key = settings.and_then(|s| {
                    d.trace.steps.iter().find_map(|step| match &step.kind {
                        StepKind::TriggerReceived {
                            sensor: Some(i), ..
                        } => s.sources.get(*i as usize).map(|m| m.key.clone()),
                        _ => None,
                    })
                });
                (name, key, st.offset_min)
            };
            let notice = AlertNotice {
                camera_name,
                source: source_key,
                utc_offset_min: offset_min,
                state: if *rev == 1 {
                    NoticeState::New
                } else {
                    NoticeState::Update
                },
                alert_id: *alert_id,
                rev: *rev,
                camera: d.camera,
                class: d.class,
                severity: Some(*severity),
                unverified: *unverified,
                late: *late,
                always: always
                    .and_then(|a| serde_json::to_value(a).ok())
                    .and_then(|v| v.as_str().map(str::to_owned)),
                explanation: d.explanation.clone(),
                at_ms: d.decided_ms,
                decision_json: decision.to_string(),
                snapshot: None,
                test: test_flag.is_some(),
            };
            self.queue_notice(st, d.trigger_id, notice, *severity, instant, fx);
        }
    }

    /// Delivers a notice to every enabled output whose minimum severity it
    /// meets (after the snapshot wait for outputs that want pictures).
    fn queue_notice(
        &self,
        st: &mut State,
        trigger_id: u64,
        notice: AlertNotice,
        severity: Severity,
        instant: Instant,
        fx: &mut Vec<Effect>,
    ) {
        let Some(engine) = st.engine.as_ref() else {
            return;
        };
        let settings = engine.settings();
        let mut add = Vec::new();
        for (slot, o) in st.outputs.iter().enumerate() {
            let Some(cfg) = settings.outputs.get(o.index as usize) else {
                continue;
            };
            if !cfg.enabled || severity < cfg.min_severity {
                continue;
            }
            // A simulated trigger reaches Home Assistant only when the
            // person asked for it.
            if notice.test
                && o.kind == OutputKind::HomeAssistant
                && st.test_alerts.get(&notice.alert_id) != Some(&true)
            {
                continue;
            }
            let wait = notice.state != NoticeState::Settled
                && cfg.pictures
                && o.output.wants_picture()
                && !st.snapshots.has(notice.alert_id);
            add.push(Delivery {
                trigger_id,
                slot,
                notice: notice.clone(),
                due: if wait {
                    instant + SNAPSHOT_WAIT
                } else {
                    instant
                },
                sent: false,
                done: Arc::new(AtomicBool::new(false)),
                timeout: instant + DELIVERY_TIMEOUT,
            });
        }
        for d in add {
            if st.deliveries.len() >= MAX_DELIVERIES {
                warn!("alert deliveries at their limit; a notice was dropped");
                break;
            }
            st.deliveries.push(d);
        }
        self.due_deliveries(st, instant, fx);
    }

    fn due_deliveries(&self, st: &mut State, instant: Instant, fx: &mut Vec<Effect>) {
        let me = self.self_arc();
        for i in 0..st.deliveries.len() {
            let d = &st.deliveries[i];
            if d.sent {
                continue;
            }
            let ready = instant >= d.due || st.snapshots.has(d.notice.alert_id);
            if !ready {
                continue;
            }
            let Some(slot) = st.outputs.get(d.slot) else {
                continue;
            };
            let (index, kind, output) = (slot.index, slot.kind, slot.output.clone());
            let wants = output.wants_picture();
            let snapshot = if wants && d.notice.state != NoticeState::Settled {
                st.snapshots.get(d.notice.alert_id).map(Arc::from)
            } else {
                None
            };
            let d = &mut st.deliveries[i];
            d.sent = true;
            d.timeout = instant + DELIVERY_TIMEOUT;
            let mut notice = d.notice.clone();
            notice.snapshot = snapshot;
            let (trigger_id, done) = (d.trigger_id, d.done.clone());
            let me = me.clone();
            let report: Box<dyn FnOnce(bool, Option<String>) + Send> =
                Box::new(move |ok, summary| me.delivered(trigger_id, index, kind, ok, summary));
            fx.push(Effect::Deliver(
                output,
                notice,
                DeliveryDone {
                    report: Some(report),
                    done,
                },
            ));
        }
    }

    /// An output's result: a `delivery` step, logged and reported.
    fn delivered(
        &self,
        trigger_id: u64,
        index: u32,
        kind: OutputKind,
        ok: bool,
        summary: Option<String>,
    ) {
        let mut st = lock(&self.state);
        let (_, now) = self.now(&st);
        let step = Step {
            at_ms: now.ms,
            kind: StepKind::Delivery {
                output: index,
                kind,
                ok,
                summary: summary.map(|t| clean_summary(&t)).filter(|t| !t.is_empty()),
            },
        };
        let s = serde_json::to_value(&step).unwrap_or(Value::Null);
        st.log.append(
            &json!({"logged_ms": now.ms, "delivery": {"trigger_id": trigger_id, "step": s}})
                .to_string(),
            now.ms,
        );
        st.deliveries
            .retain(|d| !(d.done.load(Ordering::SeqCst) && d.sent));
        drop(st);
        self.step_event(trigger_id, &step);
        if !ok {
            warn!(output = index, "alert delivery failed");
        }
    }

    fn run_later(&self, fx: Vec<Effect>) {
        for e in fx {
            match e {
                Effect::Deliver(o, n, done) => {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        o.deliver(&n, done)
                    }));
                    if r.is_err() {
                        warn!("an alert output panicked");
                    }
                }
            }
        }
    }

    /// Keeps an alerted camera woken for "still present" re-checks.
    fn hold(&self, st: &mut State, cam: CameraId, class: Class, instant: Instant) {
        let (period, limit) = st.engine.as_ref().map_or((10, 600), |e| {
            let t = e.settings().timing;
            (t.recheck_period_s, t.recheck_limit_s)
        });
        let Some(w) = st.wakes.get_mut(&cam) else {
            return;
        };
        let h = w.hold.get_or_insert(Hold {
            classes: BTreeSet::new(),
            until: instant + Duration::from_secs(u64::from(limit)),
            next: instant + Duration::from_secs(u64::from(period)),
            pending: None,
        });
        h.classes.insert(class);
    }

    /// Ends a check: the engine decides it, and its cameras are let go.
    fn finish_check(&self, st: &mut State, id: u64, fx: &mut Vec<Effect>) {
        let (_, now) = self.now(st);
        let Some(_c) = st.checks.remove(&id) else {
            return;
        };
        let decided = st
            .engine
            .as_mut()
            .and_then(|e| e.decide(id, DecideInputs::default(), now));
        self.forget_check(st, id);
        if let Some(d) = decided {
            self.record(st, d, true, fx);
        }
    }

    /// Takes a check out of requests, the queue, and wakes.
    fn forget_check(&self, st: &mut State, id: u64) {
        st.waiting.retain(|(_, _, _, c)| *c != id);
        let keys: Vec<String> = st
            .requests
            .iter()
            .filter(|(_, r)| r.checks.contains(&id))
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            let empty = st.requests.get_mut(&k).is_some_and(|r| {
                r.checks.retain(|c| *c != id);
                r.checks.is_empty() && r.purpose == Purpose::Check
            });
            if empty {
                self.drop_request(st, &k, "canceled");
            }
        }
        for w in st.wakes.values_mut() {
            w.checks.remove(&id);
        }
    }

    // ---- The pump: timers, queue, decisions ----

    fn pump(&self) {
        let mut fx = Vec::new();
        {
            let mut st = lock(&self.state);
            if st.engine.is_some() {
                self.pump_locked(&mut st, &mut fx);
            }
        }
        self.run_later(fx);
    }

    fn pump_locked(&self, st: &mut State, fx: &mut Vec<Effect>) {
        let (instant, now) = self.now(st);

        // Requests nobody answered in time.
        let expired: Vec<String> = st
            .requests
            .iter()
            .filter(|(_, r)| r.deadline <= instant)
            .map(|(k, _)| k.clone())
            .collect();
        for k in expired {
            self.drop_request(st, &k, "timeout");
        }

        // The engine's own timers.
        if st
            .last_tick
            .is_none_or(|t| instant.duration_since(t) >= TICK)
        {
            st.last_tick = Some(instant);
            if let Some(engine) = st.engine.as_mut() {
                let tick = engine.tick(now);
                st.mutes.retain(|_, until| *until > now.ms);
                for d in tick.decided {
                    st.checks.remove(&d.trigger_id);
                    self.forget_check(st, d.trigger_id);
                    self.record(st, d, true, fx);
                }
                for key in tick.settled {
                    self.settled(st, key, instant, fx);
                }
            }
        }

        // Checks that are done.
        let done: Vec<u64> = st
            .checks
            .iter()
            .filter(|(_, c)| {
                let more = !c.reached && now.ms <= c.until_ms && c.want.values().any(|&n| n > 0);
                c.reached || (c.outstanding.is_empty() && !more) || instant >= c.deadline
            })
            .map(|(&id, _)| id)
            .collect();
        for id in done {
            self.finish_check(st, id, fx);
        }

        // Holds that ended.
        let ended: Vec<CameraId> = st
            .wakes
            .iter()
            .filter(|(_, w)| {
                w.hold
                    .as_ref()
                    .is_some_and(|h| instant >= h.until || h.classes.is_empty())
            })
            .map(|(c, _)| *c)
            .collect();
        for c in ended {
            if let Some(w) = st.wakes.get_mut(&c)
                && let Some(Hold {
                    pending: Some(k), ..
                }) = w.hold.take()
            {
                self.drop_request(st, &k, "canceled");
            }
        }

        // Wakes nobody needs any more.
        let idle: Vec<CameraId> = st
            .wakes
            .iter()
            .filter(|(_, w)| w.checks.is_empty() && w.hold.is_none())
            .map(|(c, _)| *c)
            .collect();
        for c in idle {
            self.stop_wake(st, c);
        }

        // Waiting cameras, by priority.
        if !st.waiting.is_empty() {
            st.waiting.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            let waiting = std::mem::take(&mut st.waiting);
            for (p, order, cam, check) in waiting {
                let wanted = st.checks.get(&check).is_some_and(|c| {
                    !c.reached && now.ms <= c.until_ms && c.want.get(&cam).copied().unwrap_or(0) > 0
                });
                if !wanted {
                    continue;
                }
                if st.wakes.contains_key(&cam) || self.room_for(st, cam.device) {
                    self.attach(st, cam, check, p, now);
                } else {
                    st.waiting.push((p, order, cam, check));
                }
            }
        }

        // Re-checks of an alerted camera read the low-quality stream (a
        // refused one falls back to the main stream, once).
        let to_low: Vec<CameraId> = st
            .wakes
            .iter()
            .filter(|(c, w)| {
                w.checks.is_empty()
                    && w.hold.is_some()
                    && !w.low_failed
                    && w.tier == frames::tier_of(**c)
            })
            .map(|(c, _)| *c)
            .collect();
        for c in to_low {
            self.restart_wake(st, c, frames::recheck_tier_of(c));
        }
        let back: Vec<CameraId> = st
            .wakes
            .iter()
            .filter(|(c, w)| w.failed && w.hold.is_some() && w.tier != frames::tier_of(**c))
            .map(|(c, _)| *c)
            .collect();
        for c in back {
            if let Some(w) = st.wakes.get_mut(&c) {
                w.low_failed = true;
            }
            self.restart_wake(st, c, frames::tier_of(c));
        }

        // What each wake wants next, and cover fallbacks.
        let acq = st.acq;
        let cams: Vec<CameraId> = st.wakes.keys().copied().collect();
        for cam in cams {
            let checks_want = st.wakes[&cam].checks.iter().any(|id| {
                st.checks.get(id).is_some_and(|c| {
                    !c.reached && now.ms <= c.until_ms && c.want.get(&cam).copied().unwrap_or(0) > 0
                })
            });
            let w = st.wakes.get_mut(&cam).expect("listed");
            let hold_wants = w
                .hold
                .as_ref()
                .is_some_and(|h| h.pending.is_none() && instant >= h.next);
            w.want = if w.failed {
                Want::Nothing
            } else if checks_want {
                match w.last_pts {
                    None => Want::Keyframe,
                    Some(base) => {
                        let gap = if w.pictures <= 1 {
                            acq.second_picture
                        } else {
                            acq.picture_interval
                        };
                        Want::AtOrAfter {
                            target: base + u64::try_from(gap.as_micros()).unwrap_or(u64::MAX),
                            base,
                        }
                    }
                }
            } else if hold_wants {
                Want::Keyframe
            } else {
                Want::Nothing
            };
            let need_cover = acq.cover_fallback
                && checks_want
                && !w.cover_tried
                && w.first_picture.is_none()
                && (w.failed || instant >= w.started + acq.first_picture_timeout);
            if need_cover {
                w.cover_tried = true;
                let generation = w.generation;
                if let Some((session, _)) = st.sessions.get(&u64::from(cam.device)).cloned() {
                    let inner = self.self_arc();
                    let offset = i64::from(st.offset_min) * 60;
                    let end = now.ms.div_euclid(1000) + offset;
                    let task = self.runtime.spawn(async move {
                        cover(inner, session, cam, generation, end).await;
                    });
                    if let Some(w) = st.wakes.get_mut(&cam) {
                        w.cover = Some(task);
                    }
                }
            }
        }

        // Deliveries: due ones, and results that never came.
        self.due_deliveries(st, instant, fx);
        let mut lost = Vec::new();
        st.deliveries.retain(|d| {
            if d.done.load(Ordering::SeqCst) {
                return false;
            }
            if d.sent && instant >= d.timeout {
                lost.push(d.clone_for_timeout());
                return false;
            }
            true
        });
        for (trigger_id, slot, done) in lost {
            if !done.swap(true, Ordering::SeqCst)
                && let Some(o) = st.outputs.get(slot)
            {
                let (index, kind) = (o.index, o.kind);
                let me = self.self_arc();
                // Recorded outside this lock by the service's next turn.
                self.runtime
                    .spawn(async move { me.delivered(trigger_id, index, kind, false, None) });
            }
        }
    }

    /// An episode settled: report it, deliver "settled", end its re-checks.
    fn settled(&self, st: &mut State, key: EpisodeKey, instant: Instant, fx: &mut Vec<Effect>) {
        if let Some(w) = st.wakes.get_mut(&key.camera)
            && let Some(h) = w.hold.as_mut()
        {
            h.classes.remove(&key.class);
        }
        let Some(a) = st.alerted.remove(&key) else {
            return;
        };
        let (_, now) = self.now(st);
        self.send(json!({
            "type": "alert",
            "state": "settled",
            "alert_id": a.alert_id,
            "rev": a.rev,
            "trigger_id": a.trigger_id,
            "camera": camera_json(key.camera),
            "class": class_name(key.class),
            "at_ms": now.ms,
            "test": st.test_alerts.contains_key(&a.alert_id),
        }));
        let notice = AlertNotice {
            state: NoticeState::Settled,
            alert_id: a.alert_id,
            rev: a.rev,
            camera: Some(key.camera),
            camera_name: st
                .engine
                .as_ref()
                .map(|e| e.settings().camera(key.camera).display_name())
                .unwrap_or_default(),
            source: None,
            utc_offset_min: st.offset_min,
            class: key.class,
            severity: None,
            unverified: false,
            late: false,
            always: None,
            explanation: String::new(),
            at_ms: now.ms,
            decision_json: String::new(),
            snapshot: None,
            test: st.test_alerts.contains_key(&a.alert_id),
        };
        self.queue_notice(st, a.trigger_id, notice, a.severity, instant, fx);
    }

    /// The next instant the pump has something to do.
    fn next_deadline(&self) -> Instant {
        let st = lock(&self.state);
        let now = Instant::now();
        let mut next = now + TICK;
        if st.engine.is_none() {
            return now + Duration::from_secs(3600);
        }
        // The pump has just handled everything due; a time already past
        // that is still listed waits for something else (an answer, a
        // picture), which wakes the service on its own.
        let mut consider = |t: Instant| {
            if t > now && t < next {
                next = t;
            }
        };
        for r in st.requests.values() {
            consider(r.deadline);
        }
        for c in st.checks.values() {
            consider(c.deadline);
        }
        for w in st.wakes.values() {
            if w.first_picture.is_none() && !w.cover_tried {
                consider(w.started + st.acq.first_picture_timeout);
            }
            if let Some(h) = &w.hold {
                consider(h.next);
                consider(h.until);
            }
        }
        for d in &st.deliveries {
            consider(if d.sent { d.timeout } else { d.due });
        }
        next
    }

    /// The service is stopping: every task, request, input, and output.
    async fn stop_all(&self) {
        let (tasks, inputs, outputs) = {
            let mut st = lock(&self.state);
            let mut tasks = Vec::new();
            for (_, w) in std::mem::take(&mut st.wakes) {
                w.task.abort();
                tasks.push(w.task);
                if let Some(c) = w.cover {
                    c.abort();
                    tasks.push(c);
                }
            }
            st.sessions.clear();
            st.checks.clear();
            st.waiting.clear();
            st.requests.clear();
            st.request_bytes = 0;
            st.deliveries.clear();
            (
                tasks,
                std::mem::take(&mut st.inputs),
                std::mem::take(&mut st.outputs),
            )
        };
        for t in tasks {
            let _ = t.await;
        }
        for mut i in inputs {
            i.input.stop();
        }
        for o in outputs {
            o.output.shutdown();
        }
    }
}

impl Delivery {
    fn clone_for_timeout(&self) -> (u64, usize, Arc<AtomicBool>) {
        (self.trigger_id, self.slot, self.done.clone())
    }
}

fn recheck_period(st: &State) -> Duration {
    Duration::from_secs(u64::from(
        st.engine
            .as_ref()
            .map_or(10, |e| e.settings().timing.recheck_period_s),
    ))
}

/// A region polygon's bounding box with a margin, as a crop.
fn polygon_box(points: &[[f32; 2]]) -> Option<[f32; 4]> {
    if points.len() < 3 {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (1.0f32, 1.0f32, 0.0f32, 0.0f32);
    for p in points {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    let m = 0.05;
    let (x0, y0) = ((x0 - m).max(0.0), (y0 - m).max(0.0));
    let (x1, y1) = ((x1 + m).min(1.0), (y1 + m).min(1.0));
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}

/// The cover-image fallback: the recording's picture of the last ten
/// seconds, when the live stream gave none in time.
async fn cover(
    inner: Arc<Inner>,
    session: Arc<DeviceSession>,
    cam: CameraId,
    generation: u64,
    end_s: i64,
) {
    let Ok(channel) = u8::try_from(cam.channel) else {
        return;
    };
    let q = bc_protocol::archive::CoverRequest {
        channel,
        uid: None,
        stream: frames::tier_of(cam),
        start: recordings::date_time(end_s - 10),
        end: recordings::date_time(end_s),
    };
    match session.cover_image(&q).await {
        Ok(Some(img)) => {
            let b = Bundle::single(
                frames::codec(img.codec),
                img.width,
                img.height,
                img.pts_us,
                img.keyframe,
            );
            inner.wake_picture(cam, generation, b);
        }
        Ok(None) => debug!("no cover image for a check"),
        Err(e) => debug!(error = ?e, "cover image for a check failed"),
    }
}

/// Parses detector boxes: `{"detections":[{"class"|"label":..,"score":..,
/// "box":{"x","y","w","h"}}]}` in normalized full-picture coordinates.
pub(crate) fn parse_detections(text: &str) -> Result<Vec<Detection>, AlertError> {
    let v: Value = serde_json::from_str(text).map_err(|_| AlertError::InvalidArgument)?;
    let list = v
        .get("detections")
        .and_then(Value::as_array)
        .ok_or(AlertError::InvalidArgument)?;
    let mut out = Vec::new();
    for d in list.iter().take(MAX_DETECTIONS) {
        let label = d["class"].as_str().or_else(|| d["label"].as_str());
        let Some(class) = label.and_then(class_of_label) else {
            continue;
        };
        let score = d["score"].as_f64().ok_or(AlertError::InvalidArgument)?;
        let b = &d["box"];
        let get = |k: &str| b[k].as_f64().ok_or(AlertError::InvalidArgument);
        let (x, y, w, h) = (get("x")?, get("y")?, get("w")?, get("h")?);
        if ![score, x, y, w, h].iter().all(|f| f.is_finite()) {
            return Err(AlertError::InvalidArgument);
        }
        let c = |f: f64| f.clamp(0.0, 1.0) as f32;
        out.push(Detection {
            class,
            score: c(score),
            bbox: BBox::new(c(x), c(y), c(w), c(h)),
        });
    }
    Ok(out)
}

impl Inner {
    /// The app's boxes for a request.
    pub(crate) fn answer(&self, key: &str, detections: Vec<Detection>) -> Result<(), AlertError> {
        let me = self;
        let r = (|| {
            let mut st = lock(&me.state);
            let (instant, now) = me.now(&st);
            let Some(r) = st.requests.get(key) else {
                return Err(AlertError::UnknownId);
            };
            let (cam, purpose, at_ms, checks) = (r.camera, r.purpose, r.at_ms, r.checks.clone());
            match purpose {
                Purpose::Check => {
                    let sample = Sample {
                        camera: cam,
                        at_ms,
                        detections: detections.clone(),
                    };
                    for id in checks {
                        let progress = st.engine.as_mut().and_then(|e| e.sample(id, &sample, now));
                        if let Some(p) = progress {
                            for s in &p.steps {
                                me.step_event(id, s);
                            }
                            if p.reached
                                && let Some(c) = st.checks.get_mut(&id)
                            {
                                c.reached = true;
                            }
                        }
                    }
                }
                Purpose::Recheck => {
                    let floor = st
                        .engine
                        .as_ref()
                        .map_or(0.5, |e| e.settings().verification.min_score);
                    let classes: Vec<Class> = st
                        .wakes
                        .get(&cam)
                        .and_then(|w| w.hold.as_ref())
                        .map(|h| h.classes.iter().copied().collect())
                        .unwrap_or_default();
                    for class in classes {
                        let seen = detections
                            .iter()
                            .any(|d| d.class == class && d.score >= floor);
                        let state = st
                            .engine
                            .as_mut()
                            .and_then(|e| e.observe(cam, class, seen, now));
                        if matches!(state, None | Some(EpisodeState::Settled))
                            && let Some(h) = st.wakes.get_mut(&cam).and_then(|w| w.hold.as_mut())
                        {
                            h.classes.remove(&class);
                        }
                    }
                    let period = recheck_period(&st);
                    if let Some(h) = st.wakes.get_mut(&cam).and_then(|w| w.hold.as_mut()) {
                        h.next = instant + period;
                    }
                }
            }
            me.drop_request(&mut st, key, "answered");
            Ok(())
        })();
        if r.is_ok() {
            self.wake.notify_one();
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_map_to_classes() {
        assert_eq!(class_of_label("car"), Some(Class::Vehicle));
        assert_eq!(class_of_label("Person"), Some(Class::Person));
        assert_eq!(class_of_label("dog"), Some(Class::Animal));
        assert_eq!(class_of_label("bicycle"), None);
        assert_eq!(class_of_label("suitcase"), None);
    }

    #[test]
    fn detections_parse_and_clamp() {
        let d = parse_detections(
            r#"{"detections":[{"label":"car","score":1.5,"box":{"x":-1,"y":0.2,"w":0.3,"h":0.4}},
                {"class":"kite","score":0.9,"box":{"x":0,"y":0,"w":1,"h":1}}]}"#,
        )
        .unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].class, Class::Vehicle);
        assert_eq!(d[0].score, 1.0);
        assert_eq!(d[0].bbox.x, 0.0);
        assert!(parse_detections("{}").is_err());
        assert!(parse_detections(r#"{"detections":[{"class":"person","score":0.5}]}"#).is_err());
        assert_eq!(parse_detections(r#"{"detections":[]}"#).unwrap(), vec![]);
    }

    #[test]
    fn events_become_triggers() {
        let i = intake_of_event(
            r#"{"type":"detection","device":3,"channel":1,"kind":"people","state":"start","source":"device","at_ns":1}"#,
            5,
        );
        assert!(matches!(
            i,
            Some(Intake::Camera { source: Source::Device, camera, ref kind, state: TriggerState::Start, received_ms: 5 })
                if camera == CameraId::new(3, 1, 0) && kind == "people"
        ));
        let i = intake_of_event(
            r#"{"type":"motion","device":3,"channel":1,"lens":1,"state":"stop","at_ns":1}"#,
            5,
        );
        assert!(
            matches!(i, Some(Intake::Camera { ref kind, state: TriggerState::Stop, .. }) if kind == "md")
        );
        let i = intake_of_event(
            r#"{"type":"detection","device":3,"channel":1,"kind":"smoke_alarm","state":"start","source":"local_audio","at_ns":1}"#,
            5,
        );
        assert!(matches!(
            i,
            Some(Intake::Camera {
                source: Source::LocalAudio,
                ..
            })
        ));
        assert!(intake_of_event(r#"{"type":"detection","device":3}"#, 5).is_none());
    }

    #[test]
    fn region_boxes() {
        assert_eq!(polygon_box(&[[0.1, 0.5], [0.6, 0.5]]), None);
        let b = polygon_box(&[[0.1, 0.5], [0.6, 0.5], [0.6, 1.0], [0.1, 1.0]]).unwrap();
        assert!((b[0] - 0.05).abs() < 1e-6 && (b[3] - 0.55).abs() < 1e-6);
    }
}
