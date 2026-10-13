//! Device discovery through the core (docs/FFI.md, "Device discovery"):
//! `discovered` and `discovery_done` events from mock devices on loopback,
//! the stop and restart rules, and the privacy rule that no device ID or
//! MAC address reaches an event. Searches run on the core's real runtime
//! with short, bounded durations; nothing leaves the loopback interface.

use crate::common::*;
use discovery::native::{self, RecordKind, SearchRecord};
use discovery::onvif::{self, ProbeMatch};
use discovery::runner::Ports;
use discovery::{Options, ServiceKind};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use viewer_core::{CoreConfig, CoreError};

const LO: Ipv4Addr = Ipv4Addr::LOCALHOST;

fn record() -> SearchRecord {
    SearchRecord {
        kind: RecordKind::Nvr,
        channels: 8,
        unknown: [3, 1, 1],
        mac: [0x00, 0x00, 0x5e, 0x00, 0x53, 0x10],
        address: Some(LO),
        tcp_port: 9000,
        name: "NVR 1".into(),
        uid: "UID-EXAMPLE".into(),
        product_tag: String::new(),
    }
}

type Reply = Box<dyn Fn(&[u8], SocketAddr) -> Vec<(Vec<u8>, SocketAddr)> + Send + Sync>;

/// A mock device on loopback that answers after `delay`; returns its address.
fn mock_device(rt: &tokio::runtime::Runtime, delay: Duration, reply: Reply) -> SocketAddrV4 {
    rt.block_on(async {
        let socket = UdpSocket::bind((LO, 0)).await.unwrap();
        let SocketAddr::V4(addr) = socket.local_addr().unwrap() else {
            unreachable!()
        };
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            while let Ok((n, from)) = socket.recv_from(&mut buf).await {
                for (bytes, to) in reply(&buf[..n], from) {
                    tokio::time::sleep(delay).await;
                    let _ = socket.send_to(&bytes, to).await;
                }
            }
        });
        addr
    })
}

fn free_port(rt: &tokio::runtime::Runtime) -> u16 {
    rt.block_on(async {
        UdpSocket::bind((LO, 0))
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    })
}

/// A native search device and an ONVIF device, both at 127.0.0.1, so their
/// answers merge into one found device. The two exchanges are independent, so
/// which answer arrives first is not defined; `mock_options_ordered` makes it so.
fn mock_options(rt: &tokio::runtime::Runtime, millis: u64) -> Options {
    mock_options_ordered(rt, millis, Duration::ZERO, Duration::ZERO)
}

/// As `mock_options`, with each device answering after its own delay.
fn mock_options_ordered(
    rt: &tokio::runtime::Runtime,
    millis: u64,
    search_delay: Duration,
    onvif_delay: Duration,
) -> Options {
    let reply_port = free_port(rt);
    let rec = native::encode_search_record(&record());
    let search = mock_device(
        rt,
        search_delay,
        Box::new(move |d, _from| {
            if native::is_search_request(d) {
                vec![(rec.clone(), SocketAddr::from((LO, reply_port)))]
            } else {
                Vec::new()
            }
        }),
    );
    let onvif_dev = mock_device(
        rt,
        onvif_delay,
        Box::new(|d, from| {
            onvif::probe_message_id(d)
                .map(|id| {
                    let m = ProbeMatch {
                        endpoint: Some("urn:uuid:00000000-0000-4000-8000-000000000042".into()),
                        types: vec!["NetworkVideoTransmitter".into()],
                        scopes: vec!["onvif://www.onvif.org/hardware/Model-Y".into()],
                        xaddrs: vec!["http://127.0.0.1:8000/onvif/device_service".into()],
                    };
                    vec![(onvif::encode_probe_matches(&id, &m).into_bytes(), from)]
                })
                .unwrap_or_default()
        }),
    );
    Options {
        interfaces: vec![LO],
        duration: Duration::from_millis(millis),
        rounds: 2,
        native_search: true,
        native_probe: false,
        onvif: true,
        ports: Ports {
            search_target: Some(search),
            search_reply_port: reply_port,
            probe_target: None,
            onvif_target: onvif_dev,
        },
    }
}

fn dones(rec: &Recorder) -> Vec<serde_json::Value> {
    rec.events_of("discovery_done")
}

#[test]
fn answers_arrive_as_events_and_merge_by_key_search_first() {
    answers_merge(Duration::ZERO, Duration::from_millis(300));
}

#[test]
fn answers_arrive_as_events_and_merge_by_key_onvif_first() {
    answers_merge(Duration::from_millis(300), Duration::ZERO);
}

/// Runs one search whose two answers arrive in the order the delays give, and
/// checks the events: one device, whichever answer came first.
fn answers_merge(search_delay: Duration, onvif_delay: Duration) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let options = mock_options_ordered(&rt, 1_000, search_delay, onvif_delay);
    let (core, rec) = start(CoreConfig::default());
    let search = core.discover_start(options).unwrap();
    wait_until("the search to end", 10, || !dones(&rec).is_empty());
    core.flush_events();

    let started = rec.events_of("discovery_started");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["search"], search);
    let found = rec.events_of("discovered");
    // Each answer is an event for the same key; the last one is the merge.
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(|e| e["search"] == search));
    assert_eq!(found[0]["key"], found[1]["key"]);
    let merged = &found[1];
    assert_eq!(merged["kind"], "nvr");
    assert_eq!(merged["name"], "NVR 1");
    assert_eq!(merged["address"], "127.0.0.1");
    assert_eq!(merged["port"], 9000);
    assert_eq!(merged["channels"], 8);
    assert_eq!(merged["model"], "Model-Y");
    let mut kinds: Vec<_> = merged["services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap().to_owned())
        .collect();
    kinds.sort();
    assert_eq!(kinds, ["native", "onvif"]);
    assert_eq!(ServiceKind::Native.as_str(), "native");

    let done = dones(&rec);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["search"], search);
    assert_eq!(done[0]["count"], 1);
    assert_eq!(done[0]["stopped"], false);
    assert_eq!(done[0]["errors"], serde_json::json!([]));

    // IDs and MAC addresses never reach an event.
    let all = serde_json::to_string(&rec.events()).unwrap().to_lowercase();
    for secret in ["uid-example", "00:00:5e", "urn:uuid", "000042"] {
        assert!(!all.contains(secret), "{secret} in {all}");
    }
    core.shutdown();
}

#[test]
fn stop_ends_the_search_and_a_start_while_running_restarts_it() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (core, rec) = start(CoreConfig::default());
    // Stopping with nothing running is fine and sends nothing.
    core.discover_stop().unwrap();
    core.flush_events();
    assert!(dones(&rec).is_empty());

    // A long search, stopped at once.
    let first = core.discover_start(mock_options(&rt, 30_000)).unwrap();
    let t = std::time::Instant::now();
    core.discover_stop().unwrap();
    assert!(t.elapsed() < Duration::from_secs(5), "stop must not wait");
    core.flush_events();
    let done = dones(&rec);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["search"], first);
    assert_eq!(done[0]["stopped"], true);

    // A second start while the first runs ends the first and goes on.
    let a = core.discover_start(mock_options(&rt, 30_000)).unwrap();
    let b = core.discover_start(mock_options(&rt, 400)).unwrap();
    assert!(b > a && a > first);
    wait_until("the second search to end", 10, || dones(&rec).len() == 3);
    core.flush_events();
    let done = dones(&rec);
    assert_eq!(
        (done[1]["search"].as_u64(), done[1]["stopped"].as_bool()),
        (Some(a), Some(true))
    );
    assert_eq!(
        (done[2]["search"].as_u64(), done[2]["stopped"].as_bool()),
        (Some(b), Some(false))
    );
    // No answer for a search comes after its done.
    let events = rec.events();
    let end_of = |s: u64| {
        events
            .iter()
            .position(|e| e["type"] == "discovery_done" && e["search"] == s)
            .unwrap()
    };
    for (i, e) in events.iter().enumerate() {
        if e["type"] == "discovered" {
            let s = e["search"].as_u64().unwrap();
            assert!(i < end_of(s), "answer after done: {events:?}");
        }
    }
    core.shutdown();
}

#[test]
fn shutdown_ends_a_running_search_with_its_done() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (core, rec) = start(CoreConfig::default());
    let search = core.discover_start(mock_options(&rt, 30_000)).unwrap();
    core.shutdown();
    let done = dones(&rec);
    assert_eq!(done.len(), 1, "{:?}", rec.events());
    assert_eq!(done[0]["search"], search);
    assert_eq!(done[0]["stopped"], true);
    assert_eq!(
        core.discover_start(Options::default()),
        Err(CoreError::Closed)
    );
    assert_eq!(core.discover_stop(), Err(CoreError::Closed));
}

#[test]
fn a_search_with_nothing_to_send_ends_with_no_devices() {
    let (core, rec) = start(CoreConfig::default());
    let options = Options {
        interfaces: vec![LO],
        duration: Duration::from_millis(300),
        native_search: false,
        native_probe: false,
        onvif: false,
        ..Options::default()
    };
    core.discover_start(options).unwrap();
    wait_until("the search to end", 10, || !dones(&rec).is_empty());
    let done = dones(&rec);
    assert_eq!(done[0]["count"], 0);
    assert_eq!(done[0]["stopped"], false);
    assert!(rec.events_of("discovered").is_empty());
    core.shutdown();
}

#[test]
fn a_search_that_cannot_open_a_socket_reports_it() {
    let rt = Arc::new(tokio::runtime::Runtime::new().unwrap());
    // Hold the native reply port on loopback's own address so that the one
    // backend cannot open.
    let held = std::net::UdpSocket::bind((LO, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    let (core, rec) = start(CoreConfig::default());
    let options = Options {
        interfaces: vec![LO],
        duration: Duration::from_millis(300),
        native_search: true,
        native_probe: false,
        onvif: false,
        ports: Ports {
            search_reply_port: port,
            search_target: Some(SocketAddrV4::new(LO, free_port(&rt))),
            ..Ports::default()
        },
        ..Options::default()
    };
    core.discover_start(options).unwrap();
    wait_until("the search to end", 10, || !dones(&rec).is_empty());
    let done = dones(&rec);
    assert_eq!(done[0]["count"], 0);
    let errors = done[0]["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{done:?}");
    assert!(errors[0].as_str().unwrap().contains("native search"));
    core.shutdown();
    drop(held);
}
