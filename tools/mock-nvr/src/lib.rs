//! A fake Reolink-style NVR for tests and development.
//!
//! [`spawn`] starts a server on a TCP port (use port 0 in tests and read the
//! real address from [`MockNvr::addr`]). It speaks the subset of the protocol
//! in `docs/protocol.md` that a viewer needs:
//!
//! - handshake (nonce) and login with one configured user name and password;
//!   a wrong credential is answered with status 401, and the number of
//!   concurrent logged-in sessions can be capped;
//! - keepalive (93), version (80), capabilities (199), stream info (146),
//!   per-channel version info (318), and a channel info push (145) after login;
//! - preview start (3) and stop (4): a looping synthetic video fixture is sent
//!   as BCMEDIA records at a fixed frame rate, starting at a keyframe, with the
//!   first 1024 bytes of each frame-start message encrypted.
//!
//! It never touches real hardware and answers every other command with
//! status 400.

pub mod fixture;

use bc_protocol::crypto::{SessionKey, credential_hash, md5_hex_upper_31, xor_obfuscate};
use bc_protocol::framer::{Decoder, RawMessage, encode_message_with_status};
use bc_protocol::header::Class;
use bc_protocol::media::{encode_info, encode_video};
use bc_protocol::stream::StreamTier;
use bc_protocol::xml::{self, XML_DECLARATION};
use fixture::Fixture;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
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
        }
    }
}

struct Shared {
    config: Config,
    shutdown: AtomicBool,
    logins: AtomicUsize,
    failed: AtomicU32,
    nonce_counter: AtomicU32,
}

/// A running mock NVR. Dropping it (or calling [`MockNvr::shutdown`]) stops
/// the server and closes every connection.
pub struct MockNvr {
    addr: SocketAddr,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
}

/// Starts a server on its own threads and returns once it is listening.
pub fn spawn(config: Config) -> io::Result<MockNvr> {
    let listener = TcpListener::bind(config.bind)?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let shared = Arc::new(Shared {
        config,
        shutdown: AtomicBool::new(false),
        logins: AtomicUsize::new(0),
        failed: AtomicU32::new(0),
        nonce_counter: AtomicU32::new(0),
    });
    let s = Arc::clone(&shared);
    let accept = thread::spawn(move || accept_loop(listener, s));
    Ok(MockNvr {
        addr,
        shared,
        accept: Some(accept),
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

    /// Stops the server and waits for its threads.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        if let Some(h) = self.accept.take() {
            let _ = h.join();
        }
    }
}

impl Drop for MockNvr {
    fn drop(&mut self) {
        self.stop();
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
    shared: Arc<Shared>,
    writer: Writer,
    state: State,
    nonce: String,
    key: Option<SessionKey>,
    holds_login: bool,
    streams: HashMap<u32, Stream>,
}

fn serve(stream: TcpStream, shared: Arc<Shared>) -> io::Result<()> {
    stream.set_read_timeout(Some(POLL))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    stream.set_nodelay(true)?;
    let mut reader = stream.try_clone()?;
    let mut conn = Conn {
        shared: Arc::clone(&shared),
        writer: Arc::new(Mutex::new(stream)),
        state: State::Fresh,
        nonce: String::new(),
        key: None,
        holds_login: false,
        streams: HashMap::new(),
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
        for (_, s) in self.streams.drain() {
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

    fn command(&mut self, m: &RawMessage) -> io::Result<()> {
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
            199 => self.reply_xml(
                199,
                id,
                200,
                &format!(
                    "<Support version=\"1.1\">\n<channelNum>{}</channelNum>\n</Support>\n",
                    cfg.channels
                ),
            ),
            145 | 146 => {
                let inner = if m.header.command == 145 {
                    self.channel_info_list()
                } else {
                    self.stream_info_list()
                };
                self.reply_xml(m.header.command, id, 200, &inner)
            }
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
            3 => self.start_preview(m),
            4 => self.stop_preview(m),
            other => self.reply_xml(other, id, 400, ""),
        }
    }

    fn channel_info_list(&self) -> String {
        let mut s = String::from("<ChannelInfoList version=\"1.1\">\n");
        for c in 0..self.shared.config.channels {
            s.push_str(&format!(
                "<ChannelInfo>\n<channelId>{c}</channelId>\n<name>Channel {}</name>\n\
                 <state>online</state>\n</ChannelInfo>\n",
                u32::from(c) + 1
            ));
        }
        s.push_str("</ChannelInfoList>\n");
        s
    }

    fn stream_info_list(&self) -> String {
        let cfg = &self.shared.config;
        let codec = match cfg.fixture.codec() {
            bc_protocol::media::Codec::H264 => "h264",
            bc_protocol::media::Codec::H265 => "h265",
        };
        let mut s = String::from("<StreamInfoList version=\"1.1\">\n");
        for c in 0..cfg.channels {
            s.push_str(&format!("<StreamInfo>\n<channelId>{c}</channelId>\n"));
            for t in [StreamTier::Main, StreamTier::Sub] {
                s.push_str(&format!(
                    "<encodeTable>\n<type>{}</type>\n<resolution>\n<width>{}</width>\n<height>{}</height>\n\
                     </resolution>\n<frameRate>{}</frameRate>\n<encoderType>{codec}</encoderType>\n</encodeTable>\n",
                    t.name(), cfg.width, cfg.height, cfg.fps
                ));
            }
            s.push_str("</StreamInfo>\n");
        }
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
        let Some((handle, _, _)) = self.preview_target(m) else {
            return self.reply_xml(3, id, 400, "");
        };
        if self.streams.contains_key(&handle) {
            return self.reply_xml(3, id, 400, "");
        }
        let (stop, writer) = (Arc::new(AtomicBool::new(false)), Arc::clone(&self.writer));
        let key = self.key.clone().expect("logged in");
        let (cfg_shared, stop2) = (Arc::clone(&self.shared), Arc::clone(&stop));
        // The reply and the first media are written under the same lock
        // discipline as every frame, so the reply always comes first.
        {
            let mut w = lock(&self.writer);
            let bytes = encode_message_with_status(3, id, 200, Class::Device, &[], &[])
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            w.write_all(&bytes)?;
        }
        let join = thread::spawn(move || {
            let _ = stream_media(&cfg_shared, &writer, &key, id, &stop2);
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

/// Sends the info record and then the looping fixture at the configured
/// rate until stopped or the connection fails.
fn stream_media(
    shared: &Shared,
    writer: &Writer,
    key: &SessionKey,
    id: u32,
    stop: &AtomicBool,
) -> io::Result<()> {
    let cfg = &shared.config;
    let fix = &cfg.fixture;
    let fps = cfg.fps.max(1);
    let period = Duration::from_micros(1_000_000 / u64::from(fps));
    let mut index = fix.first_keyframe();
    let mut pts_us = 0u64;
    let mut out = Vec::new();
    media_message(
        key,
        id,
        &encode_info(cfg.width, cfg.height, fps.min(255) as u8),
        &mut out,
    )?;
    let start = Instant::now();
    let mut n = 0u32;
    loop {
        {
            let mut w = lock(writer);
            if stop.load(Ordering::SeqCst) || shared.shutdown.load(Ordering::SeqCst) {
                return Ok(());
            }
            w.write_all(&out)?;
        }
        out.clear();
        let frame = &fix.frames()[index];
        let record = encode_video(
            if frame.keyframe { 0 } else { 10 },
            fix.codec(),
            pts_us,
            &[],
            &frame.data,
        );
        media_message(key, id, &record, &mut out)?;
        index = fix.next_index(index);
        pts_us += period.as_micros() as u64;
        n += 1;
        let due = start + period * n;
        while let Some(left) = due.checked_duration_since(Instant::now()) {
            if stop.load(Ordering::SeqCst) || shared.shutdown.load(Ordering::SeqCst) {
                return Ok(());
            }
            thread::sleep(left.min(POLL));
        }
    }
}
