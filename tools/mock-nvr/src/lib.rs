//! A fake Reolink-style NVR for tests and development.
//!
//! [`spawn`] starts a server on a TCP port (use port 0 in tests and read the
//! real address from [`MockNvr::addr`]). It speaks the subset of the protocol
//! in `docs/protocol.md` that a viewer needs:
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
//! - detection events ([`alarms`]): the alarm-report subscription (192)
//!   and alarm-state pushes (33) whenever a test changes a channel's state
//!   ([`MockNvr::set_alarm`]) or a scripted cycle does
//!   ([`Config::detection_cycle`]); per-channel `aitype` capabilities in
//!   the 199 reply ([`Config::aitypes`]);
//! - optionally, a fake recording store ([`archive`]): day records (142),
//!   file search (14–16), event search (272–274), cover images (298), and
//!   download by time range (143, stopped by 144), with injectable download
//!   stalls and disconnects.
//!
//! It never touches real hardware and answers every other command with
//! status 400.

pub mod alarms;
pub mod archive;
pub mod audio;
pub mod fixture;
pub mod replay;

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
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Bytes of a frame-start payload that are encrypted.
const ENCRYPT_LEN: usize = 1024;
/// Largest payload of one media message.
const CHUNK: usize = 32 * 1024;
/// How often blocking loops check for shutdown.
const POLL: Duration = Duration::from_millis(50);

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
    /// If set, a scripted detection moves through the channels: each period
    /// one channel reports motion and `people` for the first half of the
    /// period, then returns to idle, and the next channel follows.
    pub detection_cycle: Option<Duration>,
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
            detection_cycle: None,
        }
    }
}

struct Shared {
    config: Config,
    shutdown: AtomicBool,
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
}

/// Most downloads remembered for [`MockNvr::download_log`].
const DOWNLOAD_LOG_LEN: usize = 10_000;

/// A running mock NVR. Dropping it (or calling [`MockNvr::shutdown`]) stops
/// the server and closes every connection.
pub struct MockNvr {
    addr: SocketAddr,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
    cycle: Option<JoinHandle<()>>,
}

/// Starts a server on its own threads and returns once it is listening.
pub fn spawn(config: Config) -> io::Result<MockNvr> {
    let listener = TcpListener::bind(config.bind)?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let alarms = alarms::Board::new(config.channels);
    let shared = Arc::new(Shared {
        config,
        shutdown: AtomicBool::new(false),
        logins: AtomicUsize::new(0),
        failed: AtomicU32::new(0),
        nonce_counter: AtomicU32::new(0),
        faults: Mutex::new(Vec::new()),
        download_log: Mutex::new(VecDeque::new()),
        command_counts: Mutex::new(HashMap::new()),
        alarms,
        next_conn: AtomicU64::new(1),
        replay_epoch: OnceLock::new(),
    });
    let s = Arc::clone(&shared);
    let accept = thread::spawn(move || accept_loop(listener, s));
    let cycle = shared.config.detection_cycle.map(|period| {
        let s = Arc::clone(&shared);
        thread::spawn(move || detection_cycle(&s, period))
    });
    Ok(MockNvr {
        addr,
        shared,
        accept: Some(accept),
        cycle,
    })
}

impl MockNvr {
    /// The address the server listens on (with the real port).
    pub fn addr(&self) -> SocketAddr {
        self.addr
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

    /// Sessions subscribed to alarm pushes (command 192) right now.
    pub fn alarm_subscribers(&self) -> usize {
        self.shared.alarms.subscribers()
    }

    /// Stops the server and waits for its threads.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
        if let Some(h) = self.cycle.take() {
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
fn detection_cycle(shared: &Shared, period: Duration) {
    let channels = shared.config.channels.max(1);
    let half = period / 2;
    let mut ch = 0u8;
    let sleep = |d: Duration| {
        let end = Instant::now() + d;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            if shared.shutdown.load(Ordering::SeqCst) {
                return false;
            }
            thread::sleep(left.min(POLL));
        }
        !shared.shutdown.load(Ordering::SeqCst)
    };
    loop {
        shared.alarms.set(
            ch,
            alarms::SlotState {
                status: "MD".into(),
                ai_type: "people".into(),
            },
        );
        if !sleep(half) {
            return;
        }
        shared.alarms.set(ch, alarms::SlotState::default());
        if !sleep(period - half) {
            return;
        }
        ch = (ch + 1) % channels;
    }
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    let mut conns: Vec<JoinHandle<()>> = Vec::new();
    while !shared.shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let s = Arc::clone(&shared);
                conns.push(thread::spawn(move || {
                    let _ = serve(stream, s);
                }));
                conns.retain(|h| !h.is_finished());
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(_) => thread::sleep(POLL),
        }
    }
    for h in conns {
        let _ = h.join();
    }
}

type Writer = Arc<Mutex<TcpStream>>;

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
    join: JoinHandle<()>,
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
}

fn serve(stream: TcpStream, shared: Arc<Shared>) -> io::Result<()> {
    // The listener is non-blocking, and on Windows and macOS an accepted
    // socket inherits that: reads would spin instead of waiting `POLL`, and
    // a write into a full send buffer would fail with `WouldBlock` (ending a
    // download or live stream mid-way) instead of waiting for the client.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(POLL))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.set_nodelay(true)?;
    let mut reader = stream.try_clone()?;
    let mut conn = Conn {
        id: shared.next_conn.fetch_add(1, Ordering::SeqCst),
        shared: Arc::clone(&shared),
        writer: Arc::new(Mutex::new(stream)),
        state: State::Fresh,
        nonce: String::new(),
        key: None,
        holds_login: false,
        streams: HashMap::new(),
        downloads: HashMap::new(),
        file_searches: HashMap::new(),
        event_searches: HashMap::new(),
        next_event_handle: 1,
    };
    let result = conn.run(&mut reader);
    conn.close();
    result
}

impl Conn {
    fn run(&mut self, reader: &mut TcpStream) -> io::Result<()> {
        let mut dec = Decoder::new();
        let mut buf = [0u8; 8192];
        while !self.shared.shutdown.load(Ordering::SeqCst) {
            loop {
                match dec.next_message() {
                    Ok(Some(m)) => self.handle(&m)?,
                    Ok(None) => break,
                    Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e)),
                }
            }
            match reader.read(&mut buf) {
                Ok(0) => return Ok(()),
                Ok(n) => dec.push(&buf[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn close(&mut self) {
        self.shared.alarms.unsubscribe(self.id);
        for (_, s) in self.streams.drain().chain(self.downloads.drain()) {
            s.stop.store(true, Ordering::SeqCst);
            let _ = s.join.join();
        }
        if self.holds_login {
            self.holds_login = false;
            self.shared.logins.fetch_sub(1, Ordering::SeqCst);
        }
        let _ = lock(&self.writer).shutdown(Shutdown::Both);
    }

    fn send_raw(&self, bytes: &[u8]) -> io::Result<()> {
        lock(&self.writer).write_all(bytes)
    }

    fn reply(&self, cmd: u32, id: u32, status: u16, class: Class, body: &[u8]) -> io::Result<()> {
        let bytes = encode_message_with_status(cmd, id, status, class, &[], body)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        self.send_raw(&bytes)
    }

    /// Replies after login: XML encrypted with the session key.
    fn reply_xml(&self, cmd: u32, id: u32, status: u16, inner: &str) -> io::Result<()> {
        let mut body = if inner.is_empty() {
            Vec::new()
        } else {
            format!("{XML_DECLARATION}\n<body>\n{inner}</body>\n").into_bytes()
        };
        if let Some(k) = &self.key {
            k.encrypt(&mut body);
        }
        self.reply(cmd, id, status, Class::Device, &body)
    }

    fn handle(&mut self, m: &RawMessage) -> io::Result<()> {
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
                self.reply(1, h.message_id, 200, Class::DeviceHandshake, &body)?;
                self.state = State::NonceSent;
                Ok(())
            }
            (State::NonceSent, Class::Client, 1) => self.login(m),
            (State::LoggedIn, Class::Client, _) => self.command(m),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected message",
            )),
        }
    }

    fn reject(&mut self, id: u32, status: u16, remain: Option<u32>) -> io::Result<()> {
        let mut body = match remain {
            Some(r) => format!(
                "{XML_DECLARATION}\n<body>\n<LoginErrInfo>\n<remainTimes>{r}</remainTimes>\n</LoginErrInfo>\n</body>\n"
            )
            .into_bytes(),
            None => Vec::new(),
        };
        xor_obfuscate(id, &mut body);
        self.state = State::Fresh;
        self.reply(1, id, status, Class::Device, &body)
    }

    fn login(&mut self, m: &RawMessage) -> io::Result<()> {
        let id = m.header.message_id;
        let cfg = &self.shared.config;
        let mut body = m.body.clone();
        xor_obfuscate(id, &mut body);
        let text = String::from_utf8_lossy(&body).into_owned();
        let user = xml::find_text(&text, "userName").ok().flatten();
        let pass = xml::find_text(&text, "password").ok().flatten();
        let failed = self.shared.failed.load(Ordering::SeqCst);
        if failed >= cfg.max_failed_logins {
            return self.reject(id, 401, Some(0));
        }
        let ok = user.as_deref() == Some(credential_hash(&cfg.user, &self.nonce).as_str())
            && pass.as_deref() == Some(credential_hash(&cfg.password, &self.nonce).as_str());
        if !ok {
            let n = self.shared.failed.fetch_add(1, Ordering::SeqCst) + 1;
            return self.reject(id, 401, Some(cfg.max_failed_logins.saturating_sub(n)));
        }
        // Take a login slot, or give it back if the cap is exceeded.
        if self.shared.logins.fetch_add(1, Ordering::SeqCst) >= cfg.max_logins {
            self.shared.logins.fetch_sub(1, Ordering::SeqCst);
            return self.reject(id, 400, None);
        }
        self.holds_login = true;
        self.shared.failed.store(0, Ordering::SeqCst);
        self.key = Some(SessionKey::derive(&self.nonce, &cfg.password));
        self.state = State::LoggedIn;
        self.reply(1, id, 200, Class::Device, &[])?;
        // Devices push the channel list unprompted.
        self.reply_xml(145, 0, 200, &self.channel_info_list())
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

    fn command(&mut self, m: &RawMessage) -> io::Result<()> {
        *lock(&self.shared.command_counts)
            .entry(m.header.command)
            .or_insert(0) += 1;
        if matches!(
            m.header.command,
            14 | 15 | 16 | 142 | 143 | 144 | 272 | 273 | 274 | 298
        ) {
            return self.archive_command(m);
        }
        let cfg = &self.shared.config;
        let id = m.header.message_id;
        match m.header.command {
            93 => self.reply_xml(93, id, 200, "<LinkType>\n<type>LAN</type>\n</LinkType>\n"),
            80 => self.reply_xml(
                80,
                id,
                200,
                "<VersionInfo version=\"1.1\">\n<name>Mock NVR</name>\n<type>MOCK-NVR</type>\n\
                 <firmwareVersion>mock-1.0</firmwareVersion>\n</VersionInfo>\n",
            ),
            199 => self.reply_xml(199, id, 200, &self.support()),
            // Alarm report: an empty reply, then state pushes (33).
            192 => {
                self.reply_xml(192, id, 200, "")?;
                let key = self.key.clone().expect("logged in");
                self.shared
                    .alarms
                    .subscribe(self.id, Arc::clone(&self.writer), key, id);
                Ok(())
            }
            145 => self.reply_xml(145, id, 200, &self.channel_info_list()),
            // Real devices answer 146 with status 300 and a full body.
            146 => self.reply_xml(146, id, 300, &self.stream_info_list()),
            318 => {
                let text = self.request_text(m);
                match xml::find_text(&text, "channelId")
                    .ok()
                    .flatten()
                    .and_then(|s| s.parse::<u8>().ok())
                    .filter(|c| *c < cfg.channels)
                {
                    Some(c) => self.reply_xml(
                        318,
                        id,
                        200,
                        &format!(
                            "<VersionInfo version=\"1.1\">\n<channelId>{c}</channelId>\n\
                             <name>Channel {}</name>\n<type>MOCK-CAM</type>\n</VersionInfo>\n",
                            u32::from(c) + 1
                        ),
                    ),
                    None => self.reply_xml(318, id, 400, ""),
                }
            }
            // Logout: answer, then free the login slot and end the session.
            // (A real client closes TCP after the reply; ending it here too is
            // the mock's choice.)
            2 => {
                self.reply_xml(2, id, 200, "")?;
                self.close();
                Ok(())
            }
            3 => self.start_preview(m),
            4 => self.stop_preview(m),
            other => self.reply_xml(other, id, 400, ""),
        }
    }

    /// Capabilities (199): the channel count and one `item` per channel
    /// with its `aitype`.
    fn support(&self) -> String {
        let cfg = &self.shared.config;
        let mut s = format!(
            "<Support version=\"1.1\">\n<channelNum>{}</channelNum>\n",
            cfg.channels
        );
        for c in 0..cfg.channels {
            let ai = cfg.aitypes.get(usize::from(c)).copied().unwrap_or(0);
            s.push_str(&format!(
                "<item>\n<chnID>{c}</chnID>\n<aitype>{ai}</aitype>\n<aiAnimalType>0</aiAnimalType>\n</item>\n"
            ));
        }
        s.push_str("</Support>\n");
        s
    }

    fn channel_info_list(&self) -> String {
        let mut s = String::from("<ChannelInfoList version=\"1.1\">\n");
        for c in 0..self.shared.config.channels {
            s.push_str(&format!(
                "<ChannelInfo>\n<channelId>{c}</channelId>\n<state>connect</state>\n\
                 <streamSupport>mainStream,subStream</streamSupport>\n</ChannelInfo>\n"
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

    fn start_preview(&mut self, m: &RawMessage) -> io::Result<()> {
        let id = m.header.message_id;
        let Some((handle, _, tier)) = self.preview_target(m) else {
            return self.reply_xml(3, id, 400, "");
        };
        if self.streams.contains_key(&handle) {
            return self.reply_xml(3, id, 400, "");
        }
        let (stop, writer) = (Arc::new(AtomicBool::new(false)), Arc::clone(&self.writer));
        let key = self.key.clone().expect("logged in");
        let (cfg_shared, stop2) = (Arc::clone(&self.shared), Arc::clone(&stop));
        // Like a real device, send no separate reply: the first media message
        // (status 200) is the answer.
        let join = thread::spawn(move || {
            let _ = stream_media(&cfg_shared, &writer, &key, id, tier, &stop2);
        });
        self.streams.insert(handle, Stream { stop, join });
        Ok(())
    }

    fn stop_preview(&mut self, m: &RawMessage) -> io::Result<()> {
        let id = m.header.message_id;
        let Some((handle, _, _)) = self.preview_target(m) else {
            return self.reply_xml(4, id, 400, "");
        };
        let Some(s) = self.streams.remove(&handle) else {
            return self.reply_xml(4, id, 400, "");
        };
        // Set the flag while holding the write lock: the stream thread checks
        // it under the same lock, so no media follows the stop reply.
        {
            let mut w = lock(&self.writer);
            s.stop.store(true, Ordering::SeqCst);
            let bytes = encode_message_with_status(4, id, 200, Class::Device, &[], &[])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            w.write_all(&bytes)?;
        }
        let _ = s.join.join();
        Ok(())
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
fn stream_media(
    shared: &Shared,
    writer: &Writer,
    key: &SessionKey,
    id: u32,
    tier: StreamTier,
    stop: &AtomicBool,
) -> io::Result<()> {
    let cfg = &shared.config;
    if let Some(r) = &cfg.replay {
        return stream_replay(shared, writer, key, id, tier, stop, r);
    }
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
            let mut w = lock(writer);
            if stop.load(Ordering::SeqCst) || shared.shutdown.load(Ordering::SeqCst) {
                return Ok(());
            }
            w.write_all(&out)?;
        }
        out.clear();
        let record = match item {
            Item::Video { pts_us, .. } => {
                let frame = &fix.frames()[index];
                index = fix.next_index(index);
                encode_video(0, frame.keyframe, fix.codec(), pts_us, &[], &frame.data)
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
        while let Some(left) = due.checked_duration_since(Instant::now()) {
            if stop.load(Ordering::SeqCst) || shared.shutdown.load(Ordering::SeqCst) {
                return Ok(());
            }
            thread::sleep(left.min(POLL));
        }
    }
    Ok(())
}

/// Sends a recording: each record in a message of its own (chunked like any
/// other), at its recorded time divided by the speed, until stopped, the
/// recording ends (unless looping), or the connection fails. An `NVRREC01`
/// recording plays from its start on a clock that starts now; an `NVRREC02`
/// one joins the server's shared replay clock (see [`replay`]).
fn stream_replay(
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
            let mut w = lock(writer);
            if stop.load(Ordering::SeqCst) || shared.shutdown.load(Ordering::SeqCst) {
                return Ok(());
            }
            w.write_all(&out)?;
        }
        (epoch, join.playback)
    } else {
        (Instant::now(), r.replay.playback(r.looped))
    };
    for (at_us, step) in playback {
        let Step::Record(record) = step else { continue };
        let due = start + Duration::from_secs_f64(at_us as f64 / speed / 1e6);
        while let Some(left) = due.checked_duration_since(Instant::now()) {
            if stop.load(Ordering::SeqCst) || shared.shutdown.load(Ordering::SeqCst) {
                return Ok(());
            }
            thread::sleep(left.min(POLL));
        }
        out.clear();
        media_message(key, id, &record, &mut out)?;
        let mut w = lock(writer);
        if stop.load(Ordering::SeqCst) || shared.shutdown.load(Ordering::SeqCst) {
            return Ok(());
        }
        w.write_all(&out)?;
    }
    Ok(())
}

impl Conn {
    /// The recording commands (docs/protocol.md, "Recordings: search and
    /// download"). Without an [`ArchiveConfig`], each is refused with 400.
    fn archive_command(&mut self, m: &RawMessage) -> io::Result<()> {
        let id = m.header.message_id;
        let cmd = m.header.command;
        let Some(cfg) = self.shared.config.archive.clone() else {
            return self.reply_xml(cmd, id, 400, "");
        };
        let channels = self.shared.config.channels;
        match cmd {
            142 => match archive::day_records_reply(&cfg, channels, &self.request_text(m)) {
                Some(body) => self.reply_xml(142, id, 200, &body),
                None => self.reply_xml(142, id, 400, ""),
            },
            14 => {
                let r = archive::read_req(&self.request_text(m), "FileInfo");
                let (Some(ch), Some(start), Some(end)) = (r.channel, r.start, r.end) else {
                    return self.reply_xml(14, id, 400, "");
                };
                if ch >= channels {
                    return self.reply_xml(14, id, 400, "");
                }
                let files = archive::files(&cfg, ch, start, end);
                // Handles are reused once closed, as devices do.
                let handle = (0..)
                    .find(|h| !self.file_searches.contains_key(h))
                    .unwrap_or(0);
                let body = archive::file_search_summary(ch, handle, &files, start, end);
                self.file_searches.insert(handle, (files, 0));
                self.reply_xml(14, id, 200, &body)
            }
            15 => {
                let r = archive::read_req(&self.request_text(m), "FileInfo");
                let Some((files, next)) = r.handle.and_then(|h| self.file_searches.get_mut(&h))
                else {
                    return self.reply_xml(15, id, 400, "");
                };
                let end = next.saturating_add(cfg.file_page.max(1)).min(files.len());
                let page = files[*next..end].to_vec();
                *next = end;
                let body = archive::file_page(r.handle.unwrap_or(0), &page);
                self.reply_xml(15, id, 200, &body)
            }
            16 => {
                let r = archive::read_req(&self.request_text(m), "FileInfo");
                match r.handle.and_then(|h| self.file_searches.remove(&h)) {
                    Some(_) => self.reply_xml(16, id, 200, ""),
                    None => self.reply_xml(16, id, 400, ""),
                }
            }
            272 => {
                // The body travels as the payload after a channel extension.
                let r = archive::read_req(&self.payload_text(m), "findAlarmVideo");
                let (Some(ch), Some(start), Some(end)) = (r.channel, r.start, r.end) else {
                    return self.reply_xml(272, id, 400, "");
                };
                if ch >= channels || m.extension().is_empty() {
                    return self.reply_xml(272, id, 400, "");
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
            }
            273 => {
                let r = archive::read_req(&self.payload_text(m), "findAlarmVideo");
                let Some(handle) = r.handle else {
                    return self.reply_xml(273, id, 400, "");
                };
                let Some((ch, events, next)) = self.event_searches.get_mut(&handle) else {
                    return self.reply_xml(273, id, 400, "");
                };
                let end = next.saturating_add(cfg.event_page.max(1)).min(events.len());
                let page = events[*next..end].to_vec();
                *next = end;
                let finished = end >= events.len();
                let ch = *ch;
                let body = archive::event_page(&cfg, ch, handle, &page, finished);
                self.reply_xml(273, id, 200, &body)
            }
            274 => {
                let r = archive::read_req(&self.payload_text(m), "findAlarmVideo");
                match r.handle.and_then(|h| self.event_searches.remove(&h)) {
                    Some(_) => self.reply_xml(274, id, 200, ""),
                    None => self.reply_xml(274, id, 400, ""),
                }
            }
            298 => self.cover(m, &cfg),
            143 => self.start_download(m, cfg),
            144 => {
                // Stop the download this ID names; if none, any running one.
                let key = if self.downloads.contains_key(&id) {
                    Some(id)
                } else {
                    self.downloads.keys().next().copied()
                };
                let stream = key.and_then(|k| self.downloads.remove(&k));
                {
                    let mut w = lock(&self.writer);
                    if let Some(s) = &stream {
                        s.stop.store(true, Ordering::SeqCst);
                    }
                    let bytes = encode_message_with_status(144, id, 200, Class::Device, &[], &[])
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
                    w.write_all(&bytes)?;
                }
                if let Some(s) = stream {
                    let _ = s.join.join();
                }
                Ok(())
            }
            _ => self.reply_xml(cmd, id, 400, ""),
        }
    }

    /// 298: an empty acknowledgment, then the stream info and one keyframe
    /// (`EncryptLen`, capital E, as observed), then an empty status-300 end.
    fn cover(&mut self, m: &RawMessage, cfg: &ArchiveConfig) -> io::Result<()> {
        let id = m.header.message_id;
        let r = archive::read_req(&self.request_text(m), "CoverPreview");
        let (Some(ch), Some(start), Some(end)) = (r.channel, r.start, r.end) else {
            return self.reply_xml(298, id, 400, "");
        };
        if ch >= self.shared.config.channels {
            return self.reply_xml(298, id, 400, "");
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
        self.send_raw(&out)
    }

    /// 143: a size estimate, the media, and an empty status-300 end, sent by
    /// a thread of its own so 144 and other commands are served meanwhile.
    fn start_download(&mut self, m: &RawMessage, cfg: ArchiveConfig) -> io::Result<()> {
        let id = m.header.message_id;
        let r = archive::read_req(&self.request_text(m), "FileInfo");
        let (Some(ch), Some(start), Some(end)) = (r.channel, r.start, r.end) else {
            return self.reply_xml(143, id, 400, "");
        };
        if ch >= self.shared.config.channels || end < start || self.downloads.contains_key(&id) {
            return self.reply_xml(143, id, 400, "");
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
        let join = thread::spawn(move || {
            let _ = serve_download(
                &shared, &writer, &key, id, &recs, estimate, &stop2, fault, rate,
            );
        });
        self.downloads.retain(|_, s| !s.join.is_finished());
        self.downloads.insert(id, Stream { stop, join });
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
fn serve_download(
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
    let stopped = || stop.load(Ordering::SeqCst) || shared.shutdown.load(Ordering::SeqCst);
    let mut body = format!(
        "{XML_DECLARATION}\n<body>\n<FileInfoList version=\"1.1\">\n<FileInfo>\n<FileCount>1</FileCount>\n\
         <sizeL>{}</sizeL>\n<sizeH>{}</sizeH>\n</FileInfo>\n</FileInfoList>\n</body>\n",
        estimate & 0xffff_ffff,
        estimate >> 32
    )
    .into_bytes();
    key.encrypt(&mut body);
    {
        let mut w = lock(writer);
        if stopped() {
            return Ok(());
        }
        w.write_all(
            &encode_message_with_status(143, id, 200, Class::Device, &[], &body)
                .map_err(enc_err)?,
        )?;
    }
    let start = Instant::now();
    let mut sent: usize = 0;
    let mut out = Vec::new();
    for (kind, rec) in recs {
        if let Some(f) = fault
            && sent >= f.after_bytes
        {
            match f.kind {
                FaultKind::Stall => {
                    while !stopped() {
                        thread::sleep(POLL);
                    }
                    return Ok(());
                }
                FaultKind::Disconnect => {
                    let _ = lock(writer).shutdown(Shutdown::Both);
                    return Ok(());
                }
            }
        }
        out.clear();
        transfer_message(key, 143, id, *kind, rec, false, &mut out)?;
        {
            let mut w = lock(writer);
            if stopped() {
                return Ok(());
            }
            w.write_all(&out)?;
        }
        sent += rec.len();
        if let Some(rate) = rate.filter(|r| *r > 0) {
            let due = start + Duration::from_secs_f64(sent as f64 / rate as f64);
            while let Some(left) = due.checked_duration_since(Instant::now()) {
                if stopped() {
                    return Ok(());
                }
                thread::sleep(left.min(POLL));
            }
        }
    }
    let mut w = lock(writer);
    if stopped() {
        return Ok(());
    }
    w.write_all(
        &encode_message_with_status(143, id, 300, Class::Device, &[], &[]).map_err(enc_err)?,
    )
}
