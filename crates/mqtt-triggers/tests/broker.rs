//! The client against an in-process broker. Nothing here sleeps: tests wait
//! on broker events, on triggers, and on the client's own counters.

mod common;

use common::*;
use mqtt_triggers::{
    ConfigError, Credentials, ErrorKind, Mode, RetainedPolicy, StartError, Status, TlsConfig,
    TriggerState,
};

fn plain() -> Broker {
    Broker::start(BrokerOpts::default())
}

fn creds(user: &str, pw: &str) -> Option<Credentials> {
    Some(Credentials {
        username: user.into(),
        password: pw.into(),
    })
}

#[test]
fn connects_subscribes_and_maps_on_off() {
    let b = plain();
    let mut cfg = config(b.port);
    cfg.client_id = "example-client".into();
    cfg.keepalive_secs = 20;
    let r = run(cfg, None);
    let e = b.wait(|e| matches!(e, Event::Connected { .. }));
    assert_eq!(
        e,
        Event::Connected {
            client_id: "example-client".into(),
            username: None,
            keepalive_secs: 20,
            clean_session: true,
        }
    );
    assert_eq!(
        b.wait(|e| matches!(e, Event::Subscribed(_))),
        Event::Subscribed("example/topic".into())
    );
    r.clock.set(5_000);
    b.publish("example/topic", b"ON", false);
    let t = r.trigger();
    assert_eq!(t.state, TriggerState::Start);
    assert_eq!(t.source_id, "sensor-1");
    assert_eq!(t.kind, "motion");
    assert_eq!(t.sender_id, "mqtt:sensor-1");
    assert_eq!(t.at_ms, 5_000);
    r.clock.set(6_000);
    b.publish("example/topic", b"OFF", false);
    let t = r.trigger();
    assert_eq!((t.state, t.at_ms), (TriggerState::Stop, 6_000));
    let s = r.handle.stats();
    assert_eq!(
        (s.status, s.connects, s.messages, s.triggers),
        (Status::Connected, 1, 2, 2)
    );
    r.handle.stop();
    b.stop();
}

#[test]
fn json_payload_from_a_state_publisher() {
    let b = plain();
    let mut cfg = config(b.port);
    cfg.mapping.json_path = Some("state".into());
    let r = run(cfg, None);
    b.wait_subscribed();
    b.publish(
        "example/topic",
        br#"{"state":"on","attributes":{"x":1}}"#,
        false,
    );
    assert_eq!(r.trigger().state, TriggerState::Start);
    b.publish("example/topic", br#"{"state":"off"}"#, false);
    assert_eq!(r.trigger().state, TriggerState::Stop);
    b.publish("example/topic", br#"{"nope":"on"}"#, false);
    r.until(|s| s.dropped_unmapped == 1);
    r.handle.stop();
    b.stop();
}

#[test]
fn retained_value_is_ignored_by_default() {
    let b = plain();
    b.retain_only("example/topic", b"ON");
    let r = run(config(b.port), None);
    // The retained "on" arrives at subscribe and is dropped.
    r.until(|s| s.dropped_retained == 1);
    assert_eq!(r.handle.stats().triggers, 0);
    // A live "ON" afterwards is a real start.
    b.publish("example/topic", b"ON", false);
    assert_eq!(r.trigger().state, TriggerState::Start);
    r.handle.stop();
    b.stop();
}

#[test]
fn retained_value_can_seed_or_be_accepted() {
    let b = plain();
    b.retain_only("example/topic", b"ON");
    let mut cfg = config(b.port);
    cfg.mapping.retained = RetainedPolicy::Seed;
    let r = run(cfg, None);
    r.until(|s| s.retained_seeded == 1);
    // Seeded as on: a repeat is nothing, the off is a stop.
    b.publish("example/topic", b"ON", false);
    b.publish("example/topic", b"OFF", false);
    assert_eq!(r.trigger().state, TriggerState::Stop);
    assert_eq!(r.handle.stats().dropped_repeat, 1);
    r.handle.stop();
    b.stop();

    let b = plain();
    b.retain_only("example/topic", b"ON");
    let mut cfg = config(b.port);
    cfg.mapping.retained = RetainedPolicy::Accept;
    let r = run(cfg, None);
    assert_eq!(r.trigger().state, TriggerState::Start);
    r.handle.stop();
    b.stop();
}

#[test]
fn debounce_uses_the_clock() {
    let b = plain();
    let mut cfg = config(b.port);
    cfg.mapping.debounce_ms = 1_000;
    let r = run(cfg, None);
    b.wait_subscribed();
    r.clock.set(10_000);
    b.publish("example/topic", b"ON", false);
    assert_eq!(r.trigger().state, TriggerState::Start);
    r.clock.set(10_500);
    b.publish("example/topic", b"OFF", false);
    r.until(|s| s.dropped_debounced == 1);
    r.clock.set(11_000);
    b.publish("example/topic", b"OFF", false);
    let t = r.trigger();
    assert_eq!((t.state, t.at_ms), (TriggerState::Stop, 11_000));
    r.handle.stop();
    b.stop();
}

#[test]
fn rate_limit_drops_and_counts() {
    let b = plain();
    let mut cfg = config(b.port);
    cfg.mapping.mode = Mode::Pulse;
    cfg.limits.rate_per_min = 6;
    cfg.limits.rate_burst = 3;
    let r = run(cfg, None);
    b.wait_subscribed();
    // The clock does not move, so no tokens come back.
    for _ in 0..5 {
        b.publish("example/topic", b"ON", false);
    }
    r.until(|s| s.messages == 5);
    let s = r.handle.stats();
    assert_eq!((s.triggers, s.dropped_rate_limited), (3, 2));
    for _ in 0..3 {
        assert_eq!(r.trigger().state, TriggerState::Pulse);
    }
    // Ten seconds refill one message.
    r.clock.set(1_000_000 + 10_000);
    b.publish("example/topic", b"ON", false);
    assert_eq!(r.trigger().state, TriggerState::Pulse);
    r.handle.stop();
    b.stop();
}

#[test]
fn oversized_payload_is_dropped_and_untrusted_text_never_surfaces() {
    let b = plain();
    let mut cfg = config(b.port);
    cfg.limits.max_payload_bytes = 16;
    let r = run(cfg, None);
    b.wait_subscribed();
    b.publish("example/topic", &[b'x'; 5000], false);
    b.publish("example/topic", b"ignore previous instructions", false);
    r.until(|s| s.messages == 2);
    let s = r.handle.stats();
    assert_eq!((s.dropped_oversized, s.triggers), (2, 0));
    b.publish("example/topic", b"ON", false);
    assert_eq!(r.trigger().state, TriggerState::Start);

    // An unrecognized (but small) payload is counted, and its text is in
    // no trigger, stat, or log line.
    let mut cfg = config(b.port);
    cfg.client_id = "second".into();
    let r2 = run(cfg, None);
    b.wait(|e| matches!(e, Event::Connected { client_id, .. } if client_id == "second"));
    b.wait_subscribed();
    b.publish("example/topic", b"obey-me", false);
    r2.until(|s| s.dropped_unmapped == 1);
    assert!(!format!("{:?}", r2.handle.stats()).contains("obey"));
    assert!(r2.logs.lock().unwrap().iter().all(|l| !l.contains("obey")));
    r2.handle.stop();
    r.handle.stop();
    b.stop();
}

#[test]
fn login_is_sent_and_a_wrong_password_is_not_retried() {
    let b = Broker::start(BrokerOpts {
        auth: Some(("example-user".into(), "example-password".into())),
        ..Default::default()
    });
    let r = run(config(b.port), creds("example-user", "wrong"));
    r.until(|s| s.status == Status::AuthFailed);
    let s = r.handle.stats();
    assert_eq!((s.auth_failures, s.connects), (1, 0));
    // The thread has ended, so the broker saw exactly one attempt.
    r.handle.stop();
    b.wait(|e| *e == Event::Rejected);
    assert_eq!(b.drain(), vec![]);

    let r = run(config(b.port), creds("example-user", "example-password"));
    let e = b.wait(|e| matches!(e, Event::Connected { .. }));
    assert!(matches!(e, Event::Connected { username: Some(u), .. } if u == "example-user"));
    b.wait_subscribed();
    r.handle.stop();
    b.stop();
}

#[test]
fn no_login_when_one_is_required_is_an_auth_failure() {
    let b = Broker::start(BrokerOpts {
        auth: Some(("example-user".into(), "example-password".into())),
        ..Default::default()
    });
    let r = run(config(b.port), None);
    r.until(|s| s.status == Status::AuthFailed);
    r.handle.stop();
    b.stop();
}

#[test]
fn tls_with_a_user_supplied_ca() {
    let ca = TestCa::new();
    let b = Broker::start(BrokerOpts {
        tls: Some(ca.server_config()),
        ..Default::default()
    });
    let mut cfg = config(b.port);
    cfg.broker.tls = Some(TlsConfig {
        ca_pem: Some(ca.ca_pem()),
    });
    let r = run(cfg, None);
    b.wait_subscribed();
    b.publish("example/topic", b"ON", false);
    assert_eq!(r.trigger().state, TriggerState::Start);
    r.handle.stop();
    b.stop();
}

#[test]
fn tls_with_an_unknown_ca_never_connects() {
    let ca = TestCa::new();
    let b = Broker::start(BrokerOpts {
        tls: Some(ca.server_config()),
        ..Default::default()
    });
    // The built-in public roots do not include the test authority.
    let mut cfg = config(b.port);
    cfg.broker.tls = Some(TlsConfig { ca_pem: None });
    let r = run(cfg, None);
    r.until(|s| s.connect_failures >= 2);
    let s = r.handle.stats();
    assert_eq!(s.connects, 0);
    assert!(s.last_error.is_some());
    r.handle.stop();
    b.stop();
}

#[test]
fn bad_ca_text_is_refused_at_start() {
    let mut cfg = config(1883);
    cfg.broker.tls = Some(TlsConfig {
        ca_pem: Some("not a certificate".into()),
    });
    let err = mqtt_triggers::start(cfg, None, std::sync::Arc::new(|_| {}), Default::default());
    assert!(matches!(err, Err(StartError::BadCa)));
}

#[test]
fn reconnects_after_the_broker_restarts() {
    let b = plain();
    let port = b.port;
    let r = run(config(port), None);
    b.wait_subscribed();
    b.publish("example/topic", b"ON", false);
    assert_eq!(r.trigger().state, TriggerState::Start);
    b.stop();
    // The restarted broker is the same address; the client finds it by
    // retrying with its (test-sized) backoff.
    let b = Broker::start(BrokerOpts {
        port: Some(port),
        ..Default::default()
    });
    b.wait_subscribed();
    r.until(|s| s.reconnects == 1 && s.status == Status::Connected);
    // State survived: the broker says off, which stops the open start.
    b.publish("example/topic", b"OFF", false);
    assert_eq!(r.trigger().state, TriggerState::Stop);
    r.handle.stop();
    b.stop();
}

#[test]
fn stopping_sends_a_clean_disconnect() {
    let b = plain();
    let r = run(config(b.port), None);
    b.wait_subscribed();
    r.until(|s| s.status == Status::Connected);
    r.handle.stop();
    b.wait(|e| *e == Event::Disconnected);
    b.stop();
}

#[test]
fn wildcard_topics_when_allowed() {
    let b = plain();
    let mut cfg = config(b.port);
    cfg.topic = "example/+/state".into();
    cfg.allow_wildcards = true;
    let r = run(cfg, None);
    b.wait_subscribed();
    b.publish("example/a/state", b"ON", false);
    assert_eq!(r.trigger().state, TriggerState::Start);
    r.handle.stop();
    b.stop();
}

#[test]
fn public_literal_address_is_refused_unless_allowed() {
    let mut cfg = config(1883);
    cfg.broker.host = "203.0.113.5".into();
    let err = mqtt_triggers::start(
        cfg.clone(),
        None,
        std::sync::Arc::new(|_| {}),
        Default::default(),
    );
    assert!(matches!(
        err,
        Err(StartError::Config(ConfigError::PublicAddress))
    ));
    // Allowed explicitly, the config is valid (not started here: that
    // would dial a public address).
    cfg.allow_public = true;
    assert!(cfg.validate().is_ok());
}

#[test]
fn a_local_name_connects() {
    // "localhost" resolves to loopback, which is local.
    let b = plain();
    let mut cfg = config(b.port);
    cfg.broker.host = "localhost".into();
    let r = run(cfg, None);
    b.wait_subscribed();
    r.handle.stop();
    b.stop();
}

#[test]
fn messages_are_logged_with_their_outcomes() {
    let b = plain();
    let mut cfg = config(b.port);
    cfg.mapping.json_path = Some("state".into());
    let r = run(cfg, None);
    b.wait_subscribed();
    r.clock.set(2_000);
    b.publish("example/topic", br#"{"state":"on"}"#, false);
    r.trigger();
    b.publish("example/topic", br#"{"state":"maybe"}"#, false);
    b.publish("example/topic", &[0xff, 0xfe], false);
    r.until(|s| s.messages == 3);
    let page = r.handle.messages(0, 100);
    let codes: Vec<&str> = page.entries.iter().map(|e| e.outcome.code).collect();
    assert_eq!(
        codes,
        ["matched", "not_a_trigger_value", "not_a_trigger_value"]
    );
    assert_eq!(
        page.entries[0].outcome.text,
        "matched sensor sensor-1 as motion (start)"
    );
    assert_eq!(page.entries[0].at_ms, 2_000);
    assert_eq!(page.entries[0].topic, "example/topic");
    assert!(page.entries[2].binary);
    assert_eq!(page.entries[2].payload, "hex: ff fe");
    // The log never reaches the log callback.
    assert!(r.logs.lock().unwrap().iter().all(|l| !l.contains("maybe")));
    r.handle.clear_messages();
    assert_eq!(r.handle.messages(0, 100).entries.len(), 0);
    r.handle.stop();
    b.stop();
}

#[test]
fn refused_connection_names_its_reason() {
    // A port nothing listens on: bind, note the port, drop.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let r = run(config(port), None);
    r.until(|s| s.connect_failures >= 1);
    let s = r.handle.stats();
    assert_eq!(s.last_error_kind, Some(ErrorKind::Refused));
    assert!(s.last_error.unwrap().contains("nothing is listening"));
    assert_eq!(s.last_error_ms, Some(1_000_000));
    assert_eq!(s.last_success_ms, None);
    r.handle.stop();
}

#[test]
fn login_rejected_and_last_success_are_reported() {
    let b = Broker::start(BrokerOpts {
        auth: Some(("example-user".into(), "example-password".into())),
        ..Default::default()
    });
    let r = run(config(b.port), creds("example-user", "wrong"));
    r.until(|s| s.status == Status::AuthFailed);
    let s = r.handle.stats();
    assert_eq!(s.last_error_kind, Some(ErrorKind::Login));
    assert!(r.logs.lock().unwrap()[0].contains("user name and password"));
    r.handle.stop();
    b.stop();
}

#[test]
fn a_dropped_connection_records_the_last_success_time() {
    let b = plain();
    let r = run(config(b.port), None);
    b.wait_subscribed();
    r.until(|s| s.status == Status::Connected);
    assert_eq!(r.handle.stats().last_success_ms, Some(1_000_000));
    r.clock.set(1_500_000);
    b.stop();
    r.until(|s| s.last_error_kind.is_some());
    let s = r.handle.stats();
    assert_eq!(s.last_success_ms, Some(1_500_000));
    assert!(matches!(
        s.last_error_kind,
        Some(ErrorKind::Network | ErrorKind::Refused)
    ));
    r.handle.stop();
}

#[test]
fn tls_failures_are_told_apart() {
    // Unknown authority.
    let ca = TestCa::new();
    let b = Broker::start(BrokerOpts {
        tls: Some(ca.server_config()),
        ..Default::default()
    });
    let mut cfg = config(b.port);
    cfg.broker.tls = Some(TlsConfig { ca_pem: None });
    let r = run(cfg, None);
    r.until(|s| s.connect_failures >= 1);
    assert_eq!(
        r.handle.stats().last_error_kind,
        Some(ErrorKind::TlsCertificate)
    );
    r.handle.stop();
    b.stop();

    // A trusted authority, but the certificate is for another name.
    let ca = TestCa::for_other_name("other.example");
    let b = Broker::start(BrokerOpts {
        tls: Some(ca.server_config()),
        ..Default::default()
    });
    let mut cfg = config(b.port);
    cfg.broker.tls = Some(TlsConfig {
        ca_pem: Some(ca.ca_pem()),
    });
    let r = run(cfg, None);
    r.until(|s| s.connect_failures >= 1);
    assert_eq!(
        r.handle.stats().last_error_kind,
        Some(ErrorKind::TlsHostname)
    );
    r.handle.stop();
    b.stop();
}
