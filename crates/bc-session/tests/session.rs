//! End-to-end tests against an in-process fake device over in-memory pipes,
//! on a paused clock: every timeout, backoff, and retry runs in simulated
//! time. The fake speaks the protocol using bc-protocol's encoders and
//! synthetic credentials; no real device is involved.

use bc_protocol::crypto::{SessionKey, credential_hash, xor_obfuscate};
use bc_protocol::framer::{Decoder, RawMessage, encode_message_with_status};
use bc_protocol::header::Class;
use bc_protocol::media::{Codec, Record, encode_info, encode_video};
use bc_protocol::stream::StreamTier;
use bc_protocol::xml::find_text;
use bc_session::{
    BackoffConfig, ConnState, Connector, DeviceSession, FnConnector, Item, SessionConfig,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

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
    /// Refuse the login as locked, with an unlock time.
    LockedLogin,
    /// Log in, then never send another byte (a dead connection).
    SilentAfterLogin,
    /// Answer a preview start with the stream info only; the test feeds
    /// frames with [`Fake::send_live`].
    Live,
    /// Refuse every preview start with status 400.
    RefuseStart,
}

/// Frames the test asks the fake to send on a running preview.
#[derive(Clone, Debug)]
struct LiveFrames {
    handle: String,
    /// (keyframe, pts)
    frames: Vec<(bool, u64)>,
}

#[derive(Clone, Debug)]
struct Seen {
    conn: usize,
    command: u32,
    text: String,
}

struct Fake {
    mode: Mode,
    seen: Arc<Mutex<Vec<Seen>>>,
    accepted: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    accept_times: Arc<Mutex<Vec<Instant>>>,
    live: tokio::sync::broadcast::Sender<LiveFrames>,
}

impl Fake {
    async fn start(mode: Mode) -> Fake {
        Fake {
            mode,
            seen: Arc::default(),
            accepted: Arc::default(),
            active: Arc::default(),
            max_active: Arc::default(),
            accept_times: Arc::default(),
            live: tokio::sync::broadcast::channel(64).0,
        }
    }

    /// Connects to the fake through in-memory pipes, each served by a task
    /// of its own (as an accepted TCP connection would be).
    fn connector(&self) -> impl Connector + use<> {
        let mode = self.mode;
        let live = self.live.clone();
        let (seen, accepted, active, max_active, times) = (
            self.seen.clone(),
            self.accepted.clone(),
            self.active.clone(),
            self.max_active.clone(),
            self.accept_times.clone(),
        );
        FnConnector(move || {
            let (client, sock) = tokio::io::duplex(64 * 1024);
            let conn = accepted.fetch_add(1, Ordering::SeqCst);
            times.lock().unwrap().push(Instant::now());
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            max_active.fetch_max(now, Ordering::SeqCst);
            let (seen, active) = (seen.clone(), active.clone());
            let live_rx = live.subscribe();
            tokio::spawn(async move {
                serve(sock, conn, mode, seen, live_rx).await;
                active.fetch_sub(1, Ordering::SeqCst);
            });
            Ok(client)
        })
    }

    /// Sends frames on the running preview of `channel` and `tier`.
    fn send_live(&self, channel: u8, tier: StreamTier, frames: &[(bool, u64)]) {
        self.live
            .send(LiveFrames {
                handle: tier.handle(channel).to_string(),
                frames: frames.to_vec(),
            })
            .unwrap();
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

fn media_message(key: &SessionKey, id: u32, mut stream: Vec<u8>) -> Vec<u8> {
    let mut ext = b"<Extension version=\"1.1\"><binaryData>1</binaryData><encryptLen>1024</encryptLen></Extension>".to_vec();
    key.encrypt(&mut ext);
    key.encrypt(&mut stream);
    encode_message_with_status(3, id, 200, Class::Device, &ext, &stream).unwrap()
}

async fn serve(
    mut sock: impl AsyncRead + AsyncWrite + Unpin,
    conn: usize,
    mode: Mode,
    seen: Arc<Mutex<Vec<Seen>>>,
    mut live_rx: tokio::sync::broadcast::Receiver<LiveFrames>,
) {
    let key = SessionKey::derive(NONCE, PASSWORD);
    let mut dec = Decoder::new();
    let mut buf = [0u8; 4096];
    // Start message IDs by handle, for live frames.
    let mut starts: std::collections::HashMap<String, u32> = Default::default();
    loop {
        let n = tokio::select! {
            r = sock.read(&mut buf) => match r {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            },
            l = live_rx.recv() => {
                if let Ok(l) = l && let Some(id) = starts.get(&l.handle) {
                    let mut stream = Vec::new();
                    for (k, pts) in &l.frames {
                        let nal: &[u8] = if *k { &[0, 0, 0, 1, 0x65, 1] } else { &[0, 0, 0, 1, 0x41, 2] };
                        stream.extend(encode_video(0, *k, Codec::H264, *pts, &[], nal));
                    }
                    if sock.write_all(&media_message(&key, *id, stream)).await.is_err() {
                        return;
                    }
                }
                continue;
            }
        };
        dec.push(&buf[..n]);
        while let Some(m) = dec.next_message().unwrap() {
            let out = handle(&m, &key, conn, mode, &seen);
            if let Some(h) = out.started.clone() {
                starts.insert(h, m.header.message_id);
            }
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
    /// Handle of a preview this message started.
    started: Option<String>,
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
        started: None,
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
        let ok = !matches!(mode, Mode::RejectLogin | Mode::LockedLogin)
            && find_text(&text, "userName").unwrap().as_deref()
                == Some(credential_hash(USER, NONCE).as_str())
            && find_text(&text, "password").unwrap().as_deref()
                == Some(credential_hash(PASSWORD, NONCE).as_str());
        if ok {
            out.bytes
                .push(reply(1, h.message_id, 200, Class::Device, &[]));
            out.close = mode == Mode::DropAfterLogin;
        } else if mode == Mode::LockedLogin {
            let mut e = b"<body><LoginErrInfo><remainTimes>0</remainTimes><unlockTime>300</unlockTime></LoginErrInfo></body>".to_vec();
            xor_obfuscate(h.message_id, &mut e);
            out.bytes
                .push(reply(1, h.message_id, 407, Class::Device, &e));
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
    if mode == Mode::SilentAfterLogin {
        return out;
    }
    match h.command {
        2 => {
            out.bytes
                .push(reply(2, h.message_id, 200, Class::Device, &[]));
            out.close = true;
        }
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
            if mode == Mode::RefuseStart {
                out.bytes
                    .push(reply(3, h.message_id, 400, Class::Device, &[]));
                return out;
            }
            out.bytes
                .push(reply(3, h.message_id, 200, Class::Device, &[]));
            if mode == Mode::Live {
                out.started = find_text(&text, "handle").unwrap();
                out.bytes.push(media_message(
                    key,
                    h.message_id,
                    encode_info(1920, 1080, 15),
                ));
                return out;
            }
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
            out.bytes.push(media_message(key, h.message_id, stream));
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

#[tokio::test(start_paused = true)]
async fn login_succeeds_and_channels_are_demultiplexed() {
    let fake = Fake::start(Mode::Normal).await;
    let s = DeviceSession::spawn(fake.connector(), config());
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

#[tokio::test(start_paused = true)]
async fn bad_credentials_are_not_retried() {
    let fake = Fake::start(Mode::RejectLogin).await;
    let s = DeviceSession::spawn(fake.connector(), config());
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
            remain_times: Some(2),
            unlock_secs: None,
        }
    );
    // Several backoff periods pass; there must be no further attempts.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(fake.accepted.load(Ordering::SeqCst), 1);
    assert!(sub.recv().await.is_none());
    assert!(s.subscribe(1, StreamTier::Sub).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn idle_sessions_are_probed() {
    let fake = Fake::start(Mode::Normal).await;
    let mut cfg = config();
    cfg.probe_after_idle = Duration::from_millis(50);
    let s = DeviceSession::spawn(fake.connector(), cfg);
    wait_until("three connection tests", || fake.count(93) >= 3).await;
    // The device answers each test, so the connection stays up.
    assert_eq!(fake.accepted.load(Ordering::SeqCst), 1);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_silent_device_is_declared_dead_and_reconnected() {
    let fake = Fake::start(Mode::SilentAfterLogin).await;
    let mut cfg = config();
    cfg.probe_after_idle = Duration::from_millis(50);
    cfg.dead_after = Duration::from_millis(200);
    let s = DeviceSession::spawn(fake.connector(), cfg);
    let started = Instant::now();
    wait_until("a reconnect", || fake.accepted.load(Ordering::SeqCst) >= 2).await;
    // Not before the dead interval, and probes were tried first.
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert!(fake.count(93) >= 1);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_locked_account_reports_the_unlock_time() {
    let fake = Fake::start(Mode::LockedLogin).await;
    let s = DeviceSession::spawn(fake.connector(), config());
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
            status: 407,
            remain_times: Some(0),
            unlock_secs: Some(300),
        }
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake.accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn close_logs_out_after_stopping_previews() {
    let fake = Fake::start(Mode::Normal).await;
    let s = DeviceSession::spawn(fake.connector(), config());
    let mut a = s.subscribe(1, StreamTier::Sub).await.unwrap();
    recv(&mut a).await;
    s.close().await;
    wait_until("socket closed", || fake.active.load(Ordering::SeqCst) == 0).await;
    let cmds: Vec<u32> = fake
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.command)
        .collect();
    let stop = cmds.iter().position(|c| *c == 4).expect("a preview stop");
    let logout = cmds.iter().position(|c| *c == 2).expect("a logout");
    assert!(stop < logout, "{cmds:?}");
    assert_eq!(cmds.last(), Some(&2), "{cmds:?}");
    let text = &fake.seen.lock().unwrap()[logout].text;
    assert!(
        text.contains("<LoginUser") && !text.contains("password"),
        "{text}"
    );
}

#[tokio::test(start_paused = true)]
async fn reconnects_do_not_stack() {
    let fake = Fake::start(Mode::DropAfterLogin).await;
    let s = DeviceSession::spawn(fake.connector(), config());
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

#[tokio::test(start_paused = true)]
async fn previews_restart_after_reconnect() {
    let fake = Fake::start(Mode::DropFirstAfterStart).await;
    let s = DeviceSession::spawn(fake.connector(), config());
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

#[tokio::test(start_paused = true)]
async fn close_stops_previews_before_closing_the_socket() {
    let fake = Fake::start(Mode::Normal).await;
    let s = DeviceSession::spawn(fake.connector(), config());
    let mut a = s.subscribe(1, StreamTier::Sub).await.unwrap();
    let mut b = s.subscribe(4, StreamTier::Sub).await.unwrap();
    recv(&mut a).await;
    recv(&mut b).await;
    s.close().await;
    wait_until("socket closed", || fake.active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(fake.stops_for(1), 1);
    assert_eq!(fake.stops_for(4), 1);
    // Stops come after the starts and nothing else was sent but the
    // detection subscription (192) after login.
    let cmds: Vec<u32> = fake
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.command)
        .collect();
    assert!(
        cmds.iter().all(|c| [2, 3, 4, 93, 192].contains(c)),
        "{cmds:?}"
    );
    // After close the queue drains and then ends; it must not hang.
    while a.recv().await.is_some() {}
}

#[tokio::test(start_paused = true)]
async fn dropping_the_handle_closes_the_session() {
    let fake = Fake::start(Mode::Normal).await;
    let s = DeviceSession::spawn(fake.connector(), config());
    let mut a = s.subscribe(6, StreamTier::Sub).await.unwrap();
    recv(&mut a).await;
    drop(s);
    wait_until("socket closed", || fake.active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(fake.stops_for(6), 1);
}

#[tokio::test(start_paused = true)]
async fn dropping_a_subscription_stops_only_that_preview() {
    let fake = Fake::start(Mode::Normal).await;
    let s = DeviceSession::spawn(fake.connector(), config());
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

fn starts_for(fake: &Fake, channel: u8) -> usize {
    fake.seen
        .lock()
        .unwrap()
        .iter()
        .filter(|s| {
            s.command == 3
                && find_text(&s.text, "channelId").unwrap().as_deref() == Some(&channel.to_string())
        })
        .count()
}

async fn expect_video(sub: &mut bc_session::Subscription, want: (bool, u64)) {
    loop {
        let i = recv(sub).await;
        if let Some(v) = video_pts(&i) {
            assert_eq!(v, want);
            return;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn subscribers_to_one_stream_share_a_single_preview() {
    let fake = Fake::start(Mode::Normal).await;
    let s = DeviceSession::spawn(fake.connector(), config());
    let mut a = s.subscribe(2, StreamTier::Sub).await.unwrap();
    let mut b = s.subscribe(2, StreamTier::Sub).await.unwrap();
    for sub in [&mut a, &mut b] {
        assert!(matches!(
            recv(sub).await,
            Item::Media(Record::Info { width: 1920, .. })
        ));
        assert_eq!(video_pts(&recv(sub).await), Some((true, 2000)));
        assert_eq!(video_pts(&recv(sub).await), Some((false, 2001)));
    }
    assert_eq!(starts_for(&fake, 2), 1);
    assert_eq!(fake.accepted.load(Ordering::SeqCst), 1);
    s.close().await;
    wait_until("socket closed", || fake.active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(fake.stops_for(2), 1);
}

#[tokio::test(start_paused = true)]
async fn the_preview_stops_only_with_the_last_subscriber() {
    let fake = Fake::start(Mode::Live).await;
    let s = DeviceSession::spawn(fake.connector(), config());
    let mut a = s.subscribe(1, StreamTier::Sub).await.unwrap();
    let mut b = s.subscribe(1, StreamTier::Sub).await.unwrap();
    recv(&mut a).await;
    recv(&mut b).await;
    drop(a);
    // The stream goes on for the remaining subscriber.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fake.stops_for(1), 0);
    fake.send_live(1, StreamTier::Sub, &[(true, 10)]);
    expect_video(&mut b, (true, 10)).await;
    drop(b);
    wait_until("the stop", || fake.stops_for(1) == 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(fake.stops_for(1), 1);
    assert_eq!(starts_for(&fake, 1), 1);
    s.close().await;
    wait_until("socket closed", || fake.active.load(Ordering::SeqCst) == 0).await;
    assert_eq!(fake.stops_for(1), 1);
}

#[tokio::test(start_paused = true)]
async fn a_late_subscriber_starts_at_a_keyframe() {
    let fake = Fake::start(Mode::Live).await;
    let s = DeviceSession::spawn(fake.connector(), config());
    let mut a = s.subscribe(1, StreamTier::Sub).await.unwrap();
    recv(&mut a).await;
    fake.send_live(1, StreamTier::Sub, &[(true, 1), (false, 2)]);
    expect_video(&mut a, (true, 1)).await;
    expect_video(&mut a, (false, 2)).await;
    let mut b = s.subscribe(1, StreamTier::Sub).await.unwrap();
    // Stream info is replayed; no preview was started for it.
    assert!(matches!(
        recv(&mut b).await,
        Item::Media(Record::Info { .. })
    ));
    // Let the subscription reach the supervisor, then send predicted frames
    // (which b must skip) followed by a keyframe.
    tokio::time::sleep(Duration::from_millis(100)).await;
    fake.send_live(1, StreamTier::Sub, &[(false, 3)]);
    expect_video(&mut a, (false, 3)).await;
    fake.send_live(1, StreamTier::Sub, &[(false, 4), (true, 5), (false, 6)]);
    expect_video(&mut b, (true, 5)).await;
    expect_video(&mut b, (false, 6)).await;
    assert_eq!(starts_for(&fake, 1), 1);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_slow_subscriber_only_overflows_its_own_queue() {
    let fake = Fake::start(Mode::Live).await;
    let mut cfg = config();
    cfg.queue_frames = 4;
    let s = DeviceSession::spawn(fake.connector(), cfg);
    let mut slow = s.subscribe(1, StreamTier::Sub).await.unwrap();
    let mut fast = s.subscribe(1, StreamTier::Sub).await.unwrap();
    recv(&mut fast).await;
    fake.send_live(1, StreamTier::Sub, &[(true, 0)]);
    expect_video(&mut fast, (true, 0)).await;
    // One frame at a time, so the fast subscriber never falls behind; the
    // slow one never reads.
    for pts in 1..=20u64 {
        fake.send_live(1, StreamTier::Sub, &[(false, pts)]);
        expect_video(&mut fast, (false, pts)).await;
    }
    assert_eq!(fast.dropped(), 0);
    assert!(slow.dropped() > 0);
    // The slow one sees a gap, then resumes at the next keyframe.
    fake.send_live(1, StreamTier::Sub, &[(true, 100)]);
    expect_video(&mut fast, (true, 100)).await;
    let mut saw_gap = false;
    loop {
        match recv(&mut slow).await {
            Item::Discontinuity => saw_gap = true,
            i => {
                if let Some(v) = video_pts(&i) {
                    assert!(saw_gap);
                    assert_eq!(v, (true, 100));
                    break;
                }
            }
        }
    }
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn a_reconnect_restarts_a_shared_stream_once() {
    let fake = Fake::start(Mode::DropFirstAfterStart).await;
    let s = DeviceSession::spawn(fake.connector(), config());
    let mut a = s.subscribe(3, StreamTier::Sub).await.unwrap();
    let mut b = s.subscribe(3, StreamTier::Sub).await.unwrap();
    for sub in [&mut a, &mut b] {
        let mut saw_gap = false;
        loop {
            match recv(sub).await {
                Item::Discontinuity => saw_gap = true,
                i if saw_gap && video_pts(&i).is_some_and(|(k, _)| k) => break,
                _ => {}
            }
        }
    }
    assert_eq!(fake.accepted.load(Ordering::SeqCst), 2);
    let seen = fake.seen.lock().unwrap().clone();
    for conn in [0, 1] {
        let n = seen
            .iter()
            .filter(|x| x.conn == conn && x.command == 3)
            .count();
        assert_eq!(n, 1, "starts on connection {conn}");
    }
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn tiers_of_one_channel_are_separate_previews() {
    let fake = Fake::start(Mode::Normal).await;
    let s = DeviceSession::spawn(fake.connector(), config());
    let mut a = s.subscribe(2, StreamTier::Sub).await.unwrap();
    let mut b = s.subscribe(2, StreamTier::Main).await.unwrap();
    recv(&mut a).await;
    recv(&mut b).await;
    assert_eq!(starts_for(&fake, 2), 2);
    drop(a);
    wait_until("one stop", || fake.stops_for(2) == 1).await;
    assert_eq!(fake.active.load(Ordering::SeqCst), 1);
    s.close().await;
    wait_until("both stops", || fake.stops_for(2) == 2).await;
}

#[tokio::test(start_paused = true)]
async fn a_refused_start_reaches_every_subscriber() {
    let fake = Fake::start(Mode::RefuseStart).await;
    // Without a wait, a 400 is final at once.
    let mut cfg = config();
    cfg.preview_wait_limit = Duration::ZERO;
    let s = DeviceSession::spawn(fake.connector(), cfg);
    let mut a = s.subscribe(2, StreamTier::Sub).await.unwrap();
    let mut b = s.subscribe(2, StreamTier::Sub).await.unwrap();
    assert_eq!(recv(&mut a).await, Item::StartRejected { status: 400 });
    assert_eq!(recv(&mut b).await, Item::StartRejected { status: 400 });
    // A subscriber that attaches afterwards hears it too, without a second
    // start request.
    let mut c = s.subscribe(2, StreamTier::Sub).await.unwrap();
    assert_eq!(recv(&mut c).await, Item::StartRejected { status: 400 });
    assert_eq!(starts_for(&fake, 2), 1);
    s.close().await;
}

fn waiting_config(retry_ms: u64, limit_ms: u64) -> SessionConfig {
    let mut c = config();
    c.preview_retry_interval = Duration::from_millis(retry_ms);
    c.preview_wait_limit = Duration::from_millis(limit_ms);
    c
}

/// The preview starts (3) and stops (4) the fake saw for `channel`, in order.
fn starts_and_stops(fake: &Fake, channel: u8) -> Vec<u32> {
    let ch = channel.to_string();
    fake.seen
        .lock()
        .unwrap()
        .iter()
        .filter(|s| {
            (s.command == 3 || s.command == 4)
                && find_text(&s.text, "channelId").unwrap().as_deref() == Some(ch.as_str())
        })
        .map(|s| s.command)
        .collect()
}

/// Every start is stopped before the next one: never two outstanding.
fn assert_alternating(seq: &[u32]) {
    for (i, c) in seq.iter().enumerate() {
        assert_eq!(*c, if i % 2 == 0 { 3 } else { 4 }, "sequence {seq:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_400_is_waited_out_with_restarts_then_refused() {
    let fake = Fake::start(Mode::RefuseStart).await;
    let s = DeviceSession::spawn(fake.connector(), waiting_config(150, 1000));
    let started = Instant::now();
    let mut a = s.subscribe(2, StreamTier::Sub).await.unwrap();
    let mut b = s.subscribe(2, StreamTier::Sub).await.unwrap();
    assert_eq!(recv(&mut a).await, Item::WaitingForDevice { status: 400 });
    assert_eq!(recv(&mut b).await, Item::WaitingForDevice { status: 400 });
    // A late subscriber hears that the stream is being waited for.
    let mut c = s.subscribe(2, StreamTier::Sub).await.unwrap();
    assert_eq!(recv(&mut c).await, Item::WaitingForDevice { status: 400 });
    for sub in [&mut a, &mut b, &mut c] {
        // Nothing else (no second "waiting") until the wait ends.
        assert_eq!(recv(sub).await, Item::StartRejected { status: 400 });
    }
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(1000) && waited < Duration::from_millis(2500),
        "gave up after {waited:?}"
    );
    // The last start is stopped when the session gives up.
    wait_until("the final stop", || {
        starts_and_stops(&fake, 2).len().is_multiple_of(2)
    })
    .await;
    let seq = starts_and_stops(&fake, 2);
    assert_alternating(&seq);
    let starts = seq.len() / 2;
    // One start, then a restart about every 150 ms for 1 s.
    assert!((4..=8).contains(&starts), "{starts} starts");
    // Refused for good: no more starts.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(starts_and_stops(&fake, 2), seq);
    s.close().await;
    assert_eq!(starts_and_stops(&fake, 2), seq);
}

#[tokio::test(start_paused = true)]
async fn restarts_follow_the_retry_interval() {
    let fake = Fake::start(Mode::RefuseStart).await;
    let s = DeviceSession::spawn(fake.connector(), waiting_config(300, 60_000));
    let mut a = s.subscribe(1, StreamTier::Main).await.unwrap();
    assert_eq!(recv(&mut a).await, Item::WaitingForDevice { status: 400 });
    let t0 = Instant::now();
    wait_until("three starts", || starts_for(&fake, 1) >= 3).await;
    let elapsed = t0.elapsed();
    // Two restarts, each 300 ms after the previous 400.
    assert!(
        elapsed >= Duration::from_millis(550) && elapsed < Duration::from_millis(1500),
        "{elapsed:?}"
    );
    assert_alternating(&starts_and_stops(&fake, 1));
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn dropping_the_subscription_during_the_wait_stops_the_preview() {
    let fake = Fake::start(Mode::RefuseStart).await;
    let s = DeviceSession::spawn(fake.connector(), waiting_config(100, 60_000));
    let mut a = s.subscribe(3, StreamTier::Sub).await.unwrap();
    assert_eq!(recv(&mut a).await, Item::WaitingForDevice { status: 400 });
    wait_until("a restart", || starts_for(&fake, 3) >= 2).await;
    drop(a);
    wait_until("the stop", || {
        starts_and_stops(&fake, 3).len().is_multiple_of(2)
    })
    .await;
    let seq = starts_and_stops(&fake, 3);
    // No restart after the stop.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(starts_and_stops(&fake, 3), seq);
    assert_alternating(&seq);
    s.close().await;
}

#[tokio::test(start_paused = true)]
async fn closing_the_session_during_the_wait_stops_the_preview() {
    let fake = Fake::start(Mode::RefuseStart).await;
    let s = DeviceSession::spawn(fake.connector(), waiting_config(100, 60_000));
    let mut a = s.subscribe(3, StreamTier::Sub).await.unwrap();
    assert_eq!(recv(&mut a).await, Item::WaitingForDevice { status: 400 });
    s.close().await;
    let seq = starts_and_stops(&fake, 3);
    assert_alternating(&seq);
    assert_eq!(seq.len() % 2, 0, "{seq:?}");
    assert_eq!(fake.count(2), 1, "logged out");
}
