//! The work behind `bc-cli`, kept in a library so tests can run it against
//! the in-process mock NVR.
//!
//! Everything goes through one [`DeviceSession`], so one run is one login,
//! and the session is closed (previews stopped, socket shut) on every path.

use bc_protocol::media::{Codec, Record};
use bc_protocol::recording;
use bc_protocol::session::Command;
use bc_protocol::stream::StreamTier;
use bc_protocol::xml;
use bc_session::{ConnState, DeviceSession, Item, QueryError};
use std::fmt;
use std::future::Future;
use std::io::Write;
use std::time::Duration;
use tokio::time::timeout;

/// Why a command failed.
#[derive(Debug)]
pub enum CliError {
    /// The device refused the login. Not retried: devices lock accounts out
    /// after a few failures.
    LoginRefused {
        /// Reply status.
        status: u16,
        /// Attempts left before lockout, when the device says.
        remain_times: Option<u32>,
        /// Seconds until a lockout ends, when the device says.
        unlock_secs: Option<u32>,
    },
    /// No login within the time allowed.
    NotConnected(String),
    /// A query failed.
    Query(QueryError),
    /// The device refused to start the stream.
    StreamRefused(u16),
    /// The session ended while recording.
    SessionEnded,
    /// Writing the output failed.
    Io(std::io::Error),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::LoginRefused {
                status,
                remain_times,
                unlock_secs,
            } => {
                let meaning = xml::login_status_meaning(*status);
                write!(f, "login refused: {meaning} (status {status})")?;
                match (unlock_secs, remain_times) {
                    (Some(s), _) if *s > 0 => {
                        write!(f, "; locked, try again in {} min", s.div_ceil(60))
                    }
                    (_, Some(n)) if *n > 0 => write!(f, "; {n} attempts left before lockout"),
                    _ => Ok(()),
                }
            }
            CliError::NotConnected(why) => write!(f, "could not log in: {why}"),
            CliError::Query(e) => write!(f, "query failed: {e}"),
            CliError::StreamRefused(s) => write!(f, "the device refused the stream (status {s})"),
            CliError::SessionEnded => write!(f, "the session ended"),
            CliError::Io(e) => write!(f, "write failed: {e}"),
        }
    }
}

impl std::error::Error for CliError {}

impl From<QueryError> for CliError {
    fn from(e: QueryError) -> CliError {
        CliError::Query(e)
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> CliError {
        CliError::Io(e)
    }
}

/// Waits until the session is logged in. Fails at once on a refused login,
/// and after `limit` if the device cannot be reached (the session keeps
/// retrying in the background until it is closed).
pub async fn wait_logged_in(session: &DeviceSession, limit: Duration) -> Result<(), CliError> {
    let mut state = session.watch_state();
    let mut last = String::from("no answer");
    let wait = async {
        loop {
            match state.borrow_and_update().clone() {
                ConnState::Connected => return Ok(()),
                ConnState::LoginFailed {
                    status,
                    remain_times,
                    unlock_secs,
                } => {
                    return Err(CliError::LoginRefused {
                        status,
                        remain_times,
                        unlock_secs,
                    });
                }
                ConnState::Closed => return Err(CliError::SessionEnded),
                ConnState::Backoff { attempt, .. } => {
                    last = format!("{attempt} failed attempts");
                }
                ConnState::Connecting => {}
            }
            if state.changed().await.is_err() {
                return Err(CliError::SessionEnded);
            }
        }
    };
    match timeout(limit, wait).await {
        Ok(r) => r,
        Err(_) => Err(CliError::NotConnected(last)),
    }
}

/// One row of the channel listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelRow {
    /// Channel number (0-based).
    pub channel: u8,
    /// Whether a camera occupies the slot.
    pub connected: bool,
    /// Stream tiers offered.
    pub tiers: Vec<StreamTier>,
    /// Name and model, for connected channels whose query succeeded.
    pub version: Option<xml::VersionInfo>,
}

/// The device's own name and model, and its channel slots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    /// From command 80.
    pub device: xml::VersionInfo,
    /// From the 145 push, with names from 318.
    pub channels: Vec<ChannelRow>,
}

/// Lists the device's channels: the 145 push, then a 318 query per occupied
/// slot, one at a time.
pub async fn list_channels(session: &DeviceSession, limit: Duration) -> Result<Listing, CliError> {
    wait_logged_in(session, limit).await?;
    let reply = session.query(Command::VersionInfo, None).await?;
    let device = xml::parse_version_info(&reply.xml).unwrap_or_default();

    let mut rx = session.watch_channels();
    let slots = match timeout(limit, rx.wait_for(Option::is_some)).await {
        Ok(Ok(list)) => list.clone().unwrap_or_default(),
        Ok(Err(_)) => return Err(CliError::SessionEnded),
        Err(_) => {
            return Err(CliError::NotConnected(
                "no channel list from the device".into(),
            ));
        }
    };
    let mut channels = Vec::with_capacity(slots.len());
    for slot in slots {
        let version = if slot.connected {
            let r = session
                .query(Command::ChannelVersionInfo, Some(slot.channel))
                .await?;
            (r.status == 200)
                .then(|| xml::parse_version_info(&r.xml).ok())
                .flatten()
        } else {
            None
        };
        channels.push(ChannelRow {
            channel: slot.channel,
            connected: slot.connected,
            tiers: slot.tiers,
            version,
        });
    }
    Ok(Listing { device, channels })
}

/// What a recording wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordStats {
    /// Codec of the first frame written.
    pub codec: Option<Codec>,
    /// Video frames written.
    pub frames: u64,
    /// Of which keyframes.
    pub keyframes: u64,
    /// Bytes written.
    pub bytes: u64,
    /// Gaps (reconnects or a slow writer); each resumes at a keyframe.
    pub discontinuities: u64,
    /// Frames lost to a full queue.
    pub dropped: u64,
    /// Video frames skipped because the codec changed mid-stream.
    pub skipped_codec_change: u64,
    /// Timestamp span of the frames written, in microseconds.
    pub duration_us: u64,
    /// Audio frames received (written when an audio output is given).
    pub audio_frames: u64,
    /// Records written to the raw recording, if any.
    pub raw_records: u64,
}

/// Optional outputs besides the video file.
#[derive(Default)]
pub struct ExtraOutputs<'a> {
    /// Audio payloads as received (ADTS AAC frames: a playable `.aac` file).
    pub audio: Option<&'a mut dyn Write>,
    /// Every record with its arrival time ([`recording`] format), for
    /// replaying the stream later.
    pub raw: Option<recording::Writer<&'a mut dyn Write>>,
}

/// When to stop recording, besides `stop` resolving.
#[derive(Clone, Copy, Debug, Default)]
pub struct RecordLimits {
    /// Stop after this many video frames.
    pub frames: Option<u64>,
}

/// Records one channel's video as a raw Annex-B elementary stream until
/// `stop` resolves, a limit is reached, or the session ends. The first frame
/// written is a keyframe (the session gates until one arrives). Audio and a
/// raw recording of every record go to `extra` when given.
pub async fn record<W: Write>(
    session: &DeviceSession,
    channel: u8,
    tier: StreamTier,
    out: &mut W,
    mut extra: ExtraOutputs<'_>,
    limits: RecordLimits,
    stop: impl Future<Output = ()>,
) -> Result<RecordStats, CliError> {
    let mut sub = session
        .subscribe(channel, tier)
        .await
        .map_err(|_| CliError::SessionEnded)?;
    let mut stats = RecordStats::default();
    let mut first_pts = None;
    let started = std::time::Instant::now();
    let arrival = || started.elapsed().as_micros() as u64;
    tokio::pin!(stop);
    loop {
        let item = tokio::select! {
            item = sub.recv() => item,
            () = &mut stop => break,
        };
        if let Some(raw) = extra.raw.as_mut() {
            match &item {
                Some(Item::Media(r)) => {
                    raw.record(arrival(), r)?;
                    stats.raw_records += 1;
                }
                Some(Item::Discontinuity) => raw.discontinuity(arrival())?,
                _ => {}
            }
        }
        match item {
            Some(Item::Media(Record::Video {
                codec,
                keyframe,
                pts_us,
                data,
            })) => {
                if *stats.codec.get_or_insert(codec) != codec {
                    // One file holds one codec; a mixed stream would not play.
                    stats.skipped_codec_change += 1;
                    continue;
                }
                out.write_all(&data)?;
                stats.frames += 1;
                stats.keyframes += u64::from(keyframe);
                stats.bytes += data.len() as u64;
                let first = *first_pts.get_or_insert(pts_us);
                stats.duration_us = pts_us.saturating_sub(first);
                if limits.frames.is_some_and(|n| stats.frames >= n) {
                    break;
                }
            }
            Some(Item::Media(Record::Audio { data, .. })) => {
                stats.audio_frames += 1;
                if let Some(a) = extra.audio.as_mut() {
                    a.write_all(&data)?;
                }
            }
            Some(Item::Media(Record::Info { .. })) => {}
            Some(Item::Discontinuity) => stats.discontinuities += 1,
            Some(Item::StartRejected { status }) => return Err(CliError::StreamRefused(status)),
            None => {
                return Err(match session.state() {
                    ConnState::LoginFailed {
                        status,
                        remain_times,
                        unlock_secs,
                    } => CliError::LoginRefused {
                        status,
                        remain_times,
                        unlock_secs,
                    },
                    _ => CliError::SessionEnded,
                });
            }
        }
    }
    out.flush()?;
    if let Some(a) = extra.audio.as_mut() {
        a.flush()?;
    }
    if let Some(raw) = extra.raw.take() {
        raw.finish()?;
    }
    stats.dropped = sub.dropped();
    Ok(stats)
}
