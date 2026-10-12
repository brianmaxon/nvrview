//! A small in-process MQTT 3.1.1 broker for tests: plain TCP or TLS on
//! loopback, optional login check, retained messages, and events the test
//! waits on instead of sleeping.

#![allow(dead_code)]

mod ca;

pub use ca::TestCa;

use mqtt_triggers::mapping::topic_matches;
use mqtt_triggers::packet::{Decoder, Packet};
use mqtt_triggers::{Clock, Credentials, Handle, Options, SourceConfig, Trigger};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Upper bound on any wait for an event that must happen; hitting it means
/// the test failed, it is never a delay the test relies on.
pub const SAFETY: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Connected {
        client_id: String,
        username: Option<String>,
        keepalive_secs: u16,
        clean_session: bool,
    },
    Rejected,
    Subscribed(String),
    Disconnected,
}

enum Cmd {
    Publish {
        topic: String,
        payload: Vec<u8>,
        retain: bool,
    },
    Close,
}

enum Wire {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>),
}

impl Read for Wire {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(s) => s.read(b),
            Wire::Tls(s) => s.read(b),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(s) => s.write(b),
            Wire::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Wire::Plain(s) => s.flush(),
            Wire::Tls(s) => s.flush(),
        }
    }
}

#[derive(Default)]
pub struct BrokerOpts {
    pub port: Option<u16>,
    pub tls: Option<Arc<rustls::ServerConfig>>,
    /// Required login, if any.
    pub auth: Option<(String, String)>,
}

struct State {
    retained: HashMap<String, Vec<u8>>,
    conns: Vec<Sender<Cmd>>,
    stop: bool,
}

pub struct Broker {
    pub port: u16,
    state: Arc<Mutex<State>>,
    events: Receiver<Event>,
    accept: Option<JoinHandle<()>>,
    socks: Arc<Mutex<Vec<TcpStream>>>,
}

impl Broker {
    pub fn start(opts: BrokerOpts) -> Broker {
        let listener = TcpListener::bind(("127.0.0.1", opts.port.unwrap_or(0))).unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State {
            retained: HashMap::new(),
            conns: Vec::new(),
            stop: false,
        }));
        let socks = Arc::new(Mutex::new(Vec::new()));
        let (etx, events) = mpsc::channel();
        let st = state.clone();
        let sk = socks.clone();
        let accept = thread::spawn(move || {
            for stream in listener.incoming() {
                if st.lock().unwrap().stop {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let (ctx, crx) = mpsc::channel();
                st.lock().unwrap().conns.push(ctx);
                sk.lock().unwrap().push(stream.try_clone().unwrap());
                let st = st.clone();
                let etx = etx.clone();
                let tls = opts.tls.clone();
                let auth = opts.auth.clone();
                thread::spawn(move || {
                    let _ = serve(stream, tls, auth, st, crx, etx);
                });
            }
        });
        Broker {
            port,
            state,
            events,
            accept: Some(accept),
            socks,
        }
    }

    /// Delivers to subscribed clients (retain flag cleared, as a broker does
    /// for live messages) and keeps it when `retain`.
    pub fn publish(&self, topic: &str, payload: &[u8], retain: bool) {
        let mut st = self.state.lock().unwrap();
        if retain {
            st.retained.insert(topic.into(), payload.to_vec());
        }
        for c in &st.conns {
            let _ = c.send(Cmd::Publish {
                topic: topic.into(),
                payload: payload.to_vec(),
                retain: false,
            });
        }
    }

    /// Stores a retained message without delivering it to anyone connected.
    pub fn retain_only(&self, topic: &str, payload: &[u8]) {
        self.state
            .lock()
            .unwrap()
            .retained
            .insert(topic.into(), payload.to_vec());
    }

    /// Waits for an event the predicate accepts, skipping others.
    pub fn wait(&self, pred: impl Fn(&Event) -> bool) -> Event {
        loop {
            let e = self
                .events
                .recv_timeout(SAFETY)
                .expect("the broker event never came");
            if pred(&e) {
                return e;
            }
        }
    }

    pub fn wait_subscribed(&self) {
        self.wait(|e| matches!(e, Event::Subscribed(_)));
    }

    /// Events that are already queued, without waiting.
    pub fn drain(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Closes every connection and the listener.
    pub fn stop(mut self) {
        {
            let mut st = self.state.lock().unwrap();
            st.stop = true;
            for c in st.conns.drain(..) {
                let _ = c.send(Cmd::Close);
            }
        }
        for s in self.socks.lock().unwrap().drain(..) {
            let _ = s.shutdown(Shutdown::Both);
        }
        // Unblock accept().
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(j) = self.accept.take() {
            let _ = j.join();
        }
    }
}

fn serve(
    stream: TcpStream,
    tls: Option<Arc<rustls::ServerConfig>>,
    auth: Option<(String, String)>,
    state: Arc<Mutex<State>>,
    cmds: Receiver<Cmd>,
    events: Sender<Event>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(20)))?;
    let mut wire = match tls {
        None => Wire::Plain(stream),
        Some(cfg) => {
            let conn = rustls::ServerConnection::new(cfg).map_err(io::Error::other)?;
            let mut s = rustls::StreamOwned::new(conn, stream);
            while s.conn.is_handshaking() {
                match s.conn.complete_io(&mut s.sock) {
                    Ok(_) => {}
                    Err(e) if is_tick(&e) => {}
                    Err(e) => return Err(e),
                }
            }
            Wire::Tls(Box::new(s))
        }
    };
    let mut dec = Decoder::new();
    let mut filters: Vec<String> = Vec::new();
    let mut pid = 0u16;
    let send = |wire: &mut Wire, p: &Packet| -> io::Result<()> {
        let mut out = Vec::new();
        mqtt_triggers::packet::encode(p, &mut out);
        wire.write_all(&out)?;
        wire.flush()
    };
    loop {
        while let Ok(c) = cmds.try_recv() {
            match c {
                Cmd::Close => return Ok(()),
                Cmd::Publish {
                    topic,
                    payload,
                    retain,
                } => {
                    if filters.iter().any(|f| topic_matches(f, &topic)) {
                        pid = pid.wrapping_add(1).max(1);
                        send(
                            &mut wire,
                            &Packet::Publish {
                                topic,
                                payload,
                                qos: 1,
                                retain,
                                dup: false,
                                pid: Some(pid),
                            },
                        )?;
                    }
                }
            }
        }
        while let Some(p) = dec.next_packet().map_err(io::Error::other)? {
            match p {
                Packet::Connect {
                    client_id,
                    username,
                    password,
                    keepalive_secs,
                    clean_session,
                } => {
                    let ok = match &auth {
                        None => true,
                        Some((u, pw)) => {
                            username.as_deref() == Some(u.as_str())
                                && password.as_ref().map(|p| p.0.as_str()) == Some(pw.as_str())
                        }
                    };
                    send(
                        &mut wire,
                        &Packet::ConnAck {
                            session_present: false,
                            code: if ok { 0 } else { 5 },
                        },
                    )?;
                    if !ok {
                        let _ = events.send(Event::Rejected);
                        return Ok(());
                    }
                    let _ = events.send(Event::Connected {
                        client_id,
                        username,
                        keepalive_secs,
                        clean_session,
                    });
                }
                Packet::Subscribe { pid, filters: fs } => {
                    send(
                        &mut wire,
                        &Packet::SubAck {
                            pid,
                            codes: vec![1; fs.len()],
                        },
                    )?;
                    for (f, _) in fs {
                        let retained: Vec<(String, Vec<u8>)> = state
                            .lock()
                            .unwrap()
                            .retained
                            .iter()
                            .filter(|(t, _)| topic_matches(&f, t))
                            .map(|(t, p)| (t.clone(), p.clone()))
                            .collect();
                        for (topic, payload) in retained {
                            send(
                                &mut wire,
                                &Packet::Publish {
                                    topic,
                                    payload,
                                    qos: 0,
                                    retain: true,
                                    dup: false,
                                    pid: None,
                                },
                            )?;
                        }
                        filters.push(f.clone());
                        let _ = events.send(Event::Subscribed(f));
                    }
                }
                Packet::PingReq => send(&mut wire, &Packet::PingResp)?,
                Packet::Disconnect => {
                    let _ = events.send(Event::Disconnected);
                    return Ok(());
                }
                _ => {}
            }
        }
        let mut buf = [0u8; 2048];
        match wire.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => dec.push(&buf[..n]),
            Err(e) if is_tick(&e) => {}
            Err(e) => return Err(e),
        }
    }
}

fn is_tick(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

// ---- the client side -----------------------------------------------------

pub struct ManualClock(pub AtomicU64);

impl ManualClock {
    pub fn new(ms: u64) -> Arc<ManualClock> {
        Arc::new(ManualClock(AtomicU64::new(ms)))
    }
    pub fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct Running {
    pub handle: Handle,
    pub triggers: Receiver<Trigger>,
    pub clock: Arc<ManualClock>,
    pub logs: Arc<Mutex<Vec<String>>>,
}

/// A config for a source on loopback, with fast reconnects.
pub fn config(port: u16) -> SourceConfig {
    let mut c = SourceConfig::new("sensor-1", "motion", "127.0.0.1", port, "example/topic");
    c.reconnect.base_ms = 1;
    c.reconnect.max_ms = 5;
    c.mapping.debounce_ms = 0;
    c.limits.rate_per_min = 6_000;
    c.limits.rate_burst = 1_000;
    c
}

pub fn run(cfg: SourceConfig, creds: Option<Credentials>) -> Running {
    let (tx, triggers) = mpsc::channel();
    let tx = Mutex::new(tx);
    let clock = ManualClock::new(1_000_000);
    let logs = Arc::new(Mutex::new(Vec::new()));
    let l = logs.clone();
    let handle = mqtt_triggers::start(
        cfg,
        creds,
        Arc::new(move |t| {
            let _ = tx.lock().unwrap().send(t);
        }),
        Options {
            clock: clock.clone(),
            log: Some(Arc::new(move |m| l.lock().unwrap().push(m.to_string()))),
            jitter_seed: Some(7),
        },
    )
    .expect("start");
    Running {
        handle,
        triggers,
        clock,
        logs,
    }
}

impl Running {
    pub fn trigger(&self) -> Trigger {
        self.triggers.recv_timeout(SAFETY).expect("a trigger")
    }

    /// Waits until the stats satisfy the predicate.
    pub fn until(&self, pred: impl Fn(&mqtt_triggers::Stats) -> bool) {
        assert!(
            self.handle.wait_until(SAFETY, pred),
            "stats never reached the condition: {:?}",
            self.handle.stats()
        );
    }
}
