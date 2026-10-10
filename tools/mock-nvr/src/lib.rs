//! A fake Reolink-style NVR for tests and development.
//!
//! [`spawn`] starts a server on a TCP port in real time (use port 0 and read
//! the real address from [`MockNvr::addr`]); the command line tool and the C
//! and C# smoke tests use it. [`spawn_in`] runs the same server on a test's
//! tokio runtime, reached through in-memory connections ([`connect`]), so
//! on a paused clock its frame pacing, replay schedules, download rates, and
//! other delays run in simulated time: tests run as fast as the CPU allows
//! and do not depend on real time. It speaks the subset of the protocol in
//! `docs/protocol.md` that a viewer needs:
//!
//! - handshake (nonce) and login with one configured user name and password;
//!   a wrong credential is answered with status 401, and the number of
//!   concurrent logged-in sessions can be capped;
//! - logout (2), which frees the login slot at once;
//! - keepalive (93), version (80), capabilities (199), stream info (146),
//!   per-channel version info (318), and a channel info push (145) after login;
//! - preview start (3) and stop (4): a looping synthetic video fixture is sent
//!   as BCMEDIA records at a fixed frame rate, starting at a keyframe, with the
//!   first 1024 bytes of each frame-start message encrypted;
//! - optionally, audio interleaved with the video like a device's: one ADTS
//!   frame per `05wb` record, placed after every picture captured before it
//!   was sent, with a configurable audio clock skew and arrival jitter
//!   ([`audio`]).
//!
//! - optionally, a recorded stream instead (`NVRREC01`), or several tiers of
//!   one channel recorded on one clock (`NVRREC02`): the records are sent as
//!   recorded, at their recorded times, optionally accelerated or looped;
//!   with several tiers, all previews share one replay clock, so a tier
//!   started later joins at the same capture moment ([`replay`]).
//! - per channel, whether the channel list names `externStream` and how a
//!   preview of the second-lens tiers is answered ([`ExternMode`]): served,
//!   a second lens the list does not reveal, a stream-info record only,
//!   refused, or silence;
//! - optionally, third-party (ONVIF) camera channels ([`ThirdPartyConfig`]):
//!   every preview start is answered with status 400 and the first few
//!   send nothing; a later one streams on the message ID of its 400, with
//!   `:0dc`/`:1dc` video magics, as observed for such a camera behind an NVR
//!   (docs/protocol.md, "Records as the official client reads them").
//! - detection events ([`alarms`]): the alarm-report subscription (192)
//!   and alarm-state pushes (33) whenever a test changes a channel's state
//!   ([`MockNvr::set_alarm`]) or a scripted cycle does
//!   ([`Config::detection_cycle`]); per-channel `aitype` capabilities in
//!   the 199 reply ([`Config::aitypes`], and per lens of a multi-lens camera
//!   [`Config::lens_aitypes`]);
//! - optionally, a fake recording store ([`archive`]): day records (142),
//!   file search (14–16), event search (272–274), cover images (298), and
//!   download by time range (143, stopped by 144), with injectable download
//!   stalls and disconnects.
//!
//! - optionally, doorbell talk-back ([`talk`]): `ipcAudioTalk` 1 in the
//!   199 reply for [`Config::talk_channels`], the talk ability (10), and a
//!   talk (201, audio 202, close 11) whose received records are logged
//!   ([`MockNvr::talk_log`]) so tests can decode and time them.
//!
//! It never touches real hardware and answers every other command with
//! status 400.

pub mod alarms;
pub mod archive;
pub mod audio;
pub mod fixture;
pub mod replay;
pub mod talk;

use archive::{ArchiveConfig, DownloadFault, DownloadLog, FaultKind, FileEntry, MockEvent};
use audio::{AudioConfig, Item, Jitter, Schedule};
use bc_protocol::crypto::{SessionKey, credential_hash, md5_hex_upper_31, xor_obfuscate};
use bc_protocol::framer::{Decoder, RawMessage, encode_message_with_status};
use bc_protocol::header::Class;
use bc_protocol::media::{encode_audio, encode_info, encode_video};
use bc_protocol::stream::StreamTier;
use bc_protocol::xml::{self, XML_DECLARATION};
use fixture::Fixture;
use replay::{ReplayConfig, Step};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::thread;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};

/// Bytes of a frame-start payload that are encrypted.
const ENCRYPT_LEN: usize = 1024;
/// Largest payload of one media message.
const CHUNK: usize = 32 * 1024;
/// A write that cannot complete for this long fails (the client stopped
/// reading), as with a TCP send timeout.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Buffer of each direction of an in-memory connection ([`MockNvr::connect`]),
/// about what a loopback TCP connection buffers.
const PIPE_BUF: usize = 256 * 1024;

/// Server settings.
#[derive(Clone, Debug)]
pub struct Config {
    /// Address to listen on. Defaults to `127.0.0.1:0` (any free port).
    pub bind: SocketAddr,
    /// The one accepted user name.
    pub user: String,
    /// The one accepted password.
    pub password: String,
    /// Number of channels the fake NVR reports and serves.
    pub channels: u8,
    /// Most concurrently logged-in sessions; further logins are refused.
    pub max_logins: usize,
    /// Failed logins (server-wide, reset by a success) after which every
    /// login is refused with `remainTimes` 0.
    pub max_failed_logins: u32,
    /// Frames per second of the video stream.
    pub fps: u32,
    /// Width advertised in the info record and stream info.
    pub width: u32,
    /// Height advertised in the info record and stream info.
    pub height: u32,
    /// The looping video source.
    pub fixture: Arc<Fixture>,
    /// Interleaved audio, if any.
    pub audio: Option<Arc<AudioConfig>>,
    /// If set, every preview serves this recording instead of the fixture
    /// (and `audio`): see [`replay`].
    pub replay: Option<ReplayConfig>,
    /// If set, the device has recordings: see [`archive`]. Without it, the
    /// recording commands are answered with status 400.
    pub archive: Option<ArchiveConfig>,
    /// Each channel's `aitype` capability bits in the 199 reply, by channel
    /// (missing channels report 0: no AI). For example 7 is AI with people
    /// and vehicles, 23 adds dogs and cats (docs/protocol.md).
    pub aitypes: Vec<u32>,
    /// Multi-lens cameras: by channel, each lens's `aitype`, sent as one
    /// `subItem` per lens (with `chnID` 0, 1, ...) inside the channel's
    /// item of the 199 reply, as newer NVR firmware does (docs/protocol.md,
    /// "Which channels detect what"). Missing or empty: a single-lens
    /// camera, no sub-items.
    pub lens_aitypes: Vec<Vec<u32>>,
    /// If set, a scripted detection moves through the channels: each period
    /// one channel reports motion and `people` for the first half of the
    /// period, then returns to idle, and the next channel follows.
    pub detection_cycle: Option<Duration>,
    /// If set, these channels behave like third-party cameras behind an NVR.
    pub third_party: Option<ThirdPartyConfig>,
    /// By channel, whether the channel list (145) names `externStream` and
    /// how the second-lens tiers (`externStream`, `mobileStream`) answer.
    /// Missing channels: [`ExternMode::Listed`], except a multi-lens camera
    /// ([`Config::lens_aitypes`]), which is [`ExternMode::Hidden`] as
    /// observed (docs/protocol.md, "Second lenses").
    pub extern_modes: Vec<ExternMode>,
    /// By channel, the camera's `uid` in the channel list (145); missing or
    /// empty: no `uid` element.
    pub uids: Vec<String>,
    /// Channels with a speaker (`ipcAudioTalk` 1): they answer the talk
    /// ability (10) and accept talk-back ([`talk`]). Others answer 400.
    pub talk_channels: Vec<u8>,
}

/// How a channel's camera treats the second-lens tiers (`externStream` and
/// `mobileStream`), after the cameras described in docs/protocol.md,
/// "Second lenses".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExternMode {
    /// Lists `externStream` and serves it with video, like most single-lens
    /// cameras (a third quality tier of the same lens).
    #[default]
    Listed,
    /// Does not list `externStream` but serves it with video: a second lens
    /// the channel list does not reveal.
    Hidden,
    /// Does not list it and answers a preview of it with one stream-info
    /// record (size 0x0) and no video, like a camera without a second lens.
    InfoOnly,
    /// Does not list it and refuses a preview of it with status 400.
    Refused,
    /// Does not list it and never answers a preview of it.
    Silent,
}

/// Third-party (ONVIF) camera channels: each preview start is answered with
/// status 400 and an empty body. The first `refusals` starts of a channel
/// and tier (counted server-wide) send nothing more; each later one sends
/// its stream `media_delay` after the 400, on the same message ID, with
/// `:0dc`/`:1dc` video magics and `encryptLen` 1024. A stop is answered 200
/// either way.
#[derive(Clone, Debug)]
pub struct ThirdPartyConfig {
    /// The third-party channels.
    pub channels: Vec<u8>,
    /// Starts of each channel and tier answered with 400 and no stream
    /// before one streams; `u32::MAX` never streams.
    pub refusals: u32,
    /// Delay between the 400 and the stream of a start that streams.
    pub media_delay: Duration,
}

impl ThirdPartyConfig {
    /// `channels`, each refusing `refusals` starts, streaming 200 ms after
    /// the 400 of the next.
    pub fn new(channels: Vec<u8>, refusals: u32) -> ThirdPartyConfig {
        ThirdPartyConfig {
            channels,
            refusals,
            media_delay: Duration::from_millis(200),
        }
    }
}

impl Config {
    /// Default settings around `fixture`: user `example-user`, password
    /// `example-password`, 4 channels, 4 logins, 25 fps, 320x240.
    pub fn new(fixture: Fixture) -> Config {
        Config {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            user: "example-user".into(),
            password: "example-password".into(),
            channels: 4,
            max_logins: 4,
            max_failed_logins: 3,
            fps: 25,
            width: 320,
            height: 240,
            fixture: Arc::new(fixture),
            audio: None,
            replay: None,
            archive: None,
            aitypes: Vec::new(),
            lens_aitypes: Vec::new(),
            detection_cycle: None,
            third_party: None,
            extern_modes: Vec::new(),
            uids: Vec::new(),
            talk_channels: Vec::new(),
        }
    }

    /// How `channel` treats its second-lens tiers ([`Config::extern_modes`]).
    pub fn extern_mode(&self, channel: u8) -> ExternMode {
        let c = usize::from(channel);
        match self.extern_modes.get(c) {
            Some(m) => *m,
            None if self.lens_aitypes.get(c).is_some_and(|l| l.len() >= 2) => ExternMode::Hidden,
            None => ExternMode::Listed,
        }
    }
}

struct Shared {
    config: Config,
    /// Set once to stop the server; every task watches it.
    shutdown: watch::Sender<bool>,
    logins: AtomicUsize,
    failed: AtomicU32,
    nonce_counter: AtomicU32,
    faults: Mutex<Vec<DownloadFault>>,
    download_log: Mutex<VecDeque<DownloadLog>>,
    command_counts: Mutex<HashMap<u32, u64>>,
    alarms: alarms::Board,
    next_conn: AtomicU64,
    /// Start of the shared replay clock of an `NVRREC02` replay: the first
    /// preview started on this server.
    replay_epoch: OnceLock<Instant>,
    /// Starts of third-party channels so far, by (channel, handle).
    third_party_starts: Mutex<HashMap<(u8, u32), u32>>,
    /// Every talk so far, oldest first.
    talks: Mutex<Vec<talk::TalkLog>>,
    /// Status for talk opens (201); 200 accepts.
    talk_open_status: AtomicU32,
    /// Status for talk audio replies (202); 200 accepts.
    talk_audio_status: AtomicU32,
    /// The runtime every task of this server runs on.
    runtime: Handle,
    /// Connection tasks, for a clean shutdown.
    conns: Mutex<Vec<JoinHandle<()>>>,
}

impl Shared {
    fn is_shutdown(&self) -> bool {
        *self.shutdown.borrow()
    }

    /// Resolves once the server is stopping.
    async fn stopping(&self) {
        let mut rx = self.shutdown.subscribe();
        let _ = rx.wait_for(|s| *s).await;
    }

    /// Serves one connection on a task of its own.
    fn serve_conn<S>(self: &Arc<Self>, stream: S)
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let s = Arc::clone(self);
        let task = self.runtime.spawn(async move {
            let _ = serve(stream, s).await;
        });
        let mut conns = lock(&self.conns);
        conns.retain(|h| !h.is_finished());
        conns.push(task);
    }
}

/// Most downloads remembered for [`MockNvr::download_log`].
const DOWNLOAD_LOG_LEN: usize = 10_000;

/// A running mock NVR. Dropping it (or calling [`MockNvr::shutdown`]) stops
/// the server and closes every connection.
///
/// Two kinds: [`spawn`] serves TCP in real time on threads of its own (the
/// command line tool, and tests that need a real socket); [`spawn_in`]
/// runs on the caller's tokio runtime and its clock, reached only through
/// in-memory connections ([`MockNvr::connect`], [`connect`]), so tests on
/// a paused clock run the device in simulated time, deterministically and
/// without waiting.
pub struct MockNvr {
    addr: SocketAddr,
    shared: Arc<Shared>,
    /// The thread running the server's own runtime ([`spawn`] only).
    thread: Option<thread::JoinHandle<()>>,
}

/// Starts a TCP server in real time on threads of its own and returns once
/// it is listening.
pub fn spawn(config: Config) -> io::Result<MockNvr> {
    let listener = std::net::TcpListener::bind(config.bind)?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    // One worker per core (tokio's default): streams spread over the cores
    // much as the thread-per-stream server they replaced did.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .thread_name("mock-nvr")
        .enable_all()
        .build()?;
    let shared = new_shared(config, runtime.handle().clone());
    register(addr, &shared);
    let listener = {
        let _guard = runtime.enter();
        tokio::net::TcpListener::from_std(listener)?
    };
    let s = Arc::clone(&shared);
    let thread = thread::Builder::new()
        .name("mock-nvr".into())
        .spawn(move || {
            runtime.block_on(async move {
                start_tasks(&s);
                accept_loop(listener, &s).await;
                finish(&s).await;
            });
            // Tasks still running (none should be) stop here.
            runtime.shutdown_timeout(Duration::from_secs(1));
        })?;
    Ok(MockNvr {
        addr,
        shared,
        thread: Some(thread),
    })
}

/// Starts a server on `runtime` (and its clock) that is reached only
/// through in-memory connections: [`MockNvr::connect`], or [`connect`] with
/// its [`MockNvr::addr`]. Nothing listens on that address; its port is
/// [`Config::bind`]'s if not 0 (so a test can restart a device at the same
/// address), else a made-up one unique in this process.
///
/// On a paused clock (`tokio::time::pause`, or a runtime built with
/// `start_paused`), every delay the device keeps (frame pacing, replay
/// schedules, download rates, third-party delays, detection cycles) runs in
/// simulated time.
pub fn spawn_in(config: Config, runtime: &Handle) -> MockNvr {
    let addr = if config.bind.port() == 0 {
        static NEXT_PORT: AtomicU16 = AtomicU16::new(20_000);
        SocketAddr::from((config.bind.ip(), NEXT_PORT.fetch_add(1, Ordering::Relaxed)))
    } else {
        config.bind
    };
    let shared = new_shared(config, runtime.clone());
    register(addr, &shared);
    start_tasks(&shared);
    MockNvr {
        addr,
        shared,
        thread: None,
    }
}

fn new_shared(config: Config, runtime: Handle) -> Arc<Shared> {
    let (alarm_tx, alarm_rx) = mpsc::unbounded_channel();
    let alarms = alarms::Board::new(config.channels, alarm_tx);
    let shared = Arc::new(Shared {
        config,
        shutdown: watch::Sender::new(false),
        logins: AtomicUsize::new(0),
        failed: AtomicU32::new(0),
        nonce_counter: AtomicU32::new(0),
        faults: Mutex::new(Vec::new()),
        download_log: Mutex::new(VecDeque::new()),
        command_counts: Mutex::new(HashMap::new()),
        alarms,
        next_conn: AtomicU64::new(1),
        replay_epoch: OnceLock::new(),
        third_party_starts: Mutex::new(HashMap::new()),
        talks: Mutex::new(Vec::new()),
        talk_open_status: AtomicU32::new(200),
        talk_audio_status: AtomicU32::new(200),
        runtime,
        conns: Mutex::new(Vec::new()),
    });
    let s = Arc::clone(&shared);
    shared.runtime.spawn(async move {
        tokio::select! {
            () = s.stopping() => {}
            () = alarms::push_loop(&s.alarms, alarm_rx) => {}
        }
    });
    shared
}

/// The server's own background tasks.
fn start_tasks(shared: &Arc<Shared>) {
    if let Some(period) = shared.config.detection_cycle {
        let s = Arc::clone(shared);
        shared.runtime.spawn(async move {
            tokio::select! {
                () = s.stopping() => {}
                () = detection_cycle(&s, period) => {}
            }
        });
    }
}

/// Waits for every connection to close after a shutdown.
async fn finish(shared: &Shared) {
    let conns: Vec<JoinHandle<()>> = lock(&shared.conns).drain(..).collect();
    for c in conns {
        let _ = c.await;
    }
}

/// Servers reachable through [`connect`], by address.
fn registry() -> &'static Mutex<HashMap<SocketAddr, Weak<Shared>>> {
    static SERVERS: OnceLock<Mutex<HashMap<SocketAddr, Weak<Shared>>>> = OnceLock::new();
    SERVERS.get_or_init(Mutex::default)
}

fn register(addr: SocketAddr, shared: &Arc<Shared>) {
    let mut r = lock(registry());
    r.retain(|_, s| s.strong_count() > 0);
    r.insert(addr, Arc::downgrade(shared));
}

/// Opens an in-memory connection to the running server at `addr` (from
/// [`MockNvr::addr`]); fails with `ConnectionRefused` if none runs there.
/// What a test's connector calls where a real client would open TCP.
pub fn connect(addr: SocketAddr) -> io::Result<DuplexStream> {
    let shared = lock(registry()).get(&addr).and_then(Weak::upgrade);
    match shared {
        Some(s) => open_pipe(&s),
        None => Err(io::ErrorKind::ConnectionRefused.into()),
    }
}

fn open_pipe(shared: &Arc<Shared>) -> io::Result<DuplexStream> {
    if shared.is_shutdown() {
        return Err(io::ErrorKind::ConnectionRefused.into());
    }
    let (client, server) = tokio::io::duplex(PIPE_BUF);
    shared.serve_conn(server);
    Ok(client)
}

impl MockNvr {
    /// The address the server listens on (with the real port), or for a
    /// server started with [`spawn_in`], the address [`connect`] knows it by.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Opens an in-memory connection to this server, served like a TCP one.
    ///
    /// # Errors
    /// `ConnectionRefused` once the server is shutting down.
    pub fn connect(&self) -> io::Result<DuplexStream> {
        open_pipe(&self.shared)
    }

    /// Number of sessions that are currently logged in.
    pub fn active_logins(&self) -> usize {
        self.shared.logins.load(Ordering::SeqCst)
    }

    /// Queues a fault for the next download that matches it (each fault
    /// applies once, in the order queued).
    pub fn inject_download_fault(&self, fault: DownloadFault) {
        lock(&self.shared.faults).push(fault);
    }

    /// Downloads (143) the server has started, oldest first (the most recent
    /// 10,000).
    pub fn download_log(&self) -> Vec<DownloadLog> {
        lock(&self.shared.download_log).iter().cloned().collect()
    }

    /// How many messages with this command number logged-in clients sent.
    pub fn command_count(&self, command: u32) -> u64 {
        lock(&self.shared.command_counts)
            .get(&command)
            .copied()
            .unwrap_or(0)
    }

    /// Sets one channel's alarm state (`status` such as `MD`, `ai_type`
    /// such as `people`; empty strings mean `none`) and, if it changed,
    /// pushes a snapshot of every channel to each subscribed session.
    /// Returns false for a channel the mock does not have.
    pub fn set_alarm(&self, channel: u8, status: &str, ai_type: &str) -> bool {
        self.shared.alarms.set(
            channel,
            alarms::SlotState {
                status: status.to_owned(),
                ai_type: ai_type.to_owned(),
            },
        )
    }

    /// Every talk so far (201 accepted), oldest first, with the audio
    /// records each received.
    pub fn talk_log(&self) -> Vec<talk::TalkLog> {
        lock(&self.shared.talks).clone()
    }

    /// Sets the status later talk opens (201) are answered with (200, the
    /// default, accepts; anything else refuses).
    pub fn set_talk_open_status(&self, status: u16) {
        self.shared
            .talk_open_status
            .store(u32::from(status), Ordering::SeqCst);
    }

    /// Sets the status later talk audio messages (202) are answered with
    /// (200 by default).
    pub fn set_talk_audio_status(&self, status: u16) {
        self.shared
            .talk_audio_status
            .store(u32::from(status), Ordering::SeqCst);
    }

    /// Sessions subscribed to alarm pushes (command 192) right now.
    pub fn alarm_subscribers(&self) -> usize {
        self.shared.alarms.subscribers()
    }

    /// Stops the server. A [`spawn`]ed server is waited for: every
    /// connection is closed when this returns. A server on the caller's
    /// runtime ([`spawn_in`]) closes its connections as that runtime runs
    /// (this may be called on it, so it does not wait); new connections
    /// are refused at once.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.shared.shutdown.send_replace(true);
        let mut r = lock(registry());
        if r.get(&self.addr)
            .is_some_and(|s| s.as_ptr() == Arc::as_ptr(&self.shared))
        {
            r.remove(&self.addr);
        }
        drop(r);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

impl Drop for MockNvr {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The scripted detection of [`Config::detection_cycle`].
async fn detection_cycle(shared: &Shared, period: Duration) {
    let channels = shared.config.channels.max(1);
    let half = period / 2;
    let mut ch = 0u8;
    loop {
        shared.alarms.set(
            ch,
            alarms::SlotState {
                status: "MD".into(),
                ai_type: "people".into(),
            },
        );
        tokio::time::sleep(half).await;
        shared.alarms.set(ch, alarms::SlotState::default());
        tokio::time::sleep(period - half).await;
        ch = (ch + 1) % channels;
    }
}

async fn accept_loop(listener: tokio::net::TcpListener, shared: &Arc<Shared>) {
    loop {
        let accepted = tokio::select! {
            () = shared.stopping() => return,
            a = listener.accept() => a,
        };
        match accepted {
            Ok((stream, _)) => {
                let _ = stream.set_nodelay(true);
                shared.serve_conn(stream);
            }
            // A failed accept (out of descriptors, an aborted handshake):
            // try again shortly, as the thread-per-connection server did.
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}

/// The sending half of one connection, shared by the connection's task and
/// its stream tasks. Each message is written whole under the lock.
pub(crate) struct ConnWriter {
    out: tokio::sync::Mutex<Option<Pin<Box<dyn AsyncWrite + Send>>>>,
    /// Set when the server ends the connection on its own (a download
    /// fault); the connection's task then closes.
    closed: watch::Sender<bool>,
}

pub(crate) type Writer = Arc<ConnWriter>;

/// The locked sending half: `None` once shut down.
pub(crate) type Out<'a> = tokio::sync::MutexGuard<'a, Option<Pin<Box<dyn AsyncWrite + Send>>>>;

impl ConnWriter {
    pub(crate) async fn lock(&self) -> Out<'_> {
        self.out.lock().await
    }

    pub(crate) async fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        let mut out = self.lock().await;
        write_locked(&mut out, bytes).await
    }

    /// Ends the connection: no more writes, and the connection's task
    /// stops reading.
    async fn shutdown(&self) {
        let mut out = self.lock().await;
        if let Some(mut w) = out.take() {
            let _ = timeout(WRITE_TIMEOUT, w.shutdown()).await;
        }
        self.closed.send_replace(true);
    }
}

/// Writes one message through a locked writer.
pub(crate) async fn write_locked(out: &mut Out<'_>, bytes: &[u8]) -> io::Result<()> {
    let w = out.as_mut().ok_or(io::ErrorKind::NotConnected)?;
    match timeout(WRITE_TIMEOUT, w.write_all(bytes)).await {
        Ok(r) => r,
        Err(_) => Err(io::ErrorKind::TimedOut.into()),
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(PartialEq)]
enum State {
    Fresh,
    NonceSent,
    LoggedIn,
}

struct Stream {
    stop: Arc<AtomicBool>,
    /// `None` for a third-party start that sends nothing.
    task: Option<JoinHandle<()>>,
}

impl Stream {
    /// Ends the stream's task (its `stop` is already set) and waits for it.
    async fn end(self) {
        if let Some(t) = self.task {
            t.abort();
            let _ = t.await;
        }
    }

    fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

struct Conn {
    /// Identifies this connection (for its alarm subscription).
    id: u64,
    shared: Arc<Shared>,
    writer: Writer,
    state: State,
    nonce: String,
    key: Option<SessionKey>,
    holds_login: bool,
    streams: HashMap<u32, Stream>,
    /// Running downloads by message ID.
    downloads: HashMap<u32, Stream>,
    /// Open file searches: handle to (files, next index).
    file_searches: HashMap<u32, (Vec<FileEntry>, usize)>,
    /// Open event searches: handle to (channel, events, next index).
    event_searches: HashMap<u32, (u8, Vec<MockEvent>, usize)>,
    next_event_handle: u32,
    /// Open talks: channel to index in the shared talk log.
    open_talks: HashMap<u8, usize>,
}

async fn serve<S>(stream: S, shared: Arc<Shared>) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut reader, writer) = tokio::io::split(stream);
    let mut conn = Conn {
        id: shared.next_conn.fetch_add(1, Ordering::SeqCst),
        shared: Arc::clone(&shared),
        writer: Arc::new(ConnWriter {
            out: tokio::sync::Mutex::new(Some(Box::pin(writer))),
            closed: watch::Sender::new(false),
        }),
        state: State::Fresh,
        nonce: String::new(),
        key: None,
        holds_login: false,
        streams: HashMap::new(),
        downloads: HashMap::new(),
        file_searches: HashMap::new(),
        event_searches: HashMap::new(),
        next_event_handle: 1,
        open_talks: HashMap::new(),
    };
    let result = conn.run(&mut reader).await;
    conn.close().await;
    result
}

impl Conn {
    async fn run(&mut self, reader: &mut (impl AsyncRead + Unpin)) -> io::Result<()> {
        let mut dec = Decoder::new();
        let mut buf = vec![0u8; 8192];
        let shared = Arc::clone(&self.shared);
        let mut closed = self.writer.closed.subscribe();
        loop {
            loop {
                match dec.next_message() {
                    Ok(Some(m)) => self.handle(&m).await?,
                    Ok(None) => break,
                    Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e)),
                }
                if *closed.borrow() {
                    return Ok(());
                }
            }
            tokio::select! {
                () = shared.stopping() => return Ok(()),
                _ = closed.wait_for(|c| *c) => return Ok(()),
                r = reader.read(&mut buf) => match r {
                    Ok(0) => return Ok(()),
                    Ok(n) => dec.push(&buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                },
            }
        }
    }

    async fn close(&mut self) {
        self.shared.alarms.unsubscribe(self.id);
        if !self.open_talks.is_empty() {
            let mut talks = lock(&self.shared.talks);
            for (_, i) in self.open_talks.drain() {
                if let Some(t) = talks.get_mut(i) {
                    t.end = talk::TalkEnd::Disconnected;
                    t.ended = Some(Instant::now());
                }
            }
        }
        let streams: Vec<Stream> = self
            .streams
            .drain()
            .chain(self.downloads.drain())
            .map(|(_, s)| s)
            .collect();
        for s in streams {
            s.stop.store(true, Ordering::SeqCst);
            s.end().await;
        }
        if self.holds_login {
            self.holds_login = false;
            self.shared.logins.fetch_sub(1, Ordering::SeqCst);
        }
        self.writer.shutdown().await;
    }

    async fn send_raw(&self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes).await
    }

    async fn reply(
        &self,
        cmd: u32,
        id: u32,
        status: u16,
        class: Class,
        body: &[u8],
    ) -> io::Result<()> {
        let bytes = encode_message_with_status(cmd, id, status, class, &[], body)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        self.send_raw(&bytes).await
    }

    /// Replies after login: XML encrypted with the session key.
    async fn reply_xml(&self, cmd: u32, id: u32, status: u16, inner: &str) -> io::Result<()> {
        let mut body = if inner.is_empty() {
            Vec::new()
        } else {
            format!("{XML_DECLARATION}\n<body>\n{inner}</body>\n").into_bytes()
        };
        if let Some(k) = &self.key {
            k.encrypt(&mut body);
        }
        self.reply(cmd, id, status, Class::Device, &body).await
    }

    async fn handle(&mut self, m: &RawMessage) -> io::Result<()> {
        let h = m.header;
        match (&self.state, h.class, h.command) {
            (State::Fresh | State::NonceSent, Class::ClientHandshake, 1) => {
                let n = self.shared.nonce_counter.fetch_add(1, Ordering::SeqCst);
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos());
                self.nonce = format!("mock-{}", md5_hex_upper_31(&format!("{n}-{nanos}")));
                let mut body = format!(
                    "{XML_DECLARATION}\n<body>\n<Encryption version=\"1.1\">\n<type>md5</type>\n\
                     <nonce>{}</nonce>\n</Encryption>\n</body>\n",
                    self.nonce
                )
                .into_bytes();
                xor_obfuscate(h.message_id, &mut body);
                self.reply(1, h.message_id, 200, Class::DeviceHandshake, &body)
                    .await?;
                self.state = State::NonceSent;
                Ok(())
            }
            (State::NonceSent, Class::Client, 1) => self.login(m).await,
            (State::LoggedIn, Class::Client, _) => self.command(m).await,
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected message",
            )),
        }
    }

    async fn reject(&mut self, id: u32, status: u16, remain: Option<u32>) -> io::Result<()> {
        let mut body = match remain {
            Some(r) => format!(
                "{XML_DECLARATION}\n<body>\n<LoginErrInfo>\n<remainTimes>{r}</remainTimes>\n</LoginErrInfo>\n</body>\n"
            )
            .into_bytes(),
            None => Vec::new(),
        };
        xor_obfuscate(id, &mut body);
        self.state = State::Fresh;
        self.reply(1, id, status, Class::Device, &body).await
    }

    async fn login(&mut self, m: &RawMessage) -> io::Result<()> {
        let id = m.header.message_id;
        let cfg = &self.shared.config;
        let mut body = m.body.clone();
        xor_obfuscate(id, &mut body);
        let text = String::from_utf8_lossy(&body).into_owned();
        let user = xml::find_text(&text, "userName").ok().flatten();
        let pass = xml::find_text(&text, "password").ok().flatten();
        let failed = self.shared.failed.load(Ordering::SeqCst);
        if failed >= cfg.max_failed_logins {
            return self.reject(id, 401, Some(0)).await;
        }
        let ok = user.as_deref() == Some(credential_hash(&cfg.user, &self.nonce).as_str())
            && pass.as_deref() == Some(credential_hash(&cfg.password, &self.nonce).as_str());
        if !ok {
            let n = self.shared.failed.fetch_add(1, Ordering::SeqCst) + 1;
            return self
                .reject(id, 401, Some(cfg.max_failed_logins.saturating_sub(n)))
                .await;
        }
        // Take a login slot, or give it back if the cap is exceeded.
        if self.shared.logins.fetch_add(1, Ordering::SeqCst) >= cfg.max_logins {
            self.shared.logins.fetch_sub(1, Ordering::SeqCst);
            return self.reject(id, 400, None).await;
        }
        self.holds_login = true;
        self.shared.failed.store(0, Ordering::SeqCst);
        self.key = Some(SessionKey::derive(&self.nonce, &cfg.password));
        self.state = State::LoggedIn;
        self.reply(1, id, 200, Class::Device, &[]).await?;
        // Devices push the channel list unprompted.
        self.reply_xml(145, 0, 200, &self.channel_info_list()).await
    }

    /// The request's XML text (extension if present, else body), decrypted.
    fn request_text(&self, m: &RawMessage) -> String {
        let part = if m.extension().is_empty() {
            m.payload()
        } else {
            m.extension()
        };
        let mut v = part.to_vec();
        if let Some(k) = &self.key {
            k.decrypt(&mut v);
        }
        String::from_utf8_lossy(&v).into_owned()
    }

    /// The payload's XML text, decrypted (bodies sent after an extension).
    fn payload_text(&self, m: &RawMessage) -> String {
        let mut v = m.payload().to_vec();
        if let Some(k) = &self.key {
            k.decrypt(&mut v);
        }
        String::from_utf8_lossy(&v).into_owned()
    }

    async fn command(&mut self, m: &RawMessage) -> io::Result<()> {
        *lock(&self.shared.command_counts)
            .entry(m.header.command)
            .or_insert(0) += 1;
        if matches!(
            m.header.command,
            14 | 15 | 16 | 142 | 143 | 144 | 272 | 273 | 274 | 298
        ) {
            return self.archive_command(m).await;
        }
        let cfg = &self.shared.config;
        let id = m.header.message_id;
        match m.header.command {
            93 => {
                self.reply_xml(93, id, 200, "<LinkType>\n<type>LAN</type>\n</LinkType>\n")
                    .await
            }
            80 => {
                self.reply_xml(
                    80,
                    id,
                    200,
                    "<VersionInfo version=\"1.1\">\n<name>Mock NVR</name>\n<type>MOCK-NVR</type>\n\
                 <firmwareVersion>mock-1.0</firmwareVersion>\n</VersionInfo>\n",
                )
                .await
            }
            199 => self.reply_xml(199, id, 200, &self.support()).await,
            // Alarm report: an empty reply, then state pushes (33).
            192 => {
                self.reply_xml(192, id, 200, "").await?;
                let key = self.key.clone().expect("logged in");
                self.shared
                    .alarms
                    .subscribe(self.id, Arc::clone(&self.writer), key, id)
                    .await;
                Ok(())
            }
            145 => {
                self.reply_xml(145, id, 200, &self.channel_info_list())
                    .await
            }
            // Real devices answer 146 with status 300 and a full body.
            146 => self.reply_xml(146, id, 300, &self.stream_info_list()).await,
            318 => {
                let text = self.request_text(m);
                match xml::find_text(&text, "channelId")
                    .ok()
                    .flatten()
                    .and_then(|s| s.parse::<u8>().ok())
                    .filter(|c| *c < cfg.channels)
                {
                    Some(c) => {
                        self.reply_xml(
                            318,
                            id,
                            200,
                            &format!(
                                "<VersionInfo version=\"1.1\">\n<channelId>{c}</channelId>\n\
                             <name>Channel {}</name>\n<type>MOCK-CAM</type>\n</VersionInfo>\n",
                                u32::from(c) + 1
                            ),
                        )
                        .await
                    }
                    None => self.reply_xml(318, id, 400, "").await,
                }
            }
            // Logout: answer, then free the login slot and end the session.
            // (A real client closes TCP after the reply; ending it here too is
            // the mock's choice.)
            2 => {
                self.reply_xml(2, id, 200, "").await?;
                self.close().await;
                Ok(())
            }
            3 => self.start_preview(m).await,
            4 => self.stop_preview(m).await,
            10 | 11 | 201 | 202 => self.talk_command(m).await,
            other => self.reply_xml(other, id, 400, "").await,
        }
    }

    /// The channel named in the request's extension.
    fn ext_channel(&self, m: &RawMessage) -> Option<u8> {
        xml::find_text(&self.request_text(m), "channelId")
            .ok()
            .flatten()
            .and_then(|s| s.parse::<u8>().ok())
    }

    /// Talk-back (10, 201, 202, 11): see [`talk`].
    async fn talk_command(&mut self, m: &RawMessage) -> io::Result<()> {
        let id = m.header.message_id;
        let cmd = m.header.command;
        let channel = self
            .ext_channel(m)
            .filter(|c| self.shared.config.talk_channels.contains(c));
        match (cmd, channel) {
            (10, Some(_)) => self.reply_xml(10, id, 200, talk::ABILITY).await,
            (201, Some(ch)) => {
                let body = self.payload_text(m);
                let ok = bc_protocol::talk::parse_open_body(&body)
                    .is_ok_and(|(c, _, audio)| c == ch && audio.is_supported());
                let status = self.shared.talk_open_status.load(Ordering::SeqCst) as u16;
                if !ok || self.open_talks.contains_key(&ch) {
                    return self.reply_xml(201, id, 400, "").await;
                }
                if status != 200 {
                    return self.reply_xml(201, id, status, "").await;
                }
                let index = {
                    let mut talks = lock(&self.shared.talks);
                    talks.push(talk::TalkLog {
                        channel: ch,
                        connection: self.id,
                        opened: Instant::now(),
                        ended: None,
                        end: talk::TalkEnd::Open,
                        records: Vec::new(),
                    });
                    talks.len() - 1
                };
                self.open_talks.insert(ch, index);
                self.reply_xml(201, id, 200, "").await
            }
            (202, Some(ch)) => {
                let at = Instant::now();
                let Some(&i) = self.open_talks.get(&ch) else {
                    return self.reply_xml(202, 0, 400, "").await;
                };
                let Some(recs) = talk::records(m.payload(), at) else {
                    return self.reply_xml(202, 0, 400, "").await;
                };
                if let Some(t) = lock(&self.shared.talks).get_mut(i) {
                    t.records.extend(recs);
                }
                let status = self.shared.talk_audio_status.load(Ordering::SeqCst) as u16;
                self.reply_xml(202, 0, status, "").await
            }
            (11, Some(ch)) => match self.open_talks.remove(&ch) {
                Some(i) => {
                    if let Some(t) = lock(&self.shared.talks).get_mut(i) {
                        t.end = talk::TalkEnd::Closed;
                        t.ended = Some(Instant::now());
                    }
                    self.reply_xml(11, id, 200, "").await
                }
                None => self.reply_xml(11, id, 400, "").await,
            },
            (202, None) => self.reply_xml(202, 0, 400, "").await,
            _ => self.reply_xml(cmd, id, 400, "").await,
        }
    }

    /// Capabilities (199): the channel count and one `item` per channel
    /// with its `aitype`, and one `subItem` per lens of a multi-lens camera.
    fn support(&self) -> String {
        let cfg = &self.shared.config;
        let mut s = format!(
            "<Support version=\"1.1\">\n<channelNum>{}</channelNum>\n",
            cfg.channels
        );
        for c in 0..cfg.channels {
            let ai = cfg.aitypes.get(usize::from(c)).copied().unwrap_or(0);
            let talk = u8::from(cfg.talk_channels.contains(&c));
            s.push_str(&format!(
                "<item>\n<chnID>{c}</chnID>\n<aitype>{ai}</aitype>\n<aiAnimalType>0</aiAnimalType>\n\
                 <ipcAudioTalk>{talk}</ipcAudioTalk>\n"
            ));
            let lenses = cfg.lens_aitypes.get(usize::from(c));
            for (lens, ai) in lenses.into_iter().flatten().enumerate() {
                s.push_str(&format!(
                    "<subItem>\n<chnID>{lens}</chnID>\n<aitype>{ai}</aitype>\n</subItem>\n"
                ));
            }
            s.push_str("</item>\n");
        }
        s.push_str("</Support>\n");
        s
    }

    fn channel_info_list(&self) -> String {
        let cfg = &self.shared.config;
        let mut s = String::from("<ChannelInfoList version=\"1.1\">\n");
        for c in 0..cfg.channels {
            let uid = match cfg.uids.get(usize::from(c)) {
                Some(u) if !u.is_empty() => format!("<uid>{u}</uid>\n"),
                _ => String::new(),
            };
            let tiers = if cfg.extern_mode(c) == ExternMode::Listed {
                "mainStream,subStream,externStream"
            } else {
                "mainStream,subStream"
            };
            s.push_str(&format!(
                "<ChannelInfo>\n<channelId>{c}</channelId>\n<state>connect</state>\n{uid}\
                 <streamSupport>{tiers}</streamSupport>\n</ChannelInfo>\n"
            ));
        }
        s.push_str("</ChannelInfoList>\n");
        s
    }

    fn stream_info_list(&self) -> String {
        let cfg = &self.shared.config;
        // 0 = H.264 and 1 = H.265 is unconfirmed (docs/protocol.md); clients
        // must take the codec from the media records.
        let enc_type = |codec| match codec {
            bc_protocol::media::Codec::H264 => 0,
            bc_protocol::media::Codec::H265 => 1,
        };
        // Every channel shares one configuration, so one entry covers all of
        // them through the channel bitmask.
        let bits: u64 = (0..cfg.channels.min(64)).fold(0, |b, c| b | 1 << c);
        let mut s = String::from("<StreamInfoList version=\"1.1\">\n<StreamInfo>\n");
        s.push_str(&format!("<channelBits>{bits}</channelBits>\n"));
        for t in [StreamTier::Main, StreamTier::Sub] {
            // A replay of several tiers describes each recorded tier itself.
            let replayed = cfg
                .replay
                .as_ref()
                .map(|r| &r.replay)
                .filter(|r| r.tiers().contains(&t));
            let (width, height, fps) = replayed
                .and_then(|r| r.info_of(Some(t)))
                .map_or((cfg.width, cfg.height, cfg.fps), |(w, h, f)| {
                    (w, h, u32::from(f))
                });
            let codec = replayed
                .and_then(|r| r.codec_of(Some(t)))
                .or_else(|| cfg.replay.as_ref().and_then(|r| r.replay.codec()))
                .unwrap_or_else(|| cfg.fixture.codec());
            s.push_str(&format!(
                "<encodeTable>\n<type>{}</type>\n<resolution>\n<width>{width}</width>\n<height>{height}</height>\n\
                 </resolution>\n<videoEncType>{}</videoEncType>\n\
                 <defaultFramerate>{fps}</defaultFramerate>\n</encodeTable>\n",
                t.name(),
                enc_type(codec),
            ));
        }
        s.push_str("</StreamInfo>\n");
        s.push_str("</StreamInfoList>\n");
        s
    }

    fn preview_target(&self, m: &RawMessage) -> Option<(u32, u8, StreamTier)> {
        let text = self.request_text(m);
        let handle: u32 = xml::find_text(&text, "handle").ok()??.parse().ok()?;
        let (tier, ch) = StreamTier::from_handle(handle)?;
        let tag_ch: u8 = xml::find_text(&text, "channelId").ok()??.parse().ok()?;
        (ch == tag_ch && ch < self.shared.config.channels).then_some((handle, ch, tier))
    }

    async fn start_preview(&mut self, m: &RawMessage) -> io::Result<()> {
        let id = m.header.message_id;
        let Some((handle, channel, tier)) = self.preview_target(m) else {
            return self.reply_xml(3, id, 400, "").await;
        };
        if self.streams.contains_key(&handle) {
            return self.reply_xml(3, id, 400, "").await;
        }
        let stop = Arc::new(AtomicBool::new(false));
        if matches!(tier, StreamTier::Extern | StreamTier::Mobile) {
            match self.shared.config.extern_mode(channel) {
                ExternMode::Listed | ExternMode::Hidden => {}
                ExternMode::Refused => return self.reply_xml(3, id, 400, "").await,
                ExternMode::Silent => {
                    self.streams.insert(handle, Stream { stop, task: None });
                    return Ok(());
                }
                ExternMode::InfoOnly => {
                    let key = self.key.clone().expect("logged in");
                    let mut out = Vec::new();
                    media_message(&key, id, &encode_info(0, 0, 25), &mut out)?;
                    self.writer.write_all(&out).await?;
                    self.streams.insert(handle, Stream { stop, task: None });
                    return Ok(());
                }
            }
        }
        let mut delay = Duration::ZERO;
        if let Some(tp) = &self.shared.config.third_party
            && tp.channels.contains(&channel)
        {
            let n = {
                let mut starts = lock(&self.shared.third_party_starts);
                let n = starts.entry((channel, handle)).or_insert(0);
                *n = n.saturating_add(1);
                *n
            };
            // Every start of a third-party channel is answered 400; the
            // stream, if any, follows on the same message ID.
            self.reply_xml(3, id, 400, "").await?;
            if n <= tp.refusals {
                self.streams.insert(handle, Stream { stop, task: None });
                return Ok(());
            }
            delay = tp.media_delay;
        }
        let writer = Arc::clone(&self.writer);
        let key = self.key.clone().expect("logged in");
        let (cfg_shared, stop2) = (Arc::clone(&self.shared), Arc::clone(&stop));
        // Like a real device, send no separate reply: the first media message
        // (status 200) is the answer.
        let task = self.shared.runtime.spawn(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let _ = stream_media(&cfg_shared, &writer, &key, id, channel, tier, &stop2).await;
        });
        self.streams.insert(
            handle,
            Stream {
                stop,
                task: Some(task),
            },
        );
        Ok(())
    }

    async fn stop_preview(&mut self, m: &RawMessage) -> io::Result<()> {
        let id = m.header.message_id;
        let Some((handle, _, _)) = self.preview_target(m) else {
            return self.reply_xml(4, id, 400, "").await;
        };
        let Some(s) = self.streams.remove(&handle) else {
            return self.reply_xml(4, id, 400, "").await;
        };
        // Set the flag while holding the write lock: the stream task checks
        // it under the same lock, so no media follows the stop reply.
        let written = {
            let mut w = self.writer.lock().await;
            s.stop.store(true, Ordering::SeqCst);
            let bytes = encode_message_with_status(4, id, 200, Class::Device, &[], &[])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            write_locked(&mut w, &bytes).await
        };
        s.end().await;
        written
    }
}

fn media_message(key: &SessionKey, id: u32, record: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
    let enc_err = |e| io::Error::new(io::ErrorKind::InvalidInput, e);
    for (n, chunk) in record.chunks(CHUNK).enumerate() {
        let mut payload = chunk.to_vec();
        let mut ext = Vec::new();
        if n == 0 {
            let enc = payload.len().min(ENCRYPT_LEN);
            key.encrypt(&mut payload[..enc]);
            ext = format!(
                "{XML_DECLARATION}\n<Extension version=\"1.1\">\n<binaryData>1</binaryData>\n\
                 <encryptLen>{ENCRYPT_LEN}</encryptLen>\n</Extension>\n"
            )
            .into_bytes();
            key.encrypt(&mut ext);
        }
        out.extend(
            encode_message_with_status(3, id, 200, Class::Device, &ext, &payload)
                .map_err(enc_err)?,
        );
    }
    Ok(())
}

/// Sends the info record and then the looping fixtures (video, and audio if
/// configured) on the schedule a device would use, until stopped or the
/// connection fails.
async fn stream_media(
    shared: &Shared,
    writer: &Writer,
    key: &SessionKey,
    id: u32,
    channel: u8,
    tier: StreamTier,
    stop: &AtomicBool,
) -> io::Result<()> {
    let cfg = &shared.config;
    if let Some(r) = &cfg.replay {
        return stream_replay(shared, writer, key, id, tier, stop, r).await;
    }
    // A third-party camera's video magics start with `:` (`:0dc`, `:1dc`).
    let first = if cfg
        .third_party
        .as_ref()
        .is_some_and(|t| t.channels.contains(&channel))
    {
        10
    } else {
        0
    };
    let fix = &cfg.fixture;
    let fps = cfg.fps.max(1);
    let audio = cfg.audio.as_deref();
    let schedule = Schedule::new(1_000_000 / u64::from(fps), audio);
    let mut jitter = Jitter::new(
        audio.map_or(1, |a| a.seed),
        audio.map_or(0, |a| a.jitter_us),
    );
    let mut index = fix.first_keyframe();
    let mut out = Vec::new();
    media_message(
        key,
        id,
        &encode_info(cfg.width, cfg.height, fps.min(255) as u8),
        &mut out,
    )?;
    let start = Instant::now();
    let mut due = start;
    for (item, send_us) in schedule {
        {
            let mut w = writer.lock().await;
            if stop.load(Ordering::SeqCst) || shared.is_shutdown() {
                return Ok(());
            }
            write_locked(&mut w, &out).await?;
        }
        out.clear();
        let record = match item {
            Item::Video { pts_us, .. } => {
                let frame = &fix.frames()[index];
                index = fix.next_index(index);
                encode_video(first, frame.keyframe, fix.codec(), pts_us, &[], &frame.data)
            }
            Item::Audio { k } => {
                let a = audio.expect("audio items only with audio");
                let frames = a.fixture.frames();
                encode_audio(a.stream_index, &frames[(k as usize) % frames.len()])
            }
        };
        media_message(key, id, &record, &mut out)?;
        // Send when the item exists, plus jitter; never before the previous
        // message (one TCP stream keeps its order).
        let at =
            start + Duration::from_micros(send_us as u64) + Duration::from_micros(jitter.next_us());
        due = due.max(at);
        sleep_until(due).await;
    }
    Ok(())
}

/// Sends a recording: each record in a message of its own (chunked like any
/// other), at its recorded time divided by the speed, until stopped, the
/// recording ends (unless looping), or the connection fails. An `NVRREC01`
/// recording plays from its start on a clock that starts now; an `NVRREC02`
/// one joins the server's shared replay clock (see [`replay`]).
async fn stream_replay(
    shared: &Shared,
    writer: &Writer,
    key: &SessionKey,
    id: u32,
    tier: StreamTier,
    stop: &AtomicBool,
    r: &ReplayConfig,
) -> io::Result<()> {
    let speed = if r.speed.is_finite() && r.speed > 0.0 {
        r.speed
    } else {
        1.0
    };
    let mut out = Vec::new();
    let (start, playback) = if r.replay.version() >= 2 {
        let epoch = *shared.replay_epoch.get_or_init(Instant::now);
        let now_us = (epoch.elapsed().as_secs_f64() * speed * 1e6) as u64;
        let Some(join) = r.replay.join(Some(tier), now_us, r.looped) else {
            return Ok(());
        };
        if let Some(info) = join.info {
            media_message(key, id, info, &mut out)?;
            let mut w = writer.lock().await;
            if stop.load(Ordering::SeqCst) || shared.is_shutdown() {
                return Ok(());
            }
            write_locked(&mut w, &out).await?;
        }
        (epoch, join.playback)
    } else {
        (Instant::now(), r.replay.playback(r.looped))
    };
    for (at_us, step) in playback {
        let Step::Record(record) = step else { continue };
        let due = start + Duration::from_secs_f64(at_us as f64 / speed / 1e6);
        sleep_until(due).await;
        out.clear();
        media_message(key, id, &record, &mut out)?;
        let mut w = writer.lock().await;
        if stop.load(Ordering::SeqCst) || shared.is_shutdown() {
            return Ok(());
        }
        write_locked(&mut w, &out).await?;
    }
    Ok(())
}

impl Conn {
    /// The recording commands (docs/protocol.md, "Recordings: search and
    /// download"). Without an [`ArchiveConfig`], each is refused with 400.
    async fn archive_command(&mut self, m: &RawMessage) -> io::Result<()> {
        let id = m.header.message_id;
        let cmd = m.header.command;
        let Some(cfg) = self.shared.config.archive.clone() else {
            return self.reply_xml(cmd, id, 400, "").await;
        };
        let channels = self.shared.config.channels;
        match cmd {
            142 => match archive::day_records_reply(&cfg, channels, &self.request_text(m)) {
                Some(body) => self.reply_xml(142, id, 200, &body).await,
                None => self.reply_xml(142, id, 400, "").await,
            },
            14 => {
                let r = archive::read_req(&self.request_text(m), "FileInfo");
                let (Some(ch), Some(start), Some(end)) = (r.channel, r.start, r.end) else {
                    return self.reply_xml(14, id, 400, "").await;
                };
                if ch >= channels {
                    return self.reply_xml(14, id, 400, "").await;
                }
                let files = archive::files(&cfg, ch, start, end);
                // Handles are reused once closed, as devices do.
                let handle = (0..)
                    .find(|h| !self.file_searches.contains_key(h))
                    .unwrap_or(0);
                let body = archive::file_search_summary(ch, handle, &files, start, end);
                self.file_searches.insert(handle, (files, 0));
                self.reply_xml(14, id, 200, &body).await
            }
            15 => {
                let r = archive::read_req(&self.request_text(m), "FileInfo");
                let Some((files, next)) = r.handle.and_then(|h| self.file_searches.get_mut(&h))
                else {
                    return self.reply_xml(15, id, 400, "").await;
                };
                let end = next.saturating_add(cfg.file_page.max(1)).min(files.len());
                let page = files[*next..end].to_vec();
                *next = end;
                let body = archive::file_page(r.handle.unwrap_or(0), &page);
                self.reply_xml(15, id, 200, &body).await
            }
            16 => {
                let r = archive::read_req(&self.request_text(m), "FileInfo");
                match r.handle.and_then(|h| self.file_searches.remove(&h)) {
                    Some(_) => self.reply_xml(16, id, 200, "").await,
                    None => self.reply_xml(16, id, 400, "").await,
                }
            }
            272 => {
                // The body travels as the payload after a channel extension.
                let r = archive::read_req(&self.payload_text(m), "findAlarmVideo");
                let (Some(ch), Some(start), Some(end)) = (r.channel, r.start, r.end) else {
                    return self.reply_xml(272, id, 400, "").await;
                };
                if ch >= channels || m.extension().is_empty() {
                    return self.reply_xml(272, id, 400, "").await;
                }
                let handle = self.next_event_handle;
                self.next_event_handle = self.next_event_handle.wrapping_add(1).max(1);
                let events = archive::events(&cfg, ch, start, end);
                self.event_searches.insert(handle, (ch, events, 0));
                self.reply_xml(
                    272,
                    id,
                    200,
                    &format!(
                        "<findAlarmVideo version=\"1.1\">\n<channelId>{ch}</channelId>\n\
                         <fileHandle>{handle}</fileHandle>\n</findAlarmVideo>\n"
                    ),
                )
                .await
            }
            273 => {
                let r = archive::read_req(&self.payload_text(m), "findAlarmVideo");
                let Some(handle) = r.handle else {
                    return self.reply_xml(273, id, 400, "").await;
                };
                let Some((ch, events, next)) = self.event_searches.get_mut(&handle) else {
                    return self.reply_xml(273, id, 400, "").await;
                };
                let end = next.saturating_add(cfg.event_page.max(1)).min(events.len());
                let page = events[*next..end].to_vec();
                *next = end;
                let finished = end >= events.len();
                let ch = *ch;
                let body = archive::event_page(&cfg, ch, handle, &page, finished);
                self.reply_xml(273, id, 200, &body).await
            }
            274 => {
                let r = archive::read_req(&self.payload_text(m), "findAlarmVideo");
                match r.handle.and_then(|h| self.event_searches.remove(&h)) {
                    Some(_) => self.reply_xml(274, id, 200, "").await,
                    None => self.reply_xml(274, id, 400, "").await,
                }
            }
            298 => self.cover(m, &cfg).await,
            143 => self.start_download(m, cfg).await,
            144 => {
                // Stop the download this ID names; if none, any running one.
                let key = if self.downloads.contains_key(&id) {
                    Some(id)
                } else {
                    self.downloads.keys().next().copied()
                };
                let stream = key.and_then(|k| self.downloads.remove(&k));
                let written = {
                    let mut w = self.writer.lock().await;
                    if let Some(s) = &stream {
                        s.stop.store(true, Ordering::SeqCst);
                    }
                    let bytes = encode_message_with_status(144, id, 200, Class::Device, &[], &[])
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
                    write_locked(&mut w, &bytes).await
                };
                if let Some(s) = stream {
                    s.end().await;
                }
                written
            }
            _ => self.reply_xml(cmd, id, 400, "").await,
        }
    }

    /// 298: an empty acknowledgment, then the stream info and one keyframe
    /// (`EncryptLen`, capital E, as observed), then an empty status-300 end.
    async fn cover(&mut self, m: &RawMessage, cfg: &ArchiveConfig) -> io::Result<()> {
        let id = m.header.message_id;
        let r = archive::read_req(&self.request_text(m), "CoverPreview");
        let (Some(ch), Some(start), Some(end)) = (r.channel, r.start, r.end) else {
            return self.reply_xml(298, id, 400, "").await;
        };
        if ch >= self.shared.config.channels {
            return self.reply_xml(298, id, 400, "").await;
        }
        let key = self.key.clone().expect("logged in");
        let c = &self.shared.config;
        let recs = archive::media(cfg, &c.fixture, c.width, c.height, start, end);
        let picked: Vec<(archive::Kind, Vec<u8>)> = recs
            .into_iter()
            .filter(|(k, _)| matches!(k, archive::Kind::Info | archive::Kind::Key))
            .take(2)
            .collect();
        let mut out = encode_message_with_status(298, id, 200, Class::Device, &[], &[])
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        for (kind, rec) in &picked {
            transfer_message(&key, 298, id, *kind, rec, true, &mut out)?;
        }
        out.extend(
            encode_message_with_status(298, id, 300, Class::Device, &[], &[])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?,
        );
        self.send_raw(&out).await
    }

    /// 143: a size estimate, the media, and an empty status-300 end, sent by
    /// a thread of its own so 144 and other commands are served meanwhile.
    async fn start_download(&mut self, m: &RawMessage, cfg: ArchiveConfig) -> io::Result<()> {
        let id = m.header.message_id;
        let r = archive::read_req(&self.request_text(m), "FileInfo");
        let (Some(ch), Some(start), Some(end)) = (r.channel, r.start, r.end) else {
            return self.reply_xml(143, id, 400, "").await;
        };
        if ch >= self.shared.config.channels || end < start || self.downloads.contains_key(&id) {
            return self.reply_xml(143, id, 400, "").await;
        }
        {
            let mut log = lock(&self.shared.download_log);
            if log.len() >= DOWNLOAD_LOG_LEN {
                log.pop_front();
            }
            log.push_back(DownloadLog {
                channel: ch,
                start,
                end,
                stream: r.stream,
            });
        }
        let fault = {
            let mut f = lock(&self.shared.faults);
            match f.iter().position(|x| x.channel.is_none_or(|c| c == ch)) {
                Some(i) if f[i].skip > 0 => {
                    f[i].skip -= 1;
                    None
                }
                Some(i) => Some(f.remove(i)),
                None => None,
            }
        };
        let key = self.key.clone().expect("logged in");
        let c = &self.shared.config;
        let recs = archive::media(&cfg, &c.fixture, c.width, c.height, start, end);
        let estimate: u64 = recs.iter().map(|(_, r)| r.len() as u64).sum();
        let (stop, writer) = (Arc::new(AtomicBool::new(false)), Arc::clone(&self.writer));
        let (shared, stop2) = (Arc::clone(&self.shared), Arc::clone(&stop));
        let rate = cfg.download_rate;
        let task = self.shared.runtime.spawn(async move {
            let _ = serve_download(
                &shared, &writer, &key, id, &recs, estimate, &stop2, fault, rate,
            )
            .await;
        });
        self.downloads.retain(|_, s| !s.is_finished());
        self.downloads.insert(
            id,
            Stream {
                stop,
                task: Some(task),
            },
        );
        Ok(())
    }
}

/// Encodes one record of a download or cover reply: the first message has
/// an extension with `binaryData` and the encrypted length (the whole
/// record for info and audio, 1024 bytes of a keyframe, 32 of a predicted
/// frame, as the official client received them); continuation messages have
/// a `binaryData`-only extension and a plain payload.
fn transfer_message(
    key: &SessionKey,
    cmd: u32,
    id: u32,
    kind: archive::Kind,
    record: &[u8],
    capital: bool,
    out: &mut Vec<u8>,
) -> io::Result<()> {
    let enc_err = |e| io::Error::new(io::ErrorKind::InvalidInput, e);
    let tag = if capital { "EncryptLen" } else { "encryptLen" };
    for (n, chunk) in record.chunks(CHUNK).enumerate() {
        let mut payload = chunk.to_vec();
        let mut ext = if n == 0 {
            let len = match kind {
                archive::Kind::Key => ENCRYPT_LEN,
                archive::Kind::Predicted => 32,
                archive::Kind::Info | archive::Kind::Audio => payload.len(),
            }
            .min(payload.len());
            key.encrypt(&mut payload[..len]);
            format!(
                "{XML_DECLARATION}\n<Extension version=\"1.1\">\n<binaryData>1</binaryData>\n\
                 <{tag}>{len}</{tag}>\n</Extension>\n"
            )
        } else {
            format!(
                "{XML_DECLARATION}\n<Extension version=\"1.1\">\n<binaryData>1</binaryData>\n</Extension>\n"
            )
        }
        .into_bytes();
        key.encrypt(&mut ext);
        out.extend(
            encode_message_with_status(cmd, id, 200, Class::Device, &ext, &payload)
                .map_err(enc_err)?,
        );
    }
    Ok(())
}

/// Sends a download: the estimate, the media (throttled to `rate` bytes per
/// second if set), and the end. Stops silently once `stop` is set (a 144
/// arrived); applies `fault` when its byte count is reached.
#[allow(clippy::too_many_arguments)]
async fn serve_download(
    shared: &Shared,
    writer: &Writer,
    key: &SessionKey,
    id: u32,
    recs: &[(archive::Kind, Vec<u8>)],
    estimate: u64,
    stop: &AtomicBool,
    fault: Option<DownloadFault>,
    rate: Option<u64>,
) -> io::Result<()> {
    let enc_err = |e| io::Error::new(io::ErrorKind::InvalidInput, e);
    let stopped = || stop.load(Ordering::SeqCst) || shared.is_shutdown();
    let mut body = format!(
        "{XML_DECLARATION}\n<body>\n<FileInfoList version=\"1.1\">\n<FileInfo>\n<FileCount>1</FileCount>\n\
         <sizeL>{}</sizeL>\n<sizeH>{}</sizeH>\n</FileInfo>\n</FileInfoList>\n</body>\n",
        estimate & 0xffff_ffff,
        estimate >> 32
    )
    .into_bytes();
    key.encrypt(&mut body);
    {
        let mut w = writer.lock().await;
        if stopped() {
            return Ok(());
        }
        write_locked(
            &mut w,
            &encode_message_with_status(143, id, 200, Class::Device, &[], &body)
                .map_err(enc_err)?,
        )
        .await?;
    }
    let start = Instant::now();
    let mut sent: usize = 0;
    let mut out = Vec::new();
    for (kind, rec) in recs {
        if let Some(f) = fault
            && sent >= f.after_bytes
        {
            match f.kind {
                // The task ends when the download is stopped (144) or the
                // connection closes.
                FaultKind::Stall => std::future::pending::<()>().await,
                FaultKind::Disconnect => {
                    writer.shutdown().await;
                    return Ok(());
                }
            }
        }
        out.clear();
        transfer_message(key, 143, id, *kind, rec, false, &mut out)?;
        {
            let mut w = writer.lock().await;
            if stopped() {
                return Ok(());
            }
            write_locked(&mut w, &out).await?;
        }
        sent += rec.len();
        if let Some(rate) = rate.filter(|r| *r > 0) {
            sleep_until(start + Duration::from_secs_f64(sent as f64 / rate as f64)).await;
        }
    }
    let mut w = writer.lock().await;
    if stopped() {
        return Ok(());
    }
    write_locked(
        &mut w,
        &encode_message_with_status(143, id, 300, Class::Device, &[], &[]).map_err(enc_err)?,
    )
    .await
}
