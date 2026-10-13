//! The seams other modules plug into: trigger inputs (an MQTT client, a
//! webhook) feed [`ExternalTriggers`]; outputs (Home Assistant, ntfy)
//! implement [`AlertOutput`]. Both are registered by kind
//! ([`crate::Core::alerts_register_input`],
//! [`crate::Core::alerts_register_output`]) and built from the matching
//! entries of the settings document ([`crate::Core::alerts_configure`]).

use alerts::model::{CameraId, Class, Severity};
use alerts::trigger::TriggerState;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

use super::Intake;

/// Why a call to the alert service failed. Maps to `NvStatus` in the C
/// interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AlertError {
    /// Bad JSON, a value out of range, or a call from inside a callback.
    InvalidArgument,
    /// No such request, alert, or snapshot (any more).
    UnknownId,
    /// The core is shutting down.
    Closed,
    /// A queue or cache is full, or a value is larger than allowed.
    Limit,
    /// The answer does not fit; carries the size needed.
    BufferTooSmall,
    /// `alerts_configure` has not been called (or turned the service off).
    NotConfigured,
    /// Something unexpected failed.
    Internal,
}

impl fmt::Display for AlertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AlertError::InvalidArgument => "invalid argument",
            AlertError::UnknownId => "unknown request or alert",
            AlertError::Closed => "the core is shutting down",
            AlertError::Limit => "limit reached",
            AlertError::BufferTooSmall => "buffer too small",
            AlertError::NotConfigured => "smart alerts are not configured",
            AlertError::Internal => "internal error",
        })
    }
}

impl std::error::Error for AlertError {}

/// A credential the app read from its keychain (an MQTT password, a Home
/// Assistant token) and passed with the settings: held in memory only,
/// never written to settings files, logs, events, or the alert log. `Debug`
/// is redacted and there is no `Display` or `Serialize`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Arc<str>);

impl Secret {
    pub fn new(value: &str) -> Secret {
        Secret(Arc::from(value))
    }

    /// The credential's text, for the one place that sends it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

/// One delivery from an external source (a sensor through MQTT or a
/// webhook). Payload fields are data only: the kind is normalized by the
/// engine (unknown names pass through as "other" and never reach an
/// explanation), and the source key must match a configured source.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternalTrigger {
    /// The configured source key it was authenticated as (`[a-z0-9_-]`,
    /// at most 32 bytes). Never taken from the payload.
    pub source: String,
    /// The kind, such as `sensor_motion` or `contact`.
    pub kind: String,
    pub state: TriggerState,
    /// When the sensor fired (UTC milliseconds), if it says; a time in the
    /// future is replaced by the arrival time.
    pub at_ms: Option<i64>,
}

/// Why an external trigger was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerRejected {
    /// The intake queue is full (the service is far behind); the trigger
    /// is dropped and counted.
    QueueFull,
    /// The service is stopped (shutdown).
    Closed,
}

/// Where trigger inputs send external triggers: a bounded queue into the
/// alert service. Cheap to clone; never blocks. Rate limits per source
/// and the source check happen in the service (a refusal is reported as a
/// `trigger_rejected` event).
#[derive(Clone)]
pub struct ExternalTriggers {
    pub(crate) tx: mpsc::Sender<Intake>,
    pub(crate) clock: crate::clock::HostClock,
}

impl ExternalTriggers {
    /// Queues one trigger; its arrival time is now.
    ///
    /// # Errors
    /// [`TriggerRejected::QueueFull`] when the queue is full,
    /// [`TriggerRejected::Closed`] after shutdown.
    pub fn put(&self, trigger: ExternalTrigger) -> Result<(), TriggerRejected> {
        let received_ms = self.clock.wall_now_us().div_euclid(1000);
        self.tx
            .try_send(Intake::External {
                trigger,
                received_ms,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => TriggerRejected::QueueFull,
                mpsc::error::TrySendError::Closed(_) => TriggerRejected::Closed,
            })
    }
}

impl fmt::Debug for ExternalTriggers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExternalTriggers").finish_non_exhaustive()
    }
}

/// What an input module gets when it is built.
#[derive(Clone)]
pub struct InputContext {
    /// The core's runtime, for the input's tasks. Tasks must end when the
    /// input is stopped.
    pub runtime: tokio::runtime::Handle,
    /// Where to send what it receives.
    pub triggers: ExternalTriggers,
    /// The credential the app passed for this input (`secrets[<id>]` of
    /// the settings, keyed by the entry's `id`), if any.
    pub secret: Option<Secret>,
    /// Sends an event of the input's own to the app (a status change),
    /// as `{"type":"alerts_input_status","index":..,"kind":..,"status":..}`
    /// with this object as `status`. Never put secrets or payload text in.
    pub status_events: Arc<dyn Fn(serde_json::Value) + Send + Sync>,
}

impl fmt::Debug for InputContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InputContext")
            .field("secret", &self.secret)
            .finish_non_exhaustive()
    }
}

/// A running trigger input (an MQTT subscription, for example). Stopped
/// when the settings drop it, change it, or the core shuts down.
pub trait AlertInput: Send + 'static {
    /// Stops receiving, closes connections, and ends the input's tasks.
    /// Called once; must not block for long.
    fn stop(&mut self);
    /// The input's current status for `alerts_get` (connection state and
    /// counters). Must be quick, and must not contain secrets or payload
    /// text.
    fn status(&self) -> Option<serde_json::Value> {
        None
    }
    /// The input's message log: entries with `seq >= after`, at most
    /// `limit` (a page as `mqtt-triggers` makes it). `None` for an input
    /// that keeps none. Memory only; never written anywhere.
    fn messages(&self, _after: u64, _limit: usize) -> Option<serde_json::Value> {
        None
    }
    /// Forgets the logged messages.
    fn clear_messages(&self) {}
}

/// Builds an input from its settings entry (the whole JSON object of one
/// `inputs[]` element, `kind` included). An error is reported in the
/// `alerts_configured` event and the input is left out.
pub type InputFactory = Arc<
    dyn Fn(&InputContext, &serde_json::Value) -> Result<Box<dyn AlertInput>, String> + Send + Sync,
>;

/// What an output module gets when it is built.
#[derive(Clone, Debug)]
pub struct OutputContext {
    /// The core's runtime, for the output's requests.
    pub runtime: tokio::runtime::Handle,
    /// The credential the app passed for this output (`secrets[<id>]` of
    /// the settings, keyed by the entry's `id`, else its `kind`), if any.
    pub secret: Option<Secret>,
}

/// Builds an output from its settings entry (the whole JSON object of one
/// `outputs[]` element, `kind` included). An error is reported in the
/// `alerts_configured` event and the output is left out.
pub type OutputFactory = Arc<
    dyn Fn(&OutputContext, &serde_json::Value) -> Result<Arc<dyn AlertOutput>, String>
        + Send
        + Sync,
>;

/// What an alert notice is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeState {
    /// A new alert (revision 1).
    New,
    /// An update of an earlier alert (a higher revision).
    Update,
    /// The alert's episode settled: the activity is over.
    Settled,
}

impl NoticeState {
    pub fn as_str(self) -> &'static str {
        match self {
            NoticeState::New => "new",
            NoticeState::Update => "update",
            NoticeState::Settled => "settled",
        }
    }
}

/// One alert, as outputs receive it. Only alerts reach outputs (quiet and
/// suppressed decisions go to the event log only).
#[derive(Clone, Debug)]
pub struct AlertNotice {
    pub state: NoticeState,
    pub alert_id: u64,
    pub rev: u32,
    pub camera: Option<CameraId>,
    /// The camera's display name from the settings.
    pub camera_name: String,
    /// The key of the configured external source that triggered it, if any.
    pub source: Option<String>,
    /// The computer's offset from UTC in minutes, for local times.
    pub utc_offset_min: i32,
    pub class: Class,
    /// `None` for [`NoticeState::Settled`].
    pub severity: Option<Severity>,
    pub unverified: bool,
    pub late: bool,
    /// The always-alert rule that applied, by its JSON name.
    pub always: Option<String>,
    /// Built by templates from the trace; never free text from inputs.
    pub explanation: String,
    /// UTC milliseconds.
    pub at_ms: i64,
    /// The full decision as JSON (the `decision` of the `alert` event);
    /// empty for [`NoticeState::Settled`].
    pub decision_json: String,
    /// A picture of the alert (JPEG from the app), when the app put one in
    /// time and the output asked for pictures.
    pub snapshot: Option<Arc<[u8]>>,
    /// A simulated or test alert: outputs mark it so the receiver can tell
    /// it from a real one.
    pub test: bool,
}

/// The core's callback for one delivery's result and its summary line.
pub(crate) type Report = Box<dyn FnOnce(bool, Option<String>) + Send>;

/// Reports one delivery's result. Dropping it without [`DeliveryDone::finish`]
/// reports a failure; a result that never comes is recorded as a failure
/// after the delivery timeout.
pub struct DeliveryDone {
    pub(crate) report: Option<Report>,
    pub(crate) done: Arc<AtomicBool>,
}

impl DeliveryDone {
    /// Records the result as a `delivery` step of the alert's trace.
    pub fn finish(self, ok: bool) {
        self.finish_with(ok, None);
    }

    /// Like [`DeliveryDone::finish`], with a short generic result line for
    /// the step (at most 200 characters are kept; no secrets, no payload
    /// text).
    pub fn finish_with(mut self, ok: bool, summary: Option<String>) {
        if let Some(r) = self.report.take()
            && !self.done.swap(true, Ordering::SeqCst)
        {
            r(ok, summary);
        }
    }
}

impl Drop for DeliveryDone {
    fn drop(&mut self) {
        if let Some(r) = self.report.take()
            && !self.done.swap(true, Ordering::SeqCst)
        {
            r(false, None);
        }
    }
}

impl fmt::Debug for DeliveryDone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeliveryDone").finish_non_exhaustive()
    }
}

/// The result of [`AlertOutput::send_test`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestResult {
    pub ok: bool,
    /// A short generic line; no secrets.
    pub summary: String,
    /// The last HTTP status seen, for outputs that speak HTTP.
    pub http_status: Option<u16>,
}

/// Where [`AlertOutput::send_test`] reports; called once, from any thread.
pub type TestDone = Box<dyn FnOnce(TestResult) + Send>;

/// An alert output (Home Assistant, ntfy, a webhook). Called on the
/// service's task: [`AlertOutput::deliver`] must return at once and do
/// its I/O elsewhere (on the runtime from [`OutputContext`]), reporting
/// through `done`.
pub trait AlertOutput: Send + Sync + 'static {
    /// One alert notice: new, updated, or settled.
    fn deliver(&self, notice: &AlertNotice, done: DeliveryDone);
    /// Whether notices should wait briefly for the app's snapshot
    /// ([`crate::Core::alert_snapshot_put`]) before delivery.
    fn wants_picture(&self) -> bool {
        false
    }
    /// Sends one test alert through this output now, whether or not any
    /// rule would match, and reports the result through `done`. `rule`
    /// picks one of the output's rules (by index); `None` tests all of
    /// them. The default reports that the output cannot be tested.
    fn send_test(&self, _notice: &AlertNotice, _rule: Option<usize>, done: TestDone) {
        done(TestResult {
            ok: false,
            summary: "this output cannot send a test".to_owned(),
            http_status: None,
        });
    }
    /// The output is no longer used (settings changed, shutdown). Must not
    /// block for long.
    fn shutdown(&self) {}
}

/// A stub output for tests: records every notice and answers with a
/// fixed result.
#[derive(Default)]
pub struct RecordingOutput {
    notices: std::sync::Mutex<Vec<AlertNotice>>,
    fail: AtomicBool,
    pictures: AtomicBool,
    shut: AtomicBool,
}

impl RecordingOutput {
    pub fn new() -> Arc<RecordingOutput> {
        Arc::new(RecordingOutput::default())
    }

    /// Answer every later delivery with a failure.
    pub fn set_failing(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }

    /// Ask for pictures.
    pub fn set_wants_picture(&self, on: bool) {
        self.pictures.store(on, Ordering::SeqCst);
    }

    /// Every notice so far, oldest first.
    pub fn notices(&self) -> Vec<AlertNotice> {
        self.notices
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Whether [`AlertOutput::shutdown`] was called.
    pub fn was_shut_down(&self) -> bool {
        self.shut.load(Ordering::SeqCst)
    }
}

impl AlertOutput for RecordingOutput {
    fn deliver(&self, notice: &AlertNotice, done: DeliveryDone) {
        self.notices
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(notice.clone());
        done.finish(!self.fail.load(Ordering::SeqCst));
    }

    fn wants_picture(&self) -> bool {
        self.pictures.load(Ordering::SeqCst)
    }

    fn shutdown(&self) {
        self.shut.store(true, Ordering::SeqCst);
    }
}

/// The frames of one verification request ([`crate::Core::alert_frames_get`]):
/// Annex-B access units from a keyframe to the picture to check, parameter
/// sets included before the keyframe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlertFrames {
    pub codec: crate::Codec,
    /// Of the picture (0 when unknown).
    pub width: u32,
    pub height: u32,
    /// The stream timestamp of the picture to check (the last access unit).
    pub pts_us: u64,
    /// Bytes of each access unit, in order; they add up to `data.len()`.
    pub sizes: Vec<usize>,
    pub data: Vec<u8>,
}
