//! Camera motion control on the device's one session: pan and tilt, preset
//! recall, and lens zoom and focus (docs/protocol.md, "Camera motion
//! control"; docs/ARCHITECTURE.md, "Device commands").
//!
//! The handle's calls ([`DeviceSession::ptz_move`] and its siblings) hand a
//! request to the supervisor, which owns the connection and every safety
//! rule, so they hold however callers behave:
//!
//! - **Who asks.** Every call names its [`MotionOrigin`]. The user's own
//!   controls ([`MotionOrigin::Manual`]) need no enable. Automation (a local
//!   API client, automatic correction) needs a grant for that camera
//!   ([`DeviceSession::set_motion_grant`], none by default), and is refused
//!   while the user steers the camera and for
//!   [`PtzConfig::manual_priority`] after the user's last command there.
//! - **What the device offers.** Nothing is sent to a channel whose
//!   capabilities (199, read on the same connection) lack it; a recall
//!   names a preset of the channel's list (190), and a lens position is
//!   clamped to the range the camera reported (294). The supervisor reads
//!   what is missing first.
//! - **Moves always end.** A move is one command; the camera presumably
//!   keeps moving until `stop`. The caller renews a held move
//!   ([`DeviceSession::ptz_move`] again with the same direction and speed;
//!   renewals are not sent and not counted) and the supervisor sends
//!   `stop` [`PtzConfig::deadman`] after the last renewal, after
//!   [`PtzConfig::max_hold`] in any case, when the device refuses the move,
//!   when a grant is withdrawn, before a different direction, when the
//!   session closes or the handle is dropped, and, for a move the
//!   connection lost before its stop, right after the next login.
//! - **Rate limits** per camera and per device ([`PtzConfig`]), sized
//!   against runaway software; `stop` is never limited. A refusal names
//!   its reason ([`PtzRefusal`]).
//! - **Logged.** Every motion command sent is logged with its channel,
//!   action and origin; lines beyond a small budget per second are counted
//!   and reported with the next line, so a runaway caller cannot flood the
//!   log.

use crate::config::{PtzConfig, RateLimit};
use crate::supervisor::{DeviceSession, QueryError};
use crate::talk::Outgoing;
use bc_protocol::ptz::{Cam, Direction, LensAxis, LensRange, Preset};
use bc_protocol::session::{Command, PtzCaps, Session, SessionError};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};
use tokio::time::{Instant, timeout};
use tracing::{info, warn};

/// Who sends a motion command (docs/ARCHITECTURE.md, "Device commands").
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MotionOrigin {
    /// The user, through the app's own controls: needs no enable.
    Manual,
    /// Automation: a local API client by its name, or automatic correction
    /// ([`MotionOrigin::AUTO_CORRECTION`]). Needs a grant for the camera.
    Automation(String),
}

impl MotionOrigin {
    /// The automation name of automatic correction (focus drift, the PTZ
    /// watchdog).
    pub const AUTO_CORRECTION: &'static str = "auto_correction";

    /// A short name for logs: `manual`, or the automation's name.
    pub fn name(&self) -> &str {
        match self {
            MotionOrigin::Manual => "manual",
            MotionOrigin::Automation(c) => c,
        }
    }

    fn is_manual(&self) -> bool {
        matches!(self, MotionOrigin::Manual)
    }
}

impl fmt::Display for MotionOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a motion call did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PtzOutcome {
    /// A move was sent and accepted.
    Moving,
    /// The same move was already running: its dead-man timer restarted;
    /// nothing was sent.
    Renewed,
    /// A stop was sent (or nothing was moving and one was sent anyway).
    Stopped,
    /// A preset recall was sent and accepted. The reply comes before the
    /// camera arrives.
    Recalled,
    /// A lens move was sent and accepted, to this (clamped) position.
    Lens(i32),
}

/// Why a motion call sent nothing, or failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PtzRefusal {
    /// Automation without a grant for this camera.
    NotGranted,
    /// Automation while the user steers this camera.
    UserSteering,
    /// Over a rate limit.
    RateLimited {
        /// When the next command would be allowed.
        retry_after: Duration,
    },
    /// The move was held for [`PtzConfig::max_hold`] and stopped; the same
    /// direction is refused until the control is released.
    MaxHold,
    /// The channel's capabilities (199) do not offer this.
    NotSupported,
    /// The channel's preset list (190) has no such preset.
    UnknownPreset,
    /// The device answered with this status.
    DeviceRefused(u16),
    /// The session is closed.
    SessionClosed,
    /// The connection dropped first.
    ConnectionLost,
    /// No answer in time.
    TimedOut,
    /// A read the command depends on (199, 294, 190) failed.
    QueryFailed,
}

impl PtzRefusal {
    /// A short name for logs and events.
    pub fn name(self) -> &'static str {
        match self {
            PtzRefusal::NotGranted => "not_granted",
            PtzRefusal::UserSteering => "user_steering",
            PtzRefusal::RateLimited { .. } => "rate_limited",
            PtzRefusal::MaxHold => "max_hold",
            PtzRefusal::NotSupported => "not_supported",
            PtzRefusal::UnknownPreset => "unknown_preset",
            PtzRefusal::DeviceRefused(_) => "device_refused",
            PtzRefusal::SessionClosed => "session_closed",
            PtzRefusal::ConnectionLost => "connection_lost",
            PtzRefusal::TimedOut => "timed_out",
            PtzRefusal::QueryFailed => "query_failed",
        }
    }

    /// Whether the core refused before sending (a limit or a rule), as
    /// opposed to the device or the connection failing.
    pub fn is_refusal(self) -> bool {
        matches!(
            self,
            PtzRefusal::NotGranted
                | PtzRefusal::UserSteering
                | PtzRefusal::RateLimited { .. }
                | PtzRefusal::MaxHold
                | PtzRefusal::NotSupported
                | PtzRefusal::UnknownPreset
        )
    }

    /// Whether the command certainly never reached the device: a core
    /// refusal, a failed read it depends on, or a closed session. The
    /// device refusing, a lost connection, and a timeout are not (the
    /// command was, or may have been, sent).
    pub fn sent_nothing(self) -> bool {
        self.is_refusal() || matches!(self, PtzRefusal::QueryFailed | PtzRefusal::SessionClosed)
    }
}

impl fmt::Display for PtzRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why a move was stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopCause {
    /// The caller stopped it ([`DeviceSession::ptz_stop`]).
    Released,
    /// The caller stopped renewing it ([`PtzConfig::deadman`]).
    Deadman,
    /// It was held for [`PtzConfig::max_hold`].
    MaxHold,
    /// A move in another direction replaced it.
    Replaced,
    /// The device refused the move; a stop went out anyway.
    DeviceRefused,
    /// The automation's grant was withdrawn.
    GrantWithdrawn,
    /// The session is closing (closed, or its handle dropped).
    SessionClosed,
    /// The connection dropped while it moved; the stop went out after the
    /// next login.
    Reconnected,
}

impl StopCause {
    /// A short name for logs and events.
    pub fn name(self) -> &'static str {
        match self {
            StopCause::Released => "released",
            StopCause::Deadman => "deadman",
            StopCause::MaxHold => "max_hold",
            StopCause::Replaced => "replaced",
            StopCause::DeviceRefused => "device_refused",
            StopCause::GrantWithdrawn => "grant_withdrawn",
            StopCause::SessionClosed => "session_closed",
            StopCause::Reconnected => "reconnected",
        }
    }
}

/// A stop the supervisor sent, for whoever shows the camera
/// ([`DeviceSession::ptz_notices`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PtzNotice {
    /// The channel.
    pub cam: Cam,
    /// Why.
    pub cause: StopCause,
    /// Who started the move that stopped (`None` for a stop sent after a
    /// reconnect or without a known move).
    pub origin: Option<MotionOrigin>,
}

/// What [`DeviceSession::ptz_info`] read about a channel's camera.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PtzInfo {
    /// Its capabilities (199).
    pub caps: PtzCaps,
    /// Its zoom motor (294), when it has lens zoom and focus.
    pub zoom: Option<LensRange>,
    /// Its focus motor (294).
    pub focus: Option<LensRange>,
    /// Its presets (190), when it has presets.
    pub presets: Vec<Preset>,
    /// The pan and tilt self-test state (340), when it pans and tilts.
    pub self_test: Option<u32>,
}

// ---------------------------------------------------------------------------
// Pure parts: rate limits, the move timers, the decisions. They take the
// time as an argument, so tests drive them with a manual clock.

/// A token bucket.
#[derive(Clone, Debug)]
pub(crate) struct Bucket {
    limit: RateLimit,
    tokens: f64,
    last: Option<Instant>,
}

impl Bucket {
    pub(crate) fn new(limit: RateLimit) -> Bucket {
        Bucket {
            limit,
            tokens: f64::from(limit.burst.max(1)),
            last: None,
        }
    }

    fn refill(&mut self, now: Instant) {
        if let Some(last) = self.last {
            let dt = now.saturating_duration_since(last).as_secs_f64();
            self.tokens = (self.tokens + dt * f64::from(self.limit.per_second))
                .min(f64::from(self.limit.burst.max(1)));
        }
        self.last = Some(now);
    }

    /// `Ok` when a token is available now (without taking it), else how
    /// long until one is.
    pub(crate) fn check(&mut self, now: Instant) -> Result<(), Duration> {
        self.refill(now);
        if self.tokens >= 1.0 {
            return Ok(());
        }
        let rate = f64::from(self.limit.per_second.max(1));
        Err(Duration::from_secs_f64((1.0 - self.tokens) / rate))
    }

    /// Takes a token (after a successful [`Bucket::check`]).
    pub(crate) fn take(&mut self) {
        self.tokens = (self.tokens - 1.0).max(0.0);
    }
}

/// Takes one token from both buckets, or neither.
fn take_both(a: &mut Bucket, b: &mut Bucket, now: Instant) -> Result<(), PtzRefusal> {
    match (a.check(now), b.check(now)) {
        (Ok(()), Ok(())) => {
            a.take();
            b.take();
            Ok(())
        }
        (x, y) => Err(PtzRefusal::RateLimited {
            retry_after: x.err().unwrap_or_default().max(y.err().unwrap_or_default()),
        }),
    }
}

/// A move in progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActiveMove {
    pub(crate) direction: Direction,
    pub(crate) speed: u8,
    pub(crate) origin: MotionOrigin,
    pub(crate) started: Instant,
    pub(crate) renewed: Instant,
}

struct ChannelMotion {
    moving: Option<ActiveMove>,
    /// After a max-hold stop: the direction refused until released, and
    /// the last attempt at it (a gap of a dead-man interval is a release).
    held_out: Option<(Direction, Instant)>,
    last_manual: Option<Instant>,
    moves: Bucket,
    presets: Bucket,
}

/// What to do with a move request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MoveDecision {
    /// Send it; `replaces` is the move it replaces (stop that first).
    Send { replaces: Option<ActiveMove> },
    /// The same move is running: renewed, nothing to send.
    Renew,
}

/// Which limit a non-move command counts against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Preset,
    Lens,
}

/// The rules for one device's motion commands: origins, limits, and move
/// timers.
pub(crate) struct Policy {
    cfg: PtzConfig,
    channels: HashMap<Cam, ChannelMotion>,
    device_moves: Bucket,
    device_presets: Bucket,
}

impl Policy {
    pub(crate) fn new(cfg: PtzConfig) -> Policy {
        Policy {
            cfg,
            channels: HashMap::new(),
            device_moves: Bucket::new(cfg.device_moves),
            device_presets: Bucket::new(cfg.device_presets),
        }
    }

    fn cam(&mut self, cam: Cam) -> &mut ChannelMotion {
        let cfg = self.cfg;
        self.channels.entry(cam).or_insert_with(|| ChannelMotion {
            moving: None,
            held_out: None,
            last_manual: None,
            moves: Bucket::new(cfg.camera_moves),
            presets: Bucket::new(cfg.camera_presets),
        })
    }

    /// The origin rules: automation needs a grant and yields to the user.
    fn check_origin(
        &mut self,
        cam: Cam,
        origin: &MotionOrigin,
        granted: bool,
        now: Instant,
    ) -> Result<(), PtzRefusal> {
        if origin.is_manual() {
            return Ok(());
        }
        if !granted {
            return Err(PtzRefusal::NotGranted);
        }
        let priority = self.cfg.manual_priority;
        let c = self.cam(cam);
        let steering = c.moving.as_ref().is_some_and(|m| m.origin.is_manual())
            || c.last_manual.is_some_and(|t| now < t + priority);
        if steering {
            return Err(PtzRefusal::UserSteering);
        }
        Ok(())
    }

    /// Decides a move (or its renewal) and records it.
    pub(crate) fn decide_move(
        &mut self,
        cam: impl Into<Cam>,
        direction: Direction,
        speed: u8,
        origin: &MotionOrigin,
        granted: bool,
        now: Instant,
    ) -> Result<MoveDecision, PtzRefusal> {
        let cam = cam.into();
        self.check_origin(cam, origin, granted, now)?;
        let deadman = self.cfg.deadman;
        self.cam(cam);
        let c = self.channels.get_mut(&cam).expect("created above");
        if let Some((d, last)) = c.held_out {
            if now >= last + deadman || d != direction {
                c.held_out = None;
            } else {
                c.held_out = Some((d, now));
                return Err(PtzRefusal::MaxHold);
            }
        }
        if let Some(m) = c.moving.as_mut()
            && m.direction == direction
            && m.speed == speed
        {
            m.renewed = now;
            if origin.is_manual() {
                c.last_manual = Some(now);
            }
            return Ok(MoveDecision::Renew);
        }
        take_both(&mut c.moves, &mut self.device_moves, now)?;
        let replaces = c.moving.replace(ActiveMove {
            direction,
            speed,
            origin: origin.clone(),
            started: now,
            renewed: now,
        });
        if origin.is_manual() {
            c.last_manual = Some(now);
        }
        Ok(MoveDecision::Send { replaces })
    }

    /// Decides a preset recall or lens move and records it.
    pub(crate) fn decide(
        &mut self,
        cam: impl Into<Cam>,
        kind: Kind,
        origin: &MotionOrigin,
        granted: bool,
        now: Instant,
    ) -> Result<(), PtzRefusal> {
        let cam = cam.into();
        self.check_origin(cam, origin, granted, now)?;
        self.cam(cam);
        let Some(c) = self.channels.get_mut(&cam) else {
            return Err(PtzRefusal::NotSupported);
        };
        let mine = match kind {
            Kind::Preset => &mut c.presets,
            Kind::Lens => &mut c.moves,
        };
        let device = match kind {
            Kind::Preset => &mut self.device_presets,
            Kind::Lens => &mut self.device_moves,
        };
        take_both(mine, device, now)?;
        if origin.is_manual() {
            c.last_manual = Some(now);
        }
        Ok(())
    }

    /// Records a stop: the move (if any) ends, and a release clears a
    /// max-hold refusal.
    pub(crate) fn stop(&mut self, cam: impl Into<Cam>, released: bool) -> Option<ActiveMove> {
        let cam = cam.into();
        let c = self.channels.get_mut(&cam)?;
        if released {
            c.held_out = None;
        }
        c.moving.take()
    }

    /// The moves whose dead-man or hold timer has run out by `now`, now
    /// ended (a max-hold one also refuses its direction until released).
    pub(crate) fn due(&mut self, now: Instant) -> Vec<(Cam, ActiveMove, StopCause)> {
        let (deadman, max_hold) = (self.cfg.deadman, self.cfg.max_hold);
        let mut out = Vec::new();
        for (ch, c) in &mut self.channels {
            let Some(m) = c.moving.as_ref() else {
                continue;
            };
            let cause = if now >= m.started + max_hold {
                StopCause::MaxHold
            } else if now >= m.renewed + deadman {
                StopCause::Deadman
            } else {
                continue;
            };
            let m = c.moving.take().expect("checked");
            if cause == StopCause::MaxHold {
                c.held_out = Some((m.direction, now));
            }
            out.push((*ch, m, cause));
        }
        out.sort_by_key(|(ch, ..)| *ch);
        out
    }

    /// When the next move timer runs out.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        self.channels
            .values()
            .filter_map(|c| c.moving.as_ref())
            .map(|m| (m.renewed + self.cfg.deadman).min(m.started + self.cfg.max_hold))
            .min()
    }

    /// Every move in progress.
    pub(crate) fn moving(&self) -> impl Iterator<Item = (Cam, &ActiveMove)> + '_ {
        self.channels
            .iter()
            .filter_map(|(ch, c)| c.moving.as_ref().map(|m| (*ch, m)))
    }

    /// Forgets every move (the connection is gone), returning them.
    pub(crate) fn take_all(&mut self) -> Vec<(Cam, ActiveMove)> {
        let mut v: Vec<_> = self
            .channels
            .iter_mut()
            .filter_map(|(ch, c)| c.moving.take().map(|m| (*ch, m)))
            .collect();
        v.sort_by_key(|(ch, _)| *ch);
        v
    }
}

/// A budget for log lines: bursts of `burst`, then `per_second`; lines
/// beyond it are counted and reported with the next one.
pub(crate) struct LineBudget {
    bucket: Bucket,
    suppressed: u64,
}

impl LineBudget {
    pub(crate) fn new(per_second: u32, burst: u32) -> LineBudget {
        LineBudget {
            bucket: Bucket::new(RateLimit { per_second, burst }),
            suppressed: 0,
        }
    }

    /// `Some(lines suppressed since the last one)` when a line may be
    /// logged now.
    pub(crate) fn allow(&mut self, now: Instant) -> Option<u64> {
        if self.bucket.check(now).is_ok() {
            self.bucket.take();
            Some(std::mem::take(&mut self.suppressed))
        } else {
            self.suppressed += 1;
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Shared between the handle and the supervisor.

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Automation grants, the requests waiting for the supervisor, and the
/// stop notices.
pub(crate) struct PtzControl {
    /// Per channel, the automation names granted motion control.
    grants: Mutex<HashMap<u8, HashSet<String>>>,
    /// Requests in the order they were made (the supervisor takes them).
    inbox: Mutex<VecDeque<PtzReq>>,
    notices: broadcast::Sender<PtzNotice>,
}

/// Requests other than stops waiting for the supervisor at most; more are
/// refused ([`PtzRefusal::RateLimited`]). Stops are always taken, up to
/// [`INBOX_HARD_MAX`].
const INBOX_MAX: usize = 256;
const INBOX_HARD_MAX: usize = 1024;

/// Stop notices kept for slow receivers.
const NOTICES_QUEUED: usize = 64;

impl PtzControl {
    pub(crate) fn new() -> PtzControl {
        PtzControl {
            grants: Mutex::new(HashMap::new()),
            inbox: Mutex::new(VecDeque::new()),
            notices: broadcast::channel(NOTICES_QUEUED).0,
        }
    }

    fn set_grant(&self, channel: u8, client: &str, granted: bool) -> bool {
        let mut g = lock(&self.grants);
        if granted {
            g.entry(channel).or_default().insert(client.to_owned())
        } else {
            let changed = g.get_mut(&channel).is_some_and(|s| s.remove(client));
            if g.get(&channel).is_some_and(HashSet::is_empty) {
                g.remove(&channel);
            }
            changed
        }
    }

    fn granted(&self, cam: Cam, origin: &MotionOrigin) -> bool {
        match origin {
            MotionOrigin::Manual => true,
            MotionOrigin::Automation(c) => lock(&self.grants)
                .get(&cam.channel)
                .is_some_and(|s| s.contains(c)),
        }
    }

    fn grants(&self, channel: u8) -> Vec<String> {
        let mut v: Vec<String> = lock(&self.grants)
            .get(&channel)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Queues a request; gives it back when the inbox is full.
    fn submit(&self, r: PtzReq) -> Result<(), PtzReq> {
        let mut q = lock(&self.inbox);
        let max = if r.op == Op::Stop {
            INBOX_HARD_MAX
        } else {
            INBOX_MAX
        };
        if q.len() >= max {
            return Err(r);
        }
        q.push_back(r);
        Ok(())
    }

    fn take_inbox(&self) -> VecDeque<PtzReq> {
        std::mem::take(&mut *lock(&self.inbox))
    }

    fn notify(&self, n: PtzNotice) {
        // No receiver is fine.
        let _ = self.notices.send(n);
    }
}

// ---------------------------------------------------------------------------
// The supervisor's side.

/// A camera motion command ([`DeviceSession::ptz_send`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PtzCommand {
    /// Start or renew a pan or tilt move (speed 1 to 64).
    Move {
        /// The direction.
        direction: Direction,
        /// The speed, limited to 1–64.
        speed: u8,
    },
    /// Stop at once.
    Stop,
    /// Recall a preset by its id.
    Preset {
        /// The preset's id (190).
        id: u32,
    },
    /// Move the lens zoom or focus to a position (clamped to the camera's
    /// range).
    Lens {
        /// Which motor.
        axis: LensAxis,
        /// The position, in the camera's steps.
        position: i32,
    },
}

type Op = PtzCommand;

impl PtzCommand {
    /// A short name for logs and events: `move`, `stop`, `preset`, `lens`.
    pub fn action(self) -> &'static str {
        match self {
            Op::Move { .. } => "move",
            Op::Stop => "stop",
            Op::Preset { .. } => "preset",
            Op::Lens { .. } => "lens",
        }
    }
}

type PtzReply = oneshot::Sender<Result<PtzOutcome, PtzRefusal>>;

pub(crate) struct PtzReq {
    cam: Cam,
    op: Op,
    origin: MotionOrigin,
    reply: Option<PtzReply>,
}

/// A device read the queued requests wait for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Fact {
    Caps,
    Lens(Cam),
    Presets(Cam),
}

struct InFlight {
    cam: Cam,
    op: Op,
    origin: MotionOrigin,
    reply: Option<PtzReply>,
    /// For a lens move: the position sent.
    position: i32,
}

/// The supervisor's camera motion state for one device.
pub(crate) struct Ptz {
    cfg: PtzConfig,
    policy: Policy,
    control: Arc<PtzControl>,
    queue: VecDeque<PtzReq>,
    in_flight: HashMap<u32, InFlight>,
    /// Reads in flight, by message ID.
    fetching: HashMap<u32, Fact>,
    /// Reads that failed on this connection.
    failed: HashSet<Fact>,
    /// Channels where a move was sent and no stop has been confirmed since.
    /// Only these are owed a stop after a lost connection.
    sent_moves: HashSet<Cam>,
    /// Channels to stop as soon as a connection is logged in.
    pending_stops: HashSet<Cam>,
    /// Lens re-reads (294) due, by channel.
    rereads: HashMap<Cam, Instant>,
    log: LineBudget,
}

/// Motion log lines allowed at once, then per second.
const LOG_BURST: u32 = 40;
const LOG_PER_SECOND: u32 = 10;

impl Ptz {
    pub(crate) fn new(cfg: PtzConfig, control: Arc<PtzControl>) -> Ptz {
        Ptz {
            cfg,
            policy: Policy::new(cfg),
            control,
            queue: VecDeque::new(),
            in_flight: HashMap::new(),
            fetching: HashMap::new(),
            failed: HashSet::new(),
            sent_moves: HashSet::new(),
            pending_stops: HashSet::new(),
            rereads: HashMap::new(),
            log: LineBudget::new(LOG_PER_SECOND, LOG_BURST),
        }
    }

    fn log_sent(&mut self, cam: Cam, action: &str, origin: &str, reason: &str, now: Instant) {
        if let Some(suppressed) = self.log.allow(now) {
            info!(
                channel = cam.channel,
                lens = cam.lens,
                action,
                origin,
                reason,
                suppressed,
                "camera motion command sent"
            );
        }
    }

    fn log_refused(
        &mut self,
        cam: Cam,
        action: &str,
        origin: &MotionOrigin,
        why: PtzRefusal,
        now: Instant,
    ) {
        if let Some(suppressed) = self.log.allow(now) {
            warn!(
                channel = cam.channel,
                lens = cam.lens,
                action,
                origin = origin.name(),
                reason = why.name(),
                suppressed,
                "camera motion command refused"
            );
        }
    }

    fn answer(&mut self, r: PtzReq, result: Result<PtzOutcome, PtzRefusal>, now: Instant) {
        if let Err(why) = result {
            self.log_refused(r.cam, r.op.action(), &r.origin, why, now);
        }
        if let Some(tx) = r.reply {
            let _ = tx.send(result);
        }
    }

    /// Builds a stop for `channel` and registers it.
    fn send_stop(
        &mut self,
        session: &mut Session,
        cam: Cam,
        origin: MotionOrigin,
        reply: Option<PtzReply>,
        reason: &str,
        out: &mut Outgoing,
    ) -> bool {
        let now = Instant::now();
        match session.ptz_stop(cam) {
            Ok((id, bytes)) => {
                self.log_sent(cam, "stop", origin.name(), reason, now);
                out.push(bytes);
                self.in_flight.insert(
                    id,
                    InFlight {
                        cam,
                        op: Op::Stop,
                        origin,
                        reply,
                        position: 0,
                    },
                );
                true
            }
            Err(e) => {
                warn!(channel = cam.channel, lens = cam.lens, error = %e, reason, "camera stop not sent");
                if let Some(tx) = reply {
                    let _ = tx.send(Err(session_refusal(&e)));
                }
                false
            }
        }
    }

    fn notify(&self, cam: Cam, cause: StopCause, origin: Option<MotionOrigin>) {
        self.control.notify(PtzNotice { cam, cause, origin });
    }

    /// The read a request still needs on this connection, if any.
    fn missing(&self, session: &Session, r: &PtzReq) -> Option<Fact> {
        if !session.ptz_caps_known() {
            return Some(Fact::Caps);
        }
        let caps = session.ptz_caps(r.cam.channel).unwrap_or_default();
        match r.op {
            Op::Preset { .. } if caps.presets && session.ptz_presets(r.cam).is_none() => {
                Some(Fact::Presets(r.cam))
            }
            Op::Lens { .. } if caps.zoom_focus && session.ptz_lens_ranges(r.cam).is_none() => {
                Some(Fact::Lens(r.cam))
            }
            _ => None,
        }
    }

    fn fetch(&mut self, session: &mut Session, fact: Fact, out: &mut Outgoing) -> bool {
        if self.fetching.values().any(|f| *f == fact) {
            return true;
        }
        let built = match fact {
            Fact::Caps => session.query_info(Command::Support, None),
            Fact::Lens(ch) => session.ptz_query(Command::PtzZoomFocus, ch),
            Fact::Presets(ch) => session.ptz_query(Command::PtzPresets, ch),
        };
        match built {
            Ok((id, bytes)) => {
                out.push(bytes);
                self.fetching.insert(id, fact);
                true
            }
            Err(_) => false,
        }
    }

    /// Sends what is due: stops owed from a lost connection, timer stops,
    /// stops for withdrawn grants, lens re-reads, and the queued requests
    /// whose reads are in.
    pub(crate) fn poll(&mut self, session: &mut Session) -> Outgoing {
        let now = Instant::now();
        let mut out = Vec::new();
        let inbox = self.control.take_inbox();
        self.queue.extend(inbox);
        if !session.is_logged_in() {
            return out;
        }
        let owed: Vec<Cam> = {
            let mut v: Vec<Cam> = self.pending_stops.drain().collect();
            v.sort_unstable();
            v
        };
        for ch in owed {
            if self.send_stop(
                session,
                ch,
                MotionOrigin::Manual,
                None,
                "after reconnect",
                &mut out,
            ) {
                self.notify(ch, StopCause::Reconnected, None);
            }
        }
        for (ch, m, cause) in self.policy.due(now) {
            self.send_stop(session, ch, m.origin.clone(), None, cause.name(), &mut out);
            self.notify(ch, cause, Some(m.origin));
        }
        let revoked: Vec<(Cam, MotionOrigin)> = self
            .policy
            .moving()
            .filter(|(ch, m)| !self.control.granted(*ch, &m.origin))
            .map(|(ch, m)| (ch, m.origin.clone()))
            .collect();
        for (ch, origin) in revoked {
            self.policy.stop(ch, false);
            self.send_stop(
                session,
                ch,
                origin.clone(),
                None,
                "grant withdrawn",
                &mut out,
            );
            self.notify(ch, StopCause::GrantWithdrawn, Some(origin));
        }
        let due: Vec<Cam> = self
            .rereads
            .iter()
            .filter(|(_, t)| **t <= now)
            .map(|(ch, _)| *ch)
            .collect();
        for ch in due {
            self.rereads.remove(&ch);
            if let Ok((id, bytes)) = session.ptz_query(Command::PtzZoomFocus, ch) {
                out.push(bytes);
                self.fetching.insert(id, Fact::Lens(ch));
            }
        }
        // Requests in order; one waiting for a read holds back the ones
        // behind it, so a channel's commands never overtake each other.
        while let Some(mut r) = self.queue.pop_front() {
            r.cam = session.ptz_resolve(r.cam);
            let abandoned = r.reply.as_ref().is_some_and(oneshot::Sender::is_closed);
            if abandoned && r.op != Op::Stop {
                continue;
            }
            if r.op != Op::Stop
                && let Some(fact) = self.missing(session, &r)
            {
                if self.failed.contains(&fact) {
                    self.answer(r, Err(PtzRefusal::QueryFailed), now);
                    continue;
                }
                if self.fetch(session, fact, &mut out) {
                    self.queue.push_front(r);
                    break;
                }
                self.answer(r, Err(PtzRefusal::QueryFailed), now);
                continue;
            }
            self.run(session, r, now, &mut out);
        }
        out
    }

    fn run(&mut self, session: &mut Session, r: PtzReq, now: Instant, out: &mut Outgoing) {
        let ch = r.cam;
        let caps = session.ptz_caps(ch.channel);
        match r.op {
            Op::Stop => {
                let m = self.policy.stop(ch, true);
                let origin = m.as_ref().map(|m| m.origin.clone());
                if self.send_stop(session, ch, r.origin.clone(), r.reply, "released", out)
                    && m.is_some()
                {
                    self.notify(ch, StopCause::Released, origin);
                }
            }
            Op::Move { direction, speed } => {
                if !caps.unwrap_or_default().pan_tilt {
                    return self.answer(r, Err(PtzRefusal::NotSupported), now);
                }
                let granted = self.control.granted(ch, &r.origin);
                match self
                    .policy
                    .decide_move(ch, direction, speed, &r.origin, granted, now)
                {
                    Err(why) => self.answer(r, Err(why), now),
                    Ok(MoveDecision::Renew) => self.answer(r, Ok(PtzOutcome::Renewed), now),
                    Ok(MoveDecision::Send { replaces }) => {
                        if let Some(old) = replaces {
                            self.send_stop(session, ch, old.origin.clone(), None, "replaced", out);
                            self.notify(ch, StopCause::Replaced, Some(old.origin));
                        }
                        match session.ptz_move(ch, direction, speed) {
                            Ok((id, bytes)) => {
                                self.sent_moves.insert(ch);
                                self.log_sent(ch, direction.name(), r.origin.name(), "", now);
                                out.push(bytes);
                                self.in_flight.insert(
                                    id,
                                    InFlight {
                                        cam: ch,
                                        op: r.op,
                                        origin: r.origin,
                                        reply: r.reply,
                                        position: 0,
                                    },
                                );
                            }
                            Err(e) => {
                                self.policy.stop(ch, false);
                                self.answer(r, Err(session_refusal(&e)), now);
                            }
                        }
                    }
                }
            }
            Op::Preset { id } => {
                if !caps.unwrap_or_default().presets {
                    return self.answer(r, Err(PtzRefusal::NotSupported), now);
                }
                if !session
                    .ptz_presets(ch)
                    .is_some_and(|l| l.iter().any(|p| p.id == id))
                {
                    return self.answer(r, Err(PtzRefusal::UnknownPreset), now);
                }
                let granted = self.control.granted(ch, &r.origin);
                if let Err(why) = self
                    .policy
                    .decide(ch, Kind::Preset, &r.origin, granted, now)
                {
                    return self.answer(r, Err(why), now);
                }
                // A recall moves the camera: a move in progress ends first.
                if let Some(old) = self.policy.stop(ch, false) {
                    self.send_stop(session, ch, old.origin.clone(), None, "replaced", out);
                    self.notify(ch, StopCause::Replaced, Some(old.origin));
                }
                match session.ptz_recall(ch, id) {
                    Ok((msg, bytes)) => {
                        self.log_sent(ch, "preset", r.origin.name(), "", now);
                        out.push(bytes);
                        self.in_flight.insert(
                            msg,
                            InFlight {
                                cam: ch,
                                op: r.op,
                                origin: r.origin,
                                reply: r.reply,
                                position: 0,
                            },
                        );
                    }
                    Err(e) => self.answer(r, Err(session_refusal(&e)), now),
                }
            }
            Op::Lens { axis, position } => {
                let has_axis = session
                    .ptz_lens_ranges(ch)
                    .is_some_and(|z| z.axis(axis).is_some());
                if !caps.unwrap_or_default().zoom_focus || !has_axis {
                    return self.answer(r, Err(PtzRefusal::NotSupported), now);
                }
                let granted = self.control.granted(ch, &r.origin);
                if let Err(why) = self.policy.decide(ch, Kind::Lens, &r.origin, granted, now) {
                    return self.answer(r, Err(why), now);
                }
                match session.ptz_lens(ch, axis, position) {
                    Ok((msg, bytes, pos)) => {
                        self.log_sent(ch, axis.name(), r.origin.name(), "", now);
                        out.push(bytes);
                        self.in_flight.insert(
                            msg,
                            InFlight {
                                cam: ch,
                                op: r.op,
                                origin: r.origin,
                                reply: r.reply,
                                position: pos,
                            },
                        );
                    }
                    Err(e) => self.answer(r, Err(session_refusal(&e)), now),
                }
            }
        }
    }

    /// Whether `message_id` is one of this module's requests.
    pub(crate) fn owns(&self, message_id: u32) -> bool {
        self.in_flight.contains_key(&message_id) || self.fetching.contains_key(&message_id)
    }

    /// The device answered one of this module's requests.
    pub(crate) fn on_reply(&mut self, message_id: u32, status: u16) {
        let now = Instant::now();
        if let Some(fact) = self.fetching.remove(&message_id) {
            // 199 answers 300 with a complete body (observed, protocol.md).
            let ok = status == 200 || (fact == Fact::Caps && status == 300);
            if !ok {
                warn!(?fact, status, "camera motion read refused");
                self.failed.insert(fact);
            }
            return;
        }
        let Some(f) = self.in_flight.remove(&message_id) else {
            return;
        };
        let ok = status == 200;
        let result = match (f.op, ok) {
            (Op::Stop, true) => {
                self.sent_moves.remove(&f.cam);
                Ok(PtzOutcome::Stopped)
            }
            (Op::Move { .. }, true) => Ok(PtzOutcome::Moving),
            (Op::Preset { .. }, true) => {
                self.rereads.insert(f.cam, now + self.cfg.lens_reread);
                Ok(PtzOutcome::Recalled)
            }
            (
                Op::Lens {
                    axis: LensAxis::Zoom,
                    ..
                },
                true,
            ) => {
                self.rereads.insert(f.cam, now + self.cfg.lens_reread);
                Ok(PtzOutcome::Lens(f.position))
            }
            (Op::Lens { .. }, true) => Ok(PtzOutcome::Lens(f.position)),
            (_, false) => Err(PtzRefusal::DeviceRefused(status)),
        };
        if !ok {
            warn!(
                channel = f.cam.channel,
                lens = f.cam.lens,
                action = f.op.action(),
                origin = f.origin.name(),
                status,
                "camera motion command refused by the device"
            );
            if matches!(f.op, Op::Move { .. }) {
                // Stop anyway, at once: the camera may have started.
                self.policy.stop(f.cam, false);
                self.pending_stops.insert(f.cam);
                self.notify(f.cam, StopCause::DeviceRefused, Some(f.origin.clone()));
            }
        }
        if let Some(tx) = f.reply {
            let _ = tx.send(result);
        }
    }

    /// When something is next due.
    pub(crate) fn next_due(&self) -> Option<Instant> {
        let reread = self.rereads.values().min().copied();
        let timers = self.policy.next_due();
        let owed = (!self.pending_stops.is_empty()).then(Instant::now);
        [reread, timers, owed].into_iter().flatten().min()
    }

    /// The connection is gone: moves and unanswered stops are owed a stop
    /// after the next login; requests waiting for an answer fail.
    pub(crate) fn connection_lost(&mut self) {
        for (ch, _) in self.policy.take_all() {
            self.pending_stops.insert(ch);
        }
        for (_, f) in self.in_flight.drain() {
            let owed = match f.op {
                Op::Move { .. } => true,
                Op::Stop => self.sent_moves.contains(&f.cam),
                _ => false,
            };
            if owed {
                self.pending_stops.insert(f.cam);
            }
            if let Some(tx) = f.reply {
                let _ = tx.send(Err(PtzRefusal::ConnectionLost));
            }
        }
        self.fetching.clear();
        self.failed.clear();
        self.rereads.clear();
        // Stops stay queued for the next connection; the rest fail.
        let queued = std::mem::take(&mut self.queue);
        for r in queued {
            if r.op == Op::Stop {
                self.queue.push_back(r);
            } else if let Some(tx) = r.reply {
                let _ = tx.send(Err(PtzRefusal::ConnectionLost));
            }
        }
        if !self.pending_stops.is_empty() {
            warn!(
                channels = self.pending_stops.len(),
                "connection lost with cameras moving; stopping them after the next login"
            );
        }
    }

    /// The session is closing gracefully: stops for every move, every
    /// owed stop, and every queued stop.
    pub(crate) fn close_all(&mut self, session: &mut Session) -> Outgoing {
        let mut out = Vec::new();
        let inbox = self.control.take_inbox();
        self.queue.extend(inbox);
        let mut stops: Vec<(Cam, Option<MotionOrigin>)> = self
            .policy
            .take_all()
            .into_iter()
            .map(|(ch, m)| (ch, Some(m.origin)))
            .collect();
        stops.extend(self.pending_stops.drain().map(|ch| (ch, None)));
        let queued = std::mem::take(&mut self.queue);
        for r in queued {
            if r.op == Op::Stop {
                stops.push((r.cam, None));
            }
            if let Some(tx) = r.reply {
                let _ = tx.send(Err(PtzRefusal::SessionClosed));
            }
        }
        stops.sort_by_key(|(ch, _)| *ch);
        stops.dedup_by_key(|(ch, _)| *ch);
        for (ch, origin) in stops {
            if self.send_stop(
                session,
                ch,
                origin.clone().unwrap_or(MotionOrigin::Manual),
                None,
                "session closing",
                &mut out,
            ) {
                self.notify(ch, StopCause::SessionClosed, origin);
            }
        }
        out
    }

    /// The session has ended: everything still waiting fails.
    pub(crate) fn finish(&mut self) {
        let inbox = self.control.take_inbox();
        self.queue.extend(inbox);
        for r in self.queue.drain(..) {
            if let Some(tx) = r.reply {
                let _ = tx.send(Err(PtzRefusal::SessionClosed));
            }
        }
        for (_, f) in self.in_flight.drain() {
            if let Some(tx) = f.reply {
                let _ = tx.send(Err(PtzRefusal::SessionClosed));
            }
        }
        let left: Vec<Cam> = self
            .policy
            .take_all()
            .into_iter()
            .map(|(ch, _)| ch)
            .chain(self.pending_stops.drain())
            .collect();
        if !left.is_empty() {
            warn!(
                channels = left.len(),
                "session ended without a connection to stop moving cameras"
            );
        }
    }
}

fn session_refusal(e: &SessionError) -> PtzRefusal {
    match e {
        SessionError::MotionNotSupported(_) => PtzRefusal::NotSupported,
        SessionError::UnknownPreset(_) => PtzRefusal::UnknownPreset,
        SessionError::MotionStateUnknown(_) => PtzRefusal::QueryFailed,
        SessionError::WrongState(_) => PtzRefusal::ConnectionLost,
        _ => PtzRefusal::QueryFailed,
    }
}

fn query_refusal(e: QueryError) -> PtzRefusal {
    match e {
        QueryError::Closed => PtzRefusal::SessionClosed,
        QueryError::ConnectionLost => PtzRefusal::ConnectionLost,
        QueryError::TimedOut => PtzRefusal::TimedOut,
        QueryError::Busy | QueryError::NotAllowed => PtzRefusal::QueryFailed,
    }
}

/// A motion command handed to the session ([`DeviceSession::ptz_send`]):
/// await [`PtzPending::outcome`] for what happened. Dropping it does not
/// cancel a stop (a stop always goes out); a move, recall or lens move
/// whose pending result was dropped before the supervisor took it is not
/// sent.
#[derive(Debug)]
pub struct PtzPending {
    rx: Option<oneshot::Receiver<Result<PtzOutcome, PtzRefusal>>>,
    refused: Option<PtzRefusal>,
    wait: Duration,
}

impl PtzPending {
    /// What the command did: the device's answer, or why nothing was
    /// sent. Gives up after the session's query timeout
    /// ([`PtzRefusal::TimedOut`]; a stop still goes out).
    pub async fn outcome(self) -> Result<PtzOutcome, PtzRefusal> {
        if let Some(r) = self.refused {
            return Err(r);
        }
        let Some(rx) = self.rx else {
            return Err(PtzRefusal::SessionClosed);
        };
        match timeout(self.wait, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(PtzRefusal::SessionClosed),
            Err(_) => Err(PtzRefusal::TimedOut),
        }
    }
}

impl DeviceSession {
    /// Hands a motion command to the session without waiting: commands
    /// reach the device in the order of these calls (a release is never
    /// overtaken by its move). See [`DeviceSession::ptz_move`] and its
    /// siblings for what each does.
    pub fn ptz_send(
        &self,
        cam: impl Into<Cam>,
        command: PtzCommand,
        origin: MotionOrigin,
    ) -> PtzPending {
        let cam = cam.into();
        let wait = self.query_timeout;
        let refused = |r| PtzPending {
            rx: None,
            refused: Some(r),
            wait,
        };
        if self.shared.is_closing() {
            return refused(PtzRefusal::SessionClosed);
        }
        let command = match command {
            Op::Move { direction, speed } => Op::Move {
                direction,
                speed: speed.clamp(bc_protocol::ptz::MIN_SPEED, bc_protocol::ptz::MAX_SPEED),
            },
            c => c,
        };
        let (tx, rx) = oneshot::channel();
        let req = PtzReq {
            cam,
            op: command,
            origin,
            reply: Some(tx),
        };
        if let Err(r) = self.ptz_control.submit(req) {
            warn!(
                channel = cam.channel,
                lens = cam.lens,
                action = r.op.action(),
                "camera motion inbox full"
            );
            return refused(PtzRefusal::RateLimited {
                retry_after: Duration::from_millis(100),
            });
        }
        self.shared.wake.notify_one();
        PtzPending {
            rx: Some(rx),
            refused: None,
            wait,
        }
    }

    async fn ptz_request(
        &self,
        cam: impl Into<Cam>,
        op: Op,
        origin: MotionOrigin,
    ) -> Result<PtzOutcome, PtzRefusal> {
        self.ptz_send(cam, op, origin).outcome().await
    }

    /// Starts or renews a pan or tilt move on `channel` at `speed` (1 to
    /// 64). Calling it again with the same direction and speed renews the
    /// move (nothing is sent); the caller renews at least every 500 ms
    /// while the control is held, and the session sends `stop`
    /// [`PtzConfig::deadman`] after the last renewal, after
    /// [`PtzConfig::max_hold`], and on every other exit path (see the
    /// module documentation). Another direction stops this one first.
    ///
    /// # Errors
    /// A [`PtzRefusal`]: the origin rules, the rate limits, the channel's
    /// capabilities, or the device.
    pub async fn ptz_move(
        &self,
        cam: impl Into<Cam>,
        direction: Direction,
        speed: u8,
        origin: MotionOrigin,
    ) -> Result<PtzOutcome, PtzRefusal> {
        self.ptz_request(cam, Op::Move { direction, speed }, origin)
            .await
    }

    /// Stops `channel`'s camera at once (18 `stop`). Allowed for any
    /// origin, never rate-limited; if the connection is down, the stop goes
    /// out after the next login.
    ///
    /// # Errors
    /// [`PtzRefusal::NotSupported`] for a channel that cannot pan or tilt,
    /// or the session or device failing.
    pub async fn ptz_stop(
        &self,
        cam: impl Into<Cam>,
        origin: MotionOrigin,
    ) -> Result<PtzOutcome, PtzRefusal> {
        self.ptz_request(cam, Op::Stop, origin).await
    }

    /// Queues a stop for `channel` without waiting for it (for drop and
    /// close paths that cannot wait). It goes out as soon as the
    /// supervisor runs, or after the next login.
    pub fn ptz_stop_now(&self, cam: impl Into<Cam>, origin: MotionOrigin) {
        drop(self.ptz_send(cam, Op::Stop, origin));
    }

    /// Recalls preset `id` (19 `toPos`) on `channel`. A move in progress
    /// there is stopped first. The reply comes before the camera arrives.
    ///
    /// # Errors
    /// A [`PtzRefusal`], including [`PtzRefusal::UnknownPreset`] for an id
    /// the channel's list lacks.
    pub async fn ptz_recall(
        &self,
        cam: impl Into<Cam>,
        id: u32,
        origin: MotionOrigin,
    ) -> Result<PtzOutcome, PtzRefusal> {
        self.ptz_request(cam, Op::Preset { id }, origin).await
    }

    /// Moves `channel`'s lens zoom or focus to `position`, clamped to the
    /// range the camera reported (294); the outcome holds the position
    /// sent.
    ///
    /// # Errors
    /// A [`PtzRefusal`].
    pub async fn ptz_lens(
        &self,
        cam: impl Into<Cam>,
        axis: LensAxis,
        position: i32,
        origin: MotionOrigin,
    ) -> Result<PtzOutcome, PtzRefusal> {
        self.ptz_request(cam, Op::Lens { axis, position }, origin)
            .await
    }

    /// Reads what `channel`'s camera can do and where its lens is: 199,
    /// then 294 (lens zoom and focus), 190 (presets) and 340 (pan and tilt)
    /// as the channel offers them. Viewing tier only.
    ///
    /// # Errors
    /// [`PtzRefusal::SessionClosed`], [`PtzRefusal::ConnectionLost`],
    /// [`PtzRefusal::TimedOut`], [`PtzRefusal::QueryFailed`].
    pub async fn ptz_info(&self, cam: impl Into<Cam>) -> Result<PtzInfo, PtzRefusal> {
        let cam = cam.into();
        let support = self
            .query(Command::Support, None)
            .await
            .map_err(query_refusal)?;
        let items = bc_protocol::detection::parse_channel_ai(&support.xml)
            .map_err(|_| PtzRefusal::QueryFailed)?;
        let caps = items
            .iter()
            .find(|i| i.channel == cam.channel)
            .map(PtzCaps::of)
            .unwrap_or_default();
        let mut info = PtzInfo {
            caps,
            ..PtzInfo::default()
        };
        let read = |cmd: Command| self.call(Box::new(move |s| s.ptz_query(cmd, cam)), None);
        if caps.zoom_focus {
            let r = read(Command::PtzZoomFocus).await.map_err(query_refusal)?;
            if r.status == 200 {
                let zf = bc_protocol::ptz::parse_zoom_focus(&r.xml)
                    .map_err(|_| PtzRefusal::QueryFailed)?;
                info.zoom = zf.zoom;
                info.focus = zf.focus;
            }
        }
        if caps.presets {
            let r = read(Command::PtzPresets).await.map_err(query_refusal)?;
            if r.status == 200 {
                info.presets =
                    bc_protocol::ptz::parse_presets(&r.xml).map_err(|_| PtzRefusal::QueryFailed)?;
            }
        }
        if caps.pan_tilt {
            let r = read(Command::PtzSelfTest).await.map_err(query_refusal)?;
            if r.status == 200 {
                info.self_test = bc_protocol::ptz::parse_self_test(&r.xml).ok();
            }
        }
        Ok(info)
    }

    /// Grants or withdraws motion control on `channel` for an automation
    /// (a local API client's name, or [`MotionOrigin::AUTO_CORRECTION`]).
    /// None is granted until granted; withdrawing stops that automation's
    /// move at once ([`StopCause::GrantWithdrawn`]). The user's own
    /// controls ([`MotionOrigin::Manual`]) need no grant.
    pub fn set_motion_grant(&self, channel: u8, client: &str, granted: bool) {
        if self.ptz_control.set_grant(channel, client, granted) {
            info!(channel, client, granted, "camera motion grant changed");
        }
        self.shared.wake.notify_one();
    }

    /// The automations granted motion control on `channel`, sorted.
    pub fn motion_grants(&self, channel: u8) -> Vec<String> {
        self.ptz_control.grants(channel)
    }

    /// Every stop the session sends from now on, with its cause: the
    /// timers, releases, replacements, withdrawn grants, refusals, closes,
    /// and stops after a reconnect. A receiver that falls behind skips to
    /// newer ones.
    pub fn ptz_notices(&self) -> broadcast::Receiver<PtzNotice> {
        self.ptz_control.notices.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PtzConfig {
        PtzConfig::default()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn bucket_allows_bursts_then_the_rate() {
        let t0 = Instant::now();
        let mut b = Bucket::new(RateLimit {
            per_second: 20,
            burst: 40,
        });
        for _ in 0..40 {
            assert!(b.check(t0).is_ok());
            b.take();
        }
        let wait = b.check(t0).unwrap_err();
        assert_eq!(wait, ms(50));
        // 50 ms later one more token.
        assert!(b.check(t0 + ms(50)).is_ok());
        b.take();
        assert!(b.check(t0 + ms(50)).is_err());
        // A quiet spell refills to the burst, never beyond.
        let mut n = 0;
        let t = t0 + Duration::from_secs(60);
        while b.check(t).is_ok() {
            b.take();
            n += 1;
        }
        assert_eq!(n, 40);
    }

    #[test]
    fn human_pace_never_reaches_the_limits() {
        // A person pressing a direction ten times a second, renewing it
        // every 100 ms in between, for a minute.
        let t0 = Instant::now();
        let mut p = Policy::new(cfg());
        let dirs = [Direction::Left, Direction::Right];
        for i in 0..600u64 {
            let now = t0 + ms(i * 100);
            let d = dirs[(i % 2) as usize];
            p.decide_move(1, d, 20, &MotionOrigin::Manual, true, now)
                .unwrap();
        }
    }

    #[test]
    fn a_runaway_loop_is_limited_per_camera_and_per_device() {
        let t0 = Instant::now();
        let mut p = Policy::new(cfg());
        let dirs = [Direction::Left, Direction::Right];
        let mut sent = 0;
        let mut refused = None;
        for i in 0..100 {
            match p.decide_move(1, dirs[i % 2], 20, &MotionOrigin::Manual, true, t0) {
                Ok(_) => sent += 1,
                Err(e) => {
                    refused = Some(e);
                    break;
                }
            }
        }
        assert_eq!(sent, 40);
        assert!(matches!(refused, Some(PtzRefusal::RateLimited { .. })));
        // Lens moves share the camera's move budget.
        assert!(matches!(
            p.decide(1, Kind::Lens, &MotionOrigin::Manual, true, t0),
            Err(PtzRefusal::RateLimited { .. })
        ));
        // Presets have their own, smaller one.
        for _ in 0..5 {
            p.decide(1, Kind::Preset, &MotionOrigin::Manual, true, t0)
                .unwrap();
        }
        assert!(matches!(
            p.decide(1, Kind::Preset, &MotionOrigin::Manual, true, t0),
            Err(PtzRefusal::RateLimited { .. })
        ));
        // The device budget (120) runs out across cameras: channels 2 and
        // 3 get 40 each, then channel 4 finds the device empty.
        for ch in [2u8, 3] {
            for i in 0..40 {
                p.decide_move(ch, dirs[i % 2], 20, &MotionOrigin::Manual, true, t0)
                    .unwrap();
            }
        }
        assert!(matches!(
            p.decide_move(4, Direction::Up, 20, &MotionOrigin::Manual, true, t0),
            Err(PtzRefusal::RateLimited { .. })
        ));
    }

    #[test]
    fn renewals_are_free_and_the_deadman_stops_a_move_left_alone() {
        let t0 = Instant::now();
        let mut p = Policy::new(cfg());
        let m = MotionOrigin::Manual;
        assert_eq!(
            p.decide_move(1, Direction::Left, 20, &m, true, t0),
            Ok(MoveDecision::Send { replaces: None })
        );
        assert_eq!(p.next_due(), Some(t0 + ms(1000)));
        // Renewed every 500 ms for 10 s: never due, never counted.
        for i in 1..=20 {
            let now = t0 + ms(i * 500);
            assert!(p.due(now).is_empty());
            assert_eq!(
                p.decide_move(1, Direction::Left, 20, &m, true, now),
                Ok(MoveDecision::Renew)
            );
        }
        let last = t0 + ms(10_000);
        assert!(p.due(last + ms(999)).is_empty());
        let due = p.due(last + ms(1000));
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].2, StopCause::Deadman);
        assert_eq!(p.moving().count(), 0);
        assert_eq!(p.next_due(), None);
        // A speed change is a new move; a different direction replaces.
        let t1 = last + ms(2000);
        p.decide_move(1, Direction::Up, 10, &m, true, t1).unwrap();
        assert!(matches!(
            p.decide_move(1, Direction::Up, 30, &m, true, t1),
            Ok(MoveDecision::Send { replaces: Some(_) })
        ));
        assert!(p.stop(1, true).is_some());
        assert!(p.stop(1, true).is_none());
    }

    #[test]
    fn max_hold_stops_and_refuses_the_same_direction_until_released() {
        let t0 = Instant::now();
        let mut p = Policy::new(cfg());
        let m = MotionOrigin::Manual;
        p.decide_move(2, Direction::Right, 20, &m, true, t0)
            .unwrap();
        let mut now = t0;
        while now < t0 + Duration::from_secs(30) {
            assert!(p.due(now).is_empty());
            p.decide_move(2, Direction::Right, 20, &m, true, now)
                .unwrap();
            now += ms(400);
        }
        assert_eq!(p.next_due(), Some(t0 + Duration::from_secs(30)));
        let due = p.due(t0 + Duration::from_secs(30));
        assert_eq!(due[0].2, StopCause::MaxHold);
        // Still held: refused, however long it is renewed.
        let mut now = t0 + Duration::from_secs(30);
        for _ in 0..10 {
            now += ms(400);
            assert_eq!(
                p.decide_move(2, Direction::Right, 20, &m, true, now),
                Err(PtzRefusal::MaxHold)
            );
        }
        // Released (a gap longer than the dead-man interval): allowed.
        assert!(
            p.decide_move(2, Direction::Right, 20, &m, true, now + ms(1000))
                .is_ok()
        );
        // An explicit release clears it at once.
        let t2 = now + Duration::from_secs(40);
        p.due(t2);
        p.decide_move(2, Direction::Right, 20, &m, true, t2)
            .unwrap_err();
        p.stop(2, true);
        assert!(p.decide_move(2, Direction::Right, 20, &m, true, t2).is_ok());
    }

    #[test]
    fn automation_needs_a_grant_and_yields_to_the_user() {
        let t0 = Instant::now();
        let mut p = Policy::new(cfg());
        let auto = MotionOrigin::Automation("api-client".to_owned());
        assert_eq!(
            p.decide_move(1, Direction::Left, 20, &auto, false, t0),
            Err(PtzRefusal::NotGranted)
        );
        assert_eq!(
            p.decide(1, Kind::Preset, &auto, false, t0),
            Err(PtzRefusal::NotGranted)
        );
        assert!(p.decide(1, Kind::Preset, &auto, true, t0).is_ok());
        // The user takes over: automation waits until the user has let go
        // for the priority interval.
        p.decide_move(1, Direction::Up, 20, &MotionOrigin::Manual, true, t0)
            .unwrap();
        assert_eq!(
            p.decide(1, Kind::Lens, &auto, true, t0 + ms(100)),
            Err(PtzRefusal::UserSteering)
        );
        p.stop(1, true);
        assert_eq!(
            p.decide_move(1, Direction::Left, 20, &auto, true, t0 + ms(4999)),
            Err(PtzRefusal::UserSteering)
        );
        assert!(
            p.decide_move(1, Direction::Left, 20, &auto, true, t0 + ms(5000))
                .is_ok()
        );
        // Another camera is not affected.
        assert!(p.decide(2, Kind::Lens, &auto, true, t0).is_ok());
    }

    #[test]
    fn log_budget_counts_what_it_suppresses() {
        let t0 = Instant::now();
        let mut b = LineBudget::new(10, 3);
        assert_eq!(b.allow(t0), Some(0));
        assert_eq!(b.allow(t0), Some(0));
        assert_eq!(b.allow(t0), Some(0));
        assert_eq!(b.allow(t0), None);
        assert_eq!(b.allow(t0), None);
        assert_eq!(b.allow(t0 + ms(100)), Some(2));
    }
}
