//! Delivery against an in-process mock of the Home Assistant REST API.
//! The mock listens on a loopback port; the clock is simulated, so retries
//! and the pulse gap never wait.

use alerts::{CameraId, Class, Severity};
use ha_output::*;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TOKEN: &str = "example-token-0123456789";

#[derive(Default)]
struct MockState {
    /// Entity states by id.
    states: HashMap<String, String>,
    /// Forced statuses for the next requests (consumed in order); 0 passes.
    forced: VecDeque<u16>,
    /// "METHOD path (auth ok)" lines, plus "sleep N" lines from the clock.
    log: Vec<String>,
    bodies: Vec<(String, Value)>,
    expected_token: String,
    /// Legacy notify services (`notify.<name>`), by bare name. Notify
    /// entities are `notify.<name>` keys of `states`.
    legacy: Vec<String>,
}

struct Mock {
    addr: SocketAddr,
    state: Arc<Mutex<MockState>>,
}

impl Mock {
    fn start() -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(MockState {
            expected_token: TOKEN.to_string(),
            ..Default::default()
        }));
        let st = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let st = st.clone();
                std::thread::spawn(move || serve(stream, st));
            }
        });
        Mock { addr, state }
    }

    fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn set(&self, entity: &str, state: &str) {
        self.state
            .lock()
            .unwrap()
            .states
            .insert(entity.to_string(), state.to_string());
    }

    /// A notify entity `notify.<name>`.
    fn add_notify_entity(&self, name: &str) {
        self.set(&format!("notify.{name}"), "unknown");
    }

    /// A legacy notify service `notify.<name>`.
    fn add_legacy(&self, name: &str) {
        self.state.lock().unwrap().legacy.push(name.to_string());
    }

    /// Removes a notify entity or legacy service, as when an integration is
    /// replaced.
    fn remove_notify(&self, name: &str) {
        let mut s = self.state.lock().unwrap();
        s.states.remove(&format!("notify.{name}"));
        s.legacy.retain(|n| n != name);
    }

    /// The request bodies seen for one path, in order.
    fn bodies_for(&self, path: &str) -> Vec<Value> {
        let s = self.state.lock().unwrap();
        s.bodies
            .iter()
            .filter(|(p, _)| p == path)
            .map(|(_, b)| b.clone())
            .collect()
    }

    fn force(&self, statuses: &[u16]) {
        self.state.lock().unwrap().forced.extend(statuses);
    }

    fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    fn state_of(&self, entity: &str) -> String {
        self.state.lock().unwrap().states[entity].clone()
    }
}

fn serve(stream: std::net::TcpStream, st: Arc<Mutex<MockState>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut auth = String::new();
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).unwrap();
        let h = h.trim_end().to_string();
        if h.is_empty() {
            break;
        }
        let lower = h.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            len = v.trim().parse().unwrap();
        }
        if lower.starts_with("authorization:") {
            auth = h["authorization:".len()..].trim().to_string();
        }
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body).unwrap();
    let json_body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    let (status, reply) = {
        let mut s = st.lock().unwrap();
        let authed = auth == format!("Bearer {}", s.expected_token);
        s.log.push(format!(
            "{method} {path}{}",
            if authed { "" } else { " (bad token)" }
        ));
        s.bodies.push((path.clone(), json_body.clone()));
        let forced = s.forced.pop_front().unwrap_or(0);
        if !authed {
            (401, json!({"message": "401: Unauthorized"}))
        } else if forced != 0 {
            (forced, json!({"message": "forced"}))
        } else if let Some(id) = path.strip_prefix("/api/states/") {
            match s.states.get(id) {
                Some(v) => (200, json!({"entity_id": id, "state": v})),
                None => (404, json!({"message": "Entity not found."})),
            }
        } else if path == "/api/states" {
            let list: Vec<Value> = s
                .states
                .iter()
                .map(|(k, v)| json!({"entity_id": k, "state": v}))
                .collect();
            (200, json!(list))
        } else if path == "/api/services" {
            let mut notify = serde_json::Map::new();
            notify.insert("send_message".into(), json!({}));
            notify.insert("persistent_notification".into(), json!({}));
            for n in &s.legacy {
                notify.insert(n.clone(), json!({}));
            }
            (
                200,
                json!([
                    {"domain": "light", "services": {"turn_on": {}}},
                    {"domain": "notify", "services": notify}
                ]),
            )
        } else if let Some(svc) = path.strip_prefix("/api/services/") {
            let entity = json_body["entity_id"].as_str().unwrap_or("").to_string();
            match svc.rsplit_once('/') {
                Some((_, "turn_on")) if s.states.contains_key(&entity) => {
                    s.states.insert(entity.clone(), "on".into());
                    (200, json!([{"entity_id": entity, "state": "on"}]))
                }
                Some((_, "turn_off")) if s.states.contains_key(&entity) => {
                    s.states.insert(entity.clone(), "off".into());
                    (200, json!([{"entity_id": entity, "state": "off"}]))
                }
                Some(("script", "turn_on")) => (200, json!([])),
                Some(("notify", "send_message")) if s.states.contains_key(&entity) => {
                    (200, json!([]))
                }
                Some(("notify", "send_message")) => (
                    400,
                    json!({"message": format!("Entity {entity} not found.")}),
                ),
                Some(("notify", name)) if s.legacy.iter().any(|n| n == name) => (200, json!([])),
                Some(("notify", name)) => (
                    400,
                    json!({"message": format!("Service notify.{name} not found.")}),
                ),
                _ => (404, json!({"message": "Service not found."})),
            }
        } else {
            (404, json!({}))
        }
    };
    let body = reply.to_string();
    let mut out = stream;
    let _ = write!(
        out,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// A simulated clock that logs its sleeps into the mock's request log, so
/// tests can assert the order of requests and waits.
struct SimClock {
    now: Mutex<u64>,
    state: Arc<Mutex<MockState>>,
}

impl Clock for SimClock {
    fn now_ms(&self) -> u64 {
        *self.now.lock().unwrap()
    }
    fn sleep(&self, d: Duration) {
        *self.now.lock().unwrap() += d.as_millis() as u64;
        self.state
            .lock()
            .unwrap()
            .log
            .push(format!("sleep {}", d.as_millis()));
    }
}

impl SimClock {
    fn advance(&self, ms: u64) {
        *self.now.lock().unwrap() += ms;
    }
}

fn alert(class: Class) -> Alert {
    Alert {
        alert_id: 1,
        rev: 1,
        camera: CameraId::new(1, 2, 0),
        camera_label: "Example Camera".into(),
        class,
        source: None,
        severity: Severity::High,
        decision: DecisionKind::Alert,
        at_ms: 1_718_900_000_000,
        utc_offset_min: 0,
        explanation: "Person on Example Camera at night".into(),
        test: false,
    }
}

fn pulse_rule(entity: &str) -> Rule {
    rule(Action::Pulse {
        entity_id: entity.into(),
    })
}

fn rule(action: Action) -> Rule {
    Rule {
        classes: vec![],
        sources: vec![],
        cameras: vec![],
        min_severity: None,
        decisions: vec![DecisionKind::Alert],
        action,
        cooldown_ms: None,
    }
}

struct Rig {
    mock: Mock,
    clock: Arc<SimClock>,
    logs: Arc<Mutex<Vec<String>>>,
}

impl Rig {
    fn new() -> Rig {
        let mock = Mock::start();
        let clock = Arc::new(SimClock {
            now: Mutex::new(0),
            state: mock.state.clone(),
        });
        Rig {
            mock,
            clock,
            logs: Arc::default(),
        }
    }

    fn output(&self, rules: Vec<Rule>, tweak: impl FnOnce(&mut Config)) -> HaOutput {
        self.output_at(&self.mock.url(), rules, tweak)
    }

    fn output_at(&self, url: &str, rules: Vec<Rule>, tweak: impl FnOnce(&mut Config)) -> HaOutput {
        let mut cfg = Config::new(url);
        cfg.rules = rules;
        tweak(&mut cfg);
        let logs = self.logs.clone();
        HaOutput::with_parts(
            cfg,
            Token::new(TOKEN),
            Arc::new(UreqHttp::new(Duration::from_secs(5))),
            self.clock.clone(),
        )
        .unwrap()
        .with_log(Arc::new(move |l| logs.lock().unwrap().push(l.to_string())))
    }
}

const E: &str = "input_boolean.example_alert";

#[test]
fn pulse_reads_turns_on_waits_the_gap_then_turns_off() {
    let rig = Rig::new();
    rig.mock.set(E, "off");
    let out = rig.output(vec![pulse_rule(E)], |_| {});
    let r = out.deliver(&alert(Class::Person));
    assert!(r.ok, "{r:?}");
    assert_eq!(r.summary, format!("Home Assistant: {E} pulsed (200)"));
    assert_eq!(
        rig.mock.log(),
        vec![
            format!("GET /api/states/{E}"),
            "POST /api/services/input_boolean/turn_on".to_string(),
            "sleep 2000".to_string(),
            "POST /api/services/input_boolean/turn_off".to_string(),
        ]
    );
    assert_eq!(rig.mock.state_of(E), "off");
    assert_eq!(r.entities[0].attempts.len(), 3);
}

#[test]
fn already_on_is_reset_to_off_before_the_pulse() {
    let rig = Rig::new();
    rig.mock.set(E, "on");
    let out = rig.output(vec![pulse_rule(E)], |c| c.pulse_gap_ms = 500);
    let r = out.deliver(&alert(Class::Person));
    assert!(r.ok, "{r:?}");
    assert!(r.summary.contains("after a reset to off"));
    assert_eq!(
        rig.mock.log(),
        vec![
            format!("GET /api/states/{E}"),
            "POST /api/services/input_boolean/turn_off".to_string(),
            "POST /api/services/input_boolean/turn_on".to_string(),
            "sleep 500".to_string(),
            "POST /api/services/input_boolean/turn_off".to_string(),
        ]
    );
    let steps: Vec<Step> = r.entities[0].attempts.iter().map(|a| a.step).collect();
    assert_eq!(
        steps,
        [Step::ReadState, Step::ResetOff, Step::TurnOn, Step::TurnOff]
    );
}

#[test]
fn turn_on_only_leaves_the_entity_on() {
    let rig = Rig::new();
    rig.mock.set(E, "off");
    let out = rig.output(
        vec![rule(Action::TurnOn {
            entity_id: E.into(),
        })],
        |_| {},
    );
    let r = out.deliver(&alert(Class::Person));
    assert!(r.ok);
    assert_eq!(rig.mock.state_of(E), "on");
    assert_eq!(rig.mock.log().len(), 1);
}

#[test]
fn unauthorized_is_reported_and_not_retried() {
    let rig = Rig::new();
    rig.mock.set(E, "off");
    rig.mock.state.lock().unwrap().expected_token = "another".into();
    let out = rig.output(vec![pulse_rule(E)], |_| {});
    let r = out.deliver(&alert(Class::Person));
    assert!(!r.ok);
    assert_eq!(r.entities[0].status, EntityStatus::Failed);
    assert!(r.summary.contains("token rejected (401)"), "{}", r.summary);
    assert_eq!(rig.mock.log().len(), 1, "no retry on 4xx");
    assert_eq!(
        r.entities[0].attempts[0].error,
        Some(ErrorKind::Unauthorized)
    );
}

#[test]
fn missing_entity_is_a_404_without_retry_or_service_calls() {
    let rig = Rig::new();
    let out = rig.output(vec![pulse_rule(E)], |_| {});
    let r = out.deliver(&alert(Class::Person));
    assert!(!r.ok);
    assert!(
        r.summary.contains("entity not found (404)"),
        "{}",
        r.summary
    );
    assert_eq!(rig.mock.log(), vec![format!("GET /api/states/{E}")]);
}

#[test]
fn a_503_is_retried_with_backoff_then_succeeds() {
    let rig = Rig::new();
    rig.mock.set(E, "off");
    rig.mock.force(&[503, 503]);
    let out = rig.output(vec![pulse_rule(E)], |_| {});
    let r = out.deliver(&alert(Class::Person));
    assert!(r.ok, "{r:?}");
    let log = rig.mock.log();
    assert_eq!(
        &log[..5],
        [
            format!("GET /api/states/{E}"),
            "sleep 500".to_string(),
            format!("GET /api/states/{E}"),
            "sleep 1000".to_string(),
            format!("GET /api/states/{E}"),
        ]
    );
    let a = &r.entities[0].attempts;
    assert_eq!(a[0].error, Some(ErrorKind::Server));
    assert_eq!(a[2].attempt, 3);
    assert_eq!(a[2].error, None);
}

#[test]
fn retries_stop_at_the_maximum_and_fail() {
    let rig = Rig::new();
    rig.mock.set(E, "off");
    rig.mock.force(&[503, 503, 503, 503]);
    let out = rig.output(vec![pulse_rule(E)], |_| {});
    let r = out.deliver(&alert(Class::Person));
    assert!(!r.ok);
    assert_eq!(r.entities[0].attempts.len(), 3);
    assert!(r.summary.contains("server error (503)"));
}

#[test]
fn a_refused_connection_is_retried_then_reported() {
    let rig = Rig::new();
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let out = rig.output_at(&format!("http://{dead}"), vec![pulse_rule(E)], |_| {});
    let r = out.deliver(&alert(Class::Person));
    assert!(!r.ok);
    let a = &r.entities[0].attempts;
    assert_eq!(a.len(), 3);
    assert!(a.iter().all(|x| x.error == Some(ErrorKind::Connect)));
    assert_eq!(rig.mock.log(), vec!["sleep 500", "sleep 1000"]);
}

#[test]
fn failing_to_turn_off_is_reported_and_still_counts_as_spoken() {
    let rig = Rig::new();
    rig.mock.set(E, "off");
    // GET ok, turn_on ok, then three 500s for turn_off.
    rig.mock.force(&[0, 0, 500, 500, 500]);
    let out = rig.output(vec![pulse_rule(E)], |_| {});
    let r = out.deliver(&alert(Class::Person));
    assert!(!r.ok);
    assert!(r.summary.contains("may be left on"), "{}", r.summary);
    let again = out.deliver(&alert(Class::Person));
    assert_eq!(again.entities[0].status, EntityStatus::Cooldown);
}

#[test]
fn cooldown_skips_a_repeat_until_it_expires() {
    let rig = Rig::new();
    rig.mock.set(E, "off");
    let out = rig.output(vec![pulse_rule(E)], |c| c.cooldown_ms = 30_000);
    assert!(out.deliver(&alert(Class::Person)).ok);
    let n = rig.mock.log().len();
    let second = out.deliver(&alert(Class::Person));
    assert!(second.ok);
    assert_eq!(second.entities[0].status, EntityStatus::Cooldown);
    assert_eq!(
        second.summary,
        format!("Home Assistant: {E} skipped (cooldown)")
    );
    assert_eq!(rig.mock.log().len(), n, "no request during the cooldown");
    rig.clock.advance(30_000);
    assert_eq!(
        out.deliver(&alert(Class::Person)).entities[0].status,
        EntityStatus::Done
    );
}

#[test]
fn first_match_versus_all_matches() {
    let rig = Rig::new();
    let (a, b, c) = (
        "input_boolean.example_a",
        "input_boolean.example_b",
        "input_boolean.example_c",
    );
    for e in [a, b, c] {
        rig.mock.set(e, "off");
    }
    let mut person = pulse_rule(a);
    person.classes = vec![Class::Person];
    let mut any_high = pulse_rule(b);
    any_high.min_severity = Some(Severity::High);
    let mut vehicle = pulse_rule(c);
    vehicle.classes = vec![Class::Vehicle];
    let rules = vec![person, any_high, vehicle];

    let first = rig.output(rules.clone(), |_| {});
    let sel: Vec<String> = first
        .select(&alert(Class::Person))
        .iter()
        .map(|r| r.action.target())
        .collect();
    assert_eq!(sel, [a]);

    let all = rig.output(rules, |c| c.mode = MatchMode::AllMatches);
    let sel: Vec<String> = all
        .select(&alert(Class::Person))
        .iter()
        .map(|r| r.action.target())
        .collect();
    assert_eq!(sel, [a, b]);
    let mut low = alert(Class::Vehicle);
    low.severity = Severity::Normal;
    let sel: Vec<String> = all.select(&low).iter().map(|r| r.action.target()).collect();
    assert_eq!(sel, [c]);
}

#[test]
fn matching_on_source_camera_and_decision() {
    let rig = Rig::new();
    let mut r = pulse_rule(E);
    r.sources = vec!["porch-sensor".into()];
    r.cameras = vec![CameraId::new(1, 2, 0)];
    let out = rig.output(vec![r], |_| {});
    let mut a = alert(Class::Person);
    assert!(out.select(&a).is_empty(), "no source");
    a.source = Some("porch-sensor".into());
    assert_eq!(out.select(&a).len(), 1);
    a.camera = CameraId::new(1, 3, 0);
    assert!(out.select(&a).is_empty(), "other camera");
    a.camera = CameraId::new(1, 2, 0);
    a.decision = DecisionKind::Quiet;
    assert!(out.select(&a).is_empty(), "alerts only by default");
}

#[test]
fn nothing_matching_and_updates_send_nothing() {
    let rig = Rig::new();
    let mut r = pulse_rule(E);
    r.classes = vec![Class::Vehicle];
    let out = rig.output(vec![r, pulse_rule("input_boolean.example_b")], |_| {});
    rig.mock.set("input_boolean.example_b", "off");
    let none = HaOutput::with_parts(
        Config::new(rig.mock.url()),
        Token::new(TOKEN),
        Arc::new(UreqHttp::new(Duration::from_secs(5))),
        rig.clock.clone(),
    )
    .unwrap();
    let r = none.deliver(&alert(Class::Person));
    assert!(r.ok && r.entities.is_empty());
    assert_eq!(r.summary, "Home Assistant: no mapping matched");
    let mut update = alert(Class::Person);
    update.rev = 2;
    let r = out.deliver(&update);
    assert!(r.ok && r.entities.is_empty());
    assert!(rig.mock.log().is_empty());
}

#[test]
fn notify_and_script_carry_only_cleaned_typed_facts() {
    let rig = Rig::new();
    rig.mock.add_legacy("example_service");
    let out = rig.output(
        vec![
            rule(Action::Notify {
                service: "example_service".into(),
                title: Some("Alert: {camera}".into()),
            }),
            rule(Action::Script {
                entity_id: "script.example_alert".into(),
            }),
        ],
        |c| {
            c.mode = MatchMode::AllMatches;
            c.message_template = "{class} at {time} on {camera}: {explanation}".into();
        },
    );
    let mut a = alert(Class::Person);
    a.camera_label = "Back {explanation}\nDoor".into();
    a.explanation = "Seen\u{7}{camera}".into();
    let r = out.deliver(&a);
    assert!(r.ok, "{r:?}");
    let bodies = rig.mock.state.lock().unwrap().bodies.clone();
    // The first request resolves the target; the notification follows.
    assert_eq!(bodies[0].0, "/api/states/notify.example_service");
    let n = &bodies[1];
    assert_eq!(n.0, "/api/services/notify/example_service");
    // 1718900000 s is 16:13:20 UTC.
    assert_eq!(
        n.1["message"],
        "person at 16:13 on Back {explanation} Door: Seen {camera}"
    );
    assert_eq!(n.1["title"], "Alert: Back {explanation} Door");
    let s = &bodies[2];
    assert_eq!(s.0, "/api/services/script/turn_on");
    assert_eq!(s.1["entity_id"], "script.example_alert");
    assert_eq!(s.1["variables"]["class"], "person");
    assert_eq!(s.1["variables"]["severity"], "high");
}

#[test]
fn the_token_never_appears_in_results_logs_or_debug_output() {
    let rig = Rig::new();
    rig.mock.set(E, "off");
    rig.mock.force(&[500, 500, 500]);
    let out = rig.output(vec![pulse_rule(E)], |_| {});
    let r = out.deliver(&alert(Class::Person));
    assert!(!r.ok);
    let all = format!(
        "{} {:?} {}",
        serde_json::to_string(&r).unwrap(),
        r,
        rig.logs.lock().unwrap().join("\n")
    );
    assert!(!all.contains(TOKEN));
    assert!(
        !rig.logs.lock().unwrap().is_empty(),
        "the failure was logged"
    );
    assert_eq!(format!("{:?}", Token::new(TOKEN)), "Token([redacted])");
    // Wrong-token path too.
    rig.mock.state.lock().unwrap().expected_token = "x".into();
    let r = rig
        .output(vec![pulse_rule(E)], |_| {})
        .deliver(&alert(Class::Person));
    assert!(!format!("{r:?}").contains(TOKEN));
}

#[test]
fn error_logs_are_rate_limited() {
    let rig = Rig::new();
    let out = rig.output(vec![pulse_rule(E)], |c| c.cooldown_ms = 0);
    // The entity does not exist: every delivery fails with a 404.
    for _ in 0..8 {
        assert!(!out.deliver(&alert(Class::Person)).ok);
    }
    assert_eq!(rig.logs.lock().unwrap().len(), 5);
    rig.clock.advance(60_000);
    assert!(!out.deliver(&alert(Class::Person)).ok);
    let logs = rig.logs.lock().unwrap().clone();
    assert_eq!(logs.len(), 7, "{logs:?}");
    assert!(logs[5].contains("3 more errors"));
}

#[test]
fn config_refuses_http_outside_the_local_network_and_bad_names() {
    // Private addresses are built from octets: the repository holds no
    // real-looking addresses outside the documentation ranges.
    let p = format!("{}.{}.{}.{}", 192, 168, 1, 10);
    let q = format!("{}.{}.{}.{}", 10, 0, 0, 5);
    let ok = |u: &str| Config::new(u).validate();
    assert!(
        ok("http://192.0.2.10:8123").is_err(),
        "documentation range is not private"
    );
    assert!(ok(&format!("http://{p}:8123")).is_ok());
    assert!(ok(&format!("http://{q}")).is_ok());
    assert!(ok("http://homeassistant.local:8123").is_ok());
    assert!(ok("http://homeassistant:8123").is_ok());
    assert!(ok("http://[::1]:8123").is_ok());
    assert!(ok("http://example.com").is_err());
    assert!(ok("http://198.51.100.7").is_err());
    assert!(ok(&format!("http://user:pw@{p}")).is_err());
    assert!(ok(&format!("ftp://{p}")).is_err());
    assert!(ok(&format!("http://{p}/?a=b")).is_err());
    let mut c = Config::new(format!("http://{p}"));
    c.rules = vec![pulse_rule("input_boolean/../x")];
    assert!(c.validate().is_err());
    c.rules = vec![pulse_rule("light.example")];
    assert!(c.validate().is_err());
    c.rules = vec![rule(Action::Notify {
        service: "a/b".into(),
        title: None,
    })];
    assert!(c.validate().is_err());
    c.rules = vec![];
    c.message_template = "{secret}".into();
    assert!(c.validate().is_err());
}

#[test]
fn https_is_accepted_for_any_host() {
    assert!(!ha_output::host_is_local("example.com"));
    // Validation only (no connection); skipped on processors without the
    // HTTPS crypto features, where construction refuses https.
    match Config::new("https://ha.example.com").validate() {
        Ok(()) | Err(ConfigError::HttpsUnsupported) => {}
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn the_worker_queue_is_bounded_and_reports_results_in_order() {
    let rig = Rig::new();
    rig.mock.set(E, "off");
    let out = Arc::new(rig.output(vec![pulse_rule(E)], |c| c.cooldown_ms = 0));
    let (started_tx, started_rx) = channel::<()>();
    let (gate_tx, gate_rx) = channel::<()>();
    let (done_tx, done_rx) = channel::<(u64, bool)>();
    let gate_rx = Mutex::new(gate_rx);
    let w = Worker::spawn(out, 1, move |a, r| {
        started_tx.send(()).unwrap();
        gate_rx.lock().unwrap().recv().unwrap();
        done_tx.send((a.alert_id, r.ok)).unwrap();
    });
    let mut a = alert(Class::Person);
    a.alert_id = 1;
    w.submit(a.clone()).unwrap();
    started_rx.recv().unwrap(); // the worker holds alert 1
    a.alert_id = 2;
    w.submit(a.clone()).unwrap(); // queued
    a.alert_id = 3;
    assert_eq!(w.submit(a), Err(SubmitError::Full));
    gate_tx.send(()).unwrap();
    gate_tx.send(()).unwrap();
    w.shutdown();
    assert_eq!(done_rx.recv().unwrap(), (1, true));
    assert_eq!(done_rx.recv().unwrap(), (2, true));
}

#[test]
fn a_test_alert_pulses_every_target_ignoring_matching_and_cooldown() {
    let rig = Rig::new();
    rig.mock.set(E, "off");
    // The rule would never match a person alert.
    let mut r = pulse_rule(E);
    r.classes = vec![Class::Vehicle];
    let out = rig.output(vec![r], |c| c.cooldown_ms = 30_000);
    assert_eq!(
        out.deliver(&alert(Class::Person)).summary,
        "Home Assistant: no mapping matched"
    );
    let first = out.deliver_test(&alert(Class::Person), None);
    assert!(first.ok, "{first:?}");
    assert_eq!(first.summary, format!("Home Assistant: {E} pulsed (200)"));
    // A second test straight away is not held back by the cooldown.
    let second = out.deliver_test(&alert(Class::Person), Some(0));
    assert_eq!(second.entities[0].status, EntityStatus::Done);
    // Out of range names no rule and is not ok.
    let none = out.deliver_test(&alert(Class::Person), Some(5));
    assert!(!none.ok && none.entities.is_empty());
}

#[test]
fn a_failed_test_alert_reports_the_http_status() {
    let rig = Rig::new();
    let out = rig.output(vec![pulse_rule(E)], |_| {});
    let r = out.deliver_test(&alert(Class::Person), None);
    assert!(!r.ok);
    assert_eq!(
        r.entities[0].attempts.last().unwrap().http_status,
        Some(404)
    );
}

fn notify_rule(name: &str) -> Rule {
    rule(Action::Notify {
        service: name.into(),
        title: Some("Alert".into()),
    })
}

#[test]
fn a_notify_entity_is_sent_with_notify_send_message() {
    let rig = Rig::new();
    rig.mock.add_notify_entity("example_phone");
    let out = rig.output(vec![notify_rule("example_phone")], |c| c.cooldown_ms = 0);
    let r = out.deliver(&alert(Class::Person));
    assert!(r.ok, "{r:?}");
    assert_eq!(
        r.summary,
        "Home Assistant: notify.example_phone sent to the notify entity (200)"
    );
    let sent = rig.mock.bodies_for("/api/services/notify/send_message");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["entity_id"], "notify.example_phone");
    assert_eq!(sent[0]["title"], "Alert");
    assert!(sent[0]["message"].as_str().unwrap().starts_with("person"));
    assert!(
        rig.mock
            .bodies_for("/api/services/notify/example_phone")
            .is_empty()
    );
}

#[test]
fn a_legacy_service_is_used_when_there_is_no_such_entity() {
    let rig = Rig::new();
    rig.mock.add_legacy("example_phone");
    let out = rig.output(vec![notify_rule("example_phone")], |c| c.cooldown_ms = 0);
    let r = out.deliver(&alert(Class::Person));
    assert!(r.ok, "{r:?}");
    assert_eq!(
        r.summary,
        "Home Assistant: notify.example_phone sent with the legacy notify service (200)"
    );
    assert_eq!(
        rig.mock
            .bodies_for("/api/services/notify/example_phone")
            .len(),
        1
    );
    assert!(
        rig.mock
            .bodies_for("/api/services/notify/send_message")
            .is_empty()
    );
}

#[test]
fn a_pasted_full_name_is_accepted_and_stored_bare() {
    let rig = Rig::new();
    rig.mock.add_notify_entity("example_phone");
    let out = rig.output(vec![notify_rule("notify.example_phone")], |_| {});
    let r = out.deliver(&alert(Class::Person));
    assert!(r.ok, "{r:?}");
    assert_eq!(r.entities[0].target, "notify.example_phone");
    let mut c = Config::new(rig.mock.url());
    c.rules = vec![notify_rule("notify.example_phone")];
    assert!(c.validate().is_ok());
    c.rules = vec![notify_rule("notify.a/b")];
    assert!(c.validate().is_err());
}

#[test]
fn the_route_is_resolved_once_and_again_after_the_cached_path_fails() {
    let rig = Rig::new();
    rig.mock.add_legacy("example_phone");
    let out = rig.output(vec![notify_rule("example_phone")], |c| c.cooldown_ms = 0);
    assert!(out.deliver(&alert(Class::Person)).ok);
    assert!(out.deliver(&alert(Class::Person)).ok);
    let resolves = |m: &Mock| {
        m.log()
            .iter()
            .filter(|l| l.as_str() == "GET /api/states/notify.example_phone")
            .count()
    };
    assert_eq!(resolves(&rig.mock), 1, "the decision is cached");
    // The integration is replaced by a notify entity of the same name: the
    // legacy path now answers 400, so the target is resolved once more.
    rig.mock.remove_notify("example_phone");
    rig.mock.add_notify_entity("example_phone");
    let r = out.deliver(&alert(Class::Person));
    assert!(r.ok, "{r:?}");
    assert!(r.summary.contains("sent to the notify entity"), "{r:?}");
    assert_eq!(resolves(&rig.mock), 2, "resolved again once");
}

#[test]
fn a_target_that_disappears_fails_with_home_assistants_words() {
    let rig = Rig::new();
    rig.mock.add_notify_entity("example_phone");
    let out = rig.output(vec![notify_rule("example_phone")], |c| c.cooldown_ms = 0);
    assert!(out.deliver(&alert(Class::Person)).ok);
    rig.mock.remove_notify("example_phone");
    let r = out.deliver(&alert(Class::Person));
    assert!(!r.ok);
    assert!(
        r.summary
            .contains("(400): Service notify.example_phone not found."),
        "{r:?}"
    );
}

#[test]
fn home_assistants_error_text_is_kept_for_a_refused_request() {
    let rig = Rig::new();
    let out = rig.output(vec![notify_rule("example_phone")], |_| {});
    let r = out.deliver(&alert(Class::Person));
    assert!(!r.ok);
    assert_eq!(
        r.summary,
        "Home Assistant: notify.example_phone failed: request refused (400): Service notify.example_phone not found."
    );
    // Another step: a pulse of an unknown entity names HA's reason too.
    let r = rig
        .output(vec![pulse_rule(E)], |_| {})
        .deliver(&alert(Class::Person));
    assert!(
        r.summary
            .contains("entity not found (404): Entity not found."),
        "{r:?}"
    );
}

#[test]
fn a_test_alert_says_which_way_it_sent() {
    let rig = Rig::new();
    rig.mock.add_notify_entity("example_phone");
    rig.mock.add_legacy("example_old");
    let out = rig.output(
        vec![notify_rule("example_phone"), notify_rule("example_old")],
        |_| {},
    );
    let r = out.deliver_test(&alert(Class::Person), None);
    assert!(r.ok, "{r:?}");
    assert!(
        r.summary
            .contains("notify.example_phone sent to the notify entity")
    );
    assert!(
        r.summary
            .contains("notify.example_old sent with the legacy notify service")
    );
}

#[test]
fn the_target_list_has_entities_then_legacy_services() {
    let rig = Rig::new();
    rig.mock.add_notify_entity("example_phone");
    rig.mock.add_legacy("example_old");
    rig.mock.set(E, "off");
    let out = rig.output(vec![], |_| {});
    let t = out.list_notify_targets().unwrap();
    let names: Vec<_> = t.iter().map(|t| (t.name.as_str(), t.kind)).collect();
    assert_eq!(
        names,
        vec![
            ("example_phone", TargetKind::Entity),
            ("example_old", TargetKind::Service)
        ]
    );
    rig.mock.state.lock().unwrap().expected_token = "other".into();
    let e = out.list_notify_targets().unwrap_err();
    assert!(e.contains("token rejected (401)"), "{e}");
}
