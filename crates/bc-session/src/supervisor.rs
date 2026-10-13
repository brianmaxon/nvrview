//! The supervisor task: the only code that connects, talks, and reconnects.

use crate::Shared;
use crate::archive::{TransferItem, TransferKind, TransferSink};
use crate::config::{Jitter, SessionConfig};
use crate::ptz::{Ptz, PtzControl};
use crate::queue::{FrameQueue, Item, Subscription};
use crate::talk::{TalkControl, TalkLimits, TalkReq, Talks};
use crate::transport::Connector;
use bc_protocol::archive::{self, PlayRequest};
use bc_protocol::detection::{self, AlarmState};
use bc_protocol::framer::Decoder;
use bc_protocol::media::{Demuxer, Record};
use bc_protocol::session::{Command, Event, Session, SessionError};
use bc_protocol::stream::StreamTier;
use bc_protocol::xml::{self, ChannelInfo};
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Semaphore, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};
use tracing::{debug, info, warn};

/// The least time between two manual retries of one stream, and between two
/// connection attempts made early because of a manual retry.
pub const MANUAL_RETRY_FLOOR: Duration = Duration::from_secs(2);

/// Manual retries waiting for the supervisor (all distinct streams).
const MAX_PENDING_RETRIES: usize = 64;

/// The least time between two reconnects forced by silent previews.
const ESCALATE_MIN_INTERVAL: Duration = Duration::from_secs(600);

/// How often a log line says which tiles have been without a picture.
const DIAGNOSTIC_INTERVAL: Duration = Duration::from_secs(60);

/// A tile counts as stuck after this long without a picture.
const STUCK_AFTER: Duration = Duration::from_secs(60);

/// Connection state, observable through [`DeviceSession::watch_state`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnState {
    /// Opening the connection or logging in.
    Connecting,
    /// Logged in; previews are running.
    Connected,
    /// Waiting before the next attempt.
    Backoff {
        /// Number of failed attempts in a row (1 after the first failure).
        attempt: u32,
        /// How long the supervisor will wait.
        delay: Duration,
    },
    /// The device refused the login. Terminal: no retries, to avoid locking
    /// the account out. Create a new session with corrected credentials.
    LoginFailed {
        /// Reply status (401 for bad credentials).
        status: u16,
        /// Remaining attempts before lockout, when the device reports it.
        remain_times: Option<u32>,
        /// Seconds until a lockout ends, when the device reports it.
        unlock_secs: Option<u32>,
    },
    /// Closed by [`DeviceSession::close`] or by dropping the handle.
    Closed,
}

/// The session has ended; no new subscriptions are possible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionClosed;

impl fmt::Display for SessionClosed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "device session is closed")
    }
}

impl std::error::Error for SessionClosed {}

/// Reply to an info query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryReply {
    /// Reply status. Most commands answer 200; 146 answers 300 with a full
    /// body (see docs/protocol.md).
    pub status: u16,
    /// Decrypted XML body (empty when the reply has none).
    pub xml: String,
}

/// Why an info query produced no reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryError {
    /// The session is closed, or the login was refused.
    Closed,
    /// The connection dropped before the reply arrived.
    ConnectionLost,
    /// No reply within [`SessionConfig::query_timeout`].
    TimedOut,
    /// Too many queries outstanding.
    Busy,
    /// The command is not a read-only info query, or a needed channel is
    /// missing.
    NotAllowed,
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            QueryError::Closed => "device session is closed",
            QueryError::ConnectionLost => "connection lost before the reply",
            QueryError::TimedOut => "no reply in time",
            QueryError::Busy => "too many queries outstanding",
            QueryError::NotAllowed => "not an allowed info query",
        })
    }
}

impl std::error::Error for QueryError {}

/// Queries outstanding at once (waiting for login plus in flight).
const MAX_QUERIES: usize = 32;

/// Alarm-state pushes a slow [`DeviceSession::alarms`] receiver may fall
/// behind by before it misses some (each push is a full snapshot, so the
/// next one it gets is complete again).
const ALARM_PUSHES_QUEUED: usize = 16;

/// One alarm-state push (command 33): every channel slot's detection state
/// as the device reported it (docs/protocol.md, "Detection events").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlarmPush {
    /// Counts logins on this session: a push from a new connection starts
    /// afresh, whatever the previous connection's last push said.
    pub connection: u64,
    /// The slots this push lists.
    pub states: Vec<AlarmState>,
}

/// A reply to a request sent through the supervisor.
#[derive(Clone, Debug)]
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) xml: String,
    pub(crate) message_id: u32,
    /// Connection generation the request went out on.
    pub(crate) conn: u64,
}

type ReplyTx = oneshot::Sender<Result<Reply, QueryError>>;

/// Builds one request with the protocol session (so only its allowlisted
/// builders can produce bytes). Returns the message ID the reply carries.
pub(crate) type Build =
    Box<dyn FnOnce(&mut Session) -> Result<(u32, Vec<u8>), SessionError> + Send>;

enum Pending {
    Info {
        cmd: Command,
        channel: Option<u8>,
    },
    /// A request built by a closure; with `conn`, only sent on that
    /// connection generation (a follow-up step that reuses a message ID).
    Call {
        build: Build,
        conn: Option<u64>,
    },
}

pub(crate) struct QueryReq {
    what: Pending,
    reply: ReplyTx,
}

/// A request whose reply is a media stream (download or cover image).
pub(crate) struct TransferReq {
    pub(crate) kind: TransferKind,
    pub(crate) build: Build,
    pub(crate) sink: Arc<TransferSink>,
    /// For a play: what its stop (7) names.
    pub(crate) play: Option<PlayRequest>,
    /// Only on this connection generation (see [`Pending::Call`]).
    pub(crate) conn: Option<u64>,
}

pub(crate) enum Request {
    Subscribe(NewSub),
    Query(QueryReq),
    Transfer(TransferReq),
    Talk(TalkReq),
}

pub(crate) struct NewSub {
    channel: u8,
    tier: StreamTier,
    queue: Arc<FrameQueue>,
}

/// Handle to a running device session. Dropping it closes the session.
pub struct DeviceSession {
    pub(crate) shared: Arc<Shared>,
    pub(crate) cmd_tx: mpsc::Sender<Request>,
    state_rx: watch::Receiver<ConnState>,
    channels_rx: watch::Receiver<Option<Vec<ChannelInfo>>>,
    channel_uids_rx: watch::Receiver<HashMap<u8, String>>,
    alarms_tx: broadcast::Sender<Arc<AlarmPush>>,
    pub(crate) query_timeout: Duration,
    queue_frames: usize,
    /// Searches and cover requests in progress at once.
    pub(crate) search_permits: Arc<Semaphore>,
    /// Downloads in progress at once.
    pub(crate) download_permits: Arc<Semaphore>,
    pub(crate) download_stall_timeout: Duration,
    pub(crate) download_buffer_bytes: usize,
    /// Plays in progress at once.
    pub(crate) play_permits: Arc<Semaphore>,
    pub(crate) play_stall_timeout: Duration,
    pub(crate) play_buffer_bytes: usize,
    pub(crate) play_group_delay: Duration,
    /// The next play's replay `seq`.
    pub(crate) play_seq: std::sync::atomic::AtomicU32,
    /// Talk-back permissions and rate limit.
    pub(crate) talk_control: Arc<TalkControl>,
    /// Camera motion grants and stop notices.
    pub(crate) ptz_control: Arc<PtzControl>,
    pub(crate) talk_starts_per_minute: u32,
    pub(crate) talk_buffer: usize,
    join: Option<JoinHandle<()>>,
}

impl DeviceSession {
    /// Starts the supervisor on the current tokio runtime. Connecting begins
    /// immediately in the background.
    pub fn spawn<C: Connector>(connector: C, config: SessionConfig) -> DeviceSession {
        let shared = Arc::new(Shared {
            closing: std::sync::atomic::AtomicBool::new(false),
            ended: std::sync::atomic::AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
            retry: std::sync::Mutex::new(Vec::new()),
        });
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        let (state_tx, state_rx) = watch::channel(ConnState::Connecting);
        let (channels_tx, channels_rx) = watch::channel(None);
        let (channel_uids_tx, channel_uids_rx) = watch::channel(HashMap::new());
        let (alarms_tx, _) = broadcast::channel(ALARM_PUSHES_QUEUED);
        let queue_frames = config.queue_frames;
        let query_timeout = config.query_timeout;
        let search_permits = Arc::new(Semaphore::new(config.max_searches.max(1)));
        let download_permits = Arc::new(Semaphore::new(config.max_downloads.max(1)));
        let download_stall_timeout = config.download_stall_timeout;
        let download_buffer_bytes = config.download_buffer_bytes;
        let play_permits = Arc::new(Semaphore::new(config.max_plays.max(1)));
        let play_stall_timeout = config.play_stall_timeout;
        let play_buffer_bytes = config.play_buffer_bytes;
        let play_group_delay = config.play_group_delay;
        // The official client chose increasing seq numbers close to the
        // current Unix time (protocol.md, "Playback"); so does this.
        let play_seq = std::sync::atomic::AtomicU32::new(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.as_secs() as u32),
        );
        let talk_control = Arc::new(TalkControl::new());
        let talk_starts_per_minute = config.talk_starts_per_minute;
        let talk_buffer = config.talk_buffer;
        let talks = Talks::new(
            talk_control.clone(),
            TalkLimits {
                idle_timeout: config.talk_idle_timeout,
                max_duration: config.talk_max_duration,
                catch_up: config.talk_catch_up,
                reply_timeout: config.query_timeout,
                tail: config.talk_tail,
                pace: config.talk_pace.max(Duration::from_millis(1)),
            },
        );
        let ptz_control = Arc::new(PtzControl::new());
        let ptz = Ptz::new(config.ptz, ptz_control.clone());
        let ctx = Ctx {
            cfg: config,
            shared: shared.clone(),
            cmd_rx,
            state_tx,
            channels_tx,
            channel_uids_tx,
            alarms_tx: alarms_tx.clone(),
            subs: Subs::default(),
            queries: Queries::default(),
            transfers: Transfers::default(),
            talks,
            ptz,
            conn_gen: 0,
            limiter: LogLimiter::new(Duration::from_secs(5)),
            wait_log: LogLimiter::new(Duration::from_secs(5)),
            silence_log: LogLimiter::new(Duration::from_secs(30)),
            diag_last: Instant::now(),
            last_escalation: None,
            last_manual_connect: None,
        };
        let join = tokio::spawn(supervise(connector, ctx));
        DeviceSession {
            shared,
            cmd_tx,
            state_rx,
            channels_rx,
            channel_uids_rx,
            alarms_tx,
            query_timeout,
            queue_frames,
            search_permits,
            download_permits,
            download_stall_timeout,
            download_buffer_bytes,
            play_permits,
            play_stall_timeout,
            play_buffer_bytes,
            play_group_delay,
            play_seq,
            talk_control,
            ptz_control,
            talk_starts_per_minute,
            talk_buffer,
            join: Some(join),
        }
    }

    /// Subscribes to a channel's stream tier. The preview starts as soon as
    /// the session is logged in and restarts after every reconnect. Drop the
    /// [`Subscription`] to stop it. Subscriptions to the same channel and
    /// tier share one device preview, which stops with the last of them.
    pub async fn subscribe(
        &self,
        channel: u8,
        tier: StreamTier,
    ) -> Result<Subscription, SessionClosed> {
        if self.shared.is_closing() {
            return Err(SessionClosed);
        }
        let queue = Arc::new(FrameQueue::new(self.queue_frames));
        self.cmd_tx
            .send(Request::Subscribe(NewSub {
                channel,
                tier,
                queue: queue.clone(),
            }))
            .await
            .map_err(|_| SessionClosed)?;
        self.shared.wake.notify_one();
        // A send that reserved its slot before the supervisor closed the
        // channel can still land after the final drain, and nobody would
        // ever close this queue. `ended` is set before the close, so if
        // the request was missed, it is visible here.
        if self.shared.ended.load(std::sync::atomic::Ordering::SeqCst) {
            queue.close();
        }
        Ok(Subscription {
            channel,
            tier,
            queue,
            shared: self.shared.clone(),
        })
    }

    /// Retries one stream by hand, now: its preview is stopped and started
    /// again (a new message ID, on the same login), whatever state it is in
    /// (waiting, refused, silent). When the device is in the reconnect
    /// backoff, the next connection attempt is made at once instead of
    /// after the wait. Never faster than [`MANUAL_RETRY_FLOOR`] per stream
    /// or per connection attempt, and never when the device refused the
    /// login ([`ConnState::LoginFailed`] stays terminal) or the session is
    /// closed. A stream nobody subscribed to is ignored.
    pub fn retry_stream(&self, channel: u8, tier: StreamTier) {
        if self.shared.is_closing()
            || matches!(
                *self.state_rx.borrow(),
                ConnState::LoginFailed { .. } | ConnState::Closed
            )
        {
            return;
        }
        {
            let mut q = self
                .shared
                .retry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if q.len() >= MAX_PENDING_RETRIES || q.contains(&(channel, tier)) {
                return;
            }
            q.push((channel, tier));
        }
        self.shared.wake.notify_one();
    }

    /// Sends a read-only info query (80, 146, 199, or 318 with a channel)
    /// and waits for its reply. A query made before login is sent once the
    /// session logs in; one in flight when the connection drops fails with
    /// [`QueryError::ConnectionLost`] and is not resent.
    pub async fn query(&self, cmd: Command, channel: Option<u8>) -> Result<QueryReply, QueryError> {
        let allowed = match cmd {
            Command::VersionInfo | Command::StreamInfoList | Command::Support => true,
            Command::ChannelVersionInfo => channel.is_some(),
            _ => false,
        };
        if !allowed {
            return Err(QueryError::NotAllowed);
        }
        if self.shared.is_closing() {
            return Err(QueryError::Closed);
        }
        let r = self.request(Pending::Info { cmd, channel }).await?;
        Ok(QueryReply {
            status: r.status,
            xml: r.xml,
        })
    }

    async fn request(&self, what: Pending) -> Result<Reply, QueryError> {
        if self.shared.is_closing() {
            return Err(QueryError::Closed);
        }
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(Request::Query(QueryReq { what, reply: tx }))
            .await
            .map_err(|_| QueryError::Closed)?;
        self.shared.wake.notify_one();
        match timeout(self.query_timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(QueryError::Closed),
            Err(_) => Err(QueryError::TimedOut),
        }
    }

    /// Sends a request built by `build` and waits for its reply. With
    /// `conn`, the request goes out only on that connection generation and
    /// otherwise fails with [`QueryError::ConnectionLost`] (a follow-up step
    /// that reuses a message ID is meaningless on a new connection).
    pub(crate) async fn call(&self, build: Build, conn: Option<u64>) -> Result<Reply, QueryError> {
        self.request(Pending::Call { build, conn }).await
    }

    /// Each channel's camera device ID (`uid`), from the latest channel list
    /// the device pushed (command 145). Recording searches send it.
    pub fn channel_uids(&self) -> HashMap<u8, String> {
        self.channel_uids_rx.borrow().clone()
    }

    /// The device's channel list, as last pushed by the device (command 145)
    /// after login; `None` until it arrives.
    pub fn watch_channels(&self) -> watch::Receiver<Option<Vec<ChannelInfo>>> {
        self.channels_rx.clone()
    }

    /// The device's alarm-state pushes (command 33), each a full snapshot,
    /// from now on. The session subscribes to them after every login
    /// (command 192) unless [`SessionConfig::alarm_report`] is off. A
    /// receiver that falls more than a few pushes behind skips to newer
    /// ones (`RecvError::Lagged`).
    pub fn alarms(&self) -> broadcast::Receiver<Arc<AlarmPush>> {
        self.alarms_tx.subscribe()
    }

    /// The current connection state.
    pub fn state(&self) -> ConnState {
        self.state_rx.borrow().clone()
    }

    /// A receiver that observes state changes.
    pub fn watch_state(&self) -> watch::Receiver<ConnState> {
        self.state_rx.clone()
    }

    /// Stops every preview, closes the socket, and waits for the supervisor
    /// to finish.
    pub async fn close(mut self) {
        self.begin_close();
        if let Some(j) = self.join.take() {
            let _ = j.await;
        }
    }

    fn begin_close(&self) {
        self.shared.closing.store(true, Ordering::SeqCst);
        self.shared.wake.notify_one();
    }
}

impl Drop for DeviceSession {
    fn drop(&mut self) {
        // The supervisor notices, stops previews, and closes the socket in
        // the background; it ends even if nobody awaits it.
        self.begin_close();
    }
}

impl fmt::Debug for DeviceSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceSession")
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

/// Allows one log line per `every`, counting what it suppressed.
struct LogLimiter {
    every: Duration,
    last: Option<Instant>,
    suppressed: u64,
}

impl LogLimiter {
    fn new(every: Duration) -> LogLimiter {
        LogLimiter {
            every,
            last: None,
            suppressed: 0,
        }
    }

    /// `Some(suppressed_since_last_line)` when a line may be logged now.
    fn allow(&mut self) -> Option<u64> {
        let now = Instant::now();
        match self.last {
            Some(t) if now.duration_since(t) < self.every => {
                self.suppressed += 1;
                None
            }
            _ => {
                self.last = Some(now);
                Some(std::mem::take(&mut self.suppressed))
            }
        }
    }
}

/// One device preview, shared by every subscription to the same channel and
/// tier. The stream is demultiplexed once; each subscriber has its own queue.
struct Stream {
    /// Message ID of the running start request; `None` until it is sent on
    /// the current connection.
    msg_id: Option<u32>,
    demux: Demuxer,
    subs: Vec<Arc<FrameQueue>>,
    /// The latest stream info record, replayed to subscribers that attach
    /// mid-stream (everything else they wait for at a keyframe).
    info: Option<Record>,
    /// Status of a refused start, replayed to late subscribers.
    rejected: Option<u16>,
    /// Media has arrived on the current start's message ID.
    saw_media: bool,
    /// The start was answered with 400 and no media has arrived yet.
    waiting: Option<Wait>,
    /// Last time the stream started or media arrived on it (the silence
    /// watchdog counts from here).
    watch_from: Instant,
    /// Silent restarts since a picture last arrived.
    silent: u32,
    /// The running start is a slow retry after a refusal; the stream stays
    /// `rejected` (and visible as such) until media arrives.
    slow: bool,
    /// When to try a refused (400) stream again; `None` for a terminal
    /// refusal.
    slow_retry_at: Option<Instant>,
    /// The user asked to restart this stream now.
    force: bool,
    last_manual: Option<Instant>,
    /// Since when no picture has arrived (`None` while one has).
    since: Option<Instant>,
}

/// A preview answered with 400, waiting for the device's stream
/// ([`SessionConfig::preview_retry_interval`]).
#[derive(Clone, Copy, Debug)]
struct Wait {
    /// Status that started the wait, replayed to late subscribers.
    status: u16,
    /// Give up at this time ([`SessionConfig::preview_wait_limit`] after the
    /// first 400).
    until: Instant,
    /// Stop and restart the preview at this time; `None` until the current
    /// start has been answered with 400 (never two starts outstanding).
    retry_at: Option<Instant>,
    /// Restarts sent so far.
    retries: u32,
}

impl Stream {
    fn new() -> Stream {
        Stream {
            msg_id: None,
            demux: Demuxer::new(),
            subs: Vec::new(),
            info: None,
            rejected: None,
            saw_media: false,
            waiting: None,
            watch_from: Instant::now(),
            silent: 0,
            slow: false,
            slow_retry_at: None,
            force: false,
            last_manual: None,
            since: Some(Instant::now()),
        }
    }

    /// The earliest time a wait needs attention.
    fn wait_due(&self) -> Option<Instant> {
        let w = self.waiting?;
        Some(w.retry_at.map_or(w.until, |r| r.min(w.until)))
    }

    /// How long a started preview may stay silent: the configured time,
    /// doubled for every silent restart in a row up to the cap. A slow retry
    /// after a refusal gets one short window.
    fn silence_window(&self, cfg: &SessionConfig) -> Option<Duration> {
        let base = cfg.preview_silence_timeout;
        if self.slow {
            return Some(if base.is_zero() {
                cfg.preview_retry_interval
            } else {
                base
            });
        }
        if base.is_zero() {
            return None;
        }
        let cap = cfg.preview_restart_cap.max(base);
        Some(base.saturating_mul(1u32 << self.silent.min(16)).min(cap))
    }

    /// When a started preview that has delivered nothing is stopped and
    /// started again (or, for a slow retry, given up until the next one).
    fn silence_due(&self, cfg: &SessionConfig) -> Option<Instant> {
        if self.msg_id.is_none() || self.waiting.is_some() || self.force {
            return None;
        }
        if self.rejected.is_some() && !self.slow {
            return None;
        }
        Some(self.watch_from + self.silence_window(cfg)?)
    }

    /// The earliest time this stream needs attention.
    fn due(&self, cfg: &SessionConfig) -> Option<Instant> {
        let slow = if self.msg_id.is_none() {
            self.slow_retry_at
        } else {
            None
        };
        [self.wait_due(), self.silence_due(cfg), slow]
            .into_iter()
            .flatten()
            .min()
    }

    /// Why this stream has no picture, for the periodic diagnostic.
    fn problem(&self) -> &'static str {
        if self.waiting.is_some() {
            "waiting on 400"
        } else if self.rejected.is_some() {
            if self.slow_retry_at.is_some() || self.slow {
                "rejected (retrying slowly)"
            } else {
                "rejected"
            }
        } else if self.msg_id.is_none() {
            "not started"
        } else if self.silent > 0 {
            "restarting (silent after start)"
        } else {
            "silent after start"
        }
    }
}

type StreamKey = (u8, StreamTier);

#[derive(Default)]
struct Subs {
    streams: HashMap<StreamKey, Stream>,
    by_msg: HashMap<u32, StreamKey>,
}

/// Hands `rec` to every queue that would keep it. Queues that are still
/// waiting for a keyframe discard it, so they cost no copy; the last
/// accepting queue gets the record itself.
fn fan_out(queues: &[Arc<FrameQueue>], rec: Record) {
    let accepting: Vec<&Arc<FrameQueue>> = queues.iter().filter(|q| q.accepts(&rec)).collect();
    let Some((last, rest)) = accepting.split_last() else {
        return;
    };
    for q in rest {
        q.push_media(rec.clone());
    }
    last.push_media(rec);
}

impl Subs {
    fn add(&mut self, n: NewSub) {
        let st = self
            .streams
            .entry((n.channel, n.tier))
            .or_insert_with(Stream::new);
        // A late subscriber starts gated (the queue's initial state): it
        // sees video from the next keyframe on, preceded by the stream info.
        if let Some(info) = &st.info {
            n.queue.push_media(info.clone());
        }
        if let Some(w) = &st.waiting {
            n.queue
                .push_control(Item::WaitingForDevice { status: w.status });
        }
        if let Some(status) = st.rejected {
            n.queue.push_control(Item::StartRejected { status });
        }
        st.subs.push(n.queue);
    }

    /// The connection is gone: previews must be restarted on the next one.
    fn connection_lost(&mut self) {
        let now = Instant::now();
        self.by_msg.clear();
        for st in self.streams.values_mut() {
            st.msg_id = None;
            st.demux = Demuxer::new();
            st.info = None;
            st.rejected = None;
            st.saw_media = false;
            st.waiting = None;
            st.silent = 0;
            st.slow = false;
            st.slow_retry_at = None;
            st.force = false;
            st.watch_from = now;
            st.since.get_or_insert(now);
            for q in &st.subs {
                q.discontinuity();
            }
        }
    }

    /// Removes subscriptions their owners dropped (without device traffic).
    fn reap_offline(&mut self) {
        for st in self.streams.values_mut() {
            st.subs.retain(|q| !q.cancelled.load(Ordering::SeqCst));
        }
        self.streams.retain(|_, st| !st.subs.is_empty());
    }

    /// The earliest time a preview needs attention (a wait, the silence
    /// watchdog, a slow retry).
    fn due(&self, cfg: &SessionConfig) -> Option<Instant> {
        self.streams.values().filter_map(|s| s.due(cfg)).min()
    }

    /// Marks a stream for an immediate restart. False when it is unknown or
    /// was retried by hand less than [`MANUAL_RETRY_FLOOR`] ago.
    fn manual_retry(&mut self, key: StreamKey, now: Instant) -> bool {
        let Some(st) = self.streams.get_mut(&key) else {
            return false;
        };
        if st.last_manual.is_some_and(|t| now < t + MANUAL_RETRY_FLOOR) {
            return false;
        }
        st.last_manual = Some(now);
        st.force = true;
        true
    }

    /// Streams without a picture for over [`STUCK_AFTER`], and why.
    fn stuck(&self, now: Instant) -> Vec<(StreamKey, &'static str)> {
        let mut v: Vec<(StreamKey, &'static str)> = self
            .streams
            .iter()
            .filter(|(_, st)| {
                !st.subs.is_empty() && st.since.is_some_and(|t| now >= t + STUCK_AFTER)
            })
            .map(|(k, st)| (*k, st.problem()))
            .collect();
        v.sort_by_key(|((c, t), _)| (*c, format!("{t:?}")));
        v
    }

    fn close_all(&mut self) {
        for st in self.streams.values() {
            for q in &st.subs {
                q.close();
            }
        }
        self.streams.clear();
        self.by_msg.clear();
    }
}

/// Info queries waiting for login, and those sent and awaiting a reply.
#[derive(Default)]
struct Queries {
    waiting: VecDeque<QueryReq>,
    in_flight: HashMap<u32, ReplyTx>,
}

impl Queries {
    fn add(&mut self, q: QueryReq) {
        // Forget queries whose callers gave up (timed out or dropped).
        self.waiting.retain(|w| !w.reply.is_closed());
        self.in_flight.retain(|_, tx| !tx.is_closed());
        if self.waiting.len() + self.in_flight.len() >= MAX_QUERIES {
            let _ = q.reply.send(Err(QueryError::Busy));
        } else {
            self.waiting.push_back(q);
        }
    }

    /// The connection is gone: replies to queries already sent will never
    /// arrive. Waiting ones are sent on the next connection.
    fn connection_lost(&mut self) {
        for (_, tx) in self.in_flight.drain() {
            let _ = tx.send(Err(QueryError::ConnectionLost));
        }
    }

    fn close_all(&mut self) {
        self.connection_lost();
        for q in self.waiting.drain(..) {
            let _ = q.reply.send(Err(QueryError::Closed));
        }
    }
}

/// One running download or cover request.
struct ActiveTransfer {
    kind: TransferKind,
    sink: Arc<TransferSink>,
    /// For a play: what its stop (7) names.
    play: Option<PlayRequest>,
    demux: Demuxer,
    /// The sink refused data (overflow) or the media was corrupt: stop it.
    abort: bool,
}

/// Downloads and cover requests waiting for login, and those running.
#[derive(Default)]
struct Transfers {
    waiting: VecDeque<TransferReq>,
    active: HashMap<u32, ActiveTransfer>,
}

impl Transfers {
    fn add(&mut self, t: TransferReq) {
        self.waiting.retain(|w| !w.sink.is_cancelled());
        if self.waiting.len() + self.active.len() >= MAX_QUERIES {
            t.sink.fail(TransferItem::Busy);
        } else {
            self.waiting.push_back(t);
        }
    }

    /// Running transfers fail; waiting ones are sent on the next connection.
    fn connection_lost(&mut self) {
        for (_, t) in self.active.drain() {
            t.sink.fail(TransferItem::ConnectionLost);
        }
    }

    fn close_all(&mut self) {
        for (_, t) in self.active.drain() {
            t.sink.fail(TransferItem::Closed);
        }
        for t in self.waiting.drain(..) {
            t.sink.fail(TransferItem::Closed);
        }
    }
}

struct Ctx {
    cfg: SessionConfig,
    shared: Arc<Shared>,
    cmd_rx: mpsc::Receiver<Request>,
    state_tx: watch::Sender<ConnState>,
    channels_tx: watch::Sender<Option<Vec<ChannelInfo>>>,
    channel_uids_tx: watch::Sender<HashMap<u8, String>>,
    alarms_tx: broadcast::Sender<Arc<AlarmPush>>,
    subs: Subs,
    queries: Queries,
    transfers: Transfers,
    talks: Talks,
    ptz: Ptz,
    /// Incremented on every successful login.
    conn_gen: u64,
    limiter: LogLimiter,
    /// Previews answered with 400 and their restarts.
    wait_log: LogLimiter,
    /// Silent-preview restarts.
    silence_log: LogLimiter,
    /// When the stuck-tile diagnostic last ran.
    diag_last: Instant,
    /// When silent previews last forced a reconnect.
    last_escalation: Option<Instant>,
    /// When a manual retry last cut a reconnect backoff short.
    last_manual_connect: Option<Instant>,
}

impl Ctx {
    fn request(&mut self, r: Request) {
        match r {
            Request::Subscribe(n) => self.subs.add(n),
            Request::Query(q) => self.queries.add(q),
            Request::Transfer(t) => self.transfers.add(t),
            Request::Talk(t) => self.talks.add(t),
        }
    }

    /// Takes the manual retries (by stream) the handle queued. True when
    /// at least one was accepted.
    fn apply_retries(&mut self) -> bool {
        let pending = std::mem::take(
            &mut *self
                .shared
                .retry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let now = Instant::now();
        let mut any = false;
        for key in pending {
            any |= self.subs.manual_retry(key, now);
        }
        any
    }

    /// Once a minute, when something is wrong: which tiles have had no
    /// picture for over a minute and why (channel and tier only).
    fn maybe_diagnose(&mut self) {
        let now = Instant::now();
        if now < self.diag_last + DIAGNOSTIC_INTERVAL {
            return;
        }
        self.diag_last = now;
        let stuck = self.subs.stuck(now);
        if stuck.is_empty() {
            return;
        }
        let mut detail = String::new();
        for ((channel, tier), why) in stuck.iter().take(16) {
            if !detail.is_empty() {
                detail.push_str("; ");
            }
            detail.push_str(&format!("channel {channel} {tier:?}: {why}"));
        }
        if stuck.len() > 16 {
            detail.push_str(&format!("; and {} more", stuck.len() - 16));
        }
        warn!(
            tiles = stuck.len(),
            state = ?*self.state_tx.borrow(),
            %detail,
            "tiles without a picture for over a minute"
        );
    }

    /// The connection is gone (failed or closed).
    fn connection_lost(&mut self) {
        self.subs.connection_lost();
        self.queries.connection_lost();
        self.transfers.connection_lost();
        self.talks.connection_lost();
        self.ptz.connection_lost();
    }

    /// Ends the session for good: refuses new subscriptions, and closes every
    /// queue (including requests still waiting in the command channel) so
    /// subscribers see the end instead of waiting forever.
    fn finish(&mut self) {
        // Before the channel closes, so a subscriber whose request lands
        // after the drain below sees it (see `DeviceSession::subscribe`).
        self.shared
            .ended
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.cmd_rx.close();
        while let Ok(r) = self.cmd_rx.try_recv() {
            self.request(r);
        }
        self.subs.close_all();
        self.queries.close_all();
        self.transfers.close_all();
        self.talks.finish();
        self.ptz.finish();
    }
}

enum Exit {
    /// Close requested; previews were stopped and the socket shut down.
    Closed,
    Rejected {
        status: u16,
        remain_times: Option<u32>,
        unlock_secs: Option<u32>,
    },
    Failed {
        reached_login: bool,
        reason: String,
    },
}

async fn wait_closing(shared: &Shared) {
    loop {
        if shared.is_closing() {
            return;
        }
        shared.wake.notified().await;
    }
}

async fn supervise<C: Connector>(connector: C, mut ctx: Ctx) {
    let mut attempt: u32 = 0;
    let mut jitter = Jitter::new();
    loop {
        if ctx.shared.is_closing() {
            break;
        }
        ctx.maybe_diagnose();
        ctx.state_tx.send_replace(ConnState::Connecting);
        let started = Instant::now();
        let connected = tokio::select! {
            r = timeout(ctx.cfg.connect_timeout, connector.connect()) => match r {
                Ok(Ok(s)) => Ok(s),
                Ok(Err(e)) => Err(format!("connect failed: {}", e.kind())),
                Err(_) => Err("connect timed out".to_owned()),
            },
            _ = wait_closing(&ctx.shared) => break,
        };
        let exit = match connected {
            Ok(stream) => run_connection(&mut ctx, stream).await,
            Err(reason) => Exit::Failed {
                reached_login: false,
                reason,
            },
        };
        match exit {
            Exit::Closed => break,
            Exit::Rejected {
                status,
                remain_times,
                unlock_secs,
            } => {
                warn!(
                    status,
                    ?remain_times,
                    ?unlock_secs,
                    "login refused; not retrying"
                );
                ctx.state_tx.send_replace(ConnState::LoginFailed {
                    status,
                    remain_times,
                    unlock_secs,
                });
                ctx.finish();
                return;
            }
            Exit::Failed {
                reached_login,
                reason,
            } => {
                ctx.connection_lost();
                if reached_login && started.elapsed() >= ctx.cfg.backoff.stable_after {
                    attempt = 0;
                }
                let delay = ctx.cfg.backoff.delay(attempt, jitter.unit());
                attempt = attempt.saturating_add(1);
                if let Some(n) = ctx.limiter.allow() {
                    warn!(attempt, retry_in_ms = delay.as_millis() as u64, suppressed = n, %reason, "connection failed");
                }
                ctx.state_tx
                    .send_replace(ConnState::Backoff { attempt, delay });
                if sleep_backoff(&mut ctx, delay).await {
                    break;
                }
            }
        }
    }
    ctx.finish();
    ctx.state_tx.send_replace(ConnState::Closed);
}

/// Waits `delay`, serving subscribe requests meanwhile. True when closing.
async fn sleep_backoff(ctx: &mut Ctx, delay: Duration) -> bool {
    let deadline = Instant::now() + delay;
    loop {
        if ctx.shared.is_closing() {
            return true;
        }
        ctx.subs.reap_offline();
        if ctx.apply_retries() {
            // A manual retry: connect now, but never faster than the floor
            // (the device's login limits).
            let now = Instant::now();
            if ctx
                .last_manual_connect
                .is_none_or(|t| now >= t + MANUAL_RETRY_FLOOR)
            {
                ctx.last_manual_connect = Some(now);
                return false;
            }
        }
        tokio::select! {
            _ = sleep_until(deadline) => return false,
            _ = ctx.shared.wake.notified() => {}
            cmd = ctx.cmd_rx.recv() => match cmd {
                Some(r) => ctx.request(r),
                None => return true,
            },
        }
    }
}

type Failure = String;

struct Conn<R, W> {
    rd: R,
    wr: W,
    dec: Decoder,
    buf: Vec<u8>,
    session: Session,
    write_timeout: Duration,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Conn<R, W> {
    async fn send(&mut self, bytes: &[u8]) -> Result<(), Failure> {
        match timeout(self.write_timeout, self.wr.write_all(bytes)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(format!("write failed: {}", e.kind())),
            Err(_) => Err("write timed out".to_owned()),
        }
    }

    /// Reads more bytes into the decoder; `Err` on EOF or I/O error.
    async fn fill(&mut self) -> Result<(), Failure> {
        match self.rd.read(&mut self.buf).await {
            Ok(0) => Err("closed by device".to_owned()),
            Ok(n) => {
                self.dec.push(&self.buf[..n]);
                Ok(())
            }
            Err(e) => Err(format!("read failed: {}", e.kind())),
        }
    }

    /// The next typed event, reading as needed (login phase).
    async fn next_event(&mut self) -> Result<Event, Failure> {
        loop {
            match self.dec.next_message() {
                Ok(Some(m)) => return self.session.handle(&m).map_err(|e| e.to_string()),
                Ok(None) => self.fill().await?,
                Err(e) => return Err(format!("framing error: {e}")),
            }
        }
    }
}

enum Login {
    Ok,
    Rejected {
        status: u16,
        remain_times: Option<u32>,
        unlock_secs: Option<u32>,
    },
}

async fn login<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    c: &mut Conn<R, W>,
) -> Result<Login, Failure> {
    let hs = c.session.handshake().map_err(|e| e.to_string())?;
    c.send(&hs).await?;
    loop {
        match c.next_event().await? {
            Event::NonceReceived => {
                let l = c.session.login().map_err(|e| e.to_string())?;
                c.send(&l).await?;
            }
            Event::LoginOk => return Ok(Login::Ok),
            Event::LoginRejected {
                status,
                remain_times,
                unlock_secs,
            } => {
                return Ok(Login::Rejected {
                    status,
                    remain_times,
                    unlock_secs,
                });
            }
            _ => {}
        }
    }
}

async fn run_connection<S>(ctx: &mut Ctx, stream: S) -> Exit
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (rd, wr) = tokio::io::split(stream);
    let mut c = Conn {
        rd,
        wr,
        dec: Decoder::new(),
        buf: vec![0u8; 64 * 1024],
        session: Session::new(ctx.cfg.user.clone(), ctx.cfg.password.clone()),
        write_timeout: ctx.cfg.write_timeout,
    };
    let result = tokio::select! {
        r = timeout(ctx.cfg.login_timeout, login(&mut c)) => match r {
            Ok(r) => r,
            Err(_) => Err("login timed out".to_owned()),
        },
        _ = wait_closing(&ctx.shared) => return Exit::Closed,
    };
    match result {
        Ok(Login::Ok) => {}
        Ok(Login::Rejected {
            status,
            remain_times,
            unlock_secs,
        }) => {
            return Exit::Rejected {
                status,
                remain_times,
                unlock_secs,
            };
        }
        Err(reason) => {
            return Exit::Failed {
                reached_login: false,
                reason,
            };
        }
    }
    info!("logged in");
    ctx.conn_gen += 1;
    // Ask for detection pushes (192), as the official client does right
    // after login. Its empty reply needs no handling; the pushes (33) are
    // recognized by command number.
    if ctx.cfg.alarm_report {
        let sent = match c.session.start_alarm_report() {
            Ok((_, bytes)) => c.send(&bytes).await,
            Err(e) => Err(e.to_string()),
        };
        if let Err(reason) = sent {
            return Exit::Failed {
                reached_login: true,
                reason,
            };
        }
    }
    ctx.state_tx.send_replace(ConnState::Connected);
    match connected_loop(ctx, &mut c).await {
        Ok(()) => {
            graceful_close(ctx, &mut c).await;
            Exit::Closed
        }
        Err(reason) => Exit::Failed {
            reached_login: true,
            reason,
        },
    }
}

/// Stops every active preview, then closes the write side. Best effort, each
/// step bounded by the write timeout.
async fn graceful_close<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    ctx: &mut Ctx,
    c: &mut Conn<R, W>,
) {
    // Stop every moving camera and close every talk first (18 stop, 11):
    // a camera must not keep moving, a speaker must not stay open.
    for bytes in ctx
        .ptz
        .close_all(&mut c.session)
        .into_iter()
        .chain(ctx.talks.close_all(&mut c.session))
    {
        if c.send(&bytes).await.is_err() {
            break;
        }
    }
    // Stop running downloads (144) and plays (7) first, so the device
    // stops sending.
    let running: Vec<u32> = ctx
        .transfers
        .active
        .iter()
        .filter(|(_, t)| matches!(t.kind, TransferKind::Download | TransferKind::Play))
        .map(|(id, _)| *id)
        .collect();
    for id in running {
        let Some(t) = ctx.transfers.active.remove(&id) else {
            continue;
        };
        let sent = match stop_bytes(&mut c.session, id, &t) {
            Some(bytes) => c.send(&bytes).await.is_ok(),
            None => true,
        };
        t.sink.mark_stopped();
        t.sink.fail(TransferItem::Closed);
        if !sent {
            break;
        }
    }
    let ids: Vec<u32> = c.session.active_previews().collect();
    for id in ids {
        match c.session.stop_preview(id) {
            Ok(bytes) => {
                if c.send(&bytes).await.is_err() {
                    break;
                }
            }
            Err(e) => debug!(%e, "stop_preview failed"),
        }
    }
    // Log out and wait briefly for the reply, as the official client does,
    // so the device frees the login slot at once.
    if let Ok(bytes) = c.session.logout()
        && c.send(&bytes).await.is_ok()
    {
        let reply = timeout(ctx.cfg.logout_timeout, async {
            loop {
                match c.next_event().await {
                    Ok(Event::Control {
                        command: 2, status, ..
                    }) => return Some(status),
                    Ok(_) => {}
                    Err(_) => return None,
                }
            }
        })
        .await;
        match reply {
            Ok(Some(status)) => debug!(status, "logged out"),
            _ => debug!("no logout reply"),
        }
    }
    let _ = timeout(c.write_timeout, c.wr.shutdown()).await;
    ctx.connection_lost();
}

/// When every started preview has been restarted for silence
/// [`SessionConfig::preview_escalate_after`] times in a row (and one more is
/// due), the connection itself is suspect: fail it, so the supervisor
/// reconnects through its normal backoff. At most once per
/// [`ESCALATE_MIN_INTERVAL`], so a camera that is simply off cannot make the
/// session log in over and over; and never while any started preview is
/// still getting media.
fn escalate_if_all_silent(ctx: &mut Ctx) -> Result<(), Failure> {
    let n = ctx.cfg.preview_escalate_after;
    if n == 0 {
        return Ok(());
    }
    let now = Instant::now();
    let mut started = 0usize;
    let mut due = false;
    for st in ctx.subs.streams.values() {
        if st.msg_id.is_none() || st.waiting.is_some() || st.rejected.is_some() {
            continue;
        }
        started += 1;
        if st.silent < n {
            return Ok(());
        }
        due |= st.silence_due(&ctx.cfg).is_some_and(|t| now >= t);
    }
    if started == 0
        || !due
        || ctx
            .last_escalation
            .is_some_and(|t| now < t + ESCALATE_MIN_INTERVAL)
    {
        return Ok(());
    }
    ctx.last_escalation = Some(now);
    warn!(
        streams = started,
        silent_restarts = n,
        "every started preview stayed silent; reconnecting"
    );
    Err("every started preview stayed silent".to_owned())
}

/// Starts one preview per stream that has subscribers, and stops the stream
/// when its last subscriber was dropped.
async fn sync_previews<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    ctx: &mut Ctx,
    c: &mut Conn<R, W>,
) -> Result<(), Failure> {
    escalate_if_all_silent(ctx)?;
    let keys: Vec<StreamKey> = ctx.subs.streams.keys().copied().collect();
    for key in keys {
        let Some(st) = ctx.subs.streams.get_mut(&key) else {
            continue;
        };
        st.subs.retain(|q| !q.cancelled.load(Ordering::SeqCst));
        if st.subs.is_empty() {
            let mid = st.msg_id;
            ctx.subs.streams.remove(&key);
            if let Some(mid) = mid {
                ctx.subs.by_msg.remove(&mid);
                let bytes = c.session.stop_preview(mid).map_err(|e| e.to_string())?;
                c.send(&bytes).await?;
            }
        } else if st.force {
            // The user asked for a fresh start: forget what was known.
            st.force = false;
            if let Some(old) = st.msg_id.take() {
                ctx.subs.by_msg.remove(&old);
                let bytes = c.session.stop_preview(old).map_err(|e| e.to_string())?;
                c.send(&bytes).await?;
            }
            st.waiting = None;
            st.rejected = None;
            st.slow = false;
            st.slow_retry_at = None;
            st.silent = 0;
            let (mid, bytes) = c
                .session
                .start_preview(key.0, key.1)
                .map_err(|e| e.to_string())?;
            st.msg_id = Some(mid);
            st.demux = Demuxer::new();
            st.saw_media = false;
            st.watch_from = Instant::now();
            ctx.subs.by_msg.insert(mid, key);
            info!(
                channel = key.0,
                tier = ?key.1,
                "preview restarted by hand"
            );
            c.send(&bytes).await?;
        } else if st.msg_id.is_none()
            && (st.rejected.is_none() || st.slow_retry_at.is_some_and(|t| Instant::now() >= t))
        {
            // A first start, or the slow retry of a refused (400) stream.
            st.slow = st.rejected.is_some();
            st.slow_retry_at = None;
            st.watch_from = Instant::now();
            st.saw_media = false;
            let (mid, bytes) = c
                .session
                .start_preview(key.0, key.1)
                .map_err(|e| e.to_string())?;
            st.msg_id = Some(mid);
            ctx.subs.by_msg.insert(mid, key);
            c.send(&bytes).await?;
        } else if let Some(w) = st.waiting {
            let now = Instant::now();
            if now >= w.until {
                // Waited long enough: the device refused the stream, for
                // now. A 400 is tried again slowly; the app keeps seeing
                // the refusal until media arrives.
                let was_slow = st.slow;
                st.waiting = None;
                st.rejected = Some(w.status);
                st.slow = false;
                st.slow_retry_at = (!ctx.cfg.preview_slow_retry.is_zero())
                    .then(|| now + ctx.cfg.preview_slow_retry);
                let mid = st.msg_id.take();
                if !was_slow {
                    for q in &st.subs {
                        q.push_control(Item::StartRejected { status: w.status });
                    }
                    warn!(
                        channel = key.0,
                        tier = ?key.1,
                        status = w.status,
                        retries = w.retries,
                        "device refused the stream: no media after waiting; retrying slowly"
                    );
                } else if let Some(n) = ctx.wait_log.allow() {
                    debug!(
                        channel = key.0,
                        tier = ?key.1,
                        suppressed = n,
                        "slow retry of a refused stream was refused again"
                    );
                }
                if let Some(mid) = mid {
                    ctx.subs.by_msg.remove(&mid);
                    let bytes = c.session.stop_preview(mid).map_err(|e| e.to_string())?;
                    c.send(&bytes).await?;
                }
            } else if w.retry_at.is_some_and(|t| now >= t) {
                // As the official client does: stop the refused start, then
                // send the same start again (with a new message ID).
                if let Some(old) = st.msg_id.take() {
                    ctx.subs.by_msg.remove(&old);
                    let bytes = c.session.stop_preview(old).map_err(|e| e.to_string())?;
                    c.send(&bytes).await?;
                }
                let (mid, bytes) = c
                    .session
                    .start_preview(key.0, key.1)
                    .map_err(|e| e.to_string())?;
                st.msg_id = Some(mid);
                st.demux = Demuxer::new();
                st.saw_media = false;
                st.watch_from = now;
                st.waiting = Some(Wait {
                    retry_at: None,
                    retries: w.retries + 1,
                    ..w
                });
                ctx.subs.by_msg.insert(mid, key);
                if let Some(n) = ctx.wait_log.allow() {
                    debug!(
                        channel = key.0,
                        tier = ?key.1,
                        retry = w.retries + 1,
                        suppressed = n,
                        "restarting a preview the device answered with 400"
                    );
                }
                c.send(&bytes).await?;
            }
        } else if st
            .silence_due(&ctx.cfg)
            .is_some_and(|t| Instant::now() >= t)
        {
            // Started, accepted, and nothing arrived: stop it and start it
            // again, on the same login. Waits double up to the cap.
            let now = Instant::now();
            let old = st.msg_id.take();
            if let Some(old) = old {
                ctx.subs.by_msg.remove(&old);
                let bytes = c.session.stop_preview(old).map_err(|e| e.to_string())?;
                c.send(&bytes).await?;
            }
            if st.slow {
                // A slow retry that stayed silent: still refused.
                st.slow = false;
                st.slow_retry_at = Some(now + ctx.cfg.preview_slow_retry);
            } else {
                st.since.get_or_insert(st.watch_from);
                st.silent += 1;
                if let Some(n) = ctx.silence_log.allow() {
                    warn!(
                        channel = key.0,
                        tier = ?key.1,
                        silent_restarts = st.silent,
                        suppressed = n,
                        "no media after the preview start; restarting it"
                    );
                }
                let (mid, bytes) = c
                    .session
                    .start_preview(key.0, key.1)
                    .map_err(|e| e.to_string())?;
                st.msg_id = Some(mid);
                st.demux = Demuxer::new();
                st.saw_media = false;
                st.watch_from = now;
                ctx.subs.by_msg.insert(mid, key);
                c.send(&bytes).await?;
            }
        }
    }
    Ok(())
}

/// Sends queries and calls that were waiting for login.
async fn sync_queries<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    ctx: &mut Ctx,
    c: &mut Conn<R, W>,
) -> Result<(), Failure> {
    while let Some(q) = ctx.queries.waiting.pop_front() {
        if q.reply.is_closed() {
            continue;
        }
        let built = match q.what {
            Pending::Info { cmd, channel } => c.session.query_info(cmd, channel),
            Pending::Call { conn: Some(g), .. } if g != ctx.conn_gen => {
                let _ = q.reply.send(Err(QueryError::ConnectionLost));
                continue;
            }
            Pending::Call { build, .. } => build(&mut c.session),
        };
        match built {
            Ok((id, bytes)) => {
                ctx.queries.in_flight.insert(id, q.reply);
                c.send(&bytes).await?;
            }
            Err(e) => {
                debug!(%e, "request not built");
                let _ = q.reply.send(Err(QueryError::NotAllowed));
            }
        }
    }
    Ok(())
}

/// Starts downloads and cover requests that were waiting, and stops those
/// whose owner cancelled them (144 for a download) or whose data could not
/// be taken.
async fn sync_transfers<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    ctx: &mut Ctx,
    c: &mut Conn<R, W>,
) -> Result<(), Failure> {
    let stop: Vec<u32> = ctx
        .transfers
        .active
        .iter()
        .filter(|(_, t)| t.abort || t.sink.is_cancelled())
        .map(|(id, _)| *id)
        .collect();
    for id in stop {
        let Some(t) = ctx.transfers.active.remove(&id) else {
            continue;
        };
        if let Some(bytes) = stop_bytes(&mut c.session, id, &t) {
            c.send(&bytes).await?;
            debug!(id, "transfer stopped");
        }
        t.sink.mark_stopped();
        t.sink.fail(TransferItem::Cancelled);
    }
    while let Some(t) = ctx.transfers.waiting.pop_front() {
        if t.sink.is_cancelled() {
            t.sink.fail(TransferItem::Cancelled);
            continue;
        }
        if t.conn.is_some_and(|g| g != ctx.conn_gen) {
            t.sink.fail(TransferItem::ConnectionLost);
            continue;
        }
        match (t.build)(&mut c.session) {
            Ok((id, bytes)) => {
                t.sink.started(id);
                ctx.transfers.active.insert(
                    id,
                    ActiveTransfer {
                        kind: t.kind,
                        sink: t.sink,
                        play: t.play,
                        demux: Demuxer::new(),
                        abort: false,
                    },
                );
                c.send(&bytes).await?;
            }
            Err(e) => {
                debug!(%e, "transfer not built");
                t.sink.fail(TransferItem::NotAllowed);
            }
        }
    }
    Ok(())
}

/// The stop of a running transfer: 144 for a download, 7 for a play,
/// nothing for a cover image (it ends by itself).
fn stop_bytes(session: &mut Session, id: u32, t: &ActiveTransfer) -> Option<Vec<u8>> {
    let r = match (t.kind, &t.play) {
        (TransferKind::Download, _) => session.stop_download(id),
        (TransferKind::Play, Some(q)) => session.stop_play(id, q),
        _ => return None,
    };
    match r {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            debug!(%e, "transfer stop not built");
            None
        }
    }
}

/// Sends camera motion commands and their stops (see [`Ptz::poll`]).
async fn sync_ptz<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    ctx: &mut Ctx,
    c: &mut Conn<R, W>,
) -> Result<(), Failure> {
    for bytes in ctx.ptz.poll(&mut c.session) {
        c.send(&bytes).await?;
    }
    Ok(())
}

/// Opens, feeds, and closes talks (see [`Talks::poll`]).
async fn sync_talks<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    ctx: &mut Ctx,
    c: &mut Conn<R, W>,
) -> Result<(), Failure> {
    for bytes in ctx.talks.poll(&mut c.session) {
        c.send(&bytes).await?;
    }
    Ok(())
}

/// Hands a download's or cover's media to its sink.
fn transfer_media(t: &mut ActiveTransfer, data: &[u8], demux_log: &mut LogLimiter) {
    if t.abort {
        return;
    }
    t.demux.push(data);
    loop {
        match t.demux.next_record() {
            Ok(Some(r)) => {
                if !t.sink.push(TransferItem::Record(r)) {
                    t.abort = true;
                    return;
                }
            }
            Ok(None) => return,
            Err(e) => {
                if let Some(n) = demux_log.allow() {
                    warn!(suppressed = n, error = %e, "bad media record in a download");
                }
                // A gap in recorded media cannot be skipped like live
                // video: fail it so the caller fetches it again.
                t.sink.fail(TransferItem::BadMedia);
                t.abort = true;
                return;
            }
        }
    }
}

fn handle_event(ctx: &mut Ctx, ev: Event, demux_log: &mut LogLimiter) {
    match ev {
        Event::Media { message_id, data } if ctx.transfers.active.contains_key(&message_id) => {
            if let Some(t) = ctx.transfers.active.get_mut(&message_id) {
                transfer_media(t, &data, demux_log);
            }
        }
        Event::Control {
            command: 143 | 298,
            message_id,
            xml,
            ..
        } if ctx.transfers.active.contains_key(&message_id) => {
            if let Some(t) = ctx.transfers.active.get(&message_id)
                && t.kind == TransferKind::Download
                && let Ok(Some(n)) = archive::parse_download_estimate(&xml)
            {
                t.sink.push(TransferItem::Estimate(n));
            }
        }
        Event::TransferEnd {
            message_id,
            status,
            foreign,
            ..
        } => {
            if let Some(f) = foreign {
                warn!(
                    message_id,
                    bytes_16_19 = format_args!("{:#010x}", f.bytes_16_19),
                    expected_seq = ?f.expected_seq.map(|s| format!("{s:#010x}")),
                    body_len = f.body_len,
                    "a play ended on a message without its seq"
                );
            }
            if let Some(t) = ctx.transfers.active.remove(&message_id)
                && !t.abort
            {
                t.sink.push(TransferItem::End(status));
            }
        }
        Event::Media { message_id, data } => {
            let Some(&ctx_key) = ctx.subs.by_msg.get(&message_id) else {
                return;
            };
            let Some(st) = ctx.subs.streams.get_mut(&ctx_key) else {
                return;
            };
            st.saw_media = true;
            st.watch_from = Instant::now();
            if let Some(w) = st.waiting.take() {
                info!(
                    retries = w.retries,
                    "stream started after the device answered its preview with 400"
                );
            }
            st.demux.push(&data);
            loop {
                match st.demux.next_record() {
                    Ok(Some(r)) => {
                        if matches!(r, Record::Info { .. }) {
                            st.info = Some(r.clone());
                        } else {
                            // A picture (or sound) is flowing: whatever was
                            // wrong is over.
                            st.silent = 0;
                            st.since = None;
                            if st.rejected.take().is_some() {
                                st.slow = false;
                                st.slow_retry_at = None;
                                info!(
                                    channel = ctx_key.0,
                                    tier = ?ctx_key.1,
                                    "stream arrived after a refusal"
                                );
                            }
                        }
                        fan_out(&st.subs, r);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        if let Some(n) = demux_log.allow() {
                            warn!(suppressed = n, error = %e, "bad media record; waiting for next keyframe");
                        }
                        st.demux.resync();
                        for q in &st.subs {
                            q.gate();
                        }
                        break;
                    }
                }
            }
        }
        Event::Control {
            command: 3,
            message_id,
            status,
            ..
        } if status != 200 => {
            let Some(st) = ctx
                .subs
                .by_msg
                .get(&message_id)
                .and_then(|k| ctx.subs.streams.get_mut(k))
            else {
                return;
            };
            if status == 400 && st.saw_media {
                // Media already proves the stream is running.
                if let Some(n) = ctx.wait_log.allow() {
                    debug!(
                        status,
                        suppressed = n,
                        "preview answered 400 after its media; ignored"
                    );
                }
            } else if status == 400 && !ctx.cfg.preview_wait_limit.is_zero() {
                // Not final (docs/protocol.md): keep the preview open for
                // media on this message ID and restart it later.
                let now = Instant::now();
                let slow = st.slow;
                // A slow retry gets one short look, not a whole new wait.
                let retry_at = (!slow).then(|| now + ctx.cfg.preview_retry_interval);
                match &mut st.waiting {
                    Some(w) => w.retry_at = retry_at,
                    None => {
                        st.waiting = Some(Wait {
                            status,
                            until: now
                                + if slow {
                                    ctx.cfg.preview_retry_interval
                                } else {
                                    ctx.cfg.preview_wait_limit
                                },
                            retry_at,
                            retries: 0,
                        });
                        if !slow {
                            for q in &st.subs {
                                q.push_control(Item::WaitingForDevice { status });
                            }
                        }
                    }
                }
                if let Some(n) = ctx.wait_log.allow() {
                    info!(
                        status,
                        suppressed = n,
                        "preview answered 400; waiting for the device's stream"
                    );
                }
            } else {
                st.waiting = None;
                st.rejected = Some(status);
                st.slow = false;
                st.slow_retry_at = None;
                for q in &st.subs {
                    q.push_control(Item::StartRejected { status });
                }
            }
        }
        Event::Control {
            command: 33, xml, ..
        } => match detection::parse_alarm_event_list(&xml) {
            Ok(states) => {
                // No receiver is fine: nobody is listening.
                let _ = ctx.alarms_tx.send(Arc::new(AlarmPush {
                    connection: ctx.conn_gen,
                    states,
                }));
            }
            Err(e) => {
                if let Some(n) = demux_log.allow() {
                    warn!(suppressed = n, error = %e, "unreadable alarm state");
                }
            }
        },
        Event::Control {
            command: command @ (201 | 202 | 11),
            message_id,
            status,
            ..
        } => ctx.talks.on_reply(command, message_id, status),
        Event::Control {
            message_id, status, ..
        } if ctx.ptz.owns(message_id) => ctx.ptz.on_reply(message_id, status),
        Event::Control {
            command: 145, xml, ..
        } => match xml::parse_channel_info_list(&xml) {
            Ok(list) => {
                ctx.channel_uids_tx
                    .send_replace(crate::archive::parse_channel_uids(&xml));
                ctx.channels_tx.send_replace(Some(list));
            }
            Err(e) => {
                if let Some(n) = demux_log.allow() {
                    warn!(suppressed = n, error = %e, "unreadable channel list");
                }
            }
        },
        Event::Control {
            message_id,
            status,
            xml,
            ..
        } => {
            if let Some(tx) = ctx.queries.in_flight.remove(&message_id) {
                let _ = tx.send(Ok(Reply {
                    status,
                    xml,
                    message_id,
                    conn: ctx.conn_gen,
                }));
            }
        }
        _ => {}
    }
}

async fn connected_loop<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    ctx: &mut Ctx,
    c: &mut Conn<R, W>,
) -> Result<(), Failure> {
    // Liveness as the official client does it (docs/protocol.md): any bytes
    // received count; after `probe_after_idle` of silence send a connection
    // test (93), at most once per that interval; after `dead_after` of
    // silence the connection is dead.
    let probe_after = ctx.cfg.probe_after_idle;
    let dead_after = ctx.cfg.dead_after;
    let mut last_rx = Instant::now();
    let mut last_probe = last_rx;
    let mut demux_log = LogLimiter::new(Duration::from_secs(5));
    let mut session_log = LogLimiter::new(Duration::from_secs(5));
    loop {
        // Serve everything already buffered before blocking again.
        loop {
            match c.dec.next_message() {
                Ok(Some(m)) => match c.session.handle(&m) {
                    Ok(ev) => handle_event(ctx, ev, &mut demux_log),
                    Err(e) => {
                        if let Some(n) = session_log.allow() {
                            warn!(suppressed = n, error = %e, "message not handled");
                        }
                    }
                },
                Ok(None) => break,
                Err(e) => return Err(format!("framing error: {e}")),
            }
        }
        if ctx.shared.is_closing() {
            return Ok(());
        }
        ctx.apply_retries();
        ctx.maybe_diagnose();
        sync_previews(ctx, c).await?;
        sync_queries(ctx, c).await?;
        sync_transfers(ctx, c).await?;
        sync_talks(ctx, c).await?;
        sync_ptz(ctx, c).await?;
        let probe_due = last_rx.max(last_probe) + probe_after;
        let dead_due = last_rx + dead_after;
        let wait_due = [
            ctx.subs.due(&ctx.cfg),
            ctx.talks.next_due(),
            ctx.ptz.next_due(),
        ]
        .into_iter()
        .flatten()
        .min();
        let wait_due = Some(wait_due.map_or(ctx.diag_last + DIAGNOSTIC_INTERVAL, |w| {
            w.min(ctx.diag_last + DIAGNOSTIC_INTERVAL)
        }));
        tokio::select! {
            r = c.fill() => {
                r?;
                last_rx = Instant::now();
            }
            () = sleep_until(wait_due.map_or(probe_due, |w| w.min(probe_due)).min(dead_due)) => {
                let now = Instant::now();
                if now >= dead_due {
                    return Err("no data from device".to_owned());
                }
                if now >= probe_due {
                    let bytes = c.session.keepalive().map_err(|e| e.to_string())?;
                    c.send(&bytes).await?;
                    last_probe = now;
                }
            }
            _ = ctx.shared.wake.notified() => {}
            cmd = ctx.cmd_rx.recv() => match cmd {
                Some(r) => ctx.request(r),
                None => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream_in(subs: &mut Subs, channel: u8, tier: StreamTier) -> &mut Stream {
        let st = subs
            .streams
            .entry((channel, tier))
            .or_insert_with(Stream::new);
        st.subs.push(Arc::new(FrameQueue::new(4)));
        st
    }

    #[test]
    fn stuck_tiles_are_listed_with_their_reason_after_a_minute() {
        let mut subs = Subs::default();
        let now = Instant::now();
        // Not started yet.
        stream_in(&mut subs, 1, StreamTier::Main);
        // Started, silent.
        stream_in(&mut subs, 2, StreamTier::Sub).msg_id = Some(7);
        // Restarted for silence.
        let st = stream_in(&mut subs, 3, StreamTier::Sub);
        st.msg_id = Some(8);
        st.silent = 2;
        // Refused for good, and refused but retried slowly.
        stream_in(&mut subs, 4, StreamTier::Sub).rejected = Some(404);
        let st = stream_in(&mut subs, 5, StreamTier::Sub);
        st.rejected = Some(400);
        st.slow_retry_at = Some(now);
        // Waiting on a 400.
        let st = stream_in(&mut subs, 6, StreamTier::Sub);
        st.waiting = Some(Wait {
            status: 400,
            until: now,
            retry_at: None,
            retries: 0,
        });
        // Has a picture.
        stream_in(&mut subs, 7, StreamTier::Sub).since = None;
        assert!(subs.stuck(now + Duration::from_secs(30)).is_empty());
        let why: Vec<(u8, &str)> = subs
            .stuck(now + STUCK_AFTER + Duration::from_secs(1))
            .into_iter()
            .map(|((c, _), w)| (c, w))
            .collect();
        assert_eq!(
            why,
            [
                (1, "not started"),
                (2, "silent after start"),
                (3, "restarting (silent after start)"),
                (4, "rejected"),
                (5, "rejected (retrying slowly)"),
                (6, "waiting on 400"),
            ]
        );
    }

    #[test]
    fn manual_retries_of_one_stream_keep_the_floor() {
        let mut subs = Subs::default();
        let now = Instant::now();
        stream_in(&mut subs, 1, StreamTier::Main);
        assert!(subs.manual_retry((1, StreamTier::Main), now));
        assert!(!subs.manual_retry((1, StreamTier::Main), now + Duration::from_secs(1)));
        assert!(subs.manual_retry((1, StreamTier::Main), now + MANUAL_RETRY_FLOOR));
        // Unknown streams are ignored.
        assert!(!subs.manual_retry((9, StreamTier::Main), now));
    }

    #[test]
    fn silent_windows_double_up_to_the_cap() {
        let cfg = SessionConfig::new("example-user", "example-password");
        let mut st = Stream::new();
        st.msg_id = Some(1);
        let w: Vec<u64> = (0..6)
            .map(|n| {
                st.silent = n;
                st.silence_window(&cfg).unwrap().as_secs()
            })
            .collect();
        assert_eq!(w, [15, 30, 60, 60, 60, 60]);
    }
}
