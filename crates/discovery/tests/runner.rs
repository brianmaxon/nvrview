//! The runner against mock devices on loopback. Every value is synthetic.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use discovery::native::{self, RecordKind, SearchRecord};
use discovery::onvif::{self, ProbeMatch};
use discovery::runner::Ports;
use discovery::{DeviceKind, Options, ServiceKind, discover, discover_with};
use tokio::net::UdpSocket;

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

/// A port nothing listens on right now.
async fn free_port() -> u16 {
    let s = UdpSocket::bind((LO, 0)).await.unwrap();
    s.local_addr().unwrap().port()
}

/// What a mock device does with each datagram it receives: the replies to
/// send, each with its destination.
type Behavior = Box<dyn Fn(&[u8], SocketAddr) -> Vec<(Vec<u8>, SocketAddr)> + Send + Sync>;

/// Starts a mock device on loopback; returns its address and a counter of
/// datagrams it received.
async fn mock(behavior: Behavior) -> (SocketAddrV4, Arc<AtomicUsize>) {
    let socket = UdpSocket::bind((LO, 0)).await.unwrap();
    let SocketAddr::V4(addr) = socket.local_addr().unwrap() else {
        unreachable!()
    };
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65536];
        while let Ok((n, from)) = socket.recv_from(&mut buf).await {
            seen.fetch_add(1, Ordering::SeqCst);
            for (reply, to) in behavior(&buf[..n], from) {
                let _ = socket.send_to(&reply, to).await;
            }
        }
    });
    (addr, count)
}

/// A mock native device answering the search request with `records` sent
/// to `reply_port`, as devices do (not to the request's source port).
async fn search_device(reply_port: u16, records: Vec<Vec<u8>>) -> (SocketAddrV4, Arc<AtomicUsize>) {
    mock(Box::new(move |d, _from| {
        if native::is_search_request(d) {
            let to = SocketAddr::from((LO, reply_port));
            records.iter().map(|r| (r.clone(), to)).collect()
        } else {
            Vec::new()
        }
    }))
    .await
}

fn options(duration_ms: u64) -> Options {
    Options {
        interfaces: vec![LO],
        duration: Duration::from_millis(duration_ms),
        rounds: 2,
        native_search: false,
        native_probe: false,
        onvif: false,
        ports: Ports::default(),
    }
}

#[tokio::test]
async fn native_search_finds_and_dedupes() {
    let reply_port = free_port().await;
    let rec = native::encode_search_record(&record());
    let (dev, received) = search_device(reply_port, vec![rec.clone(), rec]).await;
    let mut opts = options(400);
    opts.native_search = true;
    opts.ports.search_reply_port = reply_port;
    opts.ports.search_target = Some(dev);

    let mut updates = 0;
    let report = discover_with(&opts, |_| updates += 1).await.unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    // Two rounds of two requests, each answered twice: one device, one update.
    assert_eq!(received.load(Ordering::SeqCst), 4);
    assert_eq!(updates, 1);
    assert_eq!(report.devices.len(), 1);
    let d = &report.devices[0];
    assert_eq!(d.kind, DeviceKind::Nvr);
    assert_eq!(d.name.as_deref(), Some("NVR 1"));
    assert_eq!(d.channels, Some(8));
    assert_eq!(d.native_port(), Some(9000));
    assert_eq!(d.mac.as_deref(), Some("00:00:5e:00:53:10"));
    assert_eq!(d.device_id.as_deref(), Some("UID-EXAMPLE"));
    assert_eq!(d.address, std::net::IpAddr::V4(LO));
}

#[tokio::test]
async fn malformed_datagrams_are_ignored_and_the_search_ends_on_time() {
    let reply_port = free_port().await;
    let mut truncated = native::encode_search_record(&record());
    truncated.truncate(200);
    let mut no_marker = native::encode_search_record(&record());
    no_marker[104] = 0;
    let junk = vec![
        truncated,
        no_marker,
        native::SEARCH_REQUEST.to_vec(),
        vec![0xff; 1000],
        Vec::new(),
    ];
    let (dev, _) = search_device(reply_port, junk).await;
    let mut opts = options(300);
    opts.native_search = true;
    opts.ports.search_reply_port = reply_port;
    opts.ports.search_target = Some(dev);

    let started = Instant::now();
    let report = discover(&opts).await.unwrap();
    let took = started.elapsed();
    assert!(report.devices.is_empty());
    assert!(report.ignored >= 5, "ignored {}", report.ignored);
    assert!(took >= Duration::from_millis(290), "{took:?}");
    assert!(took < Duration::from_secs(2), "{took:?}");
}

#[tokio::test]
async fn silence_times_out_empty() {
    let (dev, received) = mock(Box::new(|_, _| Vec::new())).await;
    let mut opts = options(200);
    opts.native_search = true;
    opts.native_probe = true;
    opts.onvif = true;
    opts.ports.search_reply_port = free_port().await;
    opts.ports.search_target = Some(dev);
    opts.ports.probe_target = Some(dev);
    opts.ports.onvif_target = dev;
    let report = discover(&opts).await.unwrap();
    assert!(report.devices.is_empty());
    assert_eq!(report.ignored, 0);
    // Per round: two search requests, one probe, two ONVIF probes.
    assert_eq!(received.load(Ordering::SeqCst), 10);
}

#[tokio::test]
async fn probe_replies_must_echo_our_transaction() {
    let (dev, _) = mock(Box::new(|d, from| {
        let Some((tid, port)) = native::probe_reply_port(d) else {
            return Vec::new();
        };
        let to = SocketAddr::from((from.ip(), port));
        vec![
            (native::encode_probe_reply(tid, "UID-EXAMPLE", 64), to),
            // An answer to someone else's probe.
            (
                native::encode_probe_reply(tid.wrapping_add(1000), "UID-OTHER", 10),
                to,
            ),
        ]
    }))
    .await;
    let mut opts = options(300);
    opts.native_probe = true;
    opts.ports.probe_target = Some(dev);
    let report = discover(&opts).await.unwrap();
    assert_eq!(report.devices.len(), 1);
    let d = &report.devices[0];
    assert_eq!(d.battery_percent, Some(64));
    assert_eq!(d.device_id.as_deref(), Some("UID-EXAMPLE"));
    assert!(d.service(ServiceKind::Native).is_some());
    assert_eq!(d.native_port(), None);
    assert!(report.ignored >= 2);
}

fn probe_match() -> ProbeMatch {
    ProbeMatch {
        endpoint: Some("urn:uuid:00000000-0000-4000-8000-000000000042".into()),
        types: vec!["NetworkVideoTransmitter".into()],
        scopes: vec![
            "onvif://www.onvif.org/name/Camera%202".into(),
            "onvif://www.onvif.org/hardware/Model-Y".into(),
        ],
        xaddrs: vec!["http://127.0.0.1:8000/onvif/device_service".into()],
    }
}

#[tokio::test]
async fn onvif_matches_must_relate_to_our_probe() {
    let (dev, _) = mock(Box::new(|d, from| {
        let Some(id) = onvif::probe_message_id(d) else {
            return Vec::new();
        };
        vec![
            (
                onvif::encode_probe_matches(&id, &probe_match()).into_bytes(),
                from,
            ),
            (
                onvif::encode_probe_matches("uuid:someone-else", &probe_match()).into_bytes(),
                from,
            ),
        ]
    }))
    .await;
    let mut opts = options(300);
    opts.onvif = true;
    opts.ports.onvif_target = dev;
    let report = discover(&opts).await.unwrap();
    assert_eq!(report.devices.len(), 1);
    let d = &report.devices[0];
    assert_eq!(d.name.as_deref(), Some("Camera 2"));
    assert_eq!(d.model.as_deref(), Some("Model-Y"));
    let s = d.service(ServiceKind::Onvif).unwrap();
    assert_eq!(s.port, Some(8000));
    assert_eq!(
        s.url.as_deref(),
        Some("http://127.0.0.1:8000/onvif/device_service")
    );
    // Two rounds of two probes, each also drawing a foreign answer.
    assert_eq!(report.ignored, 4);
}

#[tokio::test]
async fn answers_from_every_backend_merge_into_one_device() {
    let reply_port = free_port().await;
    let (search, _) =
        search_device(reply_port, vec![native::encode_search_record(&record())]).await;
    let (probe, _) = mock(Box::new(|d, from| {
        native::probe_reply_port(d)
            .map(|(tid, port)| {
                vec![(
                    native::encode_probe_reply(tid, "UID-EXAMPLE", 90),
                    SocketAddr::from((from.ip(), port)),
                )]
            })
            .unwrap_or_default()
    }))
    .await;
    let (onvif_dev, _) = mock(Box::new(|d, from| {
        onvif::probe_message_id(d)
            .map(|id| {
                let mut m = probe_match();
                m.scopes.retain(|s| s.contains("hardware"));
                vec![(onvif::encode_probe_matches(&id, &m).into_bytes(), from)]
            })
            .unwrap_or_default()
    }))
    .await;
    let opts = Options {
        interfaces: vec![LO],
        duration: Duration::from_millis(400),
        rounds: 2,
        ports: Ports {
            search_target: Some(search),
            search_reply_port: reply_port,
            probe_target: Some(probe),
            onvif_target: onvif_dev,
        },
        ..Options::default()
    };
    let report = discover(&opts).await.unwrap();
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.devices.len(), 1, "{:?}", report.devices);
    let d = &report.devices[0];
    assert_eq!(d.kind, DeviceKind::Nvr);
    assert_eq!(d.name.as_deref(), Some("NVR 1"));
    assert_eq!(d.model.as_deref(), Some("Model-Y"));
    assert_eq!(d.battery_percent, Some(90));
    assert_eq!(d.native_port(), Some(9000));
    assert!(d.service(ServiceKind::Onvif).is_some());
    // The search record's UID came first and is kept.
    assert_eq!(d.device_id.as_deref(), Some("UID-EXAMPLE"));
}

#[tokio::test]
async fn a_busy_reply_port_is_reported_and_other_backends_go_on() {
    // Hold the reply port on the same address without SO_REUSEADDR.
    let holder = std::net::UdpSocket::bind((LO, 0)).unwrap();
    let port = holder.local_addr().unwrap().port();
    let (dev, _) = mock(Box::new(|d, from| {
        onvif::probe_message_id(d)
            .map(|id| {
                vec![(
                    onvif::encode_probe_matches(&id, &probe_match()).into_bytes(),
                    from,
                )]
            })
            .unwrap_or_default()
    }))
    .await;
    let mut opts = options(200);
    opts.native_search = true;
    opts.onvif = true;
    opts.ports.search_reply_port = port;
    opts.ports.search_target = Some(dev);
    opts.ports.onvif_target = dev;
    let report = discover(&opts).await.unwrap();
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(report.errors[0].contains("native search"));
    assert_eq!(report.devices.len(), 1);
    drop(holder);
}

#[tokio::test]
async fn no_backend_means_no_search() {
    let report = discover(&options(5_000)).await.unwrap();
    assert!(report.devices.is_empty() && report.errors.is_empty());
}
