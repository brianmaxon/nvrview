//! Sans-I/O session: builds outgoing messages and turns received messages
//! into typed events.
//!
//! The caller owns the socket. Typical use: send [`Session::handshake`], feed
//! every received [`RawMessage`] to [`Session::handle`], send
//! [`Session::login`] after [`Event::NonceReceived`], and once
//! [`Event::LoginOk`] arrives start previews and send keepalives.
//!
//! Only commands on the read-only allowlist ([`Command`]) can be produced.

use crate::crypto::{SessionKey, credential_hash, xor_obfuscate};
use crate::framer::{EncodeError, RawMessage, encode_message, encode_message_with_status};
use crate::header::Class;
use crate::stream::StreamTier;
use crate::xml::{self, PreviewPolicy, XmlError};
use std::collections::HashMap;
use std::fmt;

/// Commands this crate can send (plus 145, which devices push). Anything
/// that could change device settings is deliberately absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// 1: handshake and login.
    Login = 1,
    /// 3: preview (live view) start.
    PreviewStart = 3,
    /// 4: preview stop.
    PreviewStop = 4,
    /// 80: NVR version info (query).
    VersionInfo = 80,
    /// 93: keepalive.
    Keepalive = 93,
    /// 145: channel info list (pushed by the device).
    ChannelInfoList = 145,
    /// 146: stream info list (query).
    StreamInfoList = 146,
    /// 199: capabilities (query).
    Support = 199,
    /// 318: per-channel version info (query with a channel extension).
    ChannelVersionInfo = 318,
}

impl Command {
    /// The command number.
    pub const fn number(self) -> u32 {
        self as u32
    }

    /// Looks up an allowlisted command by number.
    pub const fn from_number(n: u32) -> Option<Command> {
        Some(match n {
            1 => Command::Login,
            3 => Command::PreviewStart,
            4 => Command::PreviewStop,
            80 => Command::VersionInfo,
            93 => Command::Keepalive,
            145 => Command::ChannelInfoList,
            146 => Command::StreamInfoList,
            199 => Command::Support,
            318 => Command::ChannelVersionInfo,
            _ => return None,
        })
    }

    /// Whether `n` is on the allowlist.
    pub const fn is_allowed(n: u32) -> bool {
        Command::from_number(n).is_some()
    }
}

/// Message ID of the handshake (and of the login, which reuses it).
const HANDSHAKE_ID: u32 = 0;
const LOGIN_ID: u32 = 0;
/// Status field the handshake request carries (`12 dc` on the wire).
const HANDSHAKE_STATUS: u16 = 0xdc12;

/// Session errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionError {
    /// The operation is not valid in the current state.
    WrongState(&'static str),
    /// A command outside the allowlist was requested.
    CommandNotAllowed(u32),
    /// A received message is not expected in the current state.
    Unexpected {
        /// Command number of the message.
        command: u32,
    },
    /// A received message is internally inconsistent (e.g. payload offset
    /// beyond the body).
    Malformed,
    /// Decrypted text is not valid UTF-8: most likely a wrong password.
    BadText,
    /// The preview message ID is not an active preview.
    UnknownStream(u32),
    /// XML parse failure.
    Xml(XmlError),
    /// Message encoding failure.
    Encode(EncodeError),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::WrongState(s) => write!(f, "wrong session state: {s}"),
            SessionError::CommandNotAllowed(c) => write!(f, "command {c} is not on the allowlist"),
            SessionError::Unexpected { command } => {
                write!(f, "unexpected message, command {command}")
            }
            SessionError::Malformed => write!(f, "malformed message"),
            SessionError::BadText => {
                write!(f, "decrypted text is not valid UTF-8 (wrong password?)")
            }
            SessionError::UnknownStream(id) => write!(f, "no active preview with message id {id}"),
            SessionError::Xml(e) => write!(f, "{e}"),
            SessionError::Encode(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<XmlError> for SessionError {
    fn from(e: XmlError) -> Self {
        SessionError::Xml(e)
    }
}

impl From<EncodeError> for SessionError {
    fn from(e: EncodeError) -> Self {
        SessionError::Encode(e)
    }
}

/// A typed result of handling one received message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The device's nonce arrived and the session key is derived; send
    /// [`Session::login`] next.
    NonceReceived,
    /// Login succeeded (status 200); the session is now encrypted.
    LoginOk,
    /// Login was refused (status 401 or other). Devices count failed attempts
    /// and lock out, so do not retry blindly.
    LoginRejected {
        /// Reply status.
        status: u16,
        /// Remaining attempts, when the device says so.
        remain_times: Option<u32>,
    },
    /// A reply to a control command.
    Control {
        /// Command number.
        command: u32,
        /// Message ID (echoes the request's).
        message_id: u32,
        /// Reply status.
        status: u16,
        /// Decrypted XML text (the extension when there is one, else the
        /// body); empty for an empty body.
        xml: String,
        /// Plain payload that followed an extension (usually empty).
        payload: Vec<u8>,
    },
    /// Media bytes for the stream started with `message_id`, decrypted as
    /// needed. Feed them to that stream's [`crate::media::Demuxer`].
    Media {
        /// Message ID of the preview start.
        message_id: u32,
        /// Payload bytes.
        data: Vec<u8>,
    },
    /// A message that was deliberately skipped (e.g. media for a stopped
    /// preview).
    Ignored {
        /// Command number.
        command: u32,
        /// Message ID.
        message_id: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    New,
    HandshakeSent,
    NonceReceived,
    LoginSent,
    LoggedIn,
    Failed,
}

struct Preview {
    channel: u8,
    tier: StreamTier,
    saw_media: bool,
}

/// One session with one device.
pub struct Session {
    user: String,
    password: String,
    state: State,
    nonce: Option<String>,
    key: Option<SessionKey>,
    next_id: u32,
    previews: HashMap<u32, Preview>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("state", &self.state)
            .field("previews", &self.previews.len())
            .finish_non_exhaustive()
    }
}

fn utf8(bytes: Vec<u8>) -> Result<String, SessionError> {
    String::from_utf8(bytes).map_err(|_| SessionError::BadText)
}

fn looks_like_xml(s: &str) -> bool {
    s.trim_start().starts_with('<')
}

impl Session {
    /// Creates a session that will log in with the given credentials.
    pub fn new(user: impl Into<String>, password: impl Into<String>) -> Session {
        Session {
            user: user.into(),
            password: password.into(),
            state: State::New,
            nonce: None,
            key: None,
            next_id: 1,
            previews: HashMap::new(),
        }
    }

    /// Whether login has succeeded.
    pub fn is_logged_in(&self) -> bool {
        self.state == State::LoggedIn
    }

    /// Message IDs of active previews.
    pub fn active_previews(&self) -> impl Iterator<Item = u32> + '_ {
        self.previews.keys().copied()
    }

    /// Allocates the next message ID (post-login only: refuses, without
    /// consuming an ID, before login).
    fn alloc_id(&mut self) -> Result<u32, SessionError> {
        if self.state != State::LoggedIn {
            return Err(SessionError::WrongState("not logged in"));
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        Ok(id)
    }

    /// Builds the first message: command 1, empty body, class 0x1465,
    /// message ID 0, status 0xdc12.
    pub fn handshake(&mut self) -> Result<Vec<u8>, SessionError> {
        if self.state != State::New {
            return Err(SessionError::WrongState("handshake already sent"));
        }
        self.state = State::HandshakeSent;
        Ok(encode_message_with_status(
            Command::Login.number(),
            HANDSHAKE_ID,
            HANDSHAKE_STATUS,
            Class::ClientHandshake,
            &[],
            &[],
        )?)
    }

    /// Builds the login message from the received nonce: command 1, class
    /// 0x1464 with payload offset 0, message ID 0, status 0. The body is
    /// XOR-obfuscated keyed on ID 0. Later messages start at ID 1.
    pub fn login(&mut self) -> Result<Vec<u8>, SessionError> {
        if self.state != State::NonceReceived {
            return Err(SessionError::WrongState("no nonce received"));
        }
        let nonce = self
            .nonce
            .as_deref()
            .ok_or(SessionError::WrongState("no nonce received"))?;
        let body = xml::login_body(
            &credential_hash(&self.user, nonce),
            &credential_hash(&self.password, nonce),
        );
        let mut bytes = body.into_bytes();
        xor_obfuscate(LOGIN_ID, &mut bytes);
        self.state = State::LoginSent;
        Ok(encode_message(
            Command::Login.number(),
            LOGIN_ID,
            Class::Client,
            &[],
            &bytes,
        )?)
    }

    fn encrypted(&self, ext: &[u8], body: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut e = ext.to_vec();
        let mut b = body.to_vec();
        if let Some(key) = &self.key {
            if !e.is_empty() {
                key.encrypt(&mut e);
            }
            if !b.is_empty() {
                key.encrypt(&mut b);
            }
        }
        (e, b)
    }

    fn send(
        &mut self,
        cmd: Command,
        id: u32,
        ext: &[u8],
        body: &[u8],
    ) -> Result<Vec<u8>, SessionError> {
        if self.state != State::LoggedIn {
            return Err(SessionError::WrongState("not logged in"));
        }
        let (e, b) = self.encrypted(ext, body);
        Ok(encode_message(cmd.number(), id, Class::Client, &e, &b)?)
    }

    /// Builds a keepalive (command 93, empty body).
    pub fn keepalive(&mut self) -> Result<Vec<u8>, SessionError> {
        let id = self.alloc_id()?;
        self.send(Command::Keepalive, id, &[], &[])
    }

    /// Builds a read-only info query: 80, 146, or 199 (empty body), or 318
    /// (needs `channel`). Any other command is refused. Returns the message
    /// ID the reply will carry, and the bytes to send.
    pub fn query_info(
        &mut self,
        cmd: Command,
        channel: Option<u8>,
    ) -> Result<(u32, Vec<u8>), SessionError> {
        let ext = match cmd {
            Command::VersionInfo | Command::StreamInfoList | Command::Support => String::new(),
            Command::ChannelVersionInfo => {
                let ch = channel.ok_or(SessionError::WrongState("channel required"))?;
                xml::channel_extension(ch)
            }
            other => return Err(SessionError::CommandNotAllowed(other.number())),
        };
        let id = self.alloc_id()?;
        Ok((id, self.send(cmd, id, ext.as_bytes(), &[])?))
    }

    /// Builds a preview start. Returns the message ID that identifies this
    /// stream in received media, and the bytes to send.
    pub fn start_preview(
        &mut self,
        channel: u8,
        tier: StreamTier,
    ) -> Result<(u32, Vec<u8>), SessionError> {
        let id = self.alloc_id()?;
        let body = xml::preview_start_body(channel, tier, PreviewPolicy::NewestIFrame);
        let bytes = self.send(Command::PreviewStart, id, &[], body.as_bytes())?;
        self.previews.insert(
            id,
            Preview {
                channel,
                tier,
                saw_media: false,
            },
        );
        Ok((id, bytes))
    }

    /// Builds the stop for the preview started with `start_id`, and forgets
    /// the stream; later media for it is ignored.
    pub fn stop_preview(&mut self, start_id: u32) -> Result<Vec<u8>, SessionError> {
        let p = self
            .previews
            .get(&start_id)
            .ok_or(SessionError::UnknownStream(start_id))?;
        let body = xml::preview_stop_body(p.channel, p.tier);
        let id = self.alloc_id()?;
        let bytes = self.send(Command::PreviewStop, id, &[], body.as_bytes())?;
        self.previews.remove(&start_id);
        Ok(bytes)
    }

    fn key(&self) -> Result<&SessionKey, SessionError> {
        self.key
            .as_ref()
            .ok_or(SessionError::WrongState("no session key"))
    }

    fn decrypt_text(&self, bytes: &[u8]) -> Result<String, SessionError> {
        let mut v = bytes.to_vec();
        self.key()?.decrypt(&mut v);
        utf8(v)
    }

    /// Plain or XOR-obfuscated XML text of a pre-encryption message, if any.
    fn clear_text(body: &[u8], message_id: u32) -> Option<String> {
        if let Ok(s) = std::str::from_utf8(body) {
            if looks_like_xml(s) {
                return Some(s.to_owned());
            }
        }
        let mut v = body.to_vec();
        xor_obfuscate(message_id, &mut v);
        String::from_utf8(v).ok().filter(|s| looks_like_xml(s))
    }

    /// Handles one received message.
    pub fn handle(&mut self, msg: &RawMessage) -> Result<Event, SessionError> {
        if msg.offset_out_of_range() {
            return Err(SessionError::Malformed);
        }
        let h = &msg.header;
        match self.state {
            State::HandshakeSent if h.command == 1 => {
                let text =
                    Self::clear_text(&msg.body, h.message_id).ok_or(SessionError::BadText)?;
                let nonce = xml::parse_nonce(&text)?;
                self.key = Some(SessionKey::derive(&nonce, &self.password));
                self.nonce = Some(nonce);
                self.state = State::NonceReceived;
                Ok(Event::NonceReceived)
            }
            State::LoginSent if h.command == 1 => {
                if h.status == 200 {
                    self.state = State::LoggedIn;
                    Ok(Event::LoginOk)
                } else {
                    self.state = State::Failed;
                    let remain_times = Self::clear_text(&msg.body, h.message_id)
                        .and_then(|t| xml::parse_remain_times(&t).ok().flatten());
                    Ok(Event::LoginRejected {
                        status: h.status,
                        remain_times,
                    })
                }
            }
            State::LoggedIn => self.handle_logged_in(msg),
            _ => Err(SessionError::Unexpected { command: h.command }),
        }
    }

    fn handle_logged_in(&mut self, msg: &RawMessage) -> Result<Event, SessionError> {
        let h = msg.header;
        let ext = msg.extension();
        let payload = msg.payload();
        let control = |xml: String, payload: Vec<u8>| Event::Control {
            command: h.command,
            message_id: h.message_id,
            status: h.status,
            xml,
            payload,
        };
        if h.command == Command::PreviewStart.number() {
            let Some(preview) = self.previews.get_mut(&h.message_id) else {
                return Ok(Event::Ignored {
                    command: h.command,
                    message_id: h.message_id,
                });
            };
            if msg.body.is_empty() {
                return Ok(control(String::new(), Vec::new()));
            }
            if ext.is_empty() {
                if preview.saw_media {
                    return Ok(Event::Media {
                        message_id: h.message_id,
                        data: payload.to_vec(),
                    });
                }
                let xml = self.decrypt_text(payload)?;
                return Ok(control(xml, Vec::new()));
            }
            let xml = self.decrypt_text(ext)?;
            if payload.is_empty() {
                return Ok(control(xml, Vec::new()));
            }
            let info = xml::parse_extension(&xml)?;
            let mut data = payload.to_vec();
            if let Some(n) = info.encrypt_len {
                let n = (n as usize).min(data.len());
                self.key()?.decrypt(&mut data[..n]);
            }
            if let Some(p) = self.previews.get_mut(&h.message_id) {
                p.saw_media = true;
            }
            return Ok(Event::Media {
                message_id: h.message_id,
                data,
            });
        }
        if msg.body.is_empty() {
            return Ok(control(String::new(), Vec::new()));
        }
        if ext.is_empty() {
            Ok(control(self.decrypt_text(payload)?, Vec::new()))
        } else {
            Ok(control(self.decrypt_text(ext)?, payload.to_vec()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framer::{Decoder, encode_message_with_status};
    use crate::media::{Codec, Demuxer, Record, encode_audio, encode_info, encode_video};
    use crate::xml::{find_text, parse_extension};

    const USER: &str = "example-user";
    const PASSWORD: &str = "example-password";
    const NONCE: &str = "example-nonce-0001";

    fn decode_one(bytes: &[u8]) -> RawMessage {
        let mut d = Decoder::new();
        d.push(bytes);
        let m = d.next_message().unwrap().unwrap();
        assert_eq!(d.buffered(), 0);
        m
    }

    /// Device side: reply to the handshake with the nonce.
    fn device_nonce_reply(request: &RawMessage) -> Vec<u8> {
        let mut body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<body>\n<Encryption version=\"1.1\">\n\
             <type>md5</type>\n<nonce>{NONCE}</nonce>\n</Encryption>\n</body>\n"
        )
        .into_bytes();
        xor_obfuscate(request.header.message_id, &mut body);
        encode_message_with_status(
            1,
            request.header.message_id,
            200,
            Class::DeviceHandshake,
            &[],
            &body,
        )
        .unwrap()
    }

    fn logged_in() -> (Session, SessionKey) {
        let mut s = Session::new(USER, PASSWORD);
        let hs = decode_one(&s.handshake().unwrap());
        assert_eq!(hs.header.command, 1);
        assert_eq!(hs.header.class, Class::ClientHandshake);
        assert!(hs.body.is_empty());
        assert_eq!(
            s.handle(&decode_one(&device_nonce_reply(&hs))).unwrap(),
            Event::NonceReceived
        );
        let login = decode_one(&s.login().unwrap());
        let ok =
            encode_message_with_status(1, login.header.message_id, 200, Class::Device, &[], &[])
                .unwrap();
        assert_eq!(s.handle(&decode_one(&ok)).unwrap(), Event::LoginOk);
        assert!(s.is_logged_in());
        (s, SessionKey::derive(NONCE, PASSWORD))
    }

    #[test]
    fn login_transcript() {
        let mut s = Session::new(USER, PASSWORD);
        assert_eq!(
            s.login(),
            Err(SessionError::WrongState("no nonce received"))
        );
        assert!(s.keepalive().is_err());
        let hs_bytes = s.handshake().unwrap();
        assert_eq!(
            hs_bytes,
            [
                0xf0, 0xde, 0xbc, 0x0a, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x12, 0xdc, 0x14, 0x65
            ]
        );
        let hs = decode_one(&hs_bytes);
        assert!(s.handshake().is_err());
        assert_eq!(
            s.handle(&decode_one(&device_nonce_reply(&hs))).unwrap(),
            Event::NonceReceived
        );
        // Device side recovers the credential hashes.
        let login_bytes = s.login().unwrap();
        let login = decode_one(&login_bytes);
        assert_eq!(&login_bytes[..4], &[0xf0, 0xde, 0xbc, 0x0a]);
        assert_eq!(login_bytes[4..8], 1u32.to_le_bytes());
        assert_eq!(login_bytes[8..12], (login.body.len() as u32).to_le_bytes());
        assert_eq!(login_bytes[12..16], [0, 0, 0, 0]);
        assert_eq!(login_bytes[16..24], [0, 0, 0x14, 0x64, 0, 0, 0, 0]);
        assert_eq!(login.header.message_id, 0);
        let mut body = login.body.clone();
        xor_obfuscate(login.header.message_id, &mut body);
        let text = String::from_utf8(body).unwrap();
        assert!(text.starts_with(xml::XML_DECLARATION));
        assert!(text.contains("<type>LAN</type>\n<udpPort>0</udpPort>"));
        assert_eq!(
            find_text(&text, "userName").unwrap().unwrap(),
            credential_hash(USER, NONCE)
        );
        assert_eq!(
            find_text(&text, "password").unwrap().unwrap(),
            credential_hash(PASSWORD, NONCE)
        );
        assert!(!text.contains(PASSWORD) && !text.contains(USER));
        // Success.
        let ok =
            encode_message_with_status(1, login.header.message_id, 200, Class::Device, &[], &[])
                .unwrap();
        assert_eq!(s.handle(&decode_one(&ok)).unwrap(), Event::LoginOk);

        // Encrypted control exchange, both directions.
        let key = SessionKey::derive(NONCE, PASSWORD);
        let ka = decode_one(&s.keepalive().unwrap());
        assert_eq!((ka.header.command, ka.header.class), (93, Class::Client));
        assert_eq!((ka.header.message_id, ka.header.status), (1, 0));
        assert!(ka.body.is_empty());
        let mut reply = b"<body><LinkType><type>LAN</type></LinkType></body>".to_vec();
        key.encrypt(&mut reply);
        let reply =
            encode_message_with_status(93, ka.header.message_id, 200, Class::Device, &[], &reply)
                .unwrap();
        match s.handle(&decode_one(&reply)).unwrap() {
            Event::Control {
                command: 93,
                message_id,
                status: 200,
                xml,
                payload,
            } => {
                assert_eq!(message_id, ka.header.message_id);
                assert!(xml.contains("<type>LAN</type>"));
                assert!(payload.is_empty());
            }
            other => panic!("{other:?}"),
        }

        // Outgoing encrypted body decrypts on the device side.
        let (id, bytes) = s.start_preview(3, StreamTier::Sub).unwrap();
        let start = decode_one(&bytes);
        assert_eq!((start.header.command, start.header.message_id), (3, id));
        let mut b = start.body.clone();
        key.decrypt(&mut b);
        let text = String::from_utf8(b).unwrap();
        assert_eq!(find_text(&text, "handle").unwrap().as_deref(), Some("259"));
        assert_eq!(
            find_text(&text, "streamType").unwrap().as_deref(),
            Some("subStream")
        );
        let stop = decode_one(&s.stop_preview(id).unwrap());
        assert_eq!(stop.header.command, 4);
        assert_ne!(stop.header.message_id, id);
        let mut b = stop.body.clone();
        key.decrypt(&mut b);
        assert_eq!(
            find_text(&String::from_utf8(b).unwrap(), "handle")
                .unwrap()
                .as_deref(),
            Some("259")
        );
        assert_eq!(s.stop_preview(id), Err(SessionError::UnknownStream(id)));
    }

    #[test]
    fn login_rejected() {
        let mut s = Session::new(USER, "wrong");
        let hs = decode_one(&s.handshake().unwrap());
        s.handle(&decode_one(&device_nonce_reply(&hs))).unwrap();
        let login = decode_one(&s.login().unwrap());
        let mut body =
            b"<body><LoginErrInfo><remainTimes>2</remainTimes></LoginErrInfo></body>".to_vec();
        xor_obfuscate(login.header.message_id, &mut body);
        let r =
            encode_message_with_status(1, login.header.message_id, 401, Class::Device, &[], &body)
                .unwrap();
        assert_eq!(
            s.handle(&decode_one(&r)).unwrap(),
            Event::LoginRejected {
                status: 401,
                remain_times: Some(2)
            }
        );
        assert!(!s.is_logged_in());
        assert!(s.start_preview(0, StreamTier::Main).is_err());
    }

    #[test]
    fn allowlist() {
        for n in [1, 3, 4, 80, 93, 145, 146, 199, 318] {
            assert!(Command::is_allowed(n));
        }
        for n in [0, 2, 5, 10, 44, 58, 76, 102, 14, 15, 16, 142, 272, 511, 600] {
            assert!(!Command::is_allowed(n), "{n}");
        }
        let (mut s, key) = logged_in();
        assert_eq!(
            s.query_info(Command::PreviewStart, None),
            Err(SessionError::CommandNotAllowed(3))
        );
        assert_eq!(
            s.query_info(Command::Login, None),
            Err(SessionError::CommandNotAllowed(1))
        );
        assert!(s.query_info(Command::ChannelVersionInfo, None).is_err());
        for c in [
            Command::VersionInfo,
            Command::StreamInfoList,
            Command::Support,
        ] {
            let m = decode_one(&s.query_info(c, None).unwrap().1);
            assert_eq!(m.header.command, c.number());
        }
        let m = decode_one(
            &s.query_info(Command::ChannelVersionInfo, Some(4))
                .unwrap()
                .1,
        );
        assert_eq!(m.header.command, 318);
        assert_eq!(m.header.payload_offset, Some(m.extension().len() as u32));
        let mut e = m.extension().to_vec();
        key.decrypt(&mut e);
        assert_eq!(
            find_text(&String::from_utf8(e).unwrap(), "channelId")
                .unwrap()
                .as_deref(),
            Some("4")
        );
    }

    fn ext_xml(encrypt_len: u32) -> Vec<u8> {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<Extension version=\"1.1\">\n\
             <binaryData>1</binaryData>\n<encryptLen>{encrypt_len}</encryptLen>\n</Extension>\n"
        )
        .into_bytes()
    }

    #[test]
    fn media_through_session_and_demuxer() {
        let (mut s, key) = logged_in();
        let (id, _) = s.start_preview(1, StreamTier::Main).unwrap();
        let (id2, _) = s.start_preview(2, StreamTier::Sub).unwrap();

        // Start reply (empty 200), then media.
        let ack = encode_message_with_status(3, id, 200, Class::Device, &[], &[]).unwrap();
        assert!(matches!(
            s.handle(&decode_one(&ack)).unwrap(),
            Event::Control { status: 200, .. }
        ));

        // Synthetic BCMEDIA stream.
        let mut stream = encode_info(3840, 2160, 25);
        let mut expect = vec![Record::Info {
            width: 3840,
            height: 2160,
            fps: 25,
        }];
        let key_nal: Vec<u8> = (0..3000u32).map(|i| (i * 7 + 1) as u8).collect();
        stream.extend(encode_video(0, true, Codec::H265, 100, &[], &key_nal));
        expect.push(Record::Video {
            codec: Codec::H265,
            keyframe: true,
            pts_us: 100,
            data: key_nal,
        });
        for i in 0..6u64 {
            let p: Vec<u8> = (0..(50 + i * 131) as usize)
                .map(|x| (x as u64 * (i + 3)) as u8)
                .collect();
            stream.extend(encode_video(0, false, Codec::H265, 200 + i, &[], &p));
            expect.push(Record::Video {
                codec: Codec::H265,
                keyframe: false,
                pts_us: 200 + i,
                data: p,
            });
            let a: Vec<u8> = (0..(20 + i as usize * 3)).map(|x| x as u8 ^ 0x5a).collect();
            stream.extend(encode_audio(5, &a));
            expect.push(Record::Audio {
                stream_index: 5,
                data: a,
            });
        }

        // Split into messages: the first carries an extension and encrypts
        // its first 1024 payload bytes; the rest are plain continuations.
        // Cut points deliberately fall inside records.
        let cuts = [1500usize, 2999, 3001, 4000, 5555];
        let mut parts: Vec<&[u8]> = Vec::new();
        let mut last = 0;
        for c in cuts.iter().copied().chain([stream.len()]) {
            parts.push(&stream[last..c.min(stream.len())]);
            last = c.min(stream.len());
        }
        let mut wire = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            if i == 0 {
                let mut ext = ext_xml(1024);
                key.encrypt(&mut ext);
                let mut p = part.to_vec();
                key.encrypt(&mut p[..1024]);
                wire.extend(
                    encode_message_with_status(3, id, 200, Class::Device, &ext, &p).unwrap(),
                );
            } else {
                wire.extend(
                    encode_message_with_status(3, id, 200, Class::Device, &[], part).unwrap(),
                );
            }
        }
        // Interleave an unrelated stream's message to check demultiplexing.
        let other = encode_audio(1, &[1, 2, 3]);
        let mut ext = ext_xml(0);
        key.encrypt(&mut ext);
        wire.extend(encode_message_with_status(3, id2, 200, Class::Device, &ext, &other).unwrap());

        let mut dec = Decoder::new();
        let mut demux = Demuxer::new();
        let mut demux2 = Demuxer::new();
        let mut got = Vec::new();
        let mut got2 = Vec::new();
        for chunk in wire.chunks(777) {
            dec.push(chunk);
            while let Some(m) = dec.next_message().unwrap() {
                match s.handle(&m).unwrap() {
                    Event::Media { message_id, data } if message_id == id => {
                        demux.push(&data);
                        while let Some(r) = demux.next_record().unwrap() {
                            got.push(r);
                        }
                    }
                    Event::Media { message_id, data } if message_id == id2 => {
                        demux2.push(&data);
                        while let Some(r) = demux2.next_record().unwrap() {
                            got2.push(r);
                        }
                    }
                    other => panic!("{other:?}"),
                }
            }
        }
        assert_eq!(got, expect);
        assert_eq!(
            got2,
            vec![Record::Audio {
                stream_index: 1,
                data: vec![1, 2, 3]
            }]
        );

        // After stop, trailing media is ignored.
        s.stop_preview(id).unwrap();
        let late = encode_message_with_status(3, id, 200, Class::Device, &[], b"junk").unwrap();
        assert!(matches!(
            s.handle(&decode_one(&late)).unwrap(),
            Event::Ignored { .. }
        ));
        assert_eq!(
            parse_extension(std::str::from_utf8(&ext_xml(7)).unwrap())
                .unwrap()
                .encrypt_len,
            Some(7)
        );
    }

    #[test]
    fn media_without_encrypt_len_is_plain_and_oversized_encrypt_len_clamps() {
        let (mut s, key) = logged_in();
        let (id, _) = s.start_preview(0, StreamTier::Sub).unwrap();
        let mut ext = b"<Extension version=\"1.1\"><binaryData>1</binaryData></Extension>".to_vec();
        key.encrypt(&mut ext);
        let m = encode_message_with_status(3, id, 200, Class::Device, &ext, b"plain!").unwrap();
        assert_eq!(
            s.handle(&decode_one(&m)).unwrap(),
            Event::Media {
                message_id: id,
                data: b"plain!".to_vec()
            }
        );
        let mut ext = ext_xml(5000);
        key.encrypt(&mut ext);
        let mut p = b"short".to_vec();
        key.encrypt(&mut p);
        let m = encode_message_with_status(3, id, 200, Class::Device, &ext, &p).unwrap();
        assert_eq!(
            s.handle(&decode_one(&m)).unwrap(),
            Event::Media {
                message_id: id,
                data: b"short".to_vec()
            }
        );
    }

    #[test]
    fn wrong_key_is_reported() {
        let (mut s, _) = logged_in();
        let reply =
            encode_message_with_status(93, 1, 200, Class::Device, &[], &[0xffu8; 40]).unwrap();
        assert_eq!(s.handle(&decode_one(&reply)), Err(SessionError::BadText));
    }

    #[test]
    fn debug_does_not_leak_credentials() {
        let s = Session::new(USER, PASSWORD);
        let d = format!("{s:?}");
        assert!(!d.contains(USER) && !d.contains(PASSWORD));
    }
}
