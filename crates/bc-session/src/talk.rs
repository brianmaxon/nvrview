//! Talk-back on the device's one session (docs/protocol.md, "Talk-back
//! (motion)").
//!
//! [`crate::DeviceSession::talk`] checks the caller's permission and the
//! rate limit, confirms the channel can talk (199 `ipcAudioTalk`, then its
//! ability, command 10), and hands the talk to the supervisor, which owns
//! the connection: it opens the talk (201), sends one 1024-sample record
//! (202) every 64 ms of a local clock while audio is buffered, and closes it
//! (11) on every exit path. Audio the caller pushes faster than real time
//! is buffered up to a fixed limit and then dropped, never queued without
//! bound.

use crate::supervisor::{DeviceSession, QueryError, Request};
use bc_protocol::session::{Command, Session, SessionError};
use bc_protocol::talk::{
    AdpcmEncoder, RECORD_DURATION_US, SAMPLES_PER_RECORD, TalkConfig, parse_talk_ability,
    parse_talk_channels,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::{Instant, timeout};
use tracing::{debug, info, warn};

/// One record's duration.
pub(crate) const RECORD: Duration = Duration::from_micros(RECORD_DURATION_US);
/// After the last record of a finishing talk, how long to wait for the
/// device to acknowledge every record before closing. (The official client
/// closed about 50 ms after its last record.)
const DRAIN_WAIT: Duration = Duration::from_millis(250);

/// Where a talk is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TalkState {
    /// Checking the channel and opening the talk.
    Starting,
    /// The device accepted the talk; pushed audio is being sent.
    Active,
    /// The talk has ended (it was active).
    Stopped(StopReason),
    /// The talk never started.
    Refused(Refusal),
}

/// Why an active talk ended. Every reason but [`StopReason::ConnectionLost`]
/// sent the close (11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// [`Talk::stop`].
    Stopped,
    /// [`Talk::finish`], after the buffered audio was sent.
    Finished,
    /// The [`Talk`] was dropped.
    Dropped,
    /// The device session is closing.
    SessionClosed,
    /// The connection dropped (nothing more could be sent).
    ConnectionLost,
    /// The device answered an audio message with this status.
    DeviceError(u16),
    /// The caller withdrew the channel's permission.
    PermissionRevoked,
    /// No audio was pushed for [`crate::SessionConfig::talk_idle_timeout`].
    Idle,
    /// The talk lasted [`crate::SessionConfig::talk_max_duration`].
    MaxDuration,
}

/// Why a talk did not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The caller has not allowed talk-back on this channel
    /// ([`crate::DeviceSession::set_motion_allowed`]).
    NotPermitted,
    /// Too many talk starts on this channel in the last minute.
    RateLimited {
        /// When the next start would be allowed.
        retry_after: Duration,
    },
    /// The channel's capabilities (199) do not mark it as talk-capable.
    NotSupported,
    /// The channel's ability (10) offers no configuration nvrview sends.
    UnsupportedAbility,
    /// A talk on this channel is already starting or active.
    Busy,
    /// The device answered the talk request (201) with this status.
    DeviceRefused(u16),
    /// The session is closed.
    SessionClosed,
    /// The connection dropped before the talk started.
    ConnectionLost,
    /// A step got no reply in time.
    TimedOut,
    /// The capabilities or ability could not be read or understood.
    QueryFailed,
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl StopReason {
    /// A short name for logs and events.
    pub fn name(self) -> &'static str {
        match self {
            StopReason::Stopped => "stopped",
            StopReason::Finished => "finished",
            StopReason::Dropped => "dropped",
            StopReason::SessionClosed => "session_closed",
            StopReason::ConnectionLost => "connection_lost",
            StopReason::DeviceError(_) => "device_error",
            StopReason::PermissionRevoked => "permission_revoked",
            StopReason::Idle => "idle",
            StopReason::MaxDuration => "max_duration",
        }
    }
}

impl Refusal {
    /// A short name for logs and events.
    pub fn name(self) -> &'static str {
        match self {
            Refusal::NotPermitted => "not_permitted",
            Refusal::RateLimited { .. } => "rate_limited",
            Refusal::NotSupported => "not_supported",
            Refusal::UnsupportedAbility => "unsupported_ability",
            Refusal::Busy => "busy",
            Refusal::DeviceRefused(_) => "device_refused",
            Refusal::SessionClosed => "session_closed",
            Refusal::ConnectionLost => "connection_lost",
            Refusal::TimedOut => "timed_out",
            Refusal::QueryFailed => "query_failed",
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::RateLimited { retry_after } => {
                write!(f, "rate_limited (retry in {} ms)", retry_after.as_millis())
            }
            Refusal::DeviceRefused(s) => write!(f, "device_refused (status {s})"),
            other => f.write_str(other.name()),
        }
    }
}

impl std::error::Error for Refusal {}

/// Counters of one talk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TalkStats {
    /// Records (202) sent.
    pub records_sent: u64,
    /// Audio replies (202) with status 200 received while this talk was the
    /// only one on the session.
    pub records_acked: u64,
    /// Samples pushed and accepted.
    pub samples_accepted: u64,
    /// Samples dropped because the buffer was full.
    pub samples_dropped: u64,
    /// Times the send clock fell too far behind and restarted from now.
    pub clock_restarts: u64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the caller asked of a running talk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    None,
    /// Send what is buffered, then close.
    Finish,
    /// Close now.
    Now(StopReason),
}

struct Buffer {
    samples: VecDeque<i16>,
    ending: Ending,
    /// When audio was last pushed.
    last_push: Instant,
}

/// State shared by a [`Talk`] and the supervisor.
pub(crate) struct TalkShared {
    pub(crate) channel: u8,
    buf: Mutex<Buffer>,
    capacity: usize,
    state_tx: watch::Sender<TalkState>,
    records_sent: AtomicU64,
    records_acked: AtomicU64,
    samples_accepted: AtomicU64,
    samples_dropped: AtomicU64,
    clock_restarts: AtomicU64,
    wake: Arc<crate::Shared>,
}

impl TalkShared {
    fn set_state(&self, s: TalkState) {
        self.state_tx.send_replace(s);
    }

    fn state(&self) -> TalkState {
        *self.state_tx.borrow()
    }

    fn ending(&self) -> Ending {
        lock(&self.buf).ending
    }

    fn end_now(&self, reason: StopReason) {
        let mut b = lock(&self.buf);
        if !matches!(b.ending, Ending::Now(_)) {
            b.ending = Ending::Now(reason);
        }
        drop(b);
        self.wake.wake.notify_one();
    }
}

/// A talk on one channel. Push 16 kHz mono samples with [`Talk::push`];
/// end it with [`Talk::stop`], [`Talk::finish`], or by dropping it. Every
/// ending that happens while the talk is open sends the close (11).
pub struct Talk {
    shared: Arc<TalkShared>,
    state_rx: watch::Receiver<TalkState>,
}

impl Talk {
    /// The channel.
    pub fn channel(&self) -> u8 {
        self.shared.channel
    }

    /// The current state.
    pub fn state(&self) -> TalkState {
        self.shared.state()
    }

    /// A receiver that observes state changes.
    pub fn watch_state(&self) -> watch::Receiver<TalkState> {
        self.state_rx.clone()
    }

    /// Queues 16 kHz mono samples to send. Returns how many were accepted:
    /// beyond the buffer's capacity ([`crate::SessionConfig::talk_buffer`])
    /// the rest are dropped and counted. Nothing is accepted once the talk
    /// is ending or over. Never blocks on the network.
    pub fn push(&self, samples: &[i16]) -> usize {
        let mut b = lock(&self.shared.buf);
        if b.ending != Ending::None
            || matches!(
                self.shared.state(),
                TalkState::Stopped(_) | TalkState::Refused(_)
            )
        {
            return 0;
        }
        let room = self.shared.capacity.saturating_sub(b.samples.len());
        let n = samples.len().min(room);
        let before = b.samples.len();
        b.samples.extend(&samples[..n]);
        b.last_push = Instant::now();
        let full_record = before / SAMPLES_PER_RECORD != b.samples.len() / SAMPLES_PER_RECORD;
        drop(b);
        self.shared
            .samples_accepted
            .fetch_add(n as u64, Ordering::Relaxed);
        if n < samples.len() {
            self.shared
                .samples_dropped
                .fetch_add((samples.len() - n) as u64, Ordering::Relaxed);
        }
        if full_record {
            self.shared.wake.wake.notify_one();
        }
        n
    }

    /// Samples buffered and not yet sent.
    pub fn buffered(&self) -> usize {
        lock(&self.shared.buf).samples.len()
    }

    /// Counters so far.
    pub fn stats(&self) -> TalkStats {
        let s = &self.shared;
        TalkStats {
            records_sent: s.records_sent.load(Ordering::Relaxed),
            records_acked: s.records_acked.load(Ordering::Relaxed),
            samples_accepted: s.samples_accepted.load(Ordering::Relaxed),
            samples_dropped: s.samples_dropped.load(Ordering::Relaxed),
            clock_restarts: s.clock_restarts.load(Ordering::Relaxed),
        }
    }

    /// Ends the talk at once (the close goes out within a few ms).
    pub fn stop(&self) {
        self.shared.end_now(StopReason::Stopped);
    }

    /// Ends the talk after the buffered audio has been sent (the last
    /// partial record padded with silence).
    pub fn finish(&self) {
        let mut b = lock(&self.shared.buf);
        if b.ending == Ending::None {
            b.ending = Ending::Finish;
        }
        drop(b);
        self.shared.wake.wake.notify_one();
    }

    /// Waits until the talk has ended or was refused, and returns that
    /// state.
    pub async fn ended(&mut self) -> TalkState {
        loop {
            let s = *self.state_rx.borrow_and_update();
            if matches!(s, TalkState::Stopped(_) | TalkState::Refused(_)) {
                return s;
            }
            if self.state_rx.changed().await.is_err() {
                return self.shared.state();
            }
        }
    }

    /// Waits while the talk is starting; `Ok` once active.
    pub(crate) async fn started(&mut self) -> Result<(), Refusal> {
        loop {
            let s = *self.state_rx.borrow_and_update();
            match s {
                TalkState::Starting => {}
                TalkState::Active => return Ok(()),
                TalkState::Refused(r) => return Err(r),
                TalkState::Stopped(StopReason::ConnectionLost) => {
                    return Err(Refusal::ConnectionLost);
                }
                TalkState::Stopped(StopReason::SessionClosed) => {
                    return Err(Refusal::SessionClosed);
                }
                TalkState::Stopped(_) => return Err(Refusal::SessionClosed),
            }
            if self.state_rx.changed().await.is_err() {
                return Err(Refusal::SessionClosed);
            }
        }
    }
}

impl Drop for Talk {
    fn drop(&mut self) {
        self.shared.end_now(StopReason::Dropped);
    }
}

impl fmt::Debug for Talk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Talk")
            .field("channel", &self.shared.channel)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

/// Per-device talk-back control shared by the handle and the supervisor:
/// the channels the caller allowed, and the start rate limit.
pub(crate) struct TalkControl {
    allowed: Mutex<HashSet<u8>>,
    starts: Mutex<HashMap<u8, VecDeque<Instant>>>,
    refusal_log: Mutex<Option<Instant>>,
}

impl TalkControl {
    pub(crate) fn new() -> TalkControl {
        TalkControl {
            allowed: Mutex::new(HashSet::new()),
            starts: Mutex::new(HashMap::new()),
            refusal_log: Mutex::new(None),
        }
    }

    pub(crate) fn set_allowed(&self, channel: u8, allowed: bool) -> bool {
        let mut a = lock(&self.allowed);
        if allowed {
            a.insert(channel)
        } else {
            a.remove(&channel)
        }
    }

    pub(crate) fn is_allowed(&self, channel: u8) -> bool {
        lock(&self.allowed).contains(&channel)
    }

    pub(crate) fn allowed(&self) -> HashSet<u8> {
        lock(&self.allowed).clone()
    }

    /// Counts a start against the channel's limit of `per_minute`, or says
    /// how long until one is allowed.
    pub(crate) fn take_start(&self, channel: u8, per_minute: u32) -> Result<(), Duration> {
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let mut s = lock(&self.starts);
        let q = s.entry(channel).or_default();
        while q.front().is_some_and(|t| now.duration_since(*t) >= window) {
            q.pop_front();
        }
        if q.len() >= per_minute as usize {
            let oldest = q.front().copied().unwrap_or(now);
            return Err(window.saturating_sub(now.duration_since(oldest)));
        }
        q.push_back(now);
        Ok(())
    }

    /// Logs a refusal, at most one line every few seconds.
    pub(crate) fn log_refusal(&self, channel: u8, why: Refusal) {
        let now = Instant::now();
        let mut last = lock(&self.refusal_log);
        if last.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(5)) {
            *last = Some(now);
            info!(channel, reason = %why, "talk-back refused");
        }
    }
}

/// Talk-back settings, copied from [`crate::SessionConfig`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct TalkLimits {
    pub(crate) idle_timeout: Duration,
    pub(crate) max_duration: Duration,
    pub(crate) catch_up: Duration,
    pub(crate) reply_timeout: Duration,
}

/// A new talk on its way to the supervisor.
pub(crate) struct TalkReq {
    pub(crate) shared: Arc<TalkShared>,
    pub(crate) config: TalkConfig,
}

/// Makes the handle and the request for a new talk.
pub(crate) fn new_talk(
    channel: u8,
    config: TalkConfig,
    buffer: usize,
    wake: Arc<crate::Shared>,
) -> (Talk, TalkReq) {
    let (state_tx, state_rx) = watch::channel(TalkState::Starting);
    let shared = Arc::new(TalkShared {
        channel,
        buf: Mutex::new(Buffer {
            samples: VecDeque::new(),
            ending: Ending::None,
            last_push: Instant::now(),
        }),
        capacity: buffer.max(SAMPLES_PER_RECORD),
        state_tx,
        records_sent: AtomicU64::new(0),
        records_acked: AtomicU64::new(0),
        samples_accepted: AtomicU64::new(0),
        samples_dropped: AtomicU64::new(0),
        clock_restarts: AtomicU64::new(0),
        wake,
    });
    (
        Talk {
            shared: shared.clone(),
            state_rx,
        },
        TalkReq { shared, config },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// 201 sent with this message ID; waiting for its reply.
    Opening { id: u32, sent: Instant },
    /// Accepted.
    Active,
}

struct Entry {
    shared: Arc<TalkShared>,
    phase: Phase,
    encoder: AdpcmEncoder,
    /// When the next record is due; `None` until the first is sent (or
    /// after the buffer ran dry).
    next_due: Option<Instant>,
    opened: Instant,
    /// The reply of the 201 said "refused" (no close needed).
    refused: Option<u16>,
    /// A finishing talk sent its last record at this time.
    drained: Option<Instant>,
}

/// The supervisor's talks.
pub(crate) struct Talks {
    pub(crate) control: Arc<TalkControl>,
    limits: TalkLimits,
    waiting: VecDeque<TalkReq>,
    entries: HashMap<u8, Entry>,
}

/// Messages to send, in order.
pub(crate) type Outgoing = Vec<Vec<u8>>;

/// Adds the close (11) of `channel`'s talk.
fn close(session: &mut Session, channel: u8, out: &mut Outgoing) {
    match session.talk_close(channel) {
        Ok((_, bytes)) => out.push(bytes),
        Err(e) => debug!(%e, "talk close not built"),
    }
}

impl Talks {
    pub(crate) fn new(control: Arc<TalkControl>, limits: TalkLimits) -> Talks {
        Talks {
            control,
            limits,
            waiting: VecDeque::new(),
            entries: HashMap::new(),
        }
    }

    pub(crate) fn add(&mut self, r: TalkReq) {
        self.waiting.push_back(r);
    }

    /// Whether anything is in progress (to keep polling).
    pub(crate) fn is_empty(&self) -> bool {
        self.waiting.is_empty() && self.entries.is_empty()
    }

    /// Brings the protocol session's motion permissions in line with the
    /// caller's.
    fn sync_permissions(&self, session: &mut Session) {
        let allowed = self.control.allowed();
        for ch in 0..=u8::MAX {
            let want = allowed.contains(&ch);
            if session.motion_allowed(ch) != want {
                session.set_motion_allowed(ch, want);
            }
        }
    }

    /// Opens waiting talks, sends due audio, and closes ended talks.
    /// Returns the messages to send now.
    pub(crate) fn poll(&mut self, session: &mut Session) -> Outgoing {
        let mut out = Vec::new();
        if self.is_empty() {
            return out;
        }
        self.sync_permissions(session);
        let now = Instant::now();
        while let Some(r) = self.waiting.pop_front() {
            let ch = r.shared.channel;
            if let Ending::Now(_) = r.shared.ending() {
                // Gave up before it was sent.
                r.shared.set_state(TalkState::Refused(Refusal::TimedOut));
                continue;
            }
            if self.entries.contains_key(&ch) {
                r.shared.set_state(TalkState::Refused(Refusal::Busy));
                self.control.log_refusal(ch, Refusal::Busy);
                continue;
            }
            match session.talk_open(ch, &r.config) {
                Ok((id, bytes)) => {
                    out.push(bytes);
                    self.entries.insert(
                        ch,
                        Entry {
                            shared: r.shared,
                            phase: Phase::Opening { id, sent: now },
                            encoder: AdpcmEncoder::new(),
                            next_due: None,
                            opened: now,
                            refused: None,
                            drained: None,
                        },
                    );
                }
                Err(SessionError::MotionNotEnabled(_)) => {
                    r.shared
                        .set_state(TalkState::Refused(Refusal::NotPermitted));
                    self.control.log_refusal(ch, Refusal::NotPermitted);
                }
                Err(SessionError::WrongState(_)) => {
                    r.shared.set_state(TalkState::Refused(Refusal::Busy));
                    self.control.log_refusal(ch, Refusal::Busy);
                }
                Err(e) => {
                    debug!(%e, "talk open not built");
                    r.shared.set_state(TalkState::Refused(Refusal::QueryFailed));
                }
            }
        }
        let channels: Vec<u8> = self.entries.keys().copied().collect();
        for ch in channels {
            let Some(e) = self.entries.get_mut(&ch) else {
                continue;
            };
            if let Some(status) = e.refused {
                session.forget_talk(ch);
                e.shared
                    .set_state(TalkState::Refused(Refusal::DeviceRefused(status)));
                self.control.log_refusal(ch, Refusal::DeviceRefused(status));
                self.entries.remove(&ch);
                continue;
            }
            match e.phase {
                Phase::Opening { sent, .. } => {
                    if now.duration_since(sent) >= self.limits.reply_timeout {
                        // No answer: close anyway, in case it did open.
                        close(session, ch, &mut out);
                        e.shared.set_state(TalkState::Refused(Refusal::TimedOut));
                        self.control.log_refusal(ch, Refusal::TimedOut);
                        self.entries.remove(&ch);
                    }
                }
                Phase::Active => {
                    let reason = Self::due_to_end(e, session, &self.limits, now).or_else(|| {
                        match Self::send_due(e, session, &self.limits, now, &mut out) {
                            // Everything buffered went out: close once the
                            // device has acknowledged it, or soon after.
                            Ok(true) => {
                                let since = *e.drained.get_or_insert(now);
                                let s = &e.shared;
                                let acked = s.records_acked.load(Ordering::Relaxed)
                                    >= s.records_sent.load(Ordering::Relaxed);
                                (acked || now.duration_since(since) >= DRAIN_WAIT)
                                    .then_some(StopReason::Finished)
                            }
                            Ok(false) => None,
                            Err(err) => {
                                debug!(%err, "talk audio not built");
                                Some(StopReason::PermissionRevoked)
                            }
                        }
                    });
                    if let Some(reason) = reason {
                        close(session, ch, &mut out);
                        Self::stopped(e, reason);
                        self.entries.remove(&ch);
                    }
                }
            }
        }
        out
    }

    fn stopped(e: &Entry, reason: StopReason) {
        let st = e.shared.records_sent.load(Ordering::Relaxed);
        info!(
            channel = e.shared.channel,
            reason = reason.name(),
            records = st,
            ms = e.opened.elapsed().as_millis() as u64,
            "talk-back stopped"
        );
        e.shared.set_state(TalkState::Stopped(reason));
    }

    fn due_to_end(
        e: &Entry,
        session: &Session,
        limits: &TalkLimits,
        now: Instant,
    ) -> Option<StopReason> {
        let b = lock(&e.shared.buf);
        if let Ending::Now(r) = b.ending {
            return Some(r);
        }
        if !session.motion_allowed(e.shared.channel) {
            return Some(StopReason::PermissionRevoked);
        }
        if now.duration_since(e.opened) >= limits.max_duration {
            return Some(StopReason::MaxDuration);
        }
        if b.ending == Ending::None
            && b.samples.len() < SAMPLES_PER_RECORD
            && now.duration_since(b.last_push) >= limits.idle_timeout
        {
            return Some(StopReason::Idle);
        }
        None
    }

    /// Sends the records that are due. True when a finishing talk has sent
    /// everything.
    fn send_due(
        e: &mut Entry,
        session: &mut Session,
        limits: &TalkLimits,
        now: Instant,
        out: &mut Outgoing,
    ) -> Result<bool, SessionError> {
        loop {
            let mut b = lock(&e.shared.buf);
            let finishing = b.ending == Ending::Finish;
            if b.samples.is_empty() && finishing {
                return Ok(true);
            }
            if b.samples.len() < SAMPLES_PER_RECORD && !finishing {
                // Ran dry: the next record starts the clock again.
                if e.next_due.is_some_and(|d| d < now) {
                    e.next_due = None;
                }
                return Ok(false);
            }
            let due = match e.next_due {
                Some(d) if d + limits.catch_up < now => {
                    // Fell too far behind (a stall): restart from now rather
                    // than bursting to catch up.
                    e.shared.clock_restarts.fetch_add(1, Ordering::Relaxed);
                    now
                }
                Some(d) => d,
                None => now,
            };
            if due > now {
                e.next_due = Some(due);
                return Ok(false);
            }
            let mut block = [0i16; SAMPLES_PER_RECORD];
            let n = b.samples.len().min(SAMPLES_PER_RECORD);
            for (dst, src) in block.iter_mut().zip(b.samples.drain(..n)) {
                *dst = src;
            }
            drop(b);
            let payload = e.encoder.encode_record(&block);
            out.push(session.talk_audio(e.shared.channel, &payload)?);
            e.shared.records_sent.fetch_add(1, Ordering::Relaxed);
            e.next_due = Some(due + RECORD);
        }
    }

    /// When [`Talks::poll`] next has something to do.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        let mut best: Option<Instant> = None;
        let mut take = |t: Instant| best = Some(best.map_or(t, |b: Instant| b.min(t)));
        for e in self.entries.values() {
            match e.phase {
                Phase::Opening { sent, .. } => take(sent + self.limits.reply_timeout),
                Phase::Active => {
                    take(e.opened + self.limits.max_duration);
                    let b = lock(&e.shared.buf);
                    take(b.last_push + self.limits.idle_timeout);
                    let ready = b.samples.len() >= SAMPLES_PER_RECORD
                        || (b.ending == Ending::Finish && !b.samples.is_empty());
                    if ready {
                        take(e.next_due.unwrap_or_else(Instant::now));
                    }
                    if let Some(t) = e.drained {
                        take(t + DRAIN_WAIT);
                    }
                }
            }
        }
        best
    }

    /// A reply to a talk command arrived.
    pub(crate) fn on_reply(&mut self, command: u32, message_id: u32, status: u16) {
        match command {
            201 => {
                let Some(e) = self
                    .entries
                    .values_mut()
                    .find(|e| matches!(e.phase, Phase::Opening { id, .. } if id == message_id))
                else {
                    return;
                };
                if status == 200 {
                    e.phase = Phase::Active;
                    e.opened = Instant::now();
                    lock(&e.shared.buf).last_push = e.opened;
                    info!(channel = e.shared.channel, "talk-back started");
                    e.shared.set_state(TalkState::Active);
                } else {
                    e.refused = Some(status);
                }
            }
            202 => {
                let active: Vec<&mut Entry> = self
                    .entries
                    .values_mut()
                    .filter(|e| e.phase == Phase::Active)
                    .collect();
                if status == 200 {
                    if let [only] = active.as_slice() {
                        only.shared.records_acked.fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    // The reply names no channel: end every talk.
                    warn!(status, "talk-back audio refused by the device");
                    for e in active {
                        e.shared.end_now(StopReason::DeviceError(status));
                    }
                }
            }
            _ => {}
        }
    }

    /// The connection is gone: nothing more can be sent.
    pub(crate) fn connection_lost(&mut self) {
        for r in self.waiting.drain(..) {
            r.shared
                .set_state(TalkState::Refused(Refusal::ConnectionLost));
        }
        for (_, e) in self.entries.drain() {
            match e.phase {
                Phase::Active => Self::stopped(&e, StopReason::ConnectionLost),
                Phase::Opening { .. } => e
                    .shared
                    .set_state(TalkState::Refused(Refusal::ConnectionLost)),
            }
        }
    }

    /// The session is closing: the closes to send for every open talk.
    pub(crate) fn close_all(&mut self, session: &mut Session) -> Outgoing {
        let mut out = Vec::new();
        for r in self.waiting.drain(..) {
            r.shared
                .set_state(TalkState::Refused(Refusal::SessionClosed));
        }
        for (ch, e) in self.entries.drain() {
            if let Ok((_, bytes)) = session.talk_close(ch) {
                out.push(bytes);
            }
            match e.phase {
                Phase::Active => Self::stopped(&e, StopReason::SessionClosed),
                Phase::Opening { .. } => e
                    .shared
                    .set_state(TalkState::Refused(Refusal::SessionClosed)),
            }
        }
        out
    }

    /// The session ended for good without a connection to send on.
    pub(crate) fn finish(&mut self) {
        for r in self.waiting.drain(..) {
            r.shared
                .set_state(TalkState::Refused(Refusal::SessionClosed));
        }
        for (_, e) in self.entries.drain() {
            match e.phase {
                Phase::Active => Self::stopped(&e, StopReason::SessionClosed),
                Phase::Opening { .. } => e
                    .shared
                    .set_state(TalkState::Refused(Refusal::SessionClosed)),
            }
        }
    }
}

fn query_refusal(e: QueryError) -> Refusal {
    match e {
        QueryError::Closed => Refusal::SessionClosed,
        QueryError::ConnectionLost => Refusal::ConnectionLost,
        QueryError::TimedOut => Refusal::TimedOut,
        QueryError::Busy | QueryError::NotAllowed => Refusal::QueryFailed,
    }
}

impl DeviceSession {
    /// Allows or withdraws camera motion control (here: talk-back) on one
    /// channel. Off for every channel until allowed. Withdrawing it stops a
    /// running talk at once ([`StopReason::PermissionRevoked`]).
    pub fn set_motion_allowed(&self, channel: u8, allowed: bool) {
        if self.talk_control.set_allowed(channel, allowed) {
            info!(channel, allowed, "camera motion control permission changed");
        }
        self.shared.wake.notify_one();
    }

    /// Whether camera motion control is allowed on `channel`.
    pub fn motion_allowed(&self, channel: u8) -> bool {
        self.talk_control.is_allowed(channel)
    }

    /// Starts talk-back on `channel` and returns once the device has
    /// accepted it. Refused without the channel's permission
    /// ([`DeviceSession::set_motion_allowed`]), beyond
    /// [`crate::SessionConfig::talk_starts_per_minute`], on a channel whose
    /// capabilities (199) lack `ipcAudioTalk` 1 or whose ability (10)
    /// offers no supported configuration, while another talk runs on the
    /// channel, or when the device refuses. Runs on the session's one
    /// connection; never logs in again.
    pub async fn talk(&self, channel: u8) -> Result<Talk, Refusal> {
        let refuse = |r: Refusal| {
            self.talk_control.log_refusal(channel, r);
            Err(r)
        };
        if self.shared.is_closing() {
            return refuse(Refusal::SessionClosed);
        }
        if !self.talk_control.is_allowed(channel) {
            return refuse(Refusal::NotPermitted);
        }
        if let Err(retry_after) = self
            .talk_control
            .take_start(channel, self.talk_starts_per_minute)
        {
            return refuse(Refusal::RateLimited { retry_after });
        }
        let support = match self.query(Command::Support, None).await {
            Ok(r) => r,
            Err(e) => return refuse(query_refusal(e)),
        };
        match parse_talk_channels(&support.xml) {
            Ok(chs) if chs.contains(&channel) => {}
            Ok(_) => return refuse(Refusal::NotSupported),
            Err(_) => return refuse(Refusal::QueryFailed),
        }
        let reply = match self
            .call(Box::new(move |s| s.talk_ability(channel)), None)
            .await
        {
            Ok(r) if r.status == 200 => r,
            Ok(_) => return refuse(Refusal::NotSupported),
            Err(e) => return refuse(query_refusal(e)),
        };
        let config = match parse_talk_ability(&reply.xml).map(|a| a.choose()) {
            Ok(Some(c)) => c,
            Ok(None) => return refuse(Refusal::UnsupportedAbility),
            Err(_) => return refuse(Refusal::QueryFailed),
        };
        info!(channel, "talk-back starting");
        let (mut talk, req) = new_talk(channel, config, self.talk_buffer, self.shared.clone());
        if self.cmd_tx.send(Request::Talk(req)).await.is_err() {
            return refuse(Refusal::SessionClosed);
        }
        self.shared.wake.notify_one();
        match timeout(self.query_timeout, talk.started()).await {
            Ok(Ok(())) => Ok(talk),
            // Dropping `talk` closes it if the device accepts it later.
            Ok(Err(r)) => refuse(r),
            Err(_) => refuse(Refusal::TimedOut),
        }
    }
}
