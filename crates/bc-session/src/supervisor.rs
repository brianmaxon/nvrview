//! The supervisor task: the only code that connects, talks, and reconnects.

use crate::Shared;
use crate::archive::{TransferItem, TransferKind, TransferSink};
use crate::config::{Jitter, SessionConfig};
use crate::queue::{FrameQueue, Item, Subscription};
use crate::transport::Connector;
use bc_protocol::archive;
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
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until, timeout};
use tracing::{debug, info, warn};

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
}

pub(crate) enum Request {
    Subscribe(NewSub),
    Query(QueryReq),
    Transfer(TransferReq),
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
    pub(crate) query_timeout: Duration,
    queue_frames: usize,
    /// Searches and cover requests in progress at once.
    pub(crate) search_permits: Arc<Semaphore>,
    /// Downloads in progress at once.
    pub(crate) download_permits: Arc<Semaphore>,
    pub(crate) download_stall_timeout: Duration,
    pub(crate) download_buffer_bytes: usize,
    join: Option<JoinHandle<()>>,
}

impl DeviceSession {
    /// Starts the supervisor on the current tokio runtime. Connecting begins
    /// immediately in the background.
    pub fn spawn<C: Connector>(connector: C, config: SessionConfig) -> DeviceSession {
        let shared = Arc::new(Shared {
            closing: std::sync::atomic::AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
        });
        let (cmd_tx, cmd_rx) = mpsc::channel(16);
        let (state_tx, state_rx) = watch::channel(ConnState::Connecting);
        let (channels_tx, channels_rx) = watch::channel(None);
        let (channel_uids_tx, channel_uids_rx) = watch::channel(HashMap::new());
        let queue_frames = config.queue_frames;
        let query_timeout = config.query_timeout;
        let search_permits = Arc::new(Semaphore::new(config.max_searches.max(1)));
        let download_permits = Arc::new(Semaphore::new(config.max_downloads.max(1)));
        let download_stall_timeout = config.download_stall_timeout;
        let download_buffer_bytes = config.download_buffer_bytes;
        let ctx = Ctx {
            cfg: config,
            shared: shared.clone(),
            cmd_rx,
            state_tx,
            channels_tx,
            channel_uids_tx,
            subs: Subs::default(),
            queries: Queries::default(),
            transfers: Transfers::default(),
            conn_gen: 0,
            limiter: LogLimiter::new(Duration::from_secs(5)),
        };
        let join = tokio::spawn(supervise(connector, ctx));
        DeviceSession {
            shared,
            cmd_tx,
            state_rx,
            channels_rx,
            channel_uids_rx,
            query_timeout,
            queue_frames,
            search_permits,
            download_permits,
            download_stall_timeout,
            download_buffer_bytes,
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
        Ok(Subscription {
            channel,
            tier,
            queue,
            shared: self.shared.clone(),
        })
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
            .or_insert_with(|| Stream {
                msg_id: None,
                demux: Demuxer::new(),
                subs: Vec::new(),
                info: None,
                rejected: None,
            });
        // A late subscriber starts gated (the queue's initial state): it
        // sees video from the next keyframe on, preceded by the stream info.
        if let Some(info) = &st.info {
            n.queue.push_media(info.clone());
        }
        if let Some(status) = st.rejected {
            n.queue.push_control(Item::StartRejected { status });
        }
        st.subs.push(n.queue);
    }

    /// The connection is gone: previews must be restarted on the next one.
    fn connection_lost(&mut self) {
        self.by_msg.clear();
        for st in self.streams.values_mut() {
            st.msg_id = None;
            st.demux = Demuxer::new();
            st.info = None;
            st.rejected = None;
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
    subs: Subs,
    queries: Queries,
    transfers: Transfers,
    /// Incremented on every successful login.
    conn_gen: u64,
    limiter: LogLimiter,
}

impl Ctx {
    fn request(&mut self, r: Request) {
        match r {
            Request::Subscribe(n) => self.subs.add(n),
            Request::Query(q) => self.queries.add(q),
            Request::Transfer(t) => self.transfers.add(t),
        }
    }

    /// The connection is gone (failed or closed).
    fn connection_lost(&mut self) {
        self.subs.connection_lost();
        self.queries.connection_lost();
        self.transfers.connection_lost();
    }

    /// Ends the session for good: refuses new subscriptions, and closes every
    /// queue (including requests still waiting in the command channel) so
    /// subscribers see the end instead of waiting forever.
    fn finish(&mut self) {
        self.cmd_rx.close();
        while let Ok(r) = self.cmd_rx.try_recv() {
            self.request(r);
        }
        self.subs.close_all();
        self.queries.close_all();
        self.transfers.close_all();
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
    // Stop running downloads first (144), so the device stops sending.
    let downloads: Vec<u32> = c.session.active_downloads().collect();
    for id in downloads {
        if let Ok(bytes) = c.session.stop_download(id)
            && c.send(&bytes).await.is_err()
        {
            break;
        }
        if let Some(t) = ctx.transfers.active.remove(&id) {
            t.sink.mark_stopped();
            t.sink.fail(TransferItem::Closed);
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

/// Starts one preview per stream that has subscribers, and stops the stream
/// when its last subscriber was dropped.
async fn sync_previews<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    ctx: &mut Ctx,
    c: &mut Conn<R, W>,
) -> Result<(), Failure> {
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
        } else if st.msg_id.is_none() {
            let (mid, bytes) = c
                .session
                .start_preview(key.0, key.1)
                .map_err(|e| e.to_string())?;
            st.msg_id = Some(mid);
            ctx.subs.by_msg.insert(mid, key);
            c.send(&bytes).await?;
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
        if t.kind == TransferKind::Download {
            match c.session.stop_download(id) {
                Ok(bytes) => {
                    c.send(&bytes).await?;
                    debug!(id, "download stopped");
                }
                Err(e) => debug!(%e, "stop_download failed"),
            }
        }
        t.sink.mark_stopped();
        t.sink.fail(TransferItem::Cancelled);
    }
    while let Some(t) = ctx.transfers.waiting.pop_front() {
        if t.sink.is_cancelled() {
            t.sink.fail(TransferItem::Cancelled);
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
            message_id, status, ..
        } => {
            if let Some(t) = ctx.transfers.active.remove(&message_id)
                && !t.abort
            {
                t.sink.push(TransferItem::End(status));
            }
        }
        Event::Media { message_id, data } => {
            let Some(st) = ctx
                .subs
                .by_msg
                .get(&message_id)
                .and_then(|k| ctx.subs.streams.get_mut(k))
            else {
                return;
            };
            st.demux.push(&data);
            loop {
                match st.demux.next_record() {
                    Ok(Some(r)) => {
                        if matches!(r, Record::Info { .. }) {
                            st.info = Some(r.clone());
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
            if let Some(st) = ctx
                .subs
                .by_msg
                .get(&message_id)
                .and_then(|k| ctx.subs.streams.get_mut(k))
            {
                st.rejected = Some(status);
                for q in &st.subs {
                    q.push_control(Item::StartRejected { status });
                }
            }
        }
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
        sync_previews(ctx, c).await?;
        sync_queries(ctx, c).await?;
        sync_transfers(ctx, c).await?;
        let probe_due = last_rx.max(last_probe) + probe_after;
        let dead_due = last_rx + dead_after;
        tokio::select! {
            r = c.fill() => {
                r?;
                last_rx = Instant::now();
            }
            () = sleep_until(probe_due.min(dead_due)) => {
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
