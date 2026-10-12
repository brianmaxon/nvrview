//! Matching alerts to entities and carrying out the calls.

use crate::config::{self, Action, Config, ConfigError, MatchMode, Rule};
use crate::http::{Http, HttpRequest, Method, Token, UreqHttp};
use crate::template::{self, Facts};
use alerts::{CameraId, Class, Severity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What the engine decided. Rules match alerts only by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Alert,
    Quiet,
    Suppress,
}

/// The typed facts of one decision, as the host passes them. Nothing from a
/// trigger payload is in here; free text is only the user's camera label
/// and the engine's own explanation, and both are cleaned before use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub alert_id: u64,
    /// 1 for a new alert; higher revisions are updates and are not
    /// delivered again.
    pub rev: u32,
    pub camera: CameraId,
    pub camera_label: String,
    pub class: Class,
    /// The trigger source id, when the trigger came from a configured
    /// source.
    pub source: Option<String>,
    pub severity: Severity,
    pub decision: DecisionKind,
    /// Event time, milliseconds since the Unix epoch.
    pub at_ms: i64,
    /// Minutes east of UTC, for the `{time}` placeholder.
    pub utc_offset_min: i32,
    pub explanation: String,
}

/// Time and sleeping, injected so tests do not wait.
pub trait Clock: Send + Sync {
    /// Milliseconds on a monotonic clock.
    fn now_ms(&self) -> u64;
    fn sleep(&self, d: Duration);
}

pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> SystemClock {
        SystemClock {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// Where rate-limited error lines go. The default is `tracing::warn!`.
pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    ReadState,
    /// Turning an entity that was already on off, before the pulse.
    ResetOff,
    TurnOn,
    TurnOff,
    Service,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Timeout,
    /// Lookup, connect, or TLS failure.
    Connect,
    Network,
    /// 5xx: retried.
    Server,
    /// 401 or 403: the token is wrong or lacks access.
    Unauthorized,
    /// 404: no such entity or service.
    NotFound,
    /// Any other 4xx.
    Client,
    /// A reply that is not what the API documents.
    BadReply,
}

/// One HTTP attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub step: Step,
    /// 1-based within its request.
    pub attempt: u32,
    pub http_status: Option<u16>,
    pub error: Option<ErrorKind>,
    pub at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityStatus {
    Done,
    Failed,
    /// Skipped: this target spoke within its cooldown.
    Cooldown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityResult {
    /// The entity id, or `notify.<service>`.
    pub target: String,
    pub status: EntityStatus,
    /// For example "Home Assistant: input_boolean.example pulsed (200)".
    pub summary: String,
    pub attempts: Vec<Attempt>,
}

/// The result the host records as a `delivery` trace step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputResult {
    /// Always `home_assistant`.
    pub output: String,
    /// False if any target failed. Nothing matching, or a cooldown, is not
    /// a failure.
    pub ok: bool,
    pub summary: String,
    pub entities: Vec<EntityResult>,
}

struct Failure {
    kind: ErrorKind,
    status: Option<u16>,
}

enum Expect {
    /// A service call: the reply is a JSON list of changed states.
    List,
    /// A state read: a JSON object with a string `state`.
    State,
}

pub struct HaOutput {
    config: Config,
    base: String,
    token: Token,
    http: Arc<dyn Http>,
    clock: Arc<dyn Clock>,
    cooldowns: Mutex<HashMap<String, u64>>,
    log: LogSink,
    log_state: Mutex<LogWindow>,
}

#[derive(Default)]
struct LogWindow {
    start_ms: u64,
    count: u32,
    suppressed: u32,
}

const LOG_PER_WINDOW: u32 = 5;
const LOG_WINDOW_MS: u64 = 60_000;
const MAX_BACKOFF_MS: u64 = 8_000;

impl HaOutput {
    /// The real network and clock.
    pub fn new(config: Config, token: Token) -> Result<HaOutput, ConfigError> {
        let timeout = Duration::from_millis(config.timeout_ms.clamp(1_000, 30_000));
        HaOutput::with_parts(
            config,
            token,
            Arc::new(UreqHttp::new(timeout)),
            Arc::new(SystemClock::new()),
        )
    }

    /// With the network and clock supplied (tests, or a host's own client).
    pub fn with_parts(
        mut config: Config,
        token: Token,
        http: Arc<dyn Http>,
        clock: Arc<dyn Clock>,
    ) -> Result<HaOutput, ConfigError> {
        config.validate()?;
        let base = config::parse_base(&config.base_url)?.root;
        config.max_attempts = config.max_attempts.clamp(1, 5);
        config.backoff_ms = config.backoff_ms.min(MAX_BACKOFF_MS);
        config.pulse_gap_ms = config.pulse_gap_ms.min(60_000);
        Ok(HaOutput {
            config,
            base,
            token,
            http,
            clock,
            cooldowns: Mutex::new(HashMap::new()),
            log: Arc::new(|line| tracing::warn!("{line}")),
            log_state: Mutex::new(LogWindow::default()),
        })
    }

    /// Replaces where error lines go.
    pub fn with_log(mut self, log: LogSink) -> HaOutput {
        self.log = log;
        self
    }

    /// The rules that fire for this alert, in order, one per distinct
    /// target.
    pub fn select<'a>(&'a self, alert: &Alert) -> Vec<&'a Rule> {
        let mut out: Vec<&Rule> = Vec::new();
        for rule in self.config.rules.iter().filter(|r| matches(r, alert)) {
            if out
                .iter()
                .any(|o| o.action.target() == rule.action.target())
            {
                continue;
            }
            out.push(rule);
            if self.config.mode == MatchMode::FirstMatch {
                break;
            }
        }
        out
    }

    /// Delivers one alert. Blocks (retries, and the pulse gap).
    pub fn deliver(&self, alert: &Alert) -> OutputResult {
        let mut entities = Vec::new();
        let summary_if_empty = if alert.rev > 1 {
            "Home Assistant: update of an earlier alert, not sent again"
        } else {
            "Home Assistant: no mapping matched"
        };
        if alert.rev <= 1 {
            for rule in self.select(alert) {
                entities.push(self.run(rule, alert));
            }
        }
        let ok = entities.iter().all(|e| e.status != EntityStatus::Failed);
        let summary = if entities.is_empty() {
            summary_if_empty.to_string()
        } else {
            entities
                .iter()
                .map(|e| e.summary.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        };
        OutputResult {
            output: "home_assistant".to_string(),
            ok,
            summary,
            entities,
        }
    }

    fn run(&self, rule: &Rule, alert: &Alert) -> EntityResult {
        let target = rule.action.target();
        let cooldown = rule.cooldown_ms.unwrap_or(self.config.cooldown_ms);
        let now = self.clock.now_ms();
        let recent = self
            .cooldowns
            .lock()
            .unwrap()
            .get(&target)
            .is_some_and(|&last| now.saturating_sub(last) < cooldown);
        if recent {
            return EntityResult {
                summary: format!("Home Assistant: {target} skipped (cooldown)"),
                target,
                status: EntityStatus::Cooldown,
                attempts: Vec::new(),
            };
        }
        let mut attempts = Vec::new();
        let outcome = match &rule.action {
            Action::Pulse { entity_id } => self.pulse(entity_id, &mut attempts),
            Action::TurnOn { entity_id } => self
                .service(
                    "turn_on",
                    entity_id,
                    json!({ "entity_id": entity_id }),
                    &mut attempts,
                )
                .map(|s| format!("turned on ({s})")),
            Action::Script { entity_id } => {
                let t = self.facts_text(alert);
                let body = json!({
                    "entity_id": entity_id,
                    "variables": {
                        "message": t.message,
                        "camera": t.camera,
                        "class": t.class,
                        "time": t.time,
                        "severity": t.severity,
                        "explanation": t.explanation,
                    }
                });
                self.service("turn_on", entity_id, body, &mut attempts)
                    .map(|s| format!("script started ({s})"))
            }
            Action::Notify { service, title } => {
                let t = self.facts_text(alert);
                let mut body = json!({ "message": t.message });
                if let Some(title) = title {
                    body["title"] = json!(t.render(title));
                }
                let path = format!("/api/services/notify/{service}");
                self.post(Step::Service, &path, &body, &mut attempts, Expect::List)
                    .map(|_| "notified (200)".to_string())
            }
        };
        match outcome {
            Ok(text) => {
                self.cooldowns
                    .lock()
                    .unwrap()
                    .insert(target.clone(), self.clock.now_ms());
                EntityResult {
                    summary: format!("Home Assistant: {target} {text}"),
                    target,
                    status: EntityStatus::Done,
                    attempts,
                }
            }
            Err(f) => {
                // A pulse that turned the entity on but could not turn it
                // off still counts as spoken.
                if attempts
                    .iter()
                    .any(|a| a.step == Step::TurnOn && a.http_status == Some(200))
                {
                    self.cooldowns
                        .lock()
                        .unwrap()
                        .insert(target.clone(), self.clock.now_ms());
                }
                let step = attempts.last().map(|a| a.step).unwrap_or(Step::Service);
                let why = describe(&f, step);
                self.log_error(&format!("{target} failed: {why}"));
                EntityResult {
                    summary: format!("Home Assistant: {target} failed: {why}"),
                    target,
                    status: EntityStatus::Failed,
                    attempts,
                }
            }
        }
    }

    /// Read the state; reset to off if on; turn on; wait; turn off.
    fn pulse(&self, entity: &str, rec: &mut Vec<Attempt>) -> Result<String, Failure> {
        let state = self.get(Step::ReadState, &format!("/api/states/{entity}"), rec)?;
        let was_on = state.get("state").and_then(Value::as_str) == Some("on");
        let body = json!({ "entity_id": entity });
        if was_on {
            self.service_step(Step::ResetOff, "turn_off", entity, &body, rec)?;
        }
        self.service_step(Step::TurnOn, "turn_on", entity, &body, rec)?;
        self.clock
            .sleep(Duration::from_millis(self.config.pulse_gap_ms));
        self.service_step(Step::TurnOff, "turn_off", entity, &body, rec)?;
        Ok(if was_on {
            "pulsed after a reset to off (200)".to_string()
        } else {
            "pulsed (200)".to_string()
        })
    }

    fn service(
        &self,
        name: &str,
        entity: &str,
        body: Value,
        rec: &mut Vec<Attempt>,
    ) -> Result<u16, Failure> {
        self.service_step(Step::Service, name, entity, &body, rec)
            .map(|_| 200)
    }

    fn service_step(
        &self,
        step: Step,
        service: &str,
        entity: &str,
        body: &Value,
        rec: &mut Vec<Attempt>,
    ) -> Result<(), Failure> {
        // Entity ids were validated to domain.object_id; for script
        // `turn_on` the domain is `script`.
        let (domain, _) = config::split_entity(entity).ok_or(Failure {
            kind: ErrorKind::Client,
            status: None,
        })?;
        let path = format!("/api/services/{domain}/{service}");
        self.post(step, &path, body, rec, Expect::List).map(|_| ())
    }

    fn get(&self, step: Step, path: &str, rec: &mut Vec<Attempt>) -> Result<Value, Failure> {
        self.call(step, Method::Get, path, None, rec, Expect::State)
    }

    fn post(
        &self,
        step: Step,
        path: &str,
        body: &Value,
        rec: &mut Vec<Attempt>,
        expect: Expect,
    ) -> Result<Value, Failure> {
        let bytes = serde_json::to_vec(body).unwrap_or_default();
        self.call(step, Method::Post, path, Some(&bytes), rec, expect)
    }

    /// One request with retries: network errors and 5xx are retried with
    /// doubling backoff up to `max_attempts`; 4xx and bad replies are not.
    fn call(
        &self,
        step: Step,
        method: Method,
        path: &str,
        body: Option<&[u8]>,
        rec: &mut Vec<Attempt>,
        expect: Expect,
    ) -> Result<Value, Failure> {
        let url = format!("{}{path}", self.base);
        let max = self.config.max_attempts;
        let mut n = 0;
        loop {
            n += 1;
            let req = HttpRequest {
                method,
                url: &url,
                token: &self.token,
                body,
            };
            let result = self.http.send(&req);
            let at_ms = self.clock.now_ms();
            let (status, failure) = match result {
                Err(e) => {
                    let kind = match e {
                        crate::http::NetError::Timeout => ErrorKind::Timeout,
                        crate::http::NetError::Connect => ErrorKind::Connect,
                        crate::http::NetError::Other => ErrorKind::Network,
                    };
                    (None, Some(kind))
                }
                Ok(resp) => {
                    let status = Some(resp.status);
                    match resp.status {
                        200 => match parse_reply(&resp.body, &expect) {
                            Some(v) => {
                                rec.push(Attempt {
                                    step,
                                    attempt: n,
                                    http_status: status,
                                    error: None,
                                    at_ms,
                                });
                                return Ok(v);
                            }
                            None => (status, Some(ErrorKind::BadReply)),
                        },
                        401 | 403 => (status, Some(ErrorKind::Unauthorized)),
                        404 => (status, Some(ErrorKind::NotFound)),
                        400..=499 => (status, Some(ErrorKind::Client)),
                        500..=599 => (status, Some(ErrorKind::Server)),
                        _ => (status, Some(ErrorKind::BadReply)),
                    }
                }
            };
            let kind = failure.unwrap_or(ErrorKind::Network);
            rec.push(Attempt {
                step,
                attempt: n,
                http_status: status,
                error: Some(kind),
                at_ms,
            });
            let retry = matches!(
                kind,
                ErrorKind::Timeout | ErrorKind::Connect | ErrorKind::Network | ErrorKind::Server
            );
            if !retry || n >= max {
                return Err(Failure { kind, status });
            }
            let wait = (self.config.backoff_ms << (n - 1)).min(MAX_BACKOFF_MS);
            self.clock.sleep(Duration::from_millis(wait));
        }
    }

    fn facts_text(&self, alert: &Alert) -> Text {
        let time = format_time(alert.at_ms, alert.utc_offset_min);
        let camera = template::clean(&alert.camera_label);
        let explanation = template::clean(&alert.explanation);
        let class = alert.class.lower().to_string();
        let severity = alert.severity.as_str().to_string();
        let message = template::render(
            &self.config.message_template,
            &Facts {
                camera: &camera,
                class: &class,
                time: &time,
                severity: &severity,
                explanation: &explanation,
            },
        );
        Text {
            message,
            camera,
            class,
            time,
            severity,
            explanation,
        }
    }

    /// Writes at most 5 lines per minute; the next line after a quiet spell
    /// says how many were dropped.
    fn log_error(&self, line: &str) {
        let now = self.clock.now_ms();
        let mut w = self.log_state.lock().unwrap();
        if w.count == 0 || now.saturating_sub(w.start_ms) >= LOG_WINDOW_MS {
            let dropped = w.suppressed;
            w.start_ms = now;
            w.count = 0;
            w.suppressed = 0;
            if dropped > 0 {
                (self.log)(&format!(
                    "Home Assistant: {dropped} more errors were not logged"
                ));
            }
        }
        if w.count < LOG_PER_WINDOW {
            w.count += 1;
            (self.log)(&format!("Home Assistant: {line}"));
        } else {
            w.suppressed += 1;
        }
    }
}

struct Text {
    message: String,
    camera: String,
    class: String,
    time: String,
    severity: String,
    explanation: String,
}

impl Text {
    fn render(&self, template: &str) -> String {
        template::render(
            template,
            &Facts {
                camera: &self.camera,
                class: &self.class,
                time: &self.time,
                severity: &self.severity,
                explanation: &self.explanation,
            },
        )
    }
}

fn matches(rule: &Rule, a: &Alert) -> bool {
    (rule.classes.is_empty() || rule.classes.contains(&a.class))
        && (rule.sources.is_empty() || a.source.as_ref().is_some_and(|s| rule.sources.contains(s)))
        && (rule.cameras.is_empty() || rule.cameras.contains(&a.camera))
        && rule.min_severity.is_none_or(|m| a.severity >= m)
        && rule.decisions.contains(&a.decision)
}

fn parse_reply(body: &[u8], expect: &Expect) -> Option<Value> {
    let v: Value = serde_json::from_slice(body).ok()?;
    match expect {
        Expect::List => v.is_array().then_some(v),
        Expect::State => v.get("state").is_some_and(Value::is_string).then_some(v),
    }
}

fn describe(f: &Failure, step: Step) -> String {
    let what = match f.kind {
        ErrorKind::Timeout => "timed out".to_string(),
        ErrorKind::Connect => "could not connect".to_string(),
        ErrorKind::Network => "network error".to_string(),
        ErrorKind::Server => "server error".to_string(),
        ErrorKind::Unauthorized => "token rejected".to_string(),
        ErrorKind::NotFound => match step {
            Step::ReadState => "entity not found".to_string(),
            _ => "entity or service not found".to_string(),
        },
        ErrorKind::Client => "request refused".to_string(),
        ErrorKind::BadReply => "unexpected reply".to_string(),
    };
    let left_on = if step == Step::TurnOff {
        "; the entity may be left on"
    } else {
        ""
    };
    match f.status {
        Some(s) => format!("{what} ({s}){left_on}"),
        None => format!("{what}{left_on}"),
    }
}

/// `HH:MM` local to the offset.
fn format_time(at_ms: i64, offset_min: i32) -> String {
    let mins = at_ms.div_euclid(60_000) + i64::from(offset_min);
    let day = mins.rem_euclid(24 * 60);
    format!("{:02}:{:02}", day / 60, day % 60)
}
