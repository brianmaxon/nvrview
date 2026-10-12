//! Sans-I/O session: builds outgoing messages and turns received messages
//! into typed events.
//!
//! The caller owns the socket. Typical use: send [`Session::handshake`], feed
//! every received [`RawMessage`] to [`Session::handle`], send
//! [`Session::login`] after [`Event::NonceReceived`], and once
//! [`Event::LoginOk`] arrives start previews and send keepalives.
//!
//! Only commands on the allowlist ([`Command`]) can be produced. Each has a
//! [`Tier`]: viewing commands are always allowed; motion commands (camera
//! motion control) only through their own builders, each from a fixed
//! template: talk-back only for a channel the caller enabled with
//! [`Session::set_motion_allowed`], and pan, tilt, presets and lens moves
//! only for a channel whose own capabilities (199, as read on this session)
//! offer them ([`Session::ptz_move`] and its siblings). Nothing that changes
//! device settings is on the list.

use crate::archive::{
    self, CoverRequest, DateTime, DayRecordChannel, DownloadRequest, EventSearch, FileSearch,
    PlayRequest,
};
use crate::crypto::{SessionKey, credential_hash, xor_obfuscate};
use crate::framer::{EncodeError, RawMessage, encode_message, encode_message_with_status};
use crate::header::Class;
use crate::ptz::{self, Cam, Direction, LensAxis, Preset, ZoomFocus};
use crate::stream::StreamTier;
use crate::talk::{self, TalkConfig};
use crate::xml::{self, PreviewPolicy, XmlError};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

/// Commands this crate can send (plus 145, which devices push). Anything
/// that could change device settings is deliberately absent. See
/// [`Command::tier`] for when each may be sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// 1: handshake and login.
    Login = 1,
    /// 2: logout, sent before closing a logged-in session. It carries no
    /// credentials and changes no settings.
    Logout = 2,
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
    /// 14: recording file search, open (viewing tier: read-only).
    FileSearchOpen = 14,
    /// 15: recording file search, fetch the next page.
    FileSearchNext = 15,
    /// 16: recording file search, close.
    FileSearchClose = 16,
    /// 142: which days have recordings (one request per month).
    DayRecords = 142,
    /// 143: download a time range of recordings (copies, changes nothing).
    Download = 143,
    /// 144: stop a download.
    DownloadStop = 144,
    /// 272: motion and detection event search, open.
    EventSearchOpen = 272,
    /// 273: event search, fetch the next page.
    EventSearchNext = 273,
    /// 274: event search, close.
    EventSearchClose = 274,
    /// 298: cover image (one keyframe) for a time range.
    Cover = 298,
    /// 123: where the next play starts (viewing tier: positions a replay
    /// on this session, changes nothing on the device).
    ReplaySeek = 123,
    /// 5: play a recording file (viewing tier).
    PlayFile = 5,
    /// 7: stop a play.
    PlayStop = 7,
    /// 192: start alarm report: subscribes this session to the device's
    /// detection pushes (33). Viewing tier: it changes no settings.
    AlarmReport = 192,
    /// 10: a channel's talk-back ability (viewing tier: a read-only query).
    TalkAbility = 10,
    /// 201: open talk-back (motion tier).
    TalkOpen = 201,
    /// 202: one talk-back audio record (motion tier).
    TalkStream = 202,
    /// 11: close talk-back (motion tier).
    TalkClose = 11,
    /// 18: pan or tilt, and stop (motion tier; only the four observed
    /// directions and `stop` can be built).
    PtzControl = 18,
    /// 19: recall a preset (motion tier; only `toPos` can be built: the
    /// same number saves and deletes presets, which is never built).
    PtzPresetRecall = 19,
    /// 295: move the lens zoom or focus to a position (motion tier).
    PtzLensMove = 295,
    /// 294: lens zoom and focus ranges and positions (viewing tier).
    PtzZoomFocus = 294,
    /// 190: the preset list (viewing tier).
    PtzPresets = 190,
    /// 340: the pan and tilt self-test state (viewing tier).
    PtzSelfTest = 340,
}

/// When a command may be sent (docs/ARCHITECTURE.md, "Device commands").
/// There is no configuration tier: such commands are not on the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Read-only: always allowed.
    Viewing,
    /// Camera motion control: talk-back only for a channel the caller
    /// enabled ([`Session::set_motion_allowed`]); pan, tilt, preset recall
    /// and lens moves only where the channel's capabilities offer them.
    /// The caller adds the origin checks, rate limits and logging
    /// (docs/ARCHITECTURE.md, "Device commands").
    Motion,
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
            2 => Command::Logout,
            3 => Command::PreviewStart,
            4 => Command::PreviewStop,
            80 => Command::VersionInfo,
            93 => Command::Keepalive,
            145 => Command::ChannelInfoList,
            146 => Command::StreamInfoList,
            199 => Command::Support,
            318 => Command::ChannelVersionInfo,
            14 => Command::FileSearchOpen,
            15 => Command::FileSearchNext,
            16 => Command::FileSearchClose,
            142 => Command::DayRecords,
            143 => Command::Download,
            144 => Command::DownloadStop,
            272 => Command::EventSearchOpen,
            273 => Command::EventSearchNext,
            274 => Command::EventSearchClose,
            298 => Command::Cover,
            123 => Command::ReplaySeek,
            5 => Command::PlayFile,
            7 => Command::PlayStop,
            192 => Command::AlarmReport,
            10 => Command::TalkAbility,
            201 => Command::TalkOpen,
            202 => Command::TalkStream,
            11 => Command::TalkClose,
            18 => Command::PtzControl,
            19 => Command::PtzPresetRecall,
            295 => Command::PtzLensMove,
            294 => Command::PtzZoomFocus,
            190 => Command::PtzPresets,
            340 => Command::PtzSelfTest,
            _ => return None,
        })
    }

    /// The command's tier.
    pub const fn tier(self) -> Tier {
        match self {
            Command::TalkOpen
            | Command::TalkStream
            | Command::TalkClose
            | Command::PtzControl
            | Command::PtzPresetRecall
            | Command::PtzLensMove => Tier::Motion,
            _ => Tier::Viewing,
        }
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
    /// A motion-tier command for a channel the caller has not enabled.
    MotionNotEnabled(u8),
    /// A camera motion command the channel's capabilities (199) do not
    /// offer.
    MotionNotSupported(u8),
    /// A camera motion command that needs a fact this session has not read
    /// from the device yet (capabilities, lens ranges, or presets).
    MotionStateUnknown(&'static str),
    /// A preset recall for an id the channel's preset list (190) lacks.
    UnknownPreset(u32),
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
    /// The preview message ID is not an active preview (or the download
    /// message ID not an active download).
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
            SessionError::MotionNotEnabled(c) => {
                write!(f, "camera motion control is not enabled for channel {c}")
            }
            SessionError::MotionNotSupported(c) => {
                write!(f, "channel {c} does not offer this camera motion")
            }
            SessionError::MotionStateUnknown(what) => write!(f, "{what} not read yet"),
            SessionError::UnknownPreset(id) => write!(f, "no preset {id} on this channel"),
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

/// The message that ended a play without the play's `seq` (see
/// [`Event::TransferEnd`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ForeignEnd {
    /// Header bytes 16 to 19, little-endian: a status and class on most
    /// messages, a replay `seq` on replay media.
    pub bytes_16_19: u32,
    /// The `seq` the play's media carry.
    pub expected_seq: Option<u32>,
    /// Body length of that message.
    pub body_len: usize,
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
        /// Seconds until a lockout ends, when the device says so.
        unlock_secs: Option<u32>,
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
    /// A download (143) or cover-image (298) reply stream has ended: the
    /// device's empty end message (status 300 when observed), or a refusal.
    /// Later messages with this ID are ignored.
    TransferEnd {
        /// Command number (143 or 298).
        command: u32,
        /// Message ID of the request.
        message_id: u32,
        /// Status of the end message.
        status: u16,
        /// For a play (5) that ended on a message without its `seq`: that
        /// message's raw header bytes 16 to 19 and body length, kept for
        /// diagnosis (how a device ends or refuses a play is not
        /// established, docs/MILESTONE-P.md, B6).
        foreign: Option<ForeignEnd>,
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

/// A request whose reply is a media stream that ends by itself: a download
/// (143) or a cover image (298).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransferKind {
    Download,
    Cover,
    /// A play (5); its media carry the 123 `seq`.
    Play,
}

struct Transfer {
    kind: TransferKind,
    saw_media: bool,
    /// For a play, the `seq` of the seek before it.
    seq: Option<u32>,
}

/// How many recent seek `seq` numbers a session remembers for routing play
/// media.
const SEEK_MEMORY: usize = 32;

/// One session with one device.
pub struct Session {
    user: String,
    password: String,
    state: State,
    nonce: Option<String>,
    key: Option<SessionKey>,
    next_id: u32,
    previews: HashMap<u32, Preview>,
    transfers: HashMap<u32, Transfer>,
    /// The `seq` of the latest seeks (at most [`SEEK_MEMORY`]): a device
    /// was observed to tag a running play's media with the `seq` of a later
    /// seek (protocol.md, "Live view and plays on one session").
    seeks: VecDeque<u32>,
    /// Channels the caller enabled motion-tier commands for.
    motion: HashSet<u8>,
    /// Channels with a talk opened (201 sent) and not yet closed.
    talks: HashSet<u8>,
    /// Camera motion facts read from the device on this session.
    ptz: PtzFacts,
}

/// Most read-only queries whose replies the session remembers to learn
/// from (199, 294, 190); older ones are forgotten.
const READS_REMEMBERED: usize = 64;

/// What the device reported about camera motion on this session: the
/// motion builders check against it, never against what a caller claims.
#[derive(Debug, Default)]
struct PtzFacts {
    /// Per channel, from the latest 199 reply; `None` until one arrived.
    caps: Option<HashMap<u8, PtzCaps>>,
    /// Per channel, how many lenses its 199 item lists (sub-items).
    lens_counts: HashMap<u8, usize>,
    /// Per channel, from the latest 294 reply.
    lens: HashMap<Cam, ZoomFocus>,
    /// Per channel, from the latest 190 reply.
    presets: HashMap<Cam, Vec<Preset>>,
    /// Queries sent and not answered yet, by message ID.
    reads: std::collections::BTreeMap<u32, (Command, Option<Cam>)>,
    /// Channels sent a move (18 with a direction) and no stop since.
    moving: HashSet<Cam>,
}

/// What a channel's camera can do, from its 199 item and its lenses'
/// sub-items together (a multi-lens camera's pan-and-tilt lens may only
/// be described in its sub-item; the official client addressed its moves
/// to the plain channel).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PtzCaps {
    /// Pan and tilt (`ptzType` 3).
    pub pan_tilt: bool,
    /// Presets (`ptzPreset` bit 0).
    pub presets: bool,
    /// Lens zoom and focus (`ptzType` 1 or 3).
    pub zoom_focus: bool,
}

impl PtzCaps {
    /// The capabilities of one 199 item, its lenses included.
    pub fn of(item: &crate::detection::ChannelAi) -> PtzCaps {
        let mut c = PtzCaps::default();
        for caps in std::iter::once(&item.caps).chain(item.lenses.iter().map(|l| &l.caps)) {
            c.pan_tilt |= caps.pan_tilt();
            c.presets |= caps.presets();
            c.zoom_focus |= caps.zoom_focus();
        }
        c
    }

    /// Whether the camera offers any motion.
    pub fn any(&self) -> bool {
        self.pan_tilt || self.presets || self.zoom_focus
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("state", &self.state)
            .field("previews", &self.previews.len())
            .field("transfers", &self.transfers.len())
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
            transfers: HashMap::new(),
            seeks: VecDeque::new(),
            motion: HashSet::new(),
            talks: HashSet::new(),
            ptz: PtzFacts::default(),
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

    /// Builds a logout (command 2). Send it before closing a logged-in
    /// session and wait briefly for its reply; the device then frees the
    /// login slot.
    pub fn logout(&mut self) -> Result<Vec<u8>, SessionError> {
        let id = self.alloc_id()?;
        self.send(Command::Logout, id, &[], xml::logout_body().as_bytes())
    }

    /// Builds a keepalive (command 93, empty body).
    pub fn keepalive(&mut self) -> Result<Vec<u8>, SessionError> {
        let id = self.alloc_id()?;
        self.send(Command::Keepalive, id, &[], &[])
    }

    /// Builds the alarm-report subscription (command 192, empty body), sent
    /// once after login. The device then pushes alarm state (command 33,
    /// see [`crate::detection`]) with the returned message ID.
    pub fn start_alarm_report(&mut self) -> Result<(u32, Vec<u8>), SessionError> {
        let id = self.alloc_id()?;
        Ok((id, self.send(Command::AlarmReport, id, &[], &[])?))
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
        let bytes = self.send(cmd, id, ext.as_bytes(), &[])?;
        if cmd == Command::Support {
            self.remember_read(id, cmd, None);
        }
        Ok((id, bytes))
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

    /// Message IDs of downloads (143) whose reply stream has not ended.
    pub fn active_downloads(&self) -> impl Iterator<Item = u32> + '_ {
        self.transfers
            .iter()
            .filter(|(_, t)| t.kind == TransferKind::Download)
            .map(|(id, _)| *id)
    }

    /// Checks that a follow-up step (15, 16, 273, 274) reuses an ID this
    /// session handed out.
    fn issued(&self, id: u32) -> Result<(), SessionError> {
        if self.state != State::LoggedIn {
            return Err(SessionError::WrongState("not logged in"));
        }
        if id == 0 || id >= self.next_id {
            return Err(SessionError::WrongState("message id was not issued"));
        }
        Ok(())
    }

    /// Builds a day-records query (command 142) for one window (the official
    /// client asks one month at a time). Returns the reply's message ID.
    pub fn day_records(
        &mut self,
        start: DateTime,
        end: DateTime,
        channels: &[DayRecordChannel],
    ) -> Result<(u32, Vec<u8>), SessionError> {
        let id = self.alloc_id()?;
        let body = archive::day_records_body(start, end, channels);
        Ok((
            id,
            self.send(Command::DayRecords, id, &[], body.as_bytes())?,
        ))
    }

    /// Builds a file search open (command 14). The fetch (15) and close (16)
    /// reuse the returned message ID.
    pub fn file_search_open(&mut self, q: &FileSearch) -> Result<(u32, Vec<u8>), SessionError> {
        let id = self.alloc_id()?;
        let body = archive::file_search_open_body(q);
        Ok((
            id,
            self.send(Command::FileSearchOpen, id, &[], body.as_bytes())?,
        ))
    }

    /// Builds a file search fetch (15, `close` false) or close (16, `close`
    /// true) with the open's message ID and the handle from its reply.
    pub fn file_search_step(
        &mut self,
        id: u32,
        channel: u8,
        uid: Option<&str>,
        handle: u32,
        close: bool,
    ) -> Result<Vec<u8>, SessionError> {
        self.issued(id)?;
        let cmd = if close {
            Command::FileSearchClose
        } else {
            Command::FileSearchNext
        };
        let body = archive::file_search_handle_body(channel, uid, handle);
        self.send(cmd, id, &[], body.as_bytes())
    }

    /// Builds an event search open (command 272): the body travels as the
    /// payload after a channel extension. 273 and 274 reuse the message ID.
    pub fn event_search_open(&mut self, q: &EventSearch) -> Result<(u32, Vec<u8>), SessionError> {
        let id = self.alloc_id()?;
        let ext = archive::event_search_extension(q.channel);
        let body = archive::event_search_open_body(q);
        Ok((
            id,
            self.send(
                Command::EventSearchOpen,
                id,
                ext.as_bytes(),
                body.as_bytes(),
            )?,
        ))
    }

    /// Builds an event search page fetch (273, `close` false) or close (274,
    /// `close` true) with the open's message ID and the `fileHandle` from
    /// its reply.
    pub fn event_search_step(
        &mut self,
        id: u32,
        channel: u8,
        file_handle: u32,
        close: bool,
    ) -> Result<Vec<u8>, SessionError> {
        self.issued(id)?;
        let cmd = if close {
            Command::EventSearchClose
        } else {
            Command::EventSearchNext
        };
        let ext = archive::event_search_extension(channel);
        let body = archive::event_search_handle_body(channel, file_handle);
        self.send(cmd, id, ext.as_bytes(), body.as_bytes())
    }

    /// Builds a cover-image request (command 298). Its replies (an empty
    /// acknowledgment, media, then an empty status-300 end) carry the
    /// returned message ID.
    pub fn cover(&mut self, q: &CoverRequest) -> Result<(u32, Vec<u8>), SessionError> {
        let id = self.alloc_id()?;
        let body = archive::cover_body(q);
        let bytes = self.send(Command::Cover, id, &[], body.as_bytes())?;
        self.transfers.insert(
            id,
            Transfer {
                kind: TransferKind::Cover,
                saw_media: false,
                seq: None,
            },
        );
        Ok((id, bytes))
    }

    /// Builds a download request (command 143). Its replies (a size estimate,
    /// media, then an empty end) carry the returned message ID.
    pub fn start_download(&mut self, q: &DownloadRequest) -> Result<(u32, Vec<u8>), SessionError> {
        let id = self.alloc_id()?;
        let body = archive::download_body(q);
        let bytes = self.send(Command::Download, id, &[], body.as_bytes())?;
        self.transfers.insert(
            id,
            Transfer {
                kind: TransferKind::Download,
                saw_media: false,
                seq: None,
            },
        );
        Ok((id, bytes))
    }

    /// Builds the stop (command 144, empty body) for the download started
    /// with `start_id`, and forgets it; later media for it is ignored.
    ///
    /// The 144 carries the download's own message ID: its body is empty, so
    /// the ID is the only thing that can name the download. (Which ID the
    /// official client used is not recorded in docs/protocol.md.)
    pub fn stop_download(&mut self, start_id: u32) -> Result<Vec<u8>, SessionError> {
        match self.transfers.get(&start_id) {
            Some(t) if t.kind == TransferKind::Download => {}
            _ => return Err(SessionError::UnknownStream(start_id)),
        }
        let bytes = self.send(Command::DownloadStop, start_id, &[], &[])?;
        self.transfers.remove(&start_id);
        Ok(bytes)
    }

    /// Builds a seek (command 123): the next play starts at the keyframe at
    /// or before `at`, and its media carry `seq`. An ordinary control reply
    /// (empty, status 200) answers it.
    pub fn replay_seek(&mut self, seq: u32, at: DateTime) -> Result<(u32, Vec<u8>), SessionError> {
        let id = self.alloc_id()?;
        let body = archive::replay_seek_body(seq, at);
        let bytes = self.send(Command::ReplaySeek, id, &[], body.as_bytes())?;
        if self.seeks.len() == SEEK_MEMORY {
            self.seeks.pop_front();
        }
        self.seeks.push_back(seq);
        Ok((id, bytes))
    }

    /// Builds a play (command 5) of one recording file at the request's speed,
    /// after a [`Session::replay_seek`] with the same `seq`. Its replies
    /// (a first reply with a stream-info record, then media whose headers
    /// carry `seq`) come with the returned message ID.
    pub fn start_play(
        &mut self,
        q: &PlayRequest,
        seq: u32,
    ) -> Result<(u32, Vec<u8>), SessionError> {
        let id = self.alloc_id()?;
        let body = archive::play_body(q);
        let bytes = self.send(Command::PlayFile, id, &[], body.as_bytes())?;
        self.transfers.insert(
            id,
            Transfer {
                kind: TransferKind::Play,
                saw_media: false,
                seq: Some(seq),
            },
        );
        Ok((id, bytes))
    }

    /// Builds the stop (command 7) of the play started with `play_id`, and
    /// forgets the play; later media for it is ignored. The stop names the
    /// file in its body, so it goes with a message ID of its own (which ID
    /// the official client used is not recorded).
    pub fn stop_play(&mut self, play_id: u32, q: &PlayRequest) -> Result<Vec<u8>, SessionError> {
        match self.transfers.get(&play_id) {
            Some(t) if t.kind == TransferKind::Play => {}
            _ => return Err(SessionError::UnknownStream(play_id)),
        }
        let id = self.alloc_id()?;
        let body = archive::play_stop_body(q);
        let bytes = self.send(Command::PlayStop, id, &[], body.as_bytes())?;
        self.transfers.remove(&play_id);
        Ok(bytes)
    }

    /// Message IDs of running plays.
    pub fn active_plays(&self) -> impl Iterator<Item = u32> + '_ {
        self.transfers
            .iter()
            .filter(|(_, t)| t.kind == TransferKind::Play)
            .map(|(id, _)| *id)
    }

    /// Enables or disables motion-tier commands (talk-back) for a channel.
    /// Off for every channel until enabled. Disabling does not close a
    /// talk; [`Session::talk_close`] stays possible so the caller can.
    pub fn set_motion_allowed(&mut self, channel: u8, allowed: bool) {
        if allowed {
            self.motion.insert(channel);
        } else {
            self.motion.remove(&channel);
        }
    }

    /// Whether motion-tier commands are enabled for `channel`.
    pub fn motion_allowed(&self, channel: u8) -> bool {
        self.motion.contains(&channel)
    }

    fn check_motion(&self, channel: u8) -> Result<(), SessionError> {
        if self.state != State::LoggedIn {
            return Err(SessionError::WrongState("not logged in"));
        }
        if !self.motion.contains(&channel) {
            return Err(SessionError::MotionNotEnabled(channel));
        }
        Ok(())
    }

    /// Builds a talk ability query (command 10, viewing tier) for a
    /// channel; parse the reply with [`talk::parse_talk_ability`].
    pub fn talk_ability(&mut self, channel: u8) -> Result<(u32, Vec<u8>), SessionError> {
        let id = self.alloc_id()?;
        let ext = talk::ability_extension(channel);
        Ok((
            id,
            self.send(Command::TalkAbility, id, ext.as_bytes(), &[])?,
        ))
    }

    /// Builds a talk open (command 201, motion tier) with a body made from
    /// the fixed template ([`talk::open_body`]). Refused unless motion is
    /// enabled for the channel and no talk is open on it. The reply (empty,
    /// status 200 when accepted) carries the returned message ID.
    pub fn talk_open(
        &mut self,
        channel: u8,
        cfg: &TalkConfig,
    ) -> Result<(u32, Vec<u8>), SessionError> {
        self.check_motion(channel)?;
        if self.talks.contains(&channel) {
            return Err(SessionError::WrongState(
                "talk already open on this channel",
            ));
        }
        let id = self.alloc_id()?;
        let ext = talk::channel_extension(channel);
        let body = talk::open_body(channel, cfg);
        let bytes = self.send(Command::TalkOpen, id, ext.as_bytes(), body.as_bytes())?;
        self.talks.insert(channel);
        Ok((id, bytes))
    }

    /// Builds one talk audio message (command 202, motion tier): message ID
    /// 0, the stream extension (encrypted), and a plain payload of one
    /// `01wb` record holding `payload` (one record from
    /// [`talk::AdpcmEncoder::encode_record`]). Refused unless a talk is open
    /// on the channel and motion is still enabled for it.
    pub fn talk_audio(&mut self, channel: u8, payload: &[u8]) -> Result<Vec<u8>, SessionError> {
        self.check_motion(channel)?;
        if !self.talks.contains(&channel) {
            return Err(SessionError::WrongState("no talk open on this channel"));
        }
        if payload.len() != talk::RECORD_PAYLOAD_LEN {
            return Err(SessionError::WrongState("talk record has the wrong length"));
        }
        let mut ext = talk::stream_extension(channel).into_bytes();
        if let Some(key) = &self.key {
            key.encrypt(&mut ext);
        }
        let record = crate::media::encode_audio(talk::STREAM_INDEX, payload);
        Ok(encode_message(
            Command::TalkStream.number(),
            0,
            Class::Client,
            &ext,
            &record,
        )?)
    }

    /// Builds the talk close (command 11, extension only) for the channel's
    /// open talk, and forgets it. Allowed even after motion was disabled,
    /// so a talk can always be closed.
    pub fn talk_close(&mut self, channel: u8) -> Result<(u32, Vec<u8>), SessionError> {
        if !self.talks.contains(&channel) {
            return Err(SessionError::WrongState("no talk open on this channel"));
        }
        let id = self.alloc_id()?;
        let ext = talk::channel_extension(channel);
        let bytes = self.send(Command::TalkClose, id, ext.as_bytes(), &[])?;
        self.talks.remove(&channel);
        Ok((id, bytes))
    }

    /// Forgets the channel's talk without closing it (the device refused
    /// the open).
    pub fn forget_talk(&mut self, channel: u8) {
        self.talks.remove(&channel);
    }

    /// Channels with an open talk (201 sent, 11 not yet).
    pub fn open_talks(&self) -> impl Iterator<Item = u8> + '_ {
        self.talks.iter().copied()
    }

    fn remember_read(&mut self, id: u32, cmd: Command, cam: Option<Cam>) {
        let reads = &mut self.ptz.reads;
        reads.insert(id, (cmd, cam));
        while reads.len() > READS_REMEMBERED {
            reads.pop_first();
        }
    }

    /// Learns camera motion facts from the reply to a remembered query.
    fn learn(&mut self, message_id: u32, status: u16, xml_text: &str) {
        let Some((cmd, channel)) = self.ptz.reads.remove(&message_id) else {
            return;
        };
        // 199 answers 300 with a complete body on some firmware (observed,
        // protocol.md); every other read here answers 200.
        if status != 200 && !(cmd == Command::Support && status == 300) {
            return;
        }
        match (cmd, channel) {
            (Command::Support, _) => {
                if let Ok(items) = crate::detection::parse_channel_ai(xml_text) {
                    self.ptz.caps =
                        Some(items.iter().map(|i| (i.channel, PtzCaps::of(i))).collect());
                    self.ptz.lens_counts =
                        items.iter().map(|i| (i.channel, i.lenses.len())).collect();
                }
            }
            (Command::PtzZoomFocus, Some(ch)) => {
                if let Ok(zf) = ptz::parse_zoom_focus(xml_text) {
                    self.ptz.lens.insert(ch, zf);
                }
            }
            (Command::PtzPresets, Some(ch)) => {
                if let Ok(p) = ptz::parse_presets(xml_text) {
                    self.ptz.presets.insert(ch, p);
                }
            }
            _ => {}
        }
    }

    /// Whether a capabilities reply (199) has been read on this session.
    pub fn ptz_caps_known(&self) -> bool {
        self.ptz.caps.is_some()
    }

    /// A channel's camera motion capabilities, from the latest 199 reply
    /// (`None` before one arrived; all false for a channel it lacks).
    pub fn ptz_caps(&self, channel: u8) -> Option<PtzCaps> {
        self.ptz
            .caps
            .as_ref()
            .map(|c| c.get(&channel).copied().unwrap_or_default())
    }

    /// The camera a request for `cam` means on this device: a second lens
    /// is kept only where the latest 199 reply lists two lenses for the
    /// channel (a tile on the `extern` stream can also be the middle
    /// quality of a single-lens camera, which has no second lens to
    /// address). Unchanged before 199 was read.
    pub fn ptz_resolve(&self, cam: Cam) -> Cam {
        if cam.lens > 0
            && self.ptz.caps.is_some()
            && self.ptz.lens_counts.get(&cam.channel).copied().unwrap_or(0) < 2
        {
            return Cam::new(cam.channel);
        }
        cam
    }

    /// A camera's lens ranges, from the latest 294 reply on this session.
    pub fn ptz_lens_ranges(&self, cam: impl Into<Cam>) -> Option<ZoomFocus> {
        self.ptz.lens.get(&cam.into()).copied()
    }

    /// A camera's presets, from the latest 190 reply on this session.
    pub fn ptz_presets(&self, cam: impl Into<Cam>) -> Option<&[Preset]> {
        self.ptz.presets.get(&cam.into()).map(Vec::as_slice)
    }

    /// Cameras sent a move and no stop since.
    pub fn ptz_moving(&self) -> impl Iterator<Item = Cam> + '_ {
        self.ptz.moving.iter().copied()
    }

    fn check_ptz(&self, cam: Cam, offers: impl Fn(&PtzCaps) -> bool) -> Result<(), SessionError> {
        if self.state != State::LoggedIn {
            return Err(SessionError::WrongState("not logged in"));
        }
        match self.ptz_caps(cam.channel) {
            None => Err(SessionError::MotionStateUnknown("capabilities")),
            Some(c) if offers(&c) => Ok(()),
            Some(_) => Err(SessionError::MotionNotSupported(cam.channel)),
        }
    }

    /// Builds a read-only PTZ query (294, 190 or 340, viewing tier) for a
    /// camera. The session learns lens ranges (294) and presets (190)
    /// from the replies.
    pub fn ptz_query(
        &mut self,
        cmd: Command,
        cam: impl Into<Cam>,
    ) -> Result<(u32, Vec<u8>), SessionError> {
        let cam = cam.into();
        if !matches!(
            cmd,
            Command::PtzZoomFocus | Command::PtzPresets | Command::PtzSelfTest
        ) {
            return Err(SessionError::CommandNotAllowed(cmd.number()));
        }
        let id = self.alloc_id()?;
        let ext = ptz::query_extension(cam);
        let bytes = self.send(cmd, id, ext.as_bytes(), &[])?;
        self.remember_read(id, cmd, Some(cam));
        Ok((id, bytes))
    }

    /// Builds a move (18 with a direction, motion tier) from the fixed
    /// template, speed limited to 1–64. Refused unless this session's 199
    /// reply says the channel pans and tilts. The caller must follow it
    /// with [`Session::ptz_stop`] on every path.
    pub fn ptz_move(
        &mut self,
        cam: impl Into<Cam>,
        direction: Direction,
        speed: u8,
    ) -> Result<(u32, Vec<u8>), SessionError> {
        let cam = cam.into();
        self.check_ptz(cam, |c| c.pan_tilt)?;
        let id = self.alloc_id()?;
        let ext = ptz::command_extension(cam);
        let body = ptz::move_body(cam.channel, direction, speed);
        let bytes = self.send(Command::PtzControl, id, ext.as_bytes(), body.as_bytes())?;
        self.ptz.moving.insert(cam);
        Ok((id, bytes))
    }

    /// Builds a stop (18 `stop`, motion tier). Always possible while logged
    /// in, except on a channel this session knows cannot pan or tilt and
    /// never moved: a stop must go out on every path, also on a new
    /// connection that has not read the capabilities yet.
    pub fn ptz_stop(&mut self, cam: impl Into<Cam>) -> Result<(u32, Vec<u8>), SessionError> {
        let cam = cam.into();
        if self.state != State::LoggedIn {
            return Err(SessionError::WrongState("not logged in"));
        }
        if !self.ptz.moving.contains(&cam)
            && self.ptz_caps(cam.channel).is_some_and(|c| !c.pan_tilt)
        {
            return Err(SessionError::MotionNotSupported(cam.channel));
        }
        let id = self.alloc_id()?;
        let ext = ptz::command_extension(cam);
        let body = ptz::stop_body(cam.channel);
        let bytes = self.send(Command::PtzControl, id, ext.as_bytes(), body.as_bytes())?;
        self.ptz.moving.remove(&cam);
        Ok((id, bytes))
    }

    /// Builds a preset recall (19 `toPos`, motion tier) with the preset's
    /// name from this session's 190 reply. Refused unless the channel has
    /// presets (199) and its list (190, read first) holds `id`. Saving and
    /// deleting presets cannot be built.
    pub fn ptz_recall(
        &mut self,
        cam: impl Into<Cam>,
        id: u32,
    ) -> Result<(u32, Vec<u8>), SessionError> {
        let cam = cam.into();
        self.check_ptz(cam, |c| c.presets)?;
        let name = self
            .ptz_presets(cam)
            .ok_or(SessionError::MotionStateUnknown("preset list"))?
            .iter()
            .find(|p| p.id == id)
            .ok_or(SessionError::UnknownPreset(id))?
            .name
            .clone();
        let msg = self.alloc_id()?;
        let ext = ptz::command_extension(cam);
        let body = ptz::recall_body(cam.channel, id, &name);
        let bytes = self.send(
            Command::PtzPresetRecall,
            msg,
            ext.as_bytes(),
            body.as_bytes(),
        )?;
        Ok((msg, bytes))
    }

    /// Builds a lens move (295, motion tier) to `position` clamped to the
    /// range of this session's latest 294 reply; returns the position sent.
    /// Refused unless the channel has lens zoom and focus (199) and the
    /// motor's range has been read.
    pub fn ptz_lens(
        &mut self,
        cam: impl Into<Cam>,
        axis: LensAxis,
        position: i32,
    ) -> Result<(u32, Vec<u8>, i32), SessionError> {
        let cam = cam.into();
        self.check_ptz(cam, |c| c.zoom_focus)?;
        let range = self
            .ptz_lens_ranges(cam)
            .ok_or(SessionError::MotionStateUnknown("lens ranges"))?
            .axis(axis)
            .ok_or(SessionError::MotionNotSupported(cam.channel))?;
        let pos = range.clamp(position);
        let id = self.alloc_id()?;
        let ext = ptz::command_extension(cam);
        let body = ptz::lens_body(cam.channel, axis, pos);
        let bytes = self.send(Command::PtzLensMove, id, ext.as_bytes(), body.as_bytes())?;
        Ok((id, bytes, pos))
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
        if let Ok(s) = std::str::from_utf8(body)
            && looks_like_xml(s)
        {
            return Some(s.to_owned());
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
                    let text = Self::clear_text(&msg.body, h.message_id);
                    let remain_times = text
                        .as_deref()
                        .and_then(|t| xml::parse_remain_times(t).ok().flatten());
                    let unlock_secs = text
                        .as_deref()
                        .and_then(|t| xml::parse_unlock_time(t).ok().flatten());
                    Ok(Event::LoginRejected {
                        status: h.status,
                        remain_times,
                        unlock_secs,
                    })
                }
            }
            State::LoggedIn => self.handle_logged_in(msg),
            _ => Err(SessionError::Unexpected { command: h.command }),
        }
    }

    fn handle_logged_in(&mut self, msg: &RawMessage) -> Result<Event, SessionError> {
        let ev = self.handle_logged_in_inner(msg)?;
        if let Event::Control {
            message_id,
            status,
            xml,
            ..
        } = &ev
            && !self.ptz.reads.is_empty()
        {
            self.learn(*message_id, *status, xml);
        }
        Ok(ev)
    }

    fn handle_logged_in_inner(&mut self, msg: &RawMessage) -> Result<Event, SessionError> {
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
        if h.command == Command::Download.number() || h.command == Command::Cover.number() {
            return self.handle_transfer(msg);
        }
        if h.command == Command::PlayFile.number() {
            return self.handle_play(msg);
        }
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

    /// A message of a play (5). Media headers carry a `seq` where other
    /// messages have a status and class (protocol.md, "Replay media
    /// headers"). The device tags a running play's media with the `seq` of
    /// the latest seek on the session, not always the play's own, so a
    /// message on the play's ID is media when its `seq` is the play's or
    /// that of any recent seek; the first reply (status 200, a stream-info
    /// record) is media too.
    ///
    /// How a device ends a play, or refuses one, has not been observed
    /// (docs/MILESTONE-P.md, B6). Here: an empty body ends the play (status
    /// 200 when it came with the play's `seq`, else its status), and a
    /// non-media message with a status other than 200 is a refusal.
    fn handle_play(&mut self, msg: &RawMessage) -> Result<Event, SessionError> {
        let h = msg.header;
        let Some(t) = self.transfers.get(&h.message_id) else {
            return Ok(Event::Ignored {
                command: h.command,
                message_id: h.message_id,
            });
        };
        let media_header = t.seq.is_some()
            && h.replay_seq
                .is_some_and(|s| Some(s) == t.seq || self.seeks.contains(&s));
        let foreign = (!media_header).then(|| ForeignEnd {
            bytes_16_19: h.replay_seq.unwrap_or(u32::from(h.status)),
            expected_seq: t.seq,
            body_len: msg.body.len(),
        });
        let end = |status: u16| Event::TransferEnd {
            command: h.command,
            message_id: h.message_id,
            status,
            foreign,
        };
        if msg.body.is_empty() {
            if !media_header && h.status == 200 && !t.saw_media {
                // An empty acknowledgment before any media.
                return Ok(Event::Control {
                    command: h.command,
                    message_id: h.message_id,
                    status: h.status,
                    xml: String::new(),
                    payload: Vec::new(),
                });
            }
            self.transfers.remove(&h.message_id);
            return Ok(end(if media_header { 200 } else { h.status }));
        }
        if !media_header && h.status != 200 {
            self.transfers.remove(&h.message_id);
            return Ok(end(h.status));
        }
        self.handle_transfer(msg)
    }

    /// A message of a download (143) or cover-image (298) reply stream.
    ///
    /// - Empty body: the end of the stream (143: any status; 298: any status
    ///   but the first, empty status-200 acknowledgment).
    /// - No extension, before any media: an XML control reply (the 143 size
    ///   estimate). After media: a plain continuation.
    /// - Extension plus payload: media; the first `encryptLen` (any case)
    ///   bytes are encrypted, the rest plain.
    fn handle_transfer(&mut self, msg: &RawMessage) -> Result<Event, SessionError> {
        let h = msg.header;
        let Some(t) = self.transfers.get(&h.message_id) else {
            return Ok(Event::Ignored {
                command: h.command,
                message_id: h.message_id,
            });
        };
        let control = |xml: String| Event::Control {
            command: h.command,
            message_id: h.message_id,
            status: h.status,
            xml,
            payload: Vec::new(),
        };
        if msg.body.is_empty() {
            if t.kind == TransferKind::Cover && h.status == 200 && !t.saw_media {
                return Ok(control(String::new()));
            }
            self.transfers.remove(&h.message_id);
            return Ok(Event::TransferEnd {
                command: h.command,
                message_id: h.message_id,
                status: h.status,
                foreign: None,
            });
        }
        let ext = msg.extension();
        let payload = msg.payload();
        if ext.is_empty() {
            if t.saw_media {
                return Ok(Event::Media {
                    message_id: h.message_id,
                    data: payload.to_vec(),
                });
            }
            return Ok(control(self.decrypt_text(payload)?));
        }
        let xml = self.decrypt_text(ext)?;
        if payload.is_empty() {
            return Ok(control(xml));
        }
        let info = xml::parse_extension(&xml)?;
        let mut data = payload.to_vec();
        if let Some(n) = info.encrypt_len {
            let n = (n as usize).min(data.len());
            if n > 0 {
                self.key()?.decrypt(&mut data[..n]);
            }
        }
        if let Some(t) = self.transfers.get_mut(&h.message_id) {
            t.saw_media = true;
        }
        Ok(Event::Media {
            message_id: h.message_id,
            data,
        })
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
                remain_times: Some(2),
                unlock_secs: None,
            }
        );
        assert!(!s.is_logged_in());
        assert!(s.start_preview(0, StreamTier::Main).is_err());
    }

    #[test]
    fn logout_is_encrypted_and_carries_no_credentials() {
        let (mut s, key) = logged_in();
        let m = decode_one(&s.logout().unwrap());
        assert_eq!(m.header.command, 2);
        assert_eq!(m.header.class, Class::Client);
        let mut body = m.payload().to_vec();
        key.decrypt(&mut body);
        let text = String::from_utf8(body).unwrap();
        assert!(text.contains("<LoginUser") && text.contains("<userVer>1</userVer>"));
        assert!(!text.contains("userName") && !text.contains("password"));
        assert!(Session::new(USER, PASSWORD).logout().is_err());
    }

    #[test]
    fn alarm_report_and_its_pushes() {
        assert!(Session::new(USER, PASSWORD).start_alarm_report().is_err());
        let (mut s, key) = logged_in();
        let (id, bytes) = s.start_alarm_report().unwrap();
        let m = decode_one(&bytes);
        assert_eq!(m.header.command, 192);
        assert_eq!(m.header.message_id, id);
        assert!(m.body.is_empty());
        // The device's state pushes carry the 192 request's message ID.
        let push = device_xml(
            &key,
            33,
            id,
            200,
            "<AlarmEventList><AlarmEvent><channelId>1</channelId><status>MD</status>\
             <AItype>people</AItype></AlarmEvent></AlarmEventList>",
        );
        match s.handle(&decode_one(&push)).unwrap() {
            Event::Control {
                command: 33,
                message_id,
                xml,
                ..
            } => {
                assert_eq!(message_id, id);
                let list = crate::detection::parse_alarm_event_list(&xml).unwrap();
                assert_eq!(list[0].ai_types, vec!["people"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn allowlist() {
        for n in [
            1, 2, 3, 4, 5, 7, 10, 11, 14, 15, 16, 18, 19, 80, 93, 123, 142, 143, 144, 145, 146,
            190, 192, 199, 201, 202, 272, 273, 274, 294, 295, 298, 318, 340,
        ] {
            assert!(Command::is_allowed(n));
            assert_eq!(Command::from_number(n).unwrap().number(), n);
        }
        // Talk-back and the PTZ moves, recall and lens moves are motion;
        // everything else is viewing.
        for n in [11, 18, 19, 201, 202, 295] {
            assert_eq!(Command::from_number(n).unwrap().tier(), Tier::Motion);
        }
        for n in [1, 2, 3, 4, 5, 7, 10, 93, 123, 143, 190, 199, 294, 340] {
            assert_eq!(Command::from_number(n).unwrap().tier(), Tier::Viewing);
        }
        // Never: playback commands not observed in use (381–383, 150, 8, 9),
        // 193 (stop alarm report: the session simply logs out), 300 (writes
        // the AI configuration) and, above all, 303 (delete recording files).
        // Not yet: the talk variants the client does not use (12, 203–205)
        // and the PTZ commands not observed (433, 468, 445, 545, 709, 224,
        // 742); 20, 34, 35, 64, 225, 331, 332 and 708 change settings.
        for n in [
            0, 8, 9, 12, 13, 20, 33, 34, 35, 44, 58, 64, 76, 102, 150, 193, 203, 204, 205, 224,
            225, 300, 303, 331, 332, 381, 382, 383, 433, 445, 468, 511, 545, 600, 708, 709, 742,
        ] {
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
            extra_header: Vec::new(),
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
                extra_header: Vec::new(),
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

    fn t(h: u8, m: u8, sec: u8) -> DateTime {
        DateTime::new(2026, 10, 9, h, m, sec).unwrap()
    }

    fn device_xml(key: &SessionKey, cmd: u32, id: u32, status: u16, inner: &str) -> Vec<u8> {
        let mut body = format!("<body>{inner}</body>").into_bytes();
        key.encrypt(&mut body);
        encode_message_with_status(cmd, id, status, Class::Device, &[], &body).unwrap()
    }

    fn device_media(
        key: &SessionKey,
        cmd: u32,
        id: u32,
        ext: &str,
        payload: &[u8],
        enc: usize,
    ) -> Vec<u8> {
        let mut e = ext.as_bytes().to_vec();
        key.encrypt(&mut e);
        let mut p = payload.to_vec();
        let n = enc.min(p.len());
        key.encrypt(&mut p[..n]);
        encode_message_with_status(cmd, id, 200, Class::Device, &e, &p).unwrap()
    }

    #[test]
    fn searches_reuse_their_message_id_and_event_bodies_travel_as_payload() {
        let (mut s, key) = logged_in();
        let q = FileSearch {
            channel: 2,
            uid: Some("UID-EXAMPLE".into()),
            stream: StreamTier::Main,
            start: t(0, 0, 0),
            end: t(23, 59, 59),
        };
        let (id, bytes) = s.file_search_open(&q).unwrap();
        let open = decode_one(&bytes);
        assert_eq!((open.header.command, open.header.message_id), (14, id));
        assert_eq!(open.header.payload_offset, Some(0));
        let next = decode_one(
            &s.file_search_step(id, 2, Some("UID-EXAMPLE"), 0, false)
                .unwrap(),
        );
        let close = decode_one(
            &s.file_search_step(id, 2, Some("UID-EXAMPLE"), 0, true)
                .unwrap(),
        );
        assert_eq!((next.header.command, next.header.message_id), (15, id));
        assert_eq!((close.header.command, close.header.message_id), (16, id));
        assert!(s.file_search_step(999, 2, None, 0, false).is_err());

        let (eid, bytes) = s
            .event_search_open(&EventSearch {
                channel: 2,
                uid: None,
                stream: StreamTier::Main,
                start: t(0, 0, 0),
                end: t(23, 59, 59),
            })
            .unwrap();
        let m = decode_one(&bytes);
        assert_eq!(m.header.command, 272);
        assert!(m.header.payload_offset.unwrap() > 0);
        let mut e = m.extension().to_vec();
        key.decrypt(&mut e);
        let e = String::from_utf8(e).unwrap();
        assert_eq!(find_text(&e, "channelId").unwrap().as_deref(), Some("2"));
        let mut p = m.payload().to_vec();
        key.decrypt(&mut p);
        let p = String::from_utf8(p).unwrap();
        assert!(p.contains("<findAlarmVideo"));
        for close in [false, true] {
            let m = decode_one(&s.event_search_step(eid, 2, 5, close).unwrap());
            assert_eq!(m.header.command, if close { 274 } else { 273 });
            assert_eq!(m.header.message_id, eid);
            let mut p = m.payload().to_vec();
            key.decrypt(&mut p);
            assert_eq!(
                find_text(&String::from_utf8(p).unwrap(), "fileHandle")
                    .unwrap()
                    .as_deref(),
                Some("5")
            );
        }
        let (did, bytes) = s
            .day_records(
                t(0, 0, 0),
                t(23, 59, 59),
                &[DayRecordChannel {
                    channel: 1,
                    uid: None,
                }],
            )
            .unwrap();
        let m = decode_one(&bytes);
        assert_eq!((m.header.command, m.header.message_id), (142, did));
        let reply = device_xml(&key, 142, did, 200, "<DayRecords/>");
        assert!(matches!(
            s.handle(&decode_one(&reply)).unwrap(),
            Event::Control { command: 142, message_id, .. } if message_id == did
        ));
    }

    #[test]
    fn download_stream_estimate_media_and_end() {
        let (mut s, key) = logged_in();
        let (pid, _) = s.start_preview(0, StreamTier::Sub).unwrap();
        let (id, bytes) = s
            .start_download(&DownloadRequest {
                channel: 1,
                uid: None,
                stream: StreamTier::Main,
                start: t(7, 42, 0),
                end: t(7, 43, 0),
            })
            .unwrap();
        let req = decode_one(&bytes);
        assert_eq!(
            (req.header.command, req.header.payload_offset),
            (143, Some(0))
        );
        assert_eq!(s.active_downloads().collect::<Vec<_>>(), vec![id]);

        let est = device_xml(
            &key,
            143,
            id,
            200,
            "<FileInfoList><FileInfo><sizeL>1234</sizeL><sizeH>0</sizeH></FileInfo></FileInfoList>",
        );
        match s.handle(&decode_one(&est)).unwrap() {
            Event::Control {
                command: 143, xml, ..
            } => {
                assert_eq!(archive::parse_download_estimate(&xml).unwrap(), Some(1234));
            }
            other => panic!("{other:?}"),
        }
        let mut stream = encode_info(640, 360, 15);
        let nal: Vec<u8> = (0..2000u32).map(|i| (i * 13) as u8).collect();
        stream.extend(encode_video(0, true, Codec::H264, 5, &[], &nal));
        stream.extend(encode_audio(5, &[9; 300]));
        // Record start with encryptLen, a continuation with binaryData only
        // (plain), and one with no extension at all (plain).
        let (a, rest) = stream.split_at(1500);
        let (b, c) = rest.split_at(400);
        let ext_enc = "<Extension version=\"1.1\"><binaryData>1</binaryData><encryptLen>1024</encryptLen></Extension>";
        let ext_plain = "<Extension version=\"1.1\"><binaryData>1</binaryData></Extension>";
        let mut wire = device_media(&key, 143, id, ext_enc, a, 1024);
        wire.extend(device_media(&key, 143, id, ext_plain, b, 0));
        wire.extend(encode_message_with_status(143, id, 200, Class::Device, &[], c).unwrap());
        // A live preview's media interleaved on the same session.
        wire.extend(device_media(
            &key,
            3,
            pid,
            ext_plain,
            &encode_audio(5, &[1, 2]),
            0,
        ));
        wire.extend(encode_message_with_status(143, id, 300, Class::Device, &[], &[]).unwrap());
        let mut dec = Decoder::new();
        dec.push(&wire);
        let mut demux = Demuxer::new();
        let mut got = Vec::new();
        let mut ended = None;
        let mut preview_media = 0;
        while let Some(m) = dec.next_message().unwrap() {
            match s.handle(&m).unwrap() {
                Event::Media { message_id, data } if message_id == id => {
                    demux.push(&data);
                    while let Some(r) = demux.next_record().unwrap() {
                        got.push(r);
                    }
                }
                Event::Media { message_id, .. } if message_id == pid => preview_media += 1,
                Event::TransferEnd {
                    command: 143,
                    message_id,
                    status,
                    ..
                } if message_id == id => {
                    ended = Some(status);
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(ended, Some(300));
        assert_eq!(preview_media, 1);
        assert_eq!(got.len(), 3);
        assert!(matches!(&got[1], Record::Video { keyframe: true, data, .. } if *data == nal));
        assert_eq!(s.active_downloads().count(), 0);
        // After the end, stray messages are ignored, and there is nothing to stop.
        let late = encode_message_with_status(143, id, 200, Class::Device, &[], b"x").unwrap();
        assert!(matches!(
            s.handle(&decode_one(&late)).unwrap(),
            Event::Ignored { .. }
        ));
        assert_eq!(s.stop_download(id), Err(SessionError::UnknownStream(id)));
    }

    #[test]
    fn play_seek_play_media_by_seq_end_and_stop() {
        let (mut s, key) = logged_in();
        let text = |key: &SessionKey, m: &RawMessage| {
            let mut b = m.payload().to_vec();
            key.decrypt(&mut b);
            String::from_utf8(b).unwrap()
        };
        let seq = 0x6a00_1234;
        let (sid, bytes) = s.replay_seek(seq, t(7, 42, 5)).unwrap();
        let seek = decode_one(&bytes);
        assert_eq!((seek.header.command, seek.header.message_id), (123, sid));
        let x = text(&key, &seek);
        assert_eq!(find_text(&x, "seq").unwrap().as_deref(), Some("1778389556"));
        assert_eq!(find_text(&x, "channelId").unwrap().as_deref(), Some("0"));
        assert!(x.contains("<seekTime>") && x.contains("<minute>42</minute>"));
        // The seek's reply is an ordinary control reply.
        let ok = encode_message_with_status(123, sid, 200, Class::Device, &[], &[]).unwrap();
        assert!(matches!(
            s.handle(&decode_one(&ok)).unwrap(),
            Event::Control {
                command: 123,
                status: 200,
                ..
            }
        ));

        let q = PlayRequest {
            channel: 3,
            uid: Some("UID-EXAMPLE".into()),
            stream: StreamTier::Sub,
            file_id: "file-id-1".into(),
            file_name: "file-name-1".into(),
            speed: 1,
        };
        let (pid, bytes) = s.start_play(&q, seq).unwrap();
        let play = decode_one(&bytes);
        assert_eq!(play.header.command, 5);
        let x = text(&key, &play);
        assert_eq!(find_text(&x, "Id").unwrap().as_deref(), Some("file-id-1"));
        assert_eq!(find_text(&x, "playSpeed").unwrap().as_deref(), Some("1"));
        assert_eq!(
            find_text(&x, "streamType").unwrap().as_deref(),
            Some("subStream")
        );
        assert_eq!(s.active_plays().collect::<Vec<_>>(), vec![pid]);

        // First reply: status 200, binaryData, a stream-info record. Then
        // media whose headers carry the seq; one of another seq is not
        // this play's media and is refused as such.
        let ext_plain = "<Extension version=\"1.1\"><binaryData>1</binaryData></Extension>";
        let mut wire = device_media(&key, 5, pid, ext_plain, &encode_info(640, 360, 15), 0);
        let nal: Vec<u8> = (0..900u32).map(|i| (i * 7) as u8).collect();
        let video = encode_video(0, true, Codec::H264, 5, &[], &nal);
        let mut e = ext_plain.as_bytes().to_vec();
        key.encrypt(&mut e);
        wire.extend(crate::framer::encode_replay_media(pid, seq, &e, &video).unwrap());
        // The end: an empty media message (how a device ends a play is not
        // known; this is the decoder's rule).
        wire.extend(crate::framer::encode_replay_media(pid, seq, &[], &[]).unwrap());
        let mut dec = Decoder::new();
        dec.push(&wire);
        let mut demux = Demuxer::new();
        let mut got = Vec::new();
        let mut ended = None;
        while let Some(m) = dec.next_message().unwrap() {
            match s.handle(&m).unwrap() {
                Event::Media { message_id, data } if message_id == pid => {
                    demux.push(&data);
                    while let Some(r) = demux.next_record().unwrap() {
                        got.push(r);
                    }
                }
                Event::TransferEnd {
                    command: 5, status, ..
                } => ended = Some(status),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(got.len(), 2);
        assert!(matches!(&got[1], Record::Video { data, .. } if *data == nal));
        assert_eq!(ended, Some(200));
        assert_eq!(s.active_plays().count(), 0);
        assert_eq!(s.stop_play(pid, &q), Err(SessionError::UnknownStream(pid)));

        // A refused play: a non-200 reply without the seq ends it with that
        // status. A running play is stopped with 7, naming the file.
        let (pid, _) = s.start_play(&q, seq + 1).unwrap();
        let refused = encode_message_with_status(5, pid, 400, Class::Device, &[], b"x").unwrap();
        assert!(matches!(
            s.handle(&decode_one(&refused)).unwrap(),
            Event::TransferEnd { status: 400, foreign: Some(f), .. }
                if f.bytes_16_19 & 0xffff == 400 && f.expected_seq == Some(seq + 1) && f.body_len == 1
        ));
        // Replay media of another seq ends it too, and the end keeps that
        // message's seq for diagnosis.
        let (pid, _) = s.start_play(&q, seq + 3).unwrap();
        let other = crate::framer::encode_replay_media(pid, seq + 9, &e, &video).unwrap();
        assert!(matches!(
            s.handle(&decode_one(&other)).unwrap(),
            Event::TransferEnd { foreign: Some(f), .. }
                if f.bytes_16_19 == seq + 9 && f.expected_seq == Some(seq + 3)
        ));
        // Media tagged with a later seek's seq stays this play's media: a
        // device was seen tagging a running play with the latest seek's seq.
        let (pid, _) = s.start_play(&q, seq + 4).unwrap();
        let (_, _) = s.replay_seek(seq + 5, t(7, 50, 0)).unwrap();
        let retagged = crate::framer::encode_replay_media(pid, seq + 5, &e, &video).unwrap();
        assert!(matches!(
            s.handle(&decode_one(&retagged)).unwrap(),
            Event::Media { message_id, .. } if message_id == pid
        ));
        s.stop_play(pid, &q).unwrap();
        let (pid, _) = s.start_play(&q, seq + 2).unwrap();
        let stop = decode_one(&s.stop_play(pid, &q).unwrap());
        assert_eq!(stop.header.command, 7);
        assert_ne!(stop.header.message_id, pid);
        assert_eq!(
            find_text(&text(&key, &stop), "name").unwrap().as_deref(),
            Some("file-name-1")
        );
        assert_eq!(s.active_plays().count(), 0);
    }

    #[test]
    fn download_stop_uses_the_download_id_and_silences_it() {
        let (mut s, key) = logged_in();
        let req = DownloadRequest {
            channel: 0,
            uid: Some("UID-EXAMPLE".into()),
            stream: StreamTier::Extern,
            start: t(1, 0, 0),
            end: t(1, 1, 0),
        };
        let (id, bytes) = s.start_download(&req).unwrap();
        let mut body = decode_one(&bytes).body;
        key.decrypt(&mut body);
        let text = String::from_utf8(body).unwrap();
        assert_eq!(
            find_text(&text, "uid").unwrap().as_deref(),
            Some("UID-EXAMPLE")
        );
        assert_eq!(
            find_text(&text, "streamType").unwrap().as_deref(),
            Some("externStream")
        );
        let stop = decode_one(&s.stop_download(id).unwrap());
        assert_eq!((stop.header.command, stop.header.message_id), (144, id));
        assert!(stop.body.is_empty());
        let late = device_media(
            &key,
            143,
            id,
            "<Extension><binaryData>1</binaryData></Extension>",
            b"data",
            0,
        );
        assert!(matches!(
            s.handle(&decode_one(&late)).unwrap(),
            Event::Ignored { .. }
        ));
        let reply = encode_message_with_status(144, id, 200, Class::Device, &[], &[]).unwrap();
        assert!(matches!(
            s.handle(&decode_one(&reply)).unwrap(),
            Event::Control {
                command: 144,
                status: 200,
                ..
            }
        ));
    }

    #[test]
    fn cover_reply_ack_media_with_capital_encrypt_len_and_end() {
        let (mut s, key) = logged_in();
        let (id, bytes) = s
            .cover(&CoverRequest {
                channel: 3,
                uid: None,
                stream: StreamTier::Main,
                start: t(0, 0, 0),
                end: t(6, 0, 0),
            })
            .unwrap();
        assert_eq!(decode_one(&bytes).header.command, 298);
        let ack = encode_message_with_status(298, id, 200, Class::Device, &[], &[]).unwrap();
        assert!(matches!(
            s.handle(&decode_one(&ack)).unwrap(),
            Event::Control {
                command: 298,
                status: 200,
                ..
            }
        ));
        let mut stream = encode_info(640, 360, 15);
        let nal: Vec<u8> = (0..3000u32).map(|i| (i * 7) as u8).collect();
        stream.extend(encode_video(0, true, Codec::H265, 1, &[], &nal));
        let ext = "<Extension version=\"1.1\"><binaryData>1</binaryData><EncryptLen>1024</EncryptLen></Extension>";
        let m = device_media(&key, 298, id, ext, &stream, 1024);
        let Event::Media { data, .. } = s.handle(&decode_one(&m)).unwrap() else {
            panic!("expected media");
        };
        assert_eq!(data, stream);
        let end = encode_message_with_status(298, id, 300, Class::Device, &[], &[]).unwrap();
        assert_eq!(
            s.handle(&decode_one(&end)).unwrap(),
            Event::TransferEnd {
                command: 298,
                message_id: id,
                status: 300,
                foreign: None,
            }
        );
    }

    #[test]
    fn talk_back_is_motion_tier_and_builds_the_observed_messages() {
        use crate::talk::{AdpcmEncoder, AudioStreamMode, Duplex, SAMPLES_PER_RECORD};
        let (mut s, key) = logged_in();
        let cfg = TalkConfig {
            duplex: Duplex::Full,
            stream_mode: AudioStreamMode::FollowVideoStream,
        };
        // The ability query is viewing tier: no permission needed.
        let (aid, bytes) = s.talk_ability(3).unwrap();
        let m = decode_one(&bytes);
        assert_eq!((m.header.command, m.header.message_id), (10, aid));
        let mut ext = m.extension().to_vec();
        key.decrypt(&mut ext);
        let ext = String::from_utf8(ext).unwrap();
        assert!(ext.contains("<channelId>3</channelId>") && ext.contains("<chnType>0</chnType>"));
        assert!(m.payload().is_empty());

        // Motion is off by default: nothing that talks can be built.
        assert_eq!(s.talk_open(3, &cfg), Err(SessionError::MotionNotEnabled(3)));
        assert_eq!(
            s.talk_audio(3, &[0; talk::RECORD_PAYLOAD_LEN]),
            Err(SessionError::MotionNotEnabled(3))
        );
        assert!(s.talk_close(3).is_err());

        s.set_motion_allowed(3, true);
        assert!(s.motion_allowed(3) && !s.motion_allowed(4));
        assert!(s.talk_audio(3, &[0; talk::RECORD_PAYLOAD_LEN]).is_err());
        let (oid, bytes) = s.talk_open(3, &cfg).unwrap();
        assert!(s.talk_open(3, &cfg).is_err(), "one talk per channel");
        assert_eq!(s.talk_open(4, &cfg), Err(SessionError::MotionNotEnabled(4)));
        let m = decode_one(&bytes);
        assert_eq!((m.header.command, m.header.message_id), (201, oid));
        let mut ext = m.extension().to_vec();
        key.decrypt(&mut ext);
        assert_eq!(String::from_utf8(ext).unwrap(), talk::channel_extension(3));
        let mut body = m.payload().to_vec();
        key.decrypt(&mut body);
        assert_eq!(String::from_utf8(body).unwrap(), talk::open_body(3, &cfg));

        // Audio: ID 0, encrypted extension, plain `01wb` payload.
        let mut enc = AdpcmEncoder::new();
        let rec = enc.encode_record(&[100; SAMPLES_PER_RECORD]);
        let m = decode_one(&s.talk_audio(3, &rec).unwrap());
        assert_eq!(
            (m.header.command, m.header.message_id, m.header.status),
            (202, 0, 0)
        );
        assert_eq!(m.header.class, Class::Client);
        let mut ext = m.extension().to_vec();
        assert_eq!(ext.len(), 131);
        key.decrypt(&mut ext);
        let ext = String::from_utf8(ext).unwrap();
        assert!(ext.contains("<binaryData>1</binaryData>"));
        assert_eq!(m.payload().len(), 528);
        let mut d = Demuxer::new();
        d.push(m.payload());
        match d.next_record().unwrap().unwrap() {
            Record::Audio { stream_index, data } => {
                assert_eq!(stream_index, 1);
                assert_eq!(data, rec);
            }
            other => panic!("{other:?}"),
        }
        assert!(s.talk_audio(3, &rec[..100]).is_err());
        assert_eq!(s.open_talks().collect::<Vec<_>>(), vec![3]);

        // Withdrawing permission stops audio but never the close.
        s.set_motion_allowed(3, false);
        assert_eq!(
            s.talk_audio(3, &rec),
            Err(SessionError::MotionNotEnabled(3))
        );
        let (cid, bytes) = s.talk_close(3).unwrap();
        let m = decode_one(&bytes);
        assert_eq!((m.header.command, m.header.message_id), (11, cid));
        assert_eq!(m.header.payload_offset, Some(m.body.len() as u32));
        assert!(s.talk_close(3).is_err());
        assert_eq!(s.open_talks().count(), 0);

        // A refused open is forgotten without a close.
        s.set_motion_allowed(3, true);
        s.talk_open(3, &cfg).unwrap();
        s.forget_talk(3);
        assert!(s.talk_close(3).is_err());
        assert!(s.talk_open(3, &cfg).is_ok());
    }

    fn payload_text(key: &SessionKey, m: &RawMessage) -> String {
        let mut b = m.payload().to_vec();
        key.decrypt(&mut b);
        String::from_utf8(b).unwrap()
    }

    #[test]
    fn ptz_commands_follow_what_the_device_reported() {
        use crate::ptz::{self, Direction, LensAxis};
        let (mut s, key) = logged_in();
        // Nothing moves before the capabilities are read, except a stop.
        assert_eq!(
            s.ptz_move(2, Direction::Left, 20),
            Err(SessionError::MotionStateUnknown("capabilities"))
        );
        assert!(s.ptz_stop(2).is_ok());
        // Channel 1: lens only. Channel 2: pan, tilt, presets, lens.
        // Channel 3: a multi-lens camera whose second lens pans and tilts.
        let (id, _) = s.query_info(Command::Support, None).unwrap();
        let support = "<Support><item><chnID>0</chnID><aitype>0</aitype><ptzType>0</ptzType></item>\
            <item><chnID>1</chnID><aitype>0</aitype><ptzType>1</ptzType></item>\
            <item><chnID>2</chnID><aitype>0</aitype><ptzType>3</ptzType><ptzPreset>3</ptzPreset></item>\
            <item><chnID>3</chnID><aitype>0</aitype><subItem><chnID>0</chnID><aitype>0</aitype>\
            </subItem><subItem><chnID>1</chnID><aitype>0</aitype><ptzType>3</ptzType></subItem></item>\
            </Support>";
        s.handle(&decode_one(&device_xml(&key, 199, id, 200, support)))
            .unwrap();
        assert!(s.ptz_caps_known());
        assert_eq!(s.ptz_caps(0), Some(PtzCaps::default()));
        assert!(s.ptz_caps(3).unwrap().pan_tilt);
        assert_eq!(
            s.ptz_move(0, Direction::Left, 20),
            Err(SessionError::MotionNotSupported(0))
        );
        assert_eq!(
            s.ptz_move(1, Direction::Left, 20),
            Err(SessionError::MotionNotSupported(1))
        );
        assert_eq!(s.ptz_stop(1), Err(SessionError::MotionNotSupported(1)));
        assert_eq!(s.ptz_recall(1, 0), Err(SessionError::MotionNotSupported(1)));

        // A move and its stop: the observed bodies, as payload after a
        // channel extension without chnType.
        let (mid, bytes) = s.ptz_move(2, Direction::Up, 40).unwrap();
        let m = decode_one(&bytes);
        assert_eq!((m.header.command, m.header.message_id), (18, mid));
        let mut ext = m.extension().to_vec();
        key.decrypt(&mut ext);
        assert_eq!(String::from_utf8(ext).unwrap(), ptz::command_extension(2));
        assert_eq!(payload_text(&key, &m), ptz::move_body(2, Direction::Up, 40));
        assert_eq!(s.ptz_moving().collect::<Vec<_>>(), vec![ptz::Cam::new(2)]);
        let m = decode_one(&s.ptz_stop(2).unwrap().1);
        assert_eq!(payload_text(&key, &m), ptz::stop_body(2));
        assert_eq!(s.ptz_moving().count(), 0);
        // A second lens is addressed in the extension, the body keeps the
        // plain channel; its moves are tracked apart from the first lens's.
        let cam = ptz::Cam::with_lens(3, 1);
        let m = decode_one(&s.ptz_move(cam, Direction::Right, 1).unwrap().1);
        let mut ext = m.extension().to_vec();
        key.decrypt(&mut ext);
        assert_eq!(String::from_utf8(ext).unwrap(), ptz::command_extension(cam));
        assert_eq!(
            payload_text(&key, &m),
            ptz::move_body(3, Direction::Right, 1)
        );
        assert_eq!(s.ptz_moving().collect::<Vec<_>>(), vec![cam]);
        let (_, bytes) = s.ptz_query(Command::PtzPresets, cam).unwrap();
        let mut ext = decode_one(&bytes).extension().to_vec();
        key.decrypt(&mut ext);
        assert!(
            String::from_utf8(ext)
                .unwrap()
                .contains("<chnType>0</chnType>\n<subChannel>1</subChannel>")
        );
        assert!(s.ptz_stop(cam).is_ok());
        assert_eq!(s.ptz_moving().count(), 0);

        // A recall needs the preset list; only listed ids, with their name.
        assert_eq!(
            s.ptz_recall(2, 1),
            Err(SessionError::MotionStateUnknown("preset list"))
        );
        let (pid, bytes) = s.ptz_query(Command::PtzPresets, 2).unwrap();
        let m = decode_one(&bytes);
        assert_eq!(m.header.command, 190);
        assert!(m.payload().is_empty());
        s.handle(&decode_one(&device_xml(
            &key,
            190,
            pid,
            200,
            "<PtzPreset><channelId>2</channelId><presetList><preset><id>1</id><name>Gate</name>\
             </preset></presetList></PtzPreset>",
        )))
        .unwrap();
        assert_eq!(s.ptz_recall(2, 4), Err(SessionError::UnknownPreset(4)));
        let m = decode_one(&s.ptz_recall(2, 1).unwrap().1);
        assert_eq!(m.header.command, 19);
        assert_eq!(payload_text(&key, &m), ptz::recall_body(2, 1, "Gate"));

        // A lens move needs the range, and is clamped to it.
        assert_eq!(
            s.ptz_lens(1, LensAxis::Zoom, 5).map(|r| r.2),
            Err(SessionError::MotionStateUnknown("lens ranges"))
        );
        let (zid, _) = s.ptz_query(Command::PtzZoomFocus, 1).unwrap();
        // A refused read teaches nothing.
        s.handle(&decode_one(&device_xml(&key, 294, zid, 400, "")))
            .unwrap();
        assert!(s.ptz_lens_ranges(1).is_none());
        let (zid, _) = s.ptz_query(Command::PtzZoomFocus, 1).unwrap();
        s.handle(&decode_one(&device_xml(
            &key,
            294,
            zid,
            200,
            "<PtzZoomFocus><channelId>1</channelId><zoom><maxPos>28</maxPos><minPos>0</minPos>\
             <curPos>3</curPos></zoom><focus><maxPos>164</maxPos><minPos>0</minPos>\
             <curPos>90</curPos></focus></PtzZoomFocus>",
        )))
        .unwrap();
        let (_, bytes, pos) = s.ptz_lens(1, LensAxis::Zoom, 99).unwrap();
        assert_eq!(pos, 28);
        let m = decode_one(&bytes);
        assert_eq!(m.header.command, 295);
        assert_eq!(
            payload_text(&key, &m),
            ptz::lens_body(1, LensAxis::Zoom, 28)
        );
        assert_eq!(s.ptz_lens(1, LensAxis::Focus, -4).unwrap().2, 0);
        // Only the PTZ reads go through ptz_query.
        assert_eq!(
            s.ptz_query(Command::PtzControl, 1),
            Err(SessionError::CommandNotAllowed(18))
        );
        assert!(s.ptz_query(Command::PtzSelfTest, 2).is_ok());
    }
}
