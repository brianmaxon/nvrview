//! The connection loop: connect, subscribe, read, map, reconnect.

use crate::config::SourceConfig;
use crate::mapping::{Mapper, Outcome, Reason, TriggerState};
use crate::messages::{MessageLog, MessageOutcome, MessagePage};
use crate::packet::{Decoder, Packet, Secret, encode};
use serde::Serialize;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// One trigger for the host's alerts pipeline. Every field is either
/// configured by the user or fixed by this crate; no payload text is in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Trigger {
    /// The source key from the config.
    pub source_id: String,
    /// The kind from the config.
    pub kind: String,
    pub state: TriggerState,
    /// Who sent it: `mqtt:` and the source key. The host treats this as the
    /// authenticated identity of the input, never as text to show.
    pub sender_id: String,
    /// When the message arrived (the clock's Unix milliseconds).
    pub at_ms: u64,
}

pub type Sink = Arc<dyn Fn(Trigger) + Send + Sync>;

/// Receives a line of log text.
pub type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

pub trait Clock: Send + Sync {
    /// Unix time in milliseconds.
    fn now_ms(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
}

/// Broker login, supplied by the host at start (from its keychain). Held in
/// memory for the connection only; never printed or logged.
#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials(..)")
    }
}

pub struct Options {
    pub clock: Arc<dyn Clock>,
    /// Receives connection problems, at most one line every 30 s (with a
    /// count of the ones held back). Never contains payload text or secrets.
    pub log: Option<LogFn>,
    /// Seed for the reconnect jitter; unset uses the time.
    pub jitter_seed: Option<u64>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            clock: Arc::new(SystemClock),
            log: None,
            jitter_seed: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Connecting,
    Connected,
    /// Waiting to retry after a failure.
    Backoff,
    /// The broker rejected the login. Not retried; stop and start again with
    /// corrected credentials.
    AuthFailed,
    /// The broker's address is not on the local network and `allow_public`
    /// is off. Retried (a name can change), at the backoff pace.
    Refused,
    Stopped,
}

/// Why the last connection attempt (or connection) failed, in the terms a
/// person fixes it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The broker's name did not resolve.
    Dns,
    /// The address is not on the local network and `allow_public` is off.
    NotLocal,
    /// Something answered at the address and refused the connection (nothing
    /// is listening on the port).
    Refused,
    /// No answer in time.
    Timeout,
    /// Another network failure (unreachable, reset, closed).
    Network,
    /// The broker's certificate is not trusted, or has expired.
    TlsCertificate,
    /// The certificate is not for the name used to reach the broker.
    TlsHostname,
    /// Another TLS failure (protocol, handshake).
    Tls,
    /// The broker rejected the user name or password.
    Login,
    /// The broker spoke MQTT in a way this client does not accept.
    Protocol,
}

/// Audit counters. Every message ends in exactly one of `triggers` or a
/// `dropped_*` counter (or `retained_seeded`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub status: Status,
    /// The last connection problem, from a fixed list of phrases.
    pub last_error: Option<&'static str>,
    /// The kind of `last_error`.
    pub last_error_kind: Option<ErrorKind>,
    /// When `last_error` happened (the clock's Unix milliseconds).
    pub last_error_ms: Option<u64>,
    /// When the connection last worked: the subscription was accepted, or
    /// the connection that had been working ended.
    pub last_success_ms: Option<u64>,
    pub connects: u64,
    pub reconnects: u64,
    pub connect_failures: u64,
    pub auth_failures: u64,
    pub protocol_errors: u64,
    pub messages: u64,
    pub triggers: u64,
    pub retained_seeded: u64,
    pub dropped_topic: u64,
    pub dropped_oversized: u64,
    pub dropped_rate_limited: u64,
    pub dropped_retained: u64,
    pub dropped_unmapped: u64,
    pub dropped_repeat: u64,
    pub dropped_transition: u64,
    pub dropped_debounced: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StartError {
    Config(crate::config::ConfigError),
    /// This processor lacks features the TLS crypto needs.
    TlsUnsupported,
    /// The CA certificate text did not hold a usable certificate.
    BadCa,
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Config(e) => e.fmt(f),
            StartError::TlsUnsupported => f.write_str("this processor cannot run the TLS crypto"),
            StartError::BadCa => f.write_str("the CA certificate is not usable"),
        }
    }
}

impl std::error::Error for StartError {}

impl From<crate::config::ConfigError> for StartError {
    fn from(e: crate::config::ConfigError) -> StartError {
        StartError::Config(e)
    }
}

struct Inner {
    stats: Stats,
    stop: bool,
    /// For `stop` to wake a blocked read.
    sock: Option<TcpStream>,
}

struct Shared {
    inner: Mutex<Inner>,
    cv: Condvar,
    messages: Mutex<MessageLog>,
    log_state: Mutex<(Option<u64>, u64)>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn update(&self, f: impl FnOnce(&mut Stats)) {
        f(&mut lock(&self.inner).stats);
        self.cv.notify_all();
    }

    fn stopped(&self) -> bool {
        lock(&self.inner).stop
    }
}

/// A running source. Dropping it stops the thread without waiting for it;
/// [`Handle::stop`] waits.
pub struct Handle {
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

impl Handle {
    pub fn stats(&self) -> Stats {
        lock(&self.shared.inner).stats.clone()
    }

    /// Received messages with `seq >= after`, oldest first, at most
    /// `limit` (memory only; see [`crate::messages`]).
    pub fn messages(&self, after: u64, limit: usize) -> MessagePage {
        lock(&self.shared.messages).page(after, limit)
    }

    /// Forgets the logged messages (the sequence numbers go on).
    pub fn clear_messages(&self) {
        lock(&self.shared.messages).clear();
    }

    /// Blocks until `pred` holds for the stats. `timeout` is a safety bound
    /// for a condition that never comes, not a delay: it returns `false` if
    /// it passes first.
    pub fn wait_until(&self, timeout: Duration, pred: impl Fn(&Stats) -> bool) -> bool {
        let g = lock(&self.shared.inner);
        let (_g, res) = self
            .shared
            .cv
            .wait_timeout_while(g, timeout, |i| !pred(&i.stats))
            .unwrap_or_else(|e| e.into_inner());
        !res.timed_out()
    }

    fn signal(&self) {
        let mut g = lock(&self.shared.inner);
        g.stop = true;
        if let Some(s) = &g.sock {
            let _ = s.shutdown(Shutdown::Read);
        }
        drop(g);
        self.shared.cv.notify_all();
    }

    /// Stops the source, sends a clean DISCONNECT when connected, and waits
    /// for the thread. Waits at most one connect attempt if it is mid-connect.
    pub fn stop(mut self) {
        self.signal();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl std::ops::Drop for Handle {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.signal();
        }
    }
}

/// The delay before reconnect attempt `attempt` (0 is the first retry):
/// exponential from `base_ms` up to `max_ms`, then jittered to between half
/// and all of it. `unit` is a random number; any value is valid.
pub fn backoff_ms(attempt: u32, base_ms: u64, max_ms: u64, unit: u64) -> u64 {
    let full = base_ms.saturating_mul(1u64 << attempt.min(20)).min(max_ms);
    let half = full / 2;
    half + if half == 0 { 0 } else { unit % (half + 1) }
}

/// Starts the source. `config` is validated first; a literal public IP is
/// refused here, a name is checked when it resolves.
pub fn start(
    config: SourceConfig,
    credentials: Option<Credentials>,
    sink: Sink,
    options: Options,
) -> Result<Handle, StartError> {
    config.validate()?;
    let tls = match &config.broker.tls {
        None => None,
        Some(t) => {
            if !tls_supported() {
                return Err(StartError::TlsUnsupported);
            }
            Some(tls_config(t.ca_pem.as_deref())?)
        }
    };
    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            stats: Stats::default(),
            stop: false,
            sock: None,
        }),
        cv: Condvar::new(),
        messages: Mutex::new(MessageLog::default()),
        log_state: Mutex::new((None, 0)),
    });
    let ctx = Ctx {
        shared: shared.clone(),
        cfg: config,
        creds: credentials,
        sink,
        clock: options.clock,
        log: options.log,
        tls,
        seed: options.jitter_seed,
    };
    let name = format!("mqtt-trigger-{}", ctx.cfg.id);
    let join = std::thread::Builder::new()
        .name(name)
        .spawn(move || run(ctx))
        .expect("spawn");
    Ok(Handle {
        shared,
        join: Some(join),
    })
}

// ---- TLS -----------------------------------------------------------------

/// The graviola crypto stops the process on a processor without the features
/// it requires, so check first.
fn tls_supported() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("aes")
            && std::arch::is_x86_feature_detected!("pclmulqdq")
            && std::arch::is_x86_feature_detected!("ssse3")
            && std::arch::is_x86_feature_detected!("avx")
            && std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("bmi1")
            && std::arch::is_x86_feature_detected!("bmi2")
            && std::arch::is_x86_feature_detected!("adx")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("neon")
            && std::arch::is_aarch64_feature_detected!("aes")
            && std::arch::is_aarch64_feature_detected!("pmull")
            && std::arch::is_aarch64_feature_detected!("sha2")
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

fn tls_config(ca_pem: Option<&str>) -> Result<Arc<rustls::ClientConfig>, StartError> {
    use rustls::pki_types::{CertificateDer, pem::PemObject};
    let mut roots = rustls::RootCertStore::empty();
    match ca_pem {
        Some(pem) => {
            let mut n = 0;
            for der in CertificateDer::pem_slice_iter(pem.as_bytes()) {
                let der = der.map_err(|_| StartError::BadCa)?;
                roots.add(der).map_err(|_| StartError::BadCa)?;
                n += 1;
            }
            if n == 0 {
                return Err(StartError::BadCa);
            }
        }
        None => roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
    }
    let cfg =
        rustls::ClientConfig::builder_with_provider(Arc::new(rustls_graviola::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|_| StartError::TlsUnsupported)?
            .with_root_certificates(roots)
            .with_no_client_auth();
    Ok(Arc::new(cfg))
}

// ---- the connection --------------------------------------------------------

enum Wire {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(s) => s.read(buf),
            Wire::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Wire::Plain(s) => s.write(buf),
            Wire::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Wire::Plain(s) => s.flush(),
            Wire::Tls(s) => s.flush(),
        }
    }
}

struct Conn {
    wire: Wire,
    dec: Decoder,
    last_rx: Instant,
    last_tx: Instant,
}

impl Conn {
    fn send(&mut self, p: &Packet) -> io::Result<()> {
        let mut out = Vec::new();
        encode(p, &mut out);
        self.wire.write_all(&out)?;
        self.wire.flush()?;
        self.last_tx = Instant::now();
        Ok(())
    }

    /// The next packet; `Ok(None)` when the read tick passed with nothing
    /// whole to return.
    fn poll(&mut self) -> Result<Option<Packet>, Fail> {
        loop {
            match self.dec.next_packet() {
                Ok(Some(p)) => return Ok(Some(p)),
                Ok(None) => {}
                Err(_) => return Err(Fail::Protocol("malformed packet from the broker")),
            }
            let mut buf = [0u8; 4096];
            match self.wire.read(&mut buf) {
                Ok(0) => {
                    return Err(Fail::Io(
                        ErrorKind::Network,
                        "the broker closed the connection",
                    ));
                }
                Ok(n) => {
                    self.last_rx = Instant::now();
                    self.dec.push(&buf[..n]);
                }
                Err(e) if is_tick(&e) => return Ok(None),
                Err(_) => return Err(Fail::Io(ErrorKind::Network, "the connection failed")),
            }
        }
    }
}

fn is_tick(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

enum Fail {
    Stopped,
    Auth,
    Refused,
    Io(ErrorKind, &'static str),
    Protocol(&'static str),
}

struct Ctx {
    shared: Arc<Shared>,
    cfg: SourceConfig,
    creds: Option<Credentials>,
    sink: Sink,
    clock: Arc<dyn Clock>,
    log: Option<LogFn>,
    tls: Option<Arc<rustls::ClientConfig>>,
    seed: Option<u64>,
}

impl Ctx {
    fn log(&self, msg: &str) {
        let Some(f) = &self.log else { return };
        let now = self.clock.now_ms();
        let mut g = lock(&self.shared.log_state);
        match g.0 {
            Some(last) if now.saturating_sub(last) < 30_000 => g.1 += 1,
            _ => {
                let held = std::mem::take(&mut g.1);
                g.0 = Some(now);
                drop(g);
                if held > 0 {
                    f(&format!(
                        "mqtt source {}: {msg} ({held} similar held back)",
                        self.cfg.id
                    ));
                } else {
                    f(&format!("mqtt source {}: {msg}", self.cfg.id));
                }
            }
        }
    }

    fn tick(&self) -> Duration {
        Duration::from_millis((u64::from(self.cfg.keepalive_secs) * 250).clamp(100, 1_000))
    }
}

static SEED_MIX: AtomicU64 = AtomicU64::new(0x9e37_79b9_7f4a_7c15);

fn run(ctx: Ctx) {
    let now = ctx.clock.now_ms();
    let mut mapper = Mapper::new(&ctx.cfg, now);
    let mut rng = ctx.seed.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64)
            ^ SEED_MIX.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed)
    }) | 1;
    let mut attempt = 0u32;
    loop {
        if ctx.shared.stopped() {
            break;
        }
        ctx.shared.update(|s| s.status = Status::Connecting);
        let mut connected = false;
        let fail = session(&ctx, &mut mapper, &mut connected);
        lock(&ctx.shared.inner).sock = None;
        if connected {
            attempt = 0;
        }
        let now = ctx.clock.now_ms();
        let (status, kind, msg) = match fail {
            Fail::Stopped => break,
            Fail::Auth => {
                ctx.shared.update(|s| {
                    s.status = Status::AuthFailed;
                    s.auth_failures += 1;
                    s.last_error = Some("the broker rejected the login");
                    s.last_error_kind = Some(ErrorKind::Login);
                    s.last_error_ms = Some(now);
                });
                ctx.log("the broker rejected the login (check the user name and password); not retrying");
                return;
            }
            Fail::Refused => (
                Status::Refused,
                ErrorKind::NotLocal,
                "the broker is not on the local network",
            ),
            Fail::Io(k, m) => (Status::Backoff, k, m),
            Fail::Protocol(m) => {
                ctx.shared.update(|s| s.protocol_errors += 1);
                (Status::Backoff, ErrorKind::Protocol, m)
            }
        };
        ctx.shared.update(|s| {
            s.status = status;
            s.last_error = Some(msg);
            s.last_error_kind = Some(kind);
            s.last_error_ms = Some(now);
            if connected {
                s.last_success_ms = Some(now);
            } else {
                s.connect_failures += 1;
            }
        });
        ctx.log(msg);
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let wait = backoff_ms(
            attempt,
            ctx.cfg.reconnect.base_ms,
            ctx.cfg.reconnect.max_ms,
            rng,
        );
        attempt = attempt.saturating_add(1);
        // Sleep on the condition variable so stop wakes it at once.
        let deadline = Instant::now() + Duration::from_millis(wait);
        let mut g = lock(&ctx.shared.inner);
        while !g.stop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            g = ctx
                .shared
                .cv
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    ctx.shared.update(|s| s.status = Status::Stopped);
}

const DNS: &str = "the broker's name did not resolve";

fn resolve(ctx: &Ctx) -> Result<Vec<SocketAddr>, Fail> {
    let b = &ctx.cfg.broker;
    let all: Vec<SocketAddr> = (b.host.as_str(), b.port)
        .to_socket_addrs()
        .map_err(|_| Fail::Io(ErrorKind::Dns, DNS))?
        .collect();
    check_addrs(all, ctx.cfg.allow_public)
}

/// Keeps only the addresses on the local network unless `allow_public`.
/// Connecting to the checked addresses (not re-resolving the name) means a
/// name cannot be pointed elsewhere between the check and the connection.
fn check_addrs(all: Vec<SocketAddr>, allow_public: bool) -> Result<Vec<SocketAddr>, Fail> {
    if allow_public {
        return Ok(all);
    }
    let local: Vec<SocketAddr> = all
        .iter()
        .copied()
        .filter(|a| crate::net::is_local(a.ip()))
        .collect();
    if local.is_empty() {
        return Err(if all.is_empty() {
            Fail::Io(ErrorKind::Dns, DNS)
        } else {
            Fail::Refused
        });
    }
    Ok(local)
}

fn tcp_fail(kind: Option<io::ErrorKind>) -> Fail {
    match kind {
        Some(io::ErrorKind::ConnectionRefused) => Fail::Io(
            ErrorKind::Refused,
            "the broker refused the connection (nothing is listening on that port)",
        ),
        Some(io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock) => Fail::Io(
            ErrorKind::Timeout,
            "the broker did not answer in time (wrong address, or a firewall)",
        ),
        _ => Fail::Io(
            ErrorKind::Network,
            "the broker could not be reached (network unreachable or connection reset)",
        ),
    }
}

/// Names the TLS failure by what a person changes to fix it.
fn tls_fail(e: &io::Error) -> Fail {
    use rustls::{CertificateError as C, Error as E};
    let inner = e.get_ref().and_then(|i| i.downcast_ref::<E>());
    match inner {
        Some(E::InvalidCertificate(c)) => match c {
            C::NotValidForName | C::NotValidForNameContext { .. } => Fail::Io(
                ErrorKind::TlsHostname,
                "TLS failed: the certificate is not valid for the broker's name",
            ),
            C::UnknownIssuer | C::UnhandledCriticalExtension => Fail::Io(
                ErrorKind::TlsCertificate,
                "TLS failed: the broker's certificate is not trusted (add its CA)",
            ),
            C::Expired
            | C::ExpiredContext { .. }
            | C::NotValidYet
            | C::NotValidYetContext { .. } => Fail::Io(
                ErrorKind::TlsCertificate,
                "TLS failed: the broker's certificate has expired or is not valid yet",
            ),
            _ => Fail::Io(
                ErrorKind::TlsCertificate,
                "TLS failed: the broker's certificate was rejected",
            ),
        },
        _ => match e.kind() {
            io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset => Fail::Io(
                ErrorKind::Tls,
                "TLS failed: the broker closed the connection (is the port a plain MQTT port?)",
            ),
            _ => Fail::Io(ErrorKind::Tls, "the TLS handshake failed"),
        },
    }
}

fn session(ctx: &Ctx, mapper: &mut Mapper, connected: &mut bool) -> Fail {
    match session_inner(ctx, mapper, connected) {
        Err(_) if ctx.shared.stopped() => Fail::Stopped,
        Err(f) => f,
        Ok(never) => match never {},
    }
}

fn session_inner(
    ctx: &Ctx,
    mapper: &mut Mapper,
    connected: &mut bool,
) -> Result<std::convert::Infallible, Fail> {
    let cfg = &ctx.cfg;
    let connect_timeout = Duration::from_millis(cfg.connect_timeout_ms);
    let addrs = resolve(ctx)?;
    let mut sock = None;
    let mut last_err = None;
    for a in addrs {
        if ctx.shared.stopped() {
            return Err(Fail::Stopped);
        }
        match TcpStream::connect_timeout(&a, connect_timeout) {
            Ok(s) => {
                sock = Some(s);
                break;
            }
            Err(e) => last_err = Some(e.kind()),
        }
    }
    let sock = sock.ok_or_else(|| tcp_fail(last_err))?;
    let _ = sock.set_nodelay(true);
    let _ = sock.set_read_timeout(Some(ctx.tick()));
    let _ = sock.set_write_timeout(Some(Duration::from_secs(10)));
    {
        let mut g = lock(&ctx.shared.inner);
        if g.stop {
            return Err(Fail::Stopped);
        }
        g.sock = sock.try_clone().ok();
    }
    let deadline = Instant::now() + connect_timeout;

    let wire = match &ctx.tls {
        None => Wire::Plain(sock),
        Some(tls) => {
            let name = match cfg.broker.host.parse::<IpAddr>() {
                Ok(ip) => rustls::pki_types::ServerName::IpAddress(ip.into()),
                Err(_) => rustls::pki_types::ServerName::try_from(cfg.broker.host.clone())
                    .map_err(|_| {
                        Fail::Io(
                            ErrorKind::TlsHostname,
                            "the broker's name is not valid for TLS",
                        )
                    })?,
            };
            let conn = rustls::ClientConnection::new(tls.clone(), name)
                .map_err(|_| Fail::Io(ErrorKind::Tls, "TLS could not start"))?;
            let mut s = rustls::StreamOwned::new(conn, sock);
            while s.conn.is_handshaking() {
                if ctx.shared.stopped() {
                    return Err(Fail::Stopped);
                }
                match s.conn.complete_io(&mut s.sock) {
                    Ok(_) => {}
                    Err(e) if is_tick(&e) => {
                        if Instant::now() >= deadline {
                            return Err(Fail::Io(
                                ErrorKind::Timeout,
                                "the TLS handshake timed out",
                            ));
                        }
                    }
                    Err(e) => return Err(tls_fail(&e)),
                }
            }
            Wire::Tls(Box::new(s))
        }
    };
    let mut conn = Conn {
        wire,
        dec: Decoder::new(),
        last_rx: Instant::now(),
        last_tx: Instant::now(),
    };

    let io_fail = |_e: io::Error| Fail::Io(ErrorKind::Network, "the connection failed");
    conn.send(&Packet::Connect {
        client_id: cfg.client_id.clone(),
        keepalive_secs: cfg.keepalive_secs,
        clean_session: cfg.clean_session,
        username: ctx.creds.as_ref().map(|c| c.username.clone()),
        password: ctx.creds.as_ref().map(|c| Secret(c.password.clone())),
    })
    .map_err(io_fail)?;
    loop {
        if ctx.shared.stopped() {
            return Err(Fail::Stopped);
        }
        match conn.poll()? {
            Some(Packet::ConnAck { code: 0, .. }) => break,
            Some(Packet::ConnAck { code: 4 | 5, .. }) => return Err(Fail::Auth),
            Some(Packet::ConnAck { code: 1, .. }) => {
                return Err(Fail::Protocol("the broker does not accept MQTT 3.1.1"));
            }
            Some(Packet::ConnAck { code: 2, .. }) => {
                return Err(Fail::Protocol("the broker rejected the client ID"));
            }
            Some(Packet::ConnAck { .. }) => {
                return Err(Fail::Io(
                    ErrorKind::Network,
                    "the broker is not available (it refused the connection)",
                ));
            }
            Some(_) => return Err(Fail::Protocol("unexpected packet before CONNACK")),
            None => {
                if Instant::now() >= deadline {
                    return Err(Fail::Io(
                        ErrorKind::Timeout,
                        "the broker did not answer the connection",
                    ));
                }
            }
        }
    }
    *connected = true;

    conn.send(&Packet::Subscribe {
        pid: 1,
        filters: vec![(cfg.topic.clone(), 1)],
    })
    .map_err(io_fail)?;
    let keepalive = Duration::from_secs(u64::from(cfg.keepalive_secs));
    let mut subscribed = false;
    loop {
        if ctx.shared.stopped() {
            let _ = conn.send(&Packet::Disconnect);
            return Err(Fail::Stopped);
        }
        let next = match conn.poll() {
            Ok(p) => p,
            Err(f) => {
                if ctx.shared.stopped() {
                    let _ = conn.send(&Packet::Disconnect);
                    return Err(Fail::Stopped);
                }
                return Err(f);
            }
        };
        match next {
            Some(Packet::SubAck { codes, .. }) => {
                if codes.first().is_none_or(|c| *c > 1) {
                    return Err(Fail::Protocol("the broker refused the subscription"));
                }
                if !subscribed {
                    subscribed = true;
                    ctx.shared.update(|s| {
                        s.connects += 1;
                        if s.connects > 1 {
                            s.reconnects += 1;
                        }
                        s.status = Status::Connected;
                        s.last_error = None;
                        s.last_error_kind = None;
                        s.last_success_ms = Some(ctx.clock.now_ms());
                    });
                }
            }
            Some(Packet::Publish {
                topic,
                payload,
                qos,
                retain,
                pid,
                ..
            }) => {
                if qos == 2 {
                    return Err(Fail::Protocol("unexpected QoS 2 delivery"));
                }
                if let (1, Some(pid)) = (qos, pid) {
                    conn.send(&Packet::PubAck { pid }).map_err(io_fail)?;
                }
                let now = ctx.clock.now_ms();
                let outcome = mapper.on_message(&topic, &payload, retain, now);
                lock(&ctx.shared.messages).push(
                    now,
                    &topic,
                    &payload,
                    retain,
                    describe(&outcome, &cfg.id, &cfg.kind),
                );
                ctx.shared.update(|s| {
                    s.messages += 1;
                    match outcome {
                        Outcome::Emit(_) => s.triggers += 1,
                        Outcome::Seeded => s.retained_seeded += 1,
                        Outcome::Dropped(d) => match d {
                            Reason::Topic => s.dropped_topic += 1,
                            Reason::Oversized => s.dropped_oversized += 1,
                            Reason::RateLimited => s.dropped_rate_limited += 1,
                            Reason::Retained => s.dropped_retained += 1,
                            Reason::Unmapped => s.dropped_unmapped += 1,
                            Reason::Repeat => s.dropped_repeat += 1,
                            Reason::Transition => s.dropped_transition += 1,
                            Reason::Debounced => s.dropped_debounced += 1,
                        },
                    }
                });
                if let Outcome::Emit(state) = outcome {
                    (ctx.sink)(Trigger {
                        source_id: cfg.id.clone(),
                        kind: cfg.kind.clone(),
                        state,
                        sender_id: format!("mqtt:{}", cfg.id),
                        at_ms: now,
                    });
                }
            }
            Some(Packet::PingResp) => {}
            Some(_) => return Err(Fail::Protocol("unexpected packet from the broker")),
            None => {}
        }
        let now = Instant::now();
        if now.duration_since(conn.last_rx) > keepalive + keepalive / 2 {
            return Err(Fail::Io(ErrorKind::Timeout, "the broker stopped answering"));
        }
        if now.duration_since(conn.last_tx) >= keepalive / 2 {
            conn.send(&Packet::PingReq).map_err(io_fail)?;
        }
    }
}

/// The log entry's outcome for a message.
fn describe(outcome: &Outcome, sensor: &str, kind: &str) -> MessageOutcome {
    let (code, text): (&'static str, String) = match outcome {
        Outcome::Emit(state) => (
            "matched",
            format!("matched sensor {sensor} as {kind} ({})", state_name(*state)),
        ),
        Outcome::Seeded => (
            "seeded",
            "ignored: retained value recorded as the starting state".to_owned(),
        ),
        Outcome::Dropped(r) => match r {
            Reason::Topic => ("topic_not_mapped", "ignored: topic not mapped".to_owned()),
            Reason::Oversized => ("oversized", "ignored: payload too large".to_owned()),
            Reason::RateLimited => ("rate_limited", "ignored: rate-limited".to_owned()),
            Reason::Retained => (
                "retained",
                "ignored: retained message (setting: ignore)".to_owned(),
            ),
            Reason::Unmapped => (
                "not_a_trigger_value",
                "ignored: payload not a trigger value".to_owned(),
            ),
            Reason::Repeat => ("repeat", "ignored: same state as before".to_owned()),
            Reason::Transition => (
                "transition",
                "ignored: this change is not set to trigger".to_owned(),
            ),
            Reason::Debounced => ("debounced", "ignored: within the debounce time".to_owned()),
        },
    };
    MessageOutcome { code, text }
}

fn state_name(s: TriggerState) -> &'static str {
    match s {
        TriggerState::Start => "start",
        TriggerState::Stop => "stop",
        TriggerState::Pulse => "pulse",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_addresses_are_filtered_or_refused() {
        let a = |ip: [u8; 4]| SocketAddr::from((ip, 1883));
        let mixed = vec![a([203, 0, 113, 5]), a([10, 0, 0, 2])];
        assert_eq!(
            check_addrs(mixed.clone(), false).ok(),
            Some(vec![a([10, 0, 0, 2])])
        );
        assert!(matches!(
            check_addrs(vec![a([203, 0, 113, 5])], false),
            Err(Fail::Refused)
        ));
        assert_eq!(check_addrs(mixed.clone(), true).ok(), Some(mixed));
    }

    #[test]
    fn backoff_grows_caps_and_jitters_within_bounds() {
        for unit in [0, 1, 12345, u64::MAX] {
            let d: Vec<u64> = (0..8).map(|a| backoff_ms(a, 1000, 10_000, unit)).collect();
            for (a, v) in d.iter().enumerate() {
                let full = (1000u64 << a).min(10_000);
                assert!(*v >= full / 2 && *v <= full, "{a}: {v}");
            }
        }
        assert_eq!(backoff_ms(100, 1000, 10_000, 0), 5_000);
    }
}
