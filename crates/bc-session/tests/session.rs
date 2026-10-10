//! End-to-end tests against an in-process fake device on loopback. The fake
//! speaks the protocol using bc-protocol's encoders and synthetic credentials;
//! no real device is involved.

use bc_protocol::crypto::{SessionKey, credential_hash, xor_obfuscate};
use bc_protocol::framer::{Decoder, RawMessage, encode_message_with_status};
use bc_protocol::header::Class;
use bc_protocol::media::{Codec, Record, encode_info, encode_video};
use bc_protocol::stream::StreamTier;
use bc_protocol::xml::find_text;
use bc_session::{BackoffConfig, ConnState, DeviceSession, Item, SessionConfig, TcpConnector};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const USER: &str = "example-user";
const PASSWORD: &str = "example-password";
const NONCE: &str = "example-nonce-0001";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    RejectLogin,
    /// Close right after a successful login.
    DropAfterLogin,
    /// On the first connection only: answer the first preview start, then close.
    DropFirstAfterStart,
}

#[derive(Clone, Debug)]
struct Seen {
    conn: usize,
    command: u32,
    text: String,
}

struct Fake {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
    accepted: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    accept_times: Arc<Mutex<Vec<Instant>>>,
}

impl Fake {
    async fn start(mode: Mode) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let f = Fake {
            addr: listener.local_addr().unwrap(),
            seen: Arc::default(),
            accepted: Arc::default(),
            active: Arc::default(),
            max_active: Arc::default(),
            accept_times: Arc::default(),
        };
        let (seen, accepted, active, max_active, times) = (
            f.seen.clone(),
            f.accepted.clone(),
            f.active.clone(),
            f.max_active.clone(),
            f.accept_times.clone(),
        );
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let conn = accepted.fetch_add(1, Ordering::SeqCst);
                times.lock().unwrap().push(Instant::now());
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now, Ordering::SeqCst);
                let (seen, active) = (seen.clone(), active.clone());
                tokio::spawn(async move {
                    serve(sock, conn, mode, seen).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        f
    }

    fn count(&self, command: u32) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.command == command)
            .count()
    }

    fn stops_for(&self, channel: u8) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|s| {
                s.command == 4
                    && find_text(&s.text, "channelId").unwrap().as_deref()
                        == Some(&channel.to_string())
            })
            .count()
    }
}

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn reply(command: u32, id: u32, status: u16, class: Class, body: &[u8]) -> Vec<u8> {
    encode_message_with_status(command, id, status, class, &[], body).unwrap()
}

async fn serve(mut sock: TcpStream, conn: usize, mode: Mode, seen: Arc<Mutex<Vec<Seen>>>) {
    let key = SessionKey::derive(NONCE, PASSWORD);
    let mut dec = Decoder::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = match sock.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        dec.push(&buf[..n]);
        while let Some(m) = dec.next_message().unwrap() {
            let out = handle(&m, &key, conn, mode, &seen);
            for bytes in out.bytes {
                if sock.write_all(&bytes).await.is_err() {
                    return;
                }
            }
            if out.close {
                return;
            }
        }
    }
}

struct Out {
    bytes: Vec<Vec<u8>>,
    close: bool,
}

fn handle(
    m: &RawMessage,
    key: &SessionKey,
    conn: usize,
    mode: Mode,
    seen: &Mutex<Vec<Seen>>,
) -> Out {
    let h = &m.header;
    let mut out = Out {
        bytes: vec![],
        close: false,
    };
    if h.command == 1 && h.class == Class::ClientHandshake {
        let mut body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<body>\n<Encryption version=\"1.1\">\n\
             <type>md5</type>\n<nonce>{NONCE}</nonce>\n</Encryption>\n</body>\n"
        )
        .into_bytes();
        xor_obfuscate(h.message_id, &mut body);
        out.bytes
            .push(reply(1, h.message_id, 200, Class::DeviceHandshake, &body));
        return out;
    }
    if h.command == 1 {
        let mut body = m.body.clone();
        xor_obfuscate(h.message_id, &mut body);
        let text = String::from_utf8(body).unwrap();
        let ok = mode != Mode::RejectLogin
            && find_text(&text, "userName").unwrap().as_deref()
                == Some(credential_hash(USER, NONCE).as_str())
            && find_text(&text, "password").unwrap().as_deref()
                == Some(credential_hash(PASSWORD, NONCE).as_str());
        if ok {
            out.bytes
                .push(reply(1, h.message_id, 200, Class::Device, &[]));
            out.close = mode == Mode::DropAfterLogin;
        } else {
            let mut e =
                b"<body><LoginErrInfo><remainTimes>2</remainTimes></LoginErrInfo></body>".to_vec();
            xor_obfuscate(h.message_id, &mut e);
            out.bytes
                .push(reply(1, h.message_id, 401, Class::Device, &e));
        }
        return out;
    }
    let mut body = m.body.clone();
    if !body.is_empty() {
        key.decrypt(&mut body);
    }
    let text = String::from_utf8(body).unwrap_or_default();
    seen.lock().unwrap().push(Seen {
        conn,
        command: h.command,
        text: text.clone(),
    });
    match h.command {
        93 => {
            let mut b = b"<body><LinkType><type>LAN</type></LinkType></body>".to_vec();
            key.encrypt(&mut b);
            out.bytes
                .push(reply(93, h.message_id, 200, Class::Device, &b));
        }
        3 => {
            let channel: u64 = find_text(&text, "channelId")
                .unwrap()
                .unwrap()
                .parse()
                .unwrap();
            out.bytes
                .push(reply(3, h.message_id, 200, Class::Device, &[]));
            let mut stream = encode_info(1920, 1080, 15);
            stream.extend(encode_video(
                0,
                true,
                Codec::H264,
                channel * 1000,
                &[],
                &[0, 0, 0, 1, 0x65, 1],
            ));
            stream.extend(encode_video(
                0,
                false,
                Codec::H264,
                channel * 1000 + 1,
                &[],
                &[0, 0, 0, 1, 0x41, 2],
            ));
            let mut ext = b"<Extension version=\"1.1\"><binaryData>1</binaryData><encryptLen>1024</encryptLen></Extension>".to_vec();
            key.encrypt(&mut ext);
            key.encrypt(&mut stream);
            out.bytes.push(
                encode_message_with_status(3, h.message_id, 200, Class::Device, &ext, &stream)
                    .unwrap(),
            );
            out.close = mode == Mode::DropFirstAfterStart && conn == 0;
        }
        4 => out
            .bytes
            .push(reply(4, h.message_id, 200, Class::Device, &[])),
        _ => {}
    }
    out
}

fn config() -> SessionConfig {
    let mut c = SessionConfig::new(USER, PASSWORD);
    c.backoff = BackoffConfig {
        initial: Duration::from_millis(30),
        max: Duration::from_millis(120),
        jitter: 0.0,
        stable_after: Duration::from_secs(60),
    };
    c
}

fn video_pts(item: &Item) -> Option<(bool, u64)> {
    match item {
        Item::Media(Record::Video {
            keyframe, pts_us, ..
        }) => Some((*keyframe, *pts_us)),
        _ => None,
    }
}

async fn recv(sub: &mut bc_session::Subscription) -> Item {
    tokio::time::timeout(Duration::from_secs(5), sub.recv())
        .await
        .expect("timed out waiting for an item")
        .expect("subscription ended")
}

#[tokio::test]
async fn login_succeeds_and_channels_are_demultiplexed() {
    let fake = Fake::start(Mode::Normal).await;
    let s = DeviceSession::spawn(TcpConnector::new(fake.addr), config());
    let mut a = s.subscribe(2, StreamTier::Sub).await.unwrap();
    let mut b = s.subscribe(5, StreamTier::Main).await.unwrap();

    for (sub, base) in [(&mut a, 2000u64), (&mut b, 5000)] {
        assert!(matches!(
            recv(sub).await,
            Item::Media(Record::Info { width: 1920, .. })
        ));
        assert_eq!(video_pts(&recv(sub).await), Some((true, base)));
        assert_eq!(video_pts(&recv(sub).await), Some((false, base + 1)));
    }
    assert_eq!(s.state(), ConnState::Connected);
    assert_eq!(fake.accepted.load(Ordering::SeqCst), 1);
    // The start requests carried the right handles.
    let seen = fake.seen.lock().unwrap().clone();
    let starts: Vec<_> = seen.iter().filter(|x| x.command == 3).collect();
    assert_eq!(starts.len(), 2);
    assert!(
        starts
            .iter()
            .any(|x| find_text(&x.text, "handle").unwrap().as_deref() == Some("258"))
    );
    assert!(
        starts
            .iter()
            .any(|x| find_text(&x.text, "handle").unwrap().as_deref() == Some("5"))
    );
    s.close().await;
}

#[tokio::test]
async fn bad_credentials_are_not_retried() {
    let fake = Fake::start(Mode::RejectLogin).await;
    let s = DeviceSession::spawn(TcpConnector::new(fake.addr), config());
    let mut sub = s.subscribe(0, StreamTier::Sub).await.unwrap();
    let mut rx = s.watch_state();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !matches!(*rx.borrow_and_update(), ConnState::LoginFailed { .. }) {
            rx.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(
        s.state(),
        ConnState::LoginFailed {
            status: 401,
            remain_times: Some(2)
        }
    );
    // Several backoff periods pass; there must be no further attempts.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(fake.accepted.load(Ordering::SeqCst), 1);
    assert!(sub.recv().await.is_none());
    assert!(s.subscribe(1, StreamTier::Sub).await.is_err());
}

#[tokio::test]
async fn keepalives_are_sent() {
    let fake = Fake::start(Mode::Normal).await;
    let mut cfg = config();
    cfg.keepalive_interval = Duration::from_millis(50);
    let s = DeviceSession::spawn(TcpConnector::new(fake.addr), cfg);
    wait_until("three keepalives", || fake.count(93) >= 3).await;
    assert_eq!(fake.accepted.load(Ordering::SeqCst), 1);
    s.close().await;
}

#[tokio::test]
async fn reconnects_do_not_stack() {
    let fake = Fake::start(Mode::DropAfterLogin).await;
    let s = DeviceSession::spawn(TcpConnector::new(fake.addr), config());
    let mut sub = s.subscribe(0, StreamTier::Sub).await.unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    let n = fake.accepted.load(Ordering::SeqCst);
    // Delays are 30, 60, 120, 120, ... ms: about 7 attempts; a hot loop or
    // stacked supervisors would make hundreds.
    assert!((3..=9).contains(&n), "{n} connection attempts");
    assert_eq!(fake.max_active.load(Ordering::SeqCst), 1);
    let t = fake.accept_times.lock().unwrap().clone();
    let gaps: Vec<_> = t.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps[0] >= Duration::from_millis(25), "{gaps:?}");
    assert!(gaps[1] >= Duration::from_millis(55), "{gaps:?}");
    assert!(gaps[2] >= Duration::from_millis(115), "{gaps:?}");
    assert!(matches!(
        s.state(),
        ConnState::Backoff { .. } | ConnState::Connecting | ConnState::Connected
    ));
    assert_eq!(recv(&mut sub).await, Item::Discontinuity);
    s.close().await;
}

#[tokio::test]
async fn previews_restart_after_reconnect() {
    let fake = Fake::start(Mode::DropFirstAfterStart).await;
    let s = DeviceSession::spawn(TcpConnector::new(fake.addr), config());
    let mut sub = s.subscribe(3, StreamTier::Sub).await.unwrap();
    // First connection: media, then the device hangs up. Unread media is
    // flushed on disconnect, so only what follows the gap is guaranteed.
    let mut saw_gap = false;
    let mut keyframe_after_gap = false;
    while !keyframe_after_gap {
        match recv(&mut sub).await {
            Item::Discontinuity => saw_gap = true,
            i if saw_gap && video_pts(&i).is_some_and(|(k, _)| k) => keyframe_after_gap = true,
            _ => {}
        }
    }
    assert!(saw_gap);
    assert_eq!(fake.accepted.load(Ordering::SeqCst), 2);
    let seen = fake.seen.lock().unwrap().clone();
    assert!(seen.iter().any(|x| x.conn == 0 && x.command == 3));
    assert!(seen.iter().any(|x| x.conn == 1 && x.command == 3));
    s.close().await;
}

#[tokio::test]
async fn close_stops_previews_before_closing_the_socket() {
    let fake = Fake::start(Mode::Normal).await;
    let s = DeviceSession::spawn(TcpConnector::new(fake.addr), config());
    let mut a = s.subscribe(1, StreamTier::Sub).await.unwrap();
    let mut b = s.subscribe(4, StreamTier::Sub).await.unwrap();
    recv(&mut a).await;
    recv(&mut b).await;
    s.close().await;
    wait_until("socket closed", || fake.active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(fake.stops_for(1), 1);
    assert_eq!(fake.stops_for(4), 1);
    // Stops come after the starts and nothing else was sent.
    let cmds: Vec<u32> = fake
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.command)
        .collect();
    assert!(cmds.iter().all(|c| [3, 4, 93].contains(c)), "{cmds:?}");
    // After close the queue drains and then ends; it must not hang.
    while a.recv().await.is_some() {}
}

#[tokio::test]
async fn dropping_the_handle_closes_the_session() {
    let fake = Fake::start(Mode::Normal).await;
    let s = DeviceSession::spawn(TcpConnector::new(fake.addr), config());
    let mut a = s.subscribe(6, StreamTier::Sub).await.unwrap();
    recv(&mut a).await;
    drop(s);
    wait_until("socket closed", || fake.active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(fake.stops_for(6), 1);
}

#[tokio::test]
async fn dropping_a_subscription_stops_only_that_preview() {
    let fake = Fake::start(Mode::Normal).await;
    let s = DeviceSession::spawn(TcpConnector::new(fake.addr), config());
    let mut a = s.subscribe(1, StreamTier::Sub).await.unwrap();
    let mut b = s.subscribe(2, StreamTier::Sub).await.unwrap();
    recv(&mut a).await;
    recv(&mut b).await;
    drop(a);
    wait_until("stop for channel 1", || fake.stops_for(1) == 1).await;
    assert_eq!(fake.stops_for(2), 0);
    assert_eq!(fake.active.load(Ordering::SeqCst), 1);
    s.close().await;
    wait_until("stop for channel 2", || fake.stops_for(2) == 1).await;
}
