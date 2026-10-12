//! `bc-cli ptz-probe`: answers four questions the official client's capture
//! cannot (docs/protocol.md, "Camera motion control"), through the core's
//! own allowlisted camera motion path ([`DeviceSession::ptz_move`] and its
//! siblings). No raw command is built here.
//!
//! - **(a) Does a move continue without a stop?** One move at the lowest
//!   speed, never renewed, so the core's dead-man timer ends it
//!   ([`session_config`] sets it to the hold time). The camera's readable
//!   state (zoom and focus positions, the self-test state) is sampled
//!   before, during and after, with timestamps; the core offers no pan and
//!   tilt position read (433 is not on the allowlist), so the watcher
//!   judges the motion by eye and the output marks the moment of the move
//!   and of the stop.
//! - **(b) What happens to a recall of a preset the camera does not list?**
//!   The core checks the list (190) first and never sends it; the probe
//!   reports the refusal and that nothing was sent.
//! - **(c) What happens at the edge of the zoom and focus range (294)?**
//!   The motor is sent to the range's edge and, after it settles (bounded),
//!   back to where it was.
//! - **(d) Do reads for a second lens differ with and without a lens
//!   field?** The core's builders carry no lens field, so this is reported
//!   as not possible and `--lens` is refused before anything is sent.
//!
//! Safety: nothing moves before the typed confirmation `yes`; the held
//! move lasts at most [`MOTION_CAP`] (the dead-man timer and the maximum
//! hold are both set from it, and the probe stops the camera itself if the
//! core's stop is late); a guard sends a stop when the probe returns by any
//! path, including an error or a panic, and Ctrl-C ends the run through the
//! same path; the session close stops a move as well (core guarantee).

use bc_protocol::ptz::{Direction, LensAxis, MIN_SPEED};
use bc_session::{
    DeviceSession, MotionOrigin, PtzInfo, PtzNotice, PtzOutcome, PtzRefusal, SessionConfig,
    StopCause,
};
use std::fmt::Write as _;
use std::future::Future;
use std::io::Write;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::broadcast;
use tokio::time::{Instant, sleep, timeout};

/// The longest a continuous move may last.
pub const MOTION_CAP: Duration = Duration::from_secs(3);
/// The default hold: how long the core lets the unrenewed move run.
pub const DEFAULT_HOLD: Duration = Duration::from_millis(1500);
/// How long a lens move is watched for arriving.
pub const LENS_SETTLE: Duration = Duration::from_secs(3);
/// The interval between samples.
const SAMPLE_EVERY: Duration = Duration::from_millis(500);

/// One part of the probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// (a) the held move.
    HeldMove,
    /// (b) the recall of an unlisted preset.
    UnknownPreset,
    /// (c) the lens edges.
    LensEdge,
    /// (d) lens-field reads.
    LensField,
}

impl Step {
    /// Every step, in order.
    pub const ALL: [Step; 4] = [
        Step::HeldMove,
        Step::UnknownPreset,
        Step::LensEdge,
        Step::LensField,
    ];

    fn letter(self) -> char {
        match self {
            Step::HeldMove => 'a',
            Step::UnknownPreset => 'b',
            Step::LensEdge => 'c',
            Step::LensField => 'd',
        }
    }
}

/// Parses `--steps a,b,c,d` (any subset, in order, no repeats).
pub fn parse_steps(s: &str) -> Option<Vec<Step>> {
    let mut v = Vec::new();
    for p in s.split(',') {
        let step = Step::ALL
            .into_iter()
            .find(|x| p.trim().len() == 1 && p.trim().starts_with(x.letter()))?;
        if v.contains(&step) {
            return None;
        }
        v.push(step);
    }
    Some(v)
}

/// Which edge of the zoom and focus range step (c) goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    /// `minPos`.
    Min,
    /// `maxPos`.
    Max,
}

/// What to run.
#[derive(Clone, Debug)]
pub struct ProbeConfig {
    /// The channel (the plain `channelId`).
    pub channel: u8,
    /// A lens number: refused, the core has no lens field.
    pub lens: Option<u8>,
    /// The direction of the held move.
    pub direction: Direction,
    /// How long the unrenewed move runs before the core's stop (at most
    /// [`MOTION_CAP`]).
    pub hold: Duration,
    /// The edge step (c) goes to.
    pub edge: Edge,
    /// The steps to run.
    pub steps: Vec<Step>,
}

impl ProbeConfig {
    /// The standard run for `channel`.
    pub fn new(channel: u8) -> ProbeConfig {
        ProbeConfig {
            channel,
            lens: None,
            direction: Direction::Left,
            hold: DEFAULT_HOLD,
            edge: Edge::Max,
            steps: Step::ALL.to_vec(),
        }
    }
}

/// Parses `--hold SECONDS`: positive and at most [`MOTION_CAP`].
pub fn parse_hold(s: &str) -> Option<Duration> {
    let secs: f64 = s.parse().ok()?;
    let d = Duration::try_from_secs_f64(secs).ok()?;
    (d > Duration::ZERO && d <= MOTION_CAP).then_some(d)
}

/// The session settings the probe needs: the dead-man timer ends the held
/// move after `hold`, and the maximum hold is the cap.
pub fn session_config(user: &str, password: &str, hold: Duration) -> SessionConfig {
    let mut cfg = SessionConfig::new(user, password);
    cfg.ptz.deadman = hold.min(MOTION_CAP);
    cfg.ptz.max_hold = MOTION_CAP;
    cfg
}

/// What happened to the held move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    /// The device's answer to the move.
    pub moved: Result<PtzOutcome, PtzRefusal>,
    /// Why and when the core stopped it (`None`: no stop notice in time).
    pub stop: Option<(StopCause, Duration)>,
    /// The probe had to stop the camera itself.
    pub forced_stop: bool,
}

/// What happened to one lens motor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LensResult {
    /// The motor.
    pub axis: LensAxis,
    /// Where it was.
    pub original: i32,
    /// The edge it was sent to.
    pub target: i32,
    /// The answer to the move to the edge.
    pub moved: Result<PtzOutcome, PtzRefusal>,
    /// The position last read while it settled.
    pub reached: Option<i32>,
    /// The position last read after the move back.
    pub restored: Option<i32>,
}

/// What the probe found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The typed confirmation was `yes`.
    pub confirmed: bool,
    /// Step (a).
    pub held: Option<Held>,
    /// Step (b): the answer (a refusal means nothing was sent).
    pub unknown_preset: Option<Result<PtzOutcome, PtzRefusal>>,
    /// Step (c), one per motor.
    pub lens: Vec<LensResult>,
}

/// Sends a stop when dropped, whichever way the probe ends.
struct StopOnDrop<'a> {
    session: &'a DeviceSession,
    channel: u8,
}

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.session
            .ptz_stop_now(self.channel, MotionOrigin::Manual);
    }
}

struct Log<'a, W: Write> {
    out: &'a mut W,
    t0: Instant,
}

impl<W: Write> Log<'_, W> {
    fn line(&mut self, text: &str) {
        let t = self.t0.elapsed().as_secs_f64();
        let _ = writeln!(self.out, "+{t:7.3}s {text}");
        let _ = self.out.flush();
    }

    fn plain(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
        let _ = self.out.flush();
    }
}

fn lens_text(r: Option<bc_protocol::ptz::LensRange>) -> String {
    r.map_or("-".to_owned(), |r| {
        format!("{} [{}..{}]", r.cur, r.min, r.max)
    })
}

fn sample_text(i: &PtzInfo) -> String {
    format!(
        "zoom {}, focus {}, self-test {}",
        lens_text(i.zoom),
        lens_text(i.focus),
        i.self_test.map_or("-".to_owned(), |t| t.to_string())
    )
}

fn refusal_text<T: std::fmt::Debug>(r: &Result<T, PtzRefusal>) -> String {
    match r {
        Ok(o) => format!("accepted: {o:?}"),
        Err(PtzRefusal::DeviceRefused(s)) => format!("the device refused it, status {s}"),
        Err(e) => format!("refused: {e}"),
    }
}

/// Runs the probe. `input` supplies the typed confirmation; `interrupt`
/// completing (Ctrl-C) ends the run through the stop path.
///
/// # Errors
/// A message: `--lens` given, a read failing, nothing offered, an
/// interrupt. Whatever the error, the camera is stopped.
pub async fn run<R, W, I>(
    session: &DeviceSession,
    cfg: &ProbeConfig,
    mut input: R,
    out: &mut W,
    interrupt: I,
) -> Result<Report, String>
where
    R: AsyncBufRead + Unpin,
    W: Write,
    I: Future<Output = ()>,
{
    if cfg.lens.is_some() {
        return Err(
            "--lens is not supported: the core's builders carry no lens field (subChannel), \
             and sending the plain channel could move another lens"
                .into(),
        );
    }
    if cfg.hold.is_zero() || cfg.hold > MOTION_CAP {
        return Err("the hold must be above 0 and at most 3 s".into());
    }
    let mut log = Log {
        out,
        t0: Instant::now(),
    };
    let info = session
        .ptz_info(cfg.channel)
        .await
        .map_err(|e| format!("cannot read the channel's capabilities: {e}"))?;
    if !info.caps.any() {
        return Err(format!("channel {} offers no motion control", cfg.channel));
    }
    log.plain(&plan_text(cfg, &info));
    log.plain("Type yes to start (anything else cancels; only reads so far): ");
    let mut answer = String::new();
    let read = input.read_line(&mut answer).await;
    let mut report = Report::default();
    if read.is_err() || answer.trim() != "yes" {
        log.line("not confirmed: no motion command was sent");
        return Ok(report);
    }
    report.confirmed = true;
    let _guard = StopOnDrop {
        session,
        channel: cfg.channel,
    };
    log.t0 = Instant::now();
    log.line("confirmed");

    let mut rx = session.ptz_notices();
    let work = steps(session, cfg, &info, &mut rx, &mut log, &mut report);
    let finished = tokio::select! {
        r = work => Some(r),
        () = interrupt => None,
    };
    match finished {
        Some(r) => r?,
        None => {
            log.line("interrupted: stopping the camera");
            let _ = session.ptz_stop(cfg.channel, MotionOrigin::Manual).await;
            return Err("interrupted".into());
        }
    }
    log.line("done");
    Ok(report)
}

fn plan_text(cfg: &ProbeConfig, info: &PtzInfo) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "ptz-probe plan for channel {} (nothing moves until you type yes):",
        cfg.channel
    );
    let _ = writeln!(
        s,
        "  offers: pan/tilt {}, presets {}, zoom/focus {}",
        info.caps.pan_tilt, info.caps.presets, info.caps.zoom_focus
    );
    let _ = writeln!(s, "  now: {}", sample_text(info));
    for step in &cfg.steps {
        match step {
            Step::HeldMove => {
                let _ = writeln!(
                    s,
                    "  (a) one {} move at speed {MIN_SPEED}, not renewed; the core stops it after {:.1} s (cap {:.0} s){}",
                    cfg.direction,
                    cfg.hold.as_secs_f64(),
                    MOTION_CAP.as_secs_f64(),
                    if info.caps.pan_tilt {
                        ""
                    } else {
                        " [skipped: no pan/tilt]"
                    }
                );
            }
            Step::UnknownPreset => {
                let _ = writeln!(
                    s,
                    "  (b) ask for a preset id the camera does not list; the core should refuse it unsent{}",
                    if info.caps.presets {
                        ""
                    } else {
                        " [skipped: no presets]"
                    }
                );
            }
            Step::LensEdge => {
                let _ = writeln!(
                    s,
                    "  (c) send zoom and focus to their {} edge ({} / {}), watch up to {:.0} s each, then back to where they were{}",
                    match cfg.edge {
                        Edge::Min => "minimum",
                        Edge::Max => "maximum",
                    },
                    lens_text(info.zoom),
                    lens_text(info.focus),
                    LENS_SETTLE.as_secs_f64(),
                    if info.caps.zoom_focus {
                        ""
                    } else {
                        " [skipped: no zoom/focus]"
                    }
                );
            }
            Step::LensField => {
                let _ = writeln!(
                    s,
                    "  (d) lens-field reads: reported only (the core has no lens field)"
                );
            }
        }
    }
    let _ = write!(
        s,
        "  a stop is sent on every exit path, Ctrl-C included; the position to restore by hand, if needed: {}",
        sample_text(info)
    );
    s
}

async fn steps<W: Write>(
    session: &DeviceSession,
    cfg: &ProbeConfig,
    info: &PtzInfo,
    rx: &mut broadcast::Receiver<PtzNotice>,
    log: &mut Log<'_, W>,
    report: &mut Report,
) -> Result<(), String> {
    for step in &cfg.steps {
        match step {
            Step::HeldMove if info.caps.pan_tilt => {
                report.held = Some(held_move(session, cfg, rx, log).await);
            }
            Step::HeldMove => log.line("(a) skipped: the channel does not pan or tilt"),
            Step::UnknownPreset if info.caps.presets => {
                let id = info
                    .presets
                    .iter()
                    .map(|p| p.id)
                    .max()
                    .map_or(0, |m| m + 1000);
                let r = session
                    .ptz_recall(cfg.channel, id, MotionOrigin::Manual)
                    .await;
                let reached = match &r {
                    Ok(_) | Err(PtzRefusal::DeviceRefused(_)) => {
                        "the core sent it (the device's answer is above)"
                    }
                    Err(e) if e.sent_nothing() => {
                        "the core never sent it (nothing to report from the device)"
                    }
                    Err(_) => "the core may have sent it (no answer arrived)",
                };
                log.line(&format!(
                    "(b) recall of unlisted preset {id}: {}; {reached}",
                    refusal_text(&r),
                ));
                report.unknown_preset = Some(r);
            }
            Step::UnknownPreset => log.line("(b) skipped: the channel has no presets"),
            Step::LensEdge if info.caps.zoom_focus => {
                for axis in [LensAxis::Zoom, LensAxis::Focus] {
                    let range = match axis {
                        LensAxis::Zoom => info.zoom,
                        LensAxis::Focus => info.focus,
                    };
                    let Some(range) = range else {
                        log.line(&format!("(c) {}: no range reported", axis.name()));
                        continue;
                    };
                    report
                        .lens
                        .push(lens_edge(session, cfg, axis, range, log).await);
                }
            }
            Step::LensEdge => log.line("(c) skipped: the channel has no zoom/focus"),
            Step::LensField => log.line(
                "(d) not possible: the core's builders carry no lens field (subChannel), so \
                 reads for a second lens, with and without it, cannot be made through them",
            ),
        }
    }
    Ok(())
}

async fn held_move<W: Write>(
    session: &DeviceSession,
    cfg: &ProbeConfig,
    rx: &mut broadcast::Receiver<PtzNotice>,
    log: &mut Log<'_, W>,
) -> Held {
    // Drop notices from before this move.
    while rx.try_recv().is_ok() {}
    log.line(&format!(
        "(a) before: {}",
        sample_or_err(session, cfg.channel).await
    ));
    let t0 = Instant::now();
    let moved = session
        .ptz_move(cfg.channel, cfg.direction, MIN_SPEED, MotionOrigin::Manual)
        .await;
    log.line(&format!(
        "(a) move {} at speed {MIN_SPEED}: {} (not renewed)",
        cfg.direction,
        refusal_text(&moved)
    ));
    if moved.is_err() {
        // The core stops the camera itself when the device refused it.
        return Held {
            moved,
            stop: None,
            forced_stop: false,
        };
    }
    sleep(cfg.hold / 2).await;
    log.line(&format!(
        "(a) during: {}",
        sample_or_err(session, cfg.channel).await
    ));
    let wait = MOTION_CAP.saturating_sub(t0.elapsed());
    let channel = cfg.channel;
    let stop = timeout(wait, async {
        loop {
            match rx.recv().await {
                Ok(n) if n.channel == channel => return Some(n.cause),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    })
    .await
    .ok()
    .flatten();
    let stopped_at = t0.elapsed();
    let mut forced_stop = false;
    match stop {
        Some(cause) => log.line(&format!(
            "(a) the core stopped the move: {} after {:.3} s",
            cause.name(),
            stopped_at.as_secs_f64()
        )),
        None => {
            forced_stop = true;
            log.line("(a) NO stop notice within the cap: stopping the camera now");
            let r = session.ptz_stop(cfg.channel, MotionOrigin::Manual).await;
            log.line(&format!("(a) forced stop: {}", refusal_text(&r)));
        }
    }
    sleep(SAMPLE_EVERY).await;
    log.line(&format!(
        "(a) after: {}",
        sample_or_err(session, cfg.channel).await
    ));
    log.line(
        "(a) pan and tilt cannot be read through the core: say whether the camera kept \
         moving until the stop line above, and whether it moved after it",
    );
    Held {
        moved,
        stop: stop.map(|c| (c, stopped_at)),
        forced_stop,
    }
}

async fn sample_or_err(session: &DeviceSession, channel: u8) -> String {
    match session.ptz_info(channel).await {
        Ok(i) => sample_text(&i),
        Err(e) => format!("read failed: {e}"),
    }
}

async fn lens_position(session: &DeviceSession, channel: u8, axis: LensAxis) -> Option<i32> {
    let i = session.ptz_info(channel).await.ok()?;
    match axis {
        LensAxis::Zoom => i.zoom,
        LensAxis::Focus => i.focus,
    }
    .map(|r| r.cur)
}

/// Reads the motor every [`SAMPLE_EVERY`] until it is at `target` or
/// [`LENS_SETTLE`] is up; logs each reading.
async fn settle<W: Write>(
    session: &DeviceSession,
    channel: u8,
    axis: LensAxis,
    target: i32,
    label: &str,
    log: &mut Log<'_, W>,
) -> Option<i32> {
    let t0 = Instant::now();
    let mut last = None;
    loop {
        sleep(SAMPLE_EVERY).await;
        last = lens_position(session, channel, axis).await.or(last);
        log.line(&format!(
            "(c) {} {label}: {} at {:.1} s",
            axis.name(),
            last.map_or("unreadable".to_owned(), |p| p.to_string()),
            t0.elapsed().as_secs_f64()
        ));
        if last == Some(target) || t0.elapsed() >= LENS_SETTLE {
            return last;
        }
    }
}

async fn lens_edge<W: Write>(
    session: &DeviceSession,
    cfg: &ProbeConfig,
    axis: LensAxis,
    range: bc_protocol::ptz::LensRange,
    log: &mut Log<'_, W>,
) -> LensResult {
    let target = match cfg.edge {
        Edge::Min => range.min,
        Edge::Max => range.max,
    };
    let moved = session
        .ptz_lens(cfg.channel, axis, target, MotionOrigin::Manual)
        .await;
    log.line(&format!(
        "(c) {} from {} to the edge {target}: {}",
        axis.name(),
        range.cur,
        refusal_text(&moved)
    ));
    let mut result = LensResult {
        axis,
        original: range.cur,
        target,
        moved,
        reached: None,
        restored: None,
    };
    if result.moved.is_err() {
        return result;
    }
    result.reached = settle(session, cfg.channel, axis, target, "settling", log).await;
    let back = session
        .ptz_lens(cfg.channel, axis, range.cur, MotionOrigin::Manual)
        .await;
    log.line(&format!(
        "(c) {} back to {}: {}",
        axis.name(),
        range.cur,
        refusal_text(&back)
    ));
    if back.is_ok() {
        result.restored = settle(session, cfg.channel, axis, range.cur, "returning", log).await;
    }
    result
}

/// A short summary of a report, for the end of a run.
pub fn format_summary(r: &Report) -> String {
    let mut s = String::from("ptz-probe summary\n");
    if !r.confirmed {
        s.push_str("  not confirmed: no motion command was sent\n");
        return s;
    }
    if let Some(h) = &r.held {
        let _ = writeln!(
            s,
            "  (a) move: {}; core stop: {}{}",
            refusal_text(&h.moved),
            h.stop.map_or("none seen".to_owned(), |(c, d)| format!(
                "{} after {:.3} s",
                c.name(),
                d.as_secs_f64()
            )),
            if h.forced_stop {
                " (the probe stopped it)"
            } else {
                ""
            }
        );
    }
    if let Some(u) = &r.unknown_preset {
        let _ = writeln!(s, "  (b) unlisted preset: {}", refusal_text(u));
    }
    for l in &r.lens {
        let _ = writeln!(
            s,
            "  (c) {}: {} -> edge {} ({}), read {:?}; back: read {:?}",
            l.axis.name(),
            l.original,
            l.target,
            refusal_text(&l.moved),
            l.reached,
            l.restored
        );
    }
    s
}
