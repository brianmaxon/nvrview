//! The supervisor task: the only code that connects, talks, and reconnects.

use crate::Shared;
use crate::config::{Jitter, SessionConfig};
use crate::queue::{FrameQueue, Item, Subscription};
use crate::transport::Connector;
use bc_protocol::framer::Decoder;
use bc_protocol::media::Demuxer;
use bc_protocol::session::{Event, Session};
use bc_protocol::stream::StreamTier;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until, timeout};
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

struct NewSub {
    id: u64,
    channel: u8,
    tier: StreamTier,
    queue: Arc<FrameQueue>,
}

/// Handle to a running device session. Dropping it closes the session.
pub struct DeviceSession {
    shared: Arc<Shared>,
    cmd_tx: mpsc::Sender<NewSub>,
    state_rx: watch::Receiver<ConnState>,
    next_sub: AtomicU64,
    queue_frames: usize,
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
        let queue_frames = config.queue_frames;
        let ctx = Ctx {
            cfg: config,
            shared: shared.clone(),
            cmd_rx,
            state_tx,
            subs: Subs::default(),
            limiter: LogLimiter::new(Duration::from_secs(5)),
        };
        let join = tokio::spawn(supervise(connector, ctx));
        DeviceSession {
            shared,
            cmd_tx,
            state_rx,
            next_sub: AtomicU64::new(0),
            queue_frames,
            join: Some(join),
        }
    }

    /// Subscribes to a channel's stream tier. The preview starts as soon as
    /// the session is logged in and restarts after every reconnect. Drop the
    /// [`Subscription`] to stop it.
    pub async fn subscribe(
        &self,
        channel: u8,
        tier: StreamTier,
    ) -> Result<Subscription, SessionClosed> {
        if self.shared.is_closing() {
            return Err(SessionClosed);
        }
        let queue = Arc::new(FrameQueue::new(self.queue_frames));
        let id = self.next_sub.fetch_add(1, Ordering::Relaxed);
        self.cmd_tx
            .send(NewSub {
                id,
                channel,
                tier,
                queue: queue.clone(),
            })
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

struct Sub {
    channel: u8,
    tier: StreamTier,
    queue: Arc<FrameQueue>,
    msg_id: Option<u32>,
    demux: Demuxer,
}

#[derive(Default)]
struct Subs {
    map: HashMap<u64, Sub>,
    by_msg: HashMap<u32, u64>,
}

impl Subs {
    fn add(&mut self, n: NewSub) {
        self.map.insert(
            n.id,
            Sub {
                channel: n.channel,
                tier: n.tier,
                queue: n.queue,
                msg_id: None,
                demux: Demuxer::new(),
            },
        );
    }

    /// The connection is gone: previews must be restarted on the next one.
    fn connection_lost(&mut self) {
        self.by_msg.clear();
        for s in self.map.values_mut() {
            s.msg_id = None;
            s.demux = Demuxer::new();
            s.queue.discontinuity();
        }
    }

    /// Removes subscriptions their owners dropped (without device traffic).
    fn reap_offline(&mut self) {
        self.map
            .retain(|_, s| !s.queue.cancelled.load(Ordering::SeqCst));
    }

    fn close_all(&mut self) {
        for s in self.map.values() {
            s.queue.close();
        }
        self.map.clear();
        self.by_msg.clear();
    }
}

struct Ctx {
    cfg: SessionConfig,
    shared: Arc<Shared>,
    cmd_rx: mpsc::Receiver<NewSub>,
    state_tx: watch::Sender<ConnState>,
    subs: Subs,
    limiter: LogLimiter,
}

enum Exit {
    /// Close requested; previews were stopped and the socket shut down.
    Closed,
    Rejected {
        status: u16,
        remain_times: Option<u32>,
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
            } => {
                warn!(status, ?remain_times, "login refused; not retrying");
                ctx.state_tx.send_replace(ConnState::LoginFailed {
                    status,
                    remain_times,
                });
                ctx.subs.close_all();
                return;
            }
            Exit::Failed {
                reached_login,
                reason,
            } => {
                ctx.subs.connection_lost();
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
    ctx.subs.close_all();
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
                Some(n) => ctx.subs.add(n),
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
            } => {
                return Ok(Login::Rejected {
                    status,
                    remain_times,
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
        }) => {
            return Exit::Rejected {
                status,
                remain_times,
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
    let _ = timeout(c.write_timeout, c.wr.shutdown()).await;
    ctx.subs.connection_lost();
}

/// Starts previews for new subscriptions and stops those their owners
/// dropped.
async fn sync_previews<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    ctx: &mut Ctx,
    c: &mut Conn<R, W>,
) -> Result<(), Failure> {
    let ids: Vec<u64> = ctx.subs.map.keys().copied().collect();
    for id in ids {
        let Some(sub) = ctx.subs.map.get_mut(&id) else {
            continue;
        };
        if sub.queue.cancelled.load(Ordering::SeqCst) {
            if let Some(mid) = sub.msg_id {
                ctx.subs.by_msg.remove(&mid);
                let bytes = c.session.stop_preview(mid).map_err(|e| e.to_string())?;
                c.send(&bytes).await?;
            }
            ctx.subs.map.remove(&id);
        } else if sub.msg_id.is_none() {
            let (mid, bytes) = c
                .session
                .start_preview(sub.channel, sub.tier)
                .map_err(|e| e.to_string())?;
            sub.msg_id = Some(mid);
            ctx.subs.by_msg.insert(mid, id);
            c.send(&bytes).await?;
        }
    }
    Ok(())
}

fn handle_event(ctx: &mut Ctx, ev: Event, demux_log: &mut LogLimiter) {
    match ev {
        Event::Media { message_id, data } => {
            let Some(sub) = ctx
                .subs
                .by_msg
                .get(&message_id)
                .and_then(|id| ctx.subs.map.get_mut(id))
            else {
                return;
            };
            sub.demux.push(&data);
            loop {
                match sub.demux.next_record() {
                    Ok(Some(r)) => sub.queue.push_media(r),
                    Ok(None) => break,
                    Err(e) => {
                        if let Some(n) = demux_log.allow() {
                            warn!(channel = sub.channel, suppressed = n, error = %e, "bad media record; waiting for next keyframe");
                        }
                        sub.demux.resync();
                        sub.queue.gate();
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
            if let Some(sub) = ctx
                .subs
                .by_msg
                .get(&message_id)
                .and_then(|id| ctx.subs.map.get(id))
            {
                sub.queue.push_control(Item::StartRejected { status });
            }
        }
        _ => {}
    }
}

async fn connected_loop<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    ctx: &mut Ctx,
    c: &mut Conn<R, W>,
) -> Result<(), Failure> {
    let period = ctx.cfg.keepalive_interval;
    let mut keepalive = interval_at(Instant::now() + period, period);
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_rx = Instant::now();
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
        tokio::select! {
            r = c.fill() => {
                r?;
                last_rx = Instant::now();
            }
            _ = keepalive.tick() => {
                // Replies and media both count as proof of life; two silent
                // intervals mean the peer is gone.
                if last_rx.elapsed() > period * 2 {
                    return Err("no data from device".to_owned());
                }
                let bytes = c.session.keepalive().map_err(|e| e.to_string())?;
                c.send(&bytes).await?;
            }
            _ = ctx.shared.wake.notified() => {}
            cmd = ctx.cmd_rx.recv() => match cmd {
                Some(n) => ctx.subs.add(n),
                None => return Ok(()),
            },
        }
    }
}
