//! `bc-cli replay-probe`: measures whether one session carries live view and
//! recorded plays at once, and how many plays at normal speed a device
//! serves beside live view (docs/MILESTONE-P.md, B5).
//!
//! One session, one live stream, then stages of 1, 2, ... up to
//! [`MAX_PLAYS`] simultaneous plays. By default every play is of one recent
//! recording on the live channel; [`ProbeConfig::plays`] instead names a
//! source per play (another channel, or an earlier recording of one), so a
//! refusal can be told apart as per file, per camera, or in total. Each stage starts its plays fresh, runs them for at most
//! [`PLAY_LIMIT`] each, and stops them (7) before the next stage. Live and
//! play video frames are counted per second. The probe stops at the first
//! refusal, error, stall, or live-stream disruption, and on the overall time
//! cap or an interrupt; every play is stopped on every path (a play dropped
//! with its reader task sends its stop too), and the caller closes the
//! session.
//!
//! By default a stage starts its plays one at a time (seek, play, seek,
//! play, each at its own time). With [`ProbeConfig::batched`] it starts them
//! as the official client does ([`bc_session::DeviceSession::play_group`]):
//! every seek back to back at one shared time, a pause, then every play
//! back to back.
//!
//! Only commands the session's viewing tier allows are sent: the file
//! search (14, 15, 16), live preview (3, 4), seek (123), play (5) at
//! `playSpeed` 1, and stop (7); on older firmware, whose file search gives
//! no `Id`, each play is preceded by the file-information request (13) the
//! session sends for a by-name play.

use crate::{CliError, wait_logged_in};
use bc_protocol::archive::{DateTime, FileSearch, PlayRequest, RecordFile};
use bc_protocol::media::{Record, wall_clock_secs};
use bc_protocol::stream::StreamTier;
use bc_session::{
    ConnState, DeviceSession, DownloadError, Item, Play, PlayStart, QueryError, SearchError,
};
use std::fmt;
use std::future::Future;
use std::io::Write;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

/// The most plays the probe ever runs at once.
pub const MAX_PLAYS: usize = 4;
/// The longest any one play runs.
pub const PLAY_LIMIT: Duration = Duration::from_secs(10);
/// The longest a whole run takes, login included.
pub const RUN_LIMIT: Duration = Duration::from_secs(90);
/// The shortest recording the probe plays.
const MIN_FILE_SECS: i64 = 60;
/// How far a recording must lie in the past (it is complete, not growing).
const MIN_AGE_SECS: i64 = 120;
/// How far a play's video may lag the time since its first frame.
pub const BEHIND_LIMIT: f64 = 2.0;
/// How long a stop may take before the probe moves on.
const STOP_WAIT: Duration = Duration::from_secs(3);

/// Where one play's recording comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaySource {
    /// Channel of the recording.
    pub channel: u8,
    /// Which finished recording: 0 the latest, 1 the one before, ...
    pub back: usize,
}

/// Parses a comma-separated list of play sources, `N` for channel N's latest
/// finished recording and `N~K` for the K-th before it (K at most 3), with 1
/// to [`MAX_PLAYS`] entries.
pub fn parse_plays(s: &str) -> Option<Vec<PlaySource>> {
    let v = s
        .split(',')
        .map(|p| {
            let (ch, back) = match p.split_once('~') {
                Some((c, b)) => (c, b.parse::<usize>().ok().filter(|b| *b <= 3)?),
                None => (p, 0),
            };
            Some(PlaySource {
                channel: ch.parse().ok()?,
                back,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    (1..=MAX_PLAYS).contains(&v.len()).then_some(v)
}

/// What to probe.
#[derive(Clone, Debug)]
pub struct ProbeConfig {
    /// Channel of the live stream and of the plays.
    pub channel: u8,
    /// Stream tier of the live stream and of the plays.
    pub tier: StreamTier,
    /// Highest number of simultaneous plays to try (1 to [`MAX_PLAYS`]).
    /// Ignored when [`plays`](Self::plays) is set.
    pub max_plays: usize,
    /// One source per play: stage `k` runs the first `k`. Empty plays the
    /// live channel's latest finished recording in every play.
    pub plays: Vec<PlaySource>,
    /// Live view alone before the first stage.
    pub baseline: Duration,
    /// How long each stage runs after its last play started (each play is
    /// still capped at [`PLAY_LIMIT`]).
    pub stage: Duration,
    /// Pause after the stops of a stage.
    pub gap: Duration,
    /// Live view alone after the last stage.
    pub tail: Duration,
    /// The device's current local time, the clock of its recording times.
    /// `None` takes the first live keyframe's wall clock, which an NVR was
    /// observed to keep in UTC (docs/protocol.md): only right on a device
    /// whose local time is UTC.
    pub now: Option<DateTime>,
    /// No live preview at all: no baseline or tail, no live checks, and
    /// `now` is required. Measures the plays alone.
    pub no_live: bool,
    /// Start each stage's plays as a group ([`bc_session::DeviceSession::play_group`],
    /// the official client's recipe: every seek back to back at one
    /// `seekTime`, a pause, then every play back to back) instead of one at
    /// a time (seek, play, seek, play, each at its own time). The shared
    /// time is 5 s after the latest start of the stage's files
    /// ([`batched_seek`]); when no common time exists, each seek goes to its
    /// own file's start + 5 s.
    pub batched: bool,
    /// Waiting for the login.
    pub login_limit: Duration,
    /// The whole run, login included (at most [`RUN_LIMIT`]).
    pub run_limit: Duration,
}

impl ProbeConfig {
    /// The defaults for `channel`: sub-stream, up to four plays, 5 s of
    /// live view alone, stages of 8 s with 1 s between them, 3 s at the end.
    pub fn new(channel: u8) -> ProbeConfig {
        ProbeConfig {
            channel,
            tier: StreamTier::Sub,
            max_plays: MAX_PLAYS,
            plays: Vec::new(),
            baseline: Duration::from_secs(5),
            stage: Duration::from_secs(8),
            gap: Duration::from_secs(1),
            tail: Duration::from_secs(3),
            now: None,
            no_live: false,
            batched: false,
            login_limit: Duration::from_secs(20),
            run_limit: RUN_LIMIT,
        }
    }
}

/// Why the probe ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every stage ran to the end.
    Completed,
    /// The device answered a seek (123) or play (5) with this status.
    Refused {
        /// Stage (number of simultaneous plays tried).
        stage: usize,
        /// Command refused: 123 or 5.
        command: u16,
        /// Reply status.
        status: u16,
    },
    /// A play delivered nothing, or its video fell more than
    /// [`BEHIND_LIMIT`] behind real time (a play bursts ahead first, so
    /// a pause after the burst is not a stall).
    Stalled {
        /// Stage.
        stage: usize,
        /// Play within the stage (1-based).
        play: usize,
    },
    /// A play ended before its time was up.
    EndedEarly {
        /// Stage.
        stage: usize,
        /// Play within the stage (1-based).
        play: usize,
    },
    /// A request or play failed otherwise.
    Error {
        /// Stage (0 before the first).
        stage: usize,
        /// What failed.
        what: String,
    },
    /// The live stream was disrupted.
    LiveDisrupted {
        /// Stage (0 for the baseline, `max_plays + 1` for the tail).
        stage: usize,
        /// How.
        what: String,
    },
    /// No suitable recording was found.
    NoRecording,
    /// The overall time cap was reached.
    TimeCap,
    /// The run was interrupted (Ctrl-C).
    Interrupted,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Outcome::Completed => write!(f, "completed every stage"),
            Outcome::Refused {
                stage,
                command,
                status,
            } => write!(
                f,
                "refusal in stage {stage}: command {command} answered status {status}"
            ),
            Outcome::Stalled { stage, play } => write!(f, "stall in stage {stage}, play {play}"),
            Outcome::EndedEarly { stage, play } => {
                write!(f, "play {play} of stage {stage} ended early")
            }
            Outcome::Error { stage, what } => write!(f, "error in stage {stage}: {what}"),
            Outcome::LiveDisrupted { stage, what } => {
                write!(f, "live stream disrupted in stage {stage}: {what}")
            }
            Outcome::NoRecording => write!(f, "no recording of at least 60 s found"),
            Outcome::TimeCap => write!(f, "overall time cap reached"),
            Outcome::Interrupted => write!(f, "interrupted"),
        }
    }
}

/// One stage's measurements.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stage {
    /// Plays tried.
    pub plays: usize,
    /// Plays that started (seek and play accepted, first media arrived).
    pub delivered: usize,
    /// Seconds measured with every play of the stage running.
    pub seconds: u32,
    /// Live video frames per second over those seconds.
    pub live_fps: f64,
    /// Each play's video frames per second over the stage's seconds.
    pub play_fps: Vec<f64>,
    /// Each play's video bytes.
    pub play_bytes: Vec<u64>,
    /// Each play's time from the play request to its first video frame.
    pub first_frame: Vec<Option<Duration>>,
    /// Each play's first keyframe wall-clock seconds less its seek time.
    pub start_offset: Vec<Option<i64>>,
    /// Each play's seconds of video delivered (timestamp span).
    pub video_secs: Vec<f64>,
    /// Each play's seconds since its first frame when the stage ended.
    pub elapsed_secs: Vec<f64>,
}

/// What a run measured.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    /// Live video frames per second with no play running.
    pub baseline_live_fps: f64,
    /// Live video frames per second after the last stage.
    pub tail_live_fps: Option<f64>,
    /// The stages run, in order.
    pub stages: Vec<Stage>,
    /// The highest number of simultaneous plays that all delivered video
    /// for their stage, beside live view.
    pub max_ok: usize,
    /// Plays requested in all.
    pub plays_started: usize,
    /// Stops (7) the session confirmed sending.
    pub stops_sent: usize,
    /// Why the run ended.
    pub outcome: Outcome,
}

#[derive(Default)]
struct Counter {
    frames: AtomicU32,
    bytes: AtomicU64,
}

#[derive(Default)]
struct LiveState {
    counter: Counter,
    /// Set at the first keyframe, with its wall clock if it has one.
    first_wall: Mutex<Option<Option<u32>>>,
    trouble: Mutex<Option<String>>,
}

#[derive(Default)]
struct PlayState {
    counter: Counter,
    first_frame: Mutex<Option<Duration>>,
    first_wall: Mutex<Option<u32>>,
    /// Arrival of the first video frame, and the first and last timestamps.
    span: Mutex<Option<(Instant, u64, u64)>>,
}

enum PlayEnd {
    Cancelled,
    Ended,
    Failed(DownloadError),
}

struct Running {
    task: JoinHandle<(Play, PlayEnd)>,
    state: Arc<PlayState>,
    seek: DateTime,
}

/// Aborts the reader tasks it still holds when dropped (an interrupt or a
/// panic): each aborted task drops its play, which sends the stop.
struct Tasks(Vec<Running>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for r in &self.0 {
            r.task.abort();
        }
    }
}

struct LiveTask(JoinHandle<()>);

impl Drop for LiveTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Run<'a, W> {
    session: &'a DeviceSession,
    cfg: &'a ProbeConfig,
    out: &'a mut W,
    stop: watch::Receiver<bool>,
    deadline: Instant,
}

impl<W: Write> Run<'_, W> {
    /// Runs `fut` unless the run is interrupted or out of time first.
    async fn bounded<T>(&mut self, fut: impl Future<Output = T>) -> Result<T, Outcome> {
        let mut stop = self.stop.clone();
        tokio::select! {
            r = fut => Ok(r),
            _ = stop.wait_for(|v| *v) => Err(Outcome::Interrupted),
            () = sleep_until(self.deadline) => Err(Outcome::TimeCap),
        }
    }

    fn line(&mut self, s: &str) {
        let _ = writeln!(self.out, "{s}");
        let _ = self.out.flush();
    }

    /// Counts video frames of live view (and of `plays`) in one-second rows
    /// until `until`, returning the full rows. Stops at a disruption or a
    /// play that ended.
    async fn measure(
        &mut self,
        label: &str,
        stage: usize,
        live: &LiveState,
        plays: &[Running],
        until: Instant,
    ) -> Result<(Vec<u32>, Vec<Vec<u32>>), Outcome> {
        let mut live_rows = Vec::new();
        let mut play_rows: Vec<Vec<u32>> = vec![Vec::new(); plays.len()];
        // Rows start now: what came before (a gap between stages) is not
        // counted.
        live.counter.frames.store(0, Ordering::Relaxed);
        let mut tick = Instant::now();
        let mut state = self.session.watch_state();
        loop {
            let next = (tick + Duration::from_secs(1)).min(until);
            if next <= tick {
                break;
            }
            self.bounded(sleep_until(next)).await?;
            let full = next - tick == Duration::from_secs(1);
            tick = next;
            let l = live.counter.frames.swap(0, Ordering::Relaxed);
            let p: Vec<u32> = plays
                .iter()
                .map(|r| r.state.counter.frames.swap(0, Ordering::Relaxed))
                .collect();
            let cols: Vec<String> = p.iter().map(|n| format!("{n:>4}")).collect();
            if self.cfg.no_live {
                self.line(&format!("{label:<9} plays {}", cols.join(" ")));
            } else {
                self.line(&format!("{label:<9} live {l:>4}  plays {}", cols.join(" ")));
            }
            if !matches!(*state.borrow_and_update(), ConnState::Connected) {
                return Err(Outcome::LiveDisrupted {
                    stage,
                    what: "the connection was lost".into(),
                });
            }
            let trouble = live.trouble.lock().unwrap().clone();
            if let (false, Some(why)) = (self.cfg.no_live, trouble) {
                return Err(Outcome::LiveDisrupted { stage, what: why });
            }
            if full {
                if l == 0 && !self.cfg.no_live {
                    return Err(Outcome::LiveDisrupted {
                        stage,
                        what: "a second without live video".into(),
                    });
                }
                live_rows.push(l);
                for (row, n) in play_rows.iter_mut().zip(&p) {
                    row.push(*n);
                }
            }
            // A play that ended is collected and classified by the caller.
            if plays.iter().any(|r| r.task.is_finished()) {
                break;
            }
        }
        Ok((live_rows, play_rows))
    }
}

fn mean(rows: &[u32]) -> f64 {
    if rows.is_empty() {
        0.0
    } else {
        rows.iter().map(|n| f64::from(*n)).sum::<f64>() / rows.len() as f64
    }
}

fn add_secs(t: DateTime, secs: i64) -> DateTime {
    DateTime::from_naive_secs(t.to_naive_secs() + secs).unwrap_or(t)
}

/// The one seek time of a batched stage: 5 s after the latest start of the
/// stage's files, when every file holds it (it lies before the earliest
/// end); `None` when the files share no such time, and each seek then goes
/// to its own file's start + 5 s (the device is likely to start every play
/// from the last seek, so such a stage is an experiment, not the client's
/// recipe).
pub fn batched_seek(files: &[&RecordFile]) -> Option<DateTime> {
    let latest = files.iter().map(|f| f.start.to_naive_secs()).max()?;
    let earliest_end = files.iter().map(|f| f.end.to_naive_secs()).min()?;
    let t = latest + 5;
    (t < earliest_end)
        .then(|| DateTime::from_naive_secs(t))
        .flatten()
}

/// What a failed start means: a refused seek, the play limit, or another
/// failure (of play number `play`, 1-based, when one by one).
fn start_error(stage: usize, e: SearchError, play: Option<usize>) -> Outcome {
    match e {
        SearchError::Refused(status) => Outcome::Refused {
            stage,
            command: 123,
            status,
        },
        SearchError::Query(QueryError::Busy) => Outcome::Error {
            stage,
            what: "the session's play limit".into(),
        },
        other => Outcome::Error {
            stage,
            what: match play {
                Some(n) => format!("play {n}: {other}"),
                None => format!("batched start: {other}"),
            },
        },
    }
}

/// The recording to play: of the recordings of at least [`MIN_FILE_SECS`]
/// that ended at least [`MIN_AGE_SECS`] before `now`, the latest (`back` 0)
/// or the `back`-th before it.
pub fn pick_recording(files: &[RecordFile], now: DateTime, back: usize) -> Option<&RecordFile> {
    let now = now.to_naive_secs();
    let mut fit: Vec<&RecordFile> = files
        .iter()
        .filter(|f| {
            let (s, e) = (f.start.to_naive_secs(), f.end.to_naive_secs());
            e - s >= MIN_FILE_SECS + 10 && e <= now - MIN_AGE_SECS
        })
        .collect();
    fit.sort_by_key(|f| std::cmp::Reverse(f.end.to_naive_secs()));
    fit.get(back).copied()
}

/// Parses `YYYY-MM-DDTHH:MM:SS` (or with a space).
pub fn parse_time(s: &str) -> Option<DateTime> {
    let (d, t) = s.split_once(['T', ' '])?;
    let mut d = d.split('-').map(str::parse::<u16>);
    let mut t = t.split(':').map(str::parse::<u8>);
    let (y, mo, da) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let (h, mi, se) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    if d.next().is_some() || t.next().is_some() {
        return None;
    }
    DateTime::new(y, u8::try_from(mo).ok()?, u8::try_from(da).ok()?, h, mi, se)
}

fn spawn_play(
    play: Play,
    requested: Instant,
    state: Arc<PlayState>,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<(Play, PlayEnd)> {
    tokio::spawn(async move {
        let mut play = play;
        let end = loop {
            let r = tokio::select! {
                r = play.next() => r,
                _ = stop.wait_for(|v| *v) => break PlayEnd::Cancelled,
            };
            match r {
                Ok(Some(Record::Video {
                    data,
                    keyframe,
                    extra_header,
                    pts_us,
                    ..
                })) => {
                    {
                        let mut sp = state.span.lock().unwrap();
                        match sp.as_mut() {
                            Some((_, first, last)) => *last = pts_us.max(*first),
                            None => *sp = Some((Instant::now(), pts_us, pts_us)),
                        }
                    }
                    let mut ff = state.first_frame.lock().unwrap();
                    if ff.is_none() {
                        *ff = Some(requested.elapsed());
                        if keyframe {
                            *state.first_wall.lock().unwrap() = wall_clock_secs(&extra_header);
                        }
                    }
                    state.counter.frames.fetch_add(1, Ordering::Relaxed);
                    state
                        .counter
                        .bytes
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                }
                Ok(Some(_)) => {}
                Ok(None) => break PlayEnd::Ended,
                Err(e) => break PlayEnd::Failed(e),
            }
        };
        (play, end)
    })
}

const LIVE_ENDED: &str = "the live stream ended";

fn sub_ended(trouble: &str) -> bool {
    trouble == LIVE_ENDED
}

fn spawn_live(mut sub: bc_session::Subscription, state: Arc<LiveState>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let trouble = match sub.recv().await {
                Some(Item::Media(Record::Video {
                    keyframe,
                    extra_header,
                    ..
                })) => {
                    if keyframe {
                        let mut w = state.first_wall.lock().unwrap();
                        if w.is_none() {
                            *w = Some(wall_clock_secs(&extra_header));
                        }
                    }
                    state.counter.frames.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                Some(Item::Media(_)) => continue,
                Some(Item::Discontinuity) => {
                    "a discontinuity (connection lost or queue overflow)".to_owned()
                }
                Some(Item::WaitingForDevice { status }) => {
                    format!("the preview was answered status {status}")
                }
                Some(Item::StartRejected { status }) => {
                    format!("the preview was refused (status {status})")
                }
                None => LIVE_ENDED.to_owned(),
            };
            let ended = sub_ended(&trouble);
            state.trouble.lock().unwrap().get_or_insert(trouble);
            if ended {
                return;
            }
            // Otherwise keep draining, so the session's queue never backs up.
        }
    })
}

/// Runs the probe on `session` (which the caller closes afterwards),
/// writing progress lines to `out`. `stop` set to true interrupts it.
/// Only a failed login is an error; every other end is in
/// [`Report::outcome`].
pub async fn replay_probe<W: Write>(
    session: &DeviceSession,
    cfg: &ProbeConfig,
    out: &mut W,
    stop: watch::Receiver<bool>,
) -> Result<Report, CliError> {
    let deadline = Instant::now() + cfg.run_limit.min(RUN_LIMIT);
    let max_plays = if cfg.plays.is_empty() {
        cfg.max_plays
    } else {
        cfg.plays.len()
    }
    .clamp(1, MAX_PLAYS);
    let mut run = Run {
        session,
        cfg,
        out,
        stop: stop.clone(),
        deadline,
    };
    let mut report = Report {
        baseline_live_fps: 0.0,
        tail_live_fps: None,
        stages: Vec::new(),
        max_ok: 0,
        plays_started: 0,
        stops_sent: 0,
        outcome: Outcome::Completed,
    };
    match run.bounded(wait_logged_in(session, cfg.login_limit)).await {
        Ok(r) => r?,
        Err(o) => {
            report.outcome = o;
            return Ok(report);
        }
    }
    let mut tasks = Tasks(Vec::new());
    let r = probe(&mut run, &mut report, &mut tasks, max_plays).await;
    report.outcome = r.err().unwrap_or(Outcome::Completed);
    // Every exit path: stop what still runs (its stage's cancel sender is
    // gone, so the reader tasks return their plays).
    report.stops_sent += stop_all(&mut tasks).await;
    Ok(report)
}

/// Collects the reader tasks and sends each play's stop; returns how many
/// stops were sent.
async fn stop_all(tasks: &mut Tasks) -> usize {
    let mut sent = 0;
    for r in std::mem::take(&mut tasks.0) {
        let mut handle = r.task;
        match tokio::time::timeout(STOP_WAIT, &mut handle).await {
            Ok(Ok((play, _))) => {
                if play.stop(STOP_WAIT).await {
                    sent += 1;
                }
            }
            // A task that did not return is aborted, dropping its play
            // (which still sends the stop).
            _ => handle.abort(),
        }
    }
    sent
}

async fn probe<W: Write>(
    run: &mut Run<'_, W>,
    report: &mut Report,
    tasks: &mut Tasks,
    max_plays: usize,
) -> Result<(), Outcome> {
    let cfg = run.cfg.clone();
    let session = run.session;
    let ch = cfg.channel;
    // The channel list: each camera's ID, when the device lists one, goes
    // with its search and plays, as the official client sends it.
    let mut channels = session.watch_channels();
    let _ = run
        .bounded(tokio::time::timeout(
            Duration::from_secs(5),
            channels.wait_for(Option::is_some),
        ))
        .await?;

    let live_state = Arc::new(LiveState::default());
    let now;
    let _live;
    if cfg.no_live {
        _live = None;
        now = cfg.now.ok_or_else(|| Outcome::Error {
            stage: 0,
            what: "without live view the run needs the device's time (--now)".into(),
        })?;
        run.line("no live view; the plays run alone");
    } else {
        let sub = run
            .bounded(session.subscribe(ch, cfg.tier))
            .await?
            .map_err(|_| Outcome::Error {
                stage: 0,
                what: "the session closed".into(),
            })?;
        _live = Some(LiveTask(spawn_live(sub, Arc::clone(&live_state))));
        // The first live keyframe.
        let first = run
            .bounded(async {
                let limit = Instant::now() + Duration::from_secs(10);
                while Instant::now() < limit {
                    if live_state.first_wall.lock().unwrap().is_some() {
                        return true;
                    }
                    if live_state.trouble.lock().unwrap().is_some() {
                        return false;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                false
            })
            .await?;
        if !first {
            let what = live_state
                .trouble
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| "no live keyframe within 10 s".into());
            return Err(Outcome::LiveDisrupted { stage: 0, what });
        }
        let live_wall = live_state
            .first_wall
            .lock()
            .unwrap()
            .flatten()
            .and_then(|w| DateTime::from_naive_secs(i64::from(w)));
        now = cfg.now.or(live_wall).ok_or_else(|| Outcome::Error {
            stage: 0,
            what: "the live keyframe has no wall clock; pass --now".into(),
        })?;
        run.line(&format!(
            "live view of channel {ch} ({}) running; the first keyframe's wall clock reads {}",
            cfg.tier.name(),
            live_wall
                .map(|d| d.to_string())
                .unwrap_or_else(|| "nothing".into())
        ));

        let until = Instant::now() + cfg.baseline;
        let (base, _) = run.measure("baseline", 0, &live_state, &[], until).await?;
        report.baseline_live_fps = mean(&base);
    }

    // Each play's recording: today's files of its channel, or yesterday's
    // when today has none that fits (the live wall clock may be on another
    // day than the index).
    let sources = if cfg.plays.is_empty() {
        vec![
            PlaySource {
                channel: ch,
                back: 0
            };
            max_plays
        ]
    } else {
        cfg.plays.clone()
    };
    let mut found: Vec<(PlaySource, Option<String>, RecordFile)> = Vec::new();
    for src in sources {
        if let Some((_, uid, f)) = found.iter().find(|(s, _, _)| *s == src) {
            let (uid, f) = (uid.clone(), f.clone());
            found.push((src, uid, f));
            continue;
        }
        let uid = session.channel_uids().get(&src.channel).cloned();
        let mut file = None;
        for back in [0, 1] {
            let day = add_secs(now, -86_400 * back);
            let q = FileSearch {
                channel: src.channel,
                uid: uid.clone(),
                stream: cfg.tier,
                start: DateTime::new(day.year, day.month, day.day, 0, 0, 0).expect("valid day"),
                end: DateTime::new(day.year, day.month, day.day, 23, 59, 59).expect("valid day"),
            };
            let files = run
                .bounded(session.search_files(&q))
                .await?
                .map_err(|e| match e {
                    SearchError::Refused(status) => Outcome::Refused {
                        stage: 0,
                        command: 14,
                        status,
                    },
                    other => Outcome::Error {
                        stage: 0,
                        what: format!("file search: {other}"),
                    },
                })?;
            if let Some(f) = pick_recording(&files, now, src.back) {
                file = Some(f.clone());
                break;
            }
        }
        let file = file.ok_or(Outcome::NoRecording)?;
        run.line(&format!(
            "play {}: channel {}, a recording of {} s ({} to {})",
            found.len() + 1,
            src.channel,
            file.end.to_naive_secs() - file.start.to_naive_secs(),
            file.start,
            file.end
        ));
        found.push((src, uid, file));
    }

    for stage in 1..=max_plays {
        if cfg.no_live {
            run.line(&format!("stage {stage}: {stage} play(s), no live view"));
        } else {
            run.line(&format!("stage {stage}: {stage} play(s) beside live view"));
        }
        let mut st = Stage {
            plays: stage,
            ..Stage::default()
        };
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mut first_start = None;
        let mut refused = None;
        let stage_files: Vec<&(PlaySource, Option<String>, RecordFile)> =
            found.iter().take(stage).collect();
        // Batched: one seek time for the stage when every file holds it.
        let shared = if cfg.batched {
            let files: Vec<&RecordFile> = stage_files.iter().map(|(_, _, f)| f).collect();
            batched_seek(&files)
        } else {
            None
        };
        let starts: Vec<PlayStart> = stage_files
            .iter()
            .enumerate()
            .map(|(j, (src, uid, file))| PlayStart {
                request: PlayRequest {
                    channel: src.channel,
                    uid: uid.clone(),
                    stream: cfg.tier,
                    file_id: file.id.clone(),
                    file_name: file.name.clone(),
                    file_start: file.start,
                    speed: 1,
                },
                at: if cfg.batched {
                    shared.unwrap_or_else(|| add_secs(file.start, 5))
                } else {
                    add_secs(file.start, 5 + 10 * j as i64)
                },
            })
            .collect();
        if cfg.batched {
            match shared {
                Some(t) => run.line(&format!(
                    "batched start: {stage} seek(s) at {t}, all plays after the pause"
                )),
                None => run.line(&format!(
                    "batched start: the files hold no common time, {stage} seek(s) at each \
                     file's start + 5 s, all plays after the pause"
                )),
            }
            report.plays_started += stage;
            match run.bounded(session.play_group(&starts, shared)).await? {
                Ok(plays) => {
                    let requested = Instant::now();
                    first_start = Some(requested);
                    for (p, start) in plays.into_iter().zip(&starts) {
                        let state = Arc::new(PlayState::default());
                        tasks.0.push(Running {
                            task: spawn_play(p, requested, Arc::clone(&state), cancel_rx.clone()),
                            state,
                            seek: start.at,
                        });
                    }
                }
                Err(e) => refused = Some(start_error(stage, e, None)),
            }
        } else {
            for (j, start) in starts.iter().enumerate() {
                report.plays_started += 1;
                let requested = Instant::now();
                let r = run.bounded(session.play(start)).await?;
                first_start.get_or_insert_with(Instant::now);
                match r {
                    Ok(p) => {
                        let state = Arc::new(PlayState::default());
                        tasks.0.push(Running {
                            task: spawn_play(p, requested, Arc::clone(&state), cancel_rx.clone()),
                            state,
                            seek: start.at,
                        });
                    }
                    Err(e) => {
                        refused = Some(start_error(stage, e, Some(j + 1)));
                        break;
                    }
                }
            }
        }
        let mut result = Ok(());
        if let Some(o) = refused {
            result = Err(o);
        } else {
            // A second of the play limit is left for the stops.
            let until = (Instant::now() + cfg.stage).min(
                first_start.unwrap_or_else(Instant::now) + PLAY_LIMIT - Duration::from_secs(1),
            );
            let label = format!("stage {stage}");
            match run
                .measure(&label, stage, &live_state, &tasks.0, until)
                .await
            {
                Ok((live_rows, play_rows)) => {
                    st.seconds = live_rows.len() as u32;
                    st.live_fps = mean(&live_rows);
                    st.play_fps = play_rows.iter().map(|r| mean(r)).collect();
                }
                Err(o) => result = Err(o),
            }
        }
        let stage_end = Instant::now();
        let _ = cancel_tx.send(true);
        // Collect the stage's plays: how each ended, then its stop.
        for (j, r) in std::mem::take(&mut tasks.0).into_iter().enumerate() {
            let mut handle = r.task;
            let ended = match tokio::time::timeout(STOP_WAIT, &mut handle).await {
                Ok(Ok((play, end))) => {
                    if play.stop(STOP_WAIT).await {
                        report.stops_sent += 1;
                    }
                    end
                }
                _ => {
                    handle.abort();
                    PlayEnd::Failed(DownloadError::Cancelled)
                }
            };
            let delivered = r.state.first_frame.lock().unwrap().is_some();
            if delivered {
                st.delivered += 1;
            }
            st.play_bytes
                .push(r.state.counter.bytes.load(Ordering::Relaxed));
            st.first_frame.push(*r.state.first_frame.lock().unwrap());
            st.start_offset.push(
                r.state
                    .first_wall
                    .lock()
                    .unwrap()
                    .map(|w| i64::from(w) - r.seek.to_naive_secs()),
            );
            let (video, elapsed) = match *r.state.span.lock().unwrap() {
                Some((at, first, last)) => (
                    (last - first) as f64 / 1e6,
                    stage_end.saturating_duration_since(at).as_secs_f64(),
                ),
                None => (0.0, 0.0),
            };
            st.video_secs.push(video);
            st.elapsed_secs.push(elapsed);
            let behind = delivered && elapsed - video > BEHIND_LIMIT;
            let o = match ended {
                PlayEnd::Cancelled if behind => Some(Outcome::Stalled { stage, play: j + 1 }),
                PlayEnd::Cancelled => None,
                PlayEnd::Ended => Some(Outcome::EndedEarly { stage, play: j + 1 }),
                PlayEnd::Failed(DownloadError::Stalled) => {
                    Some(Outcome::Stalled { stage, play: j + 1 })
                }
                PlayEnd::Failed(DownloadError::Refused(status)) => Some(Outcome::Refused {
                    stage,
                    command: 5,
                    status,
                }),
                PlayEnd::Failed(e) => Some(Outcome::Error {
                    stage,
                    what: format!("play {}: {e}", j + 1),
                }),
            };
            if let (Some(o), Ok(())) = (o, &result) {
                result = Err(o);
            }
        }
        if result.is_ok() && st.delivered < stage {
            result = Err(Outcome::Stalled {
                stage,
                play: st.delivered + 1,
            });
        }
        let fps: Vec<String> = st.play_fps.iter().map(|f| format!("{f:.1}")).collect();
        let first: Vec<String> = st
            .first_frame
            .iter()
            .map(|d| d.map_or("-".into(), |d| format!("{} ms", d.as_millis())))
            .collect();
        let offs: Vec<String> = st
            .start_offset
            .iter()
            .map(|o| o.map_or("-".into(), |o| format!("{o:+} s")))
            .collect();
        let pace: Vec<String> = st
            .video_secs
            .iter()
            .zip(&st.elapsed_secs)
            .map(|(v, e)| format!("{v:.1} s in {e:.1} s"))
            .collect();
        let live = if cfg.no_live {
            "no live view".to_string()
        } else {
            format!(
                "live {:.1} fps (baseline {:.1})",
                st.live_fps, report.baseline_live_fps
            )
        };
        run.line(&format!(
            "stage {stage} summary: {live}; plays fps [{}]; bytes {:?}; \
             first frame [{}]; first keyframe vs seek [{}]; video delivered [{}]",
            fps.join(", "),
            st.play_bytes,
            first.join(", "),
            offs.join(", "),
            pace.join(", ")
        ));
        report.stages.push(st);
        result?;
        report.max_ok = stage;
        run.bounded(tokio::time::sleep(cfg.gap)).await?;
    }
    if cfg.no_live {
        return Ok(());
    }
    let until = Instant::now() + cfg.tail;
    let (tail, _) = run
        .measure("tail", max_plays + 1, &live_state, &[], until)
        .await?;
    report.tail_live_fps = Some(mean(&tail));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u8, m: u8, s: u8) -> DateTime {
        DateTime::new(2026, 10, 8, h, m, s).unwrap()
    }

    fn file(start: DateTime, end: DateTime) -> RecordFile {
        RecordFile {
            channel: 0,
            name: format!("f{}", start.to_naive_secs()),
            id: format!("i{}", start.to_naive_secs()),
            stream: Some(StreamTier::Sub),
            contains_audio: false,
            record_types: Vec::new(),
            size: 0,
            start,
            end,
        }
    }

    #[test]
    fn a_batched_stage_seeks_five_seconds_after_the_latest_start() {
        let a = file(t(9, 0, 0), t(9, 5, 0));
        let b = file(t(9, 2, 0), t(9, 8, 0));
        // Both hold 09:02:05.
        assert_eq!(batched_seek(&[&a, &b]), Some(t(9, 2, 5)));
        assert_eq!(batched_seek(&[&a]), Some(t(9, 0, 5)));
        // Files that do not overlap share no time.
        let c = file(t(9, 10, 0), t(9, 15, 0));
        assert_eq!(batched_seek(&[&a, &c]), None);
        // The shared time must lie inside the shortest file.
        let short = file(t(9, 3, 0), t(9, 3, 4));
        assert_eq!(batched_seek(&[&a, &short]), None);
        assert_eq!(batched_seek(&[]), None);
    }

    #[test]
    fn picks_the_latest_finished_recording_of_a_minute() {
        let files = vec![
            file(t(9, 0, 0), t(9, 5, 0)),
            // Too short.
            file(t(9, 10, 0), t(9, 10, 50)),
            file(t(9, 20, 0), t(9, 22, 0)),
            // Still being written (ended less than 2 minutes ago).
            file(t(9, 25, 0), t(9, 29, 30)),
        ];
        let now = t(9, 30, 0);
        assert_eq!(pick_recording(&files, now, 0).unwrap().start, t(9, 20, 0));
        assert_eq!(pick_recording(&files, now, 1).unwrap().start, t(9, 0, 0));
        assert!(pick_recording(&files, now, 2).is_none());
        assert!(pick_recording(&files[..2], t(9, 1, 0), 0).is_none());
        assert!(pick_recording(&[], now, 0).is_none());
    }

    #[test]
    fn parses_play_sources() {
        let src = |channel, back| PlaySource { channel, back };
        assert_eq!(
            parse_plays("11,3,11~1,6"),
            Some(vec![src(11, 0), src(3, 0), src(11, 1), src(6, 0)])
        );
        assert_eq!(parse_plays("2"), Some(vec![src(2, 0)]));
        for bad in ["", "1,2,3,4,5", "x", "1~4", "1~", "~1", "256", "1,,2"] {
            assert_eq!(parse_plays(bad), None, "{bad}");
        }
    }

    #[test]
    fn parses_times() {
        assert_eq!(parse_time("2026-10-08T09:30:00"), Some(t(9, 30, 0)));
        assert_eq!(parse_time("2026-10-08 09:30:00"), Some(t(9, 30, 0)));
        assert_eq!(parse_time("2026-10-08"), None);
        assert_eq!(parse_time("2026-13-08T09:30:00"), None);
        assert_eq!(parse_time("2026-10-08T09:30:00:00"), None);
    }
}
