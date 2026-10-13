//! `bc-cli playback-probe`: runs the playback engine (viewer-core, slice C10)
//! against a real device and measures it (docs/MILESTONE-P.md).
//!
//! Unlike `replay-probe`, which drives the session layer directly, this goes
//! through [`viewer_core::Core`] exactly as an app does: it adds the device,
//! configures the recordings owner in a scratch data directory, enables the
//! channels, asks for today's days, waits for the index, opens one playback
//! of up to four cameras at a time all of them recorded, and runs a fixed
//! script (1x, an exact seek, 4x, 16x, 1x, pause, resume). One live tile of
//! the first camera runs beside it.
//!
//! No renderer exists, so the tiles' actions are consumed the way the
//! core's own tests do: every `Decode` is answered at once with `decoded`
//! (no pixels are decoded), and what the engine released and when is
//! measured from those actions and from the core's events. The engine's own
//! log lines (buffer stops, play starts, warnings) are counted through a
//! tracing layer.
//!
//! Only read-only calls are made: the core's allowlist allows nothing else.
//! The run is capped, interruptible, and on every path (error, cap,
//! interrupt, panic) the playback is closed, which stops every play, the
//! live tile is closed, and the core is shut down, which closes the one
//! session. The scratch data directory is removed.

use bc_protocol::archive::DateTime;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{EnvFilter, Targets};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use viewer_core::{Action, Callbacks, Core, CoreConfig, PlaybackState, SeekMode, StreamTier};

/// The most cameras one playback holds.
pub const MAX_CHANNELS: usize = 4;
/// The longest a whole run takes, from the first call.
pub const RUN_LIMIT: Duration = Duration::from_secs(150);
/// The recordings must end at least this long before `now`.
const MIN_AGE_SECS: i64 = 190;
/// Media time the script covers from its start (for choosing the time).
const SCRIPT_MEDIA_SECS: i64 = 330;
/// The shortest common recording the probe plays.
const MIN_WINDOW_SECS: i64 = 120;
/// How long the index may take.
const INDEX_WAIT: Duration = Duration::from_secs(30);
/// How long after the request the index is trusted to be idle even if no
/// camera shows a recording (a camera may have none).
pub const INDEX_GRACE: Duration = Duration::from_secs(8);
/// How long the opening seek may take.
const OPEN_WAIT: Duration = Duration::from_secs(20);
/// How often the sync spread is sampled.
const SAMPLE_EVERY: Duration = Duration::from_millis(250);
/// A gap between two released pictures longer than this is a stall.
const STALL_GAP: Duration = Duration::from_millis(1000);

/// What to run.
#[derive(Clone, Debug)]
pub struct ProbeConfig {
    /// Resolved device address (host part).
    pub host: String,
    /// Port.
    pub port: u16,
    /// User name.
    pub user: String,
    /// The device's current local time.
    pub now: DateTime,
    /// The device's offset from UTC, seconds.
    pub utc_offset_s: i64,
    /// One to [`MAX_CHANNELS`] distinct channels.
    pub channels: Vec<u8>,
    /// Scales every hold of the script (1.0 in a real run; tests shrink it).
    pub scale: f64,
    /// The whole run (at most [`RUN_LIMIT`] in a real run).
    pub run_limit: Duration,
    /// How long after the request the index is trusted to be idle although
    /// a camera shows no recording ([`INDEX_GRACE`] in a real run; tests
    /// shorten it).
    pub index_grace: Duration,
    /// Tests only: the core's data directory parent (`None`: the temp dir).
    pub scratch_parent: Option<PathBuf>,
}

impl ProbeConfig {
    /// The standard run.
    pub fn new(
        host: &str,
        port: u16,
        user: &str,
        now: DateTime,
        utc_offset_s: i64,
        channels: Vec<u8>,
    ) -> ProbeConfig {
        ProbeConfig {
            host: host.to_owned(),
            port,
            user: user.to_owned(),
            now,
            utc_offset_s,
            channels,
            scale: 1.0,
            run_limit: RUN_LIMIT,
            index_grace: INDEX_GRACE,
            scratch_parent: None,
        }
    }
}

/// Parses `--channels a,b,c,d`: one to four distinct channel numbers.
pub fn parse_channels(s: &str) -> Option<Vec<u8>> {
    let v = s
        .split(',')
        .map(|p| p.trim().parse::<u8>().ok())
        .collect::<Option<Vec<u8>>>()?;
    let distinct = v.iter().enumerate().all(|(i, c)| !v[..i].contains(c));
    ((1..=MAX_CHANNELS).contains(&v.len()) && distinct).then_some(v)
}

/// Parses `--utc-offset-s`: seconds from UTC, within a day's zone range.
pub fn parse_utc_offset(s: &str) -> Option<i64> {
    s.parse::<i64>().ok().filter(|o| o.abs() <= 18 * 3600)
}

// ---------------------------------------------------------------------------
// Logging: the engine's lines, counted.

/// One engine log line of interest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogKind {
    /// A camera's play stopped because its buffer was full.
    BufferStop,
    /// The engine started plays (the first start, a restart after a stop,
    /// a seek, a rate change, a resume).
    PlaysStarted,
    /// A warning or error line.
    Warn(String),
}

/// The counted engine lines, with the time each was seen.
#[derive(Default)]
pub struct LogCounts {
    lines: Mutex<Vec<(Instant, LogKind)>>,
}

impl LogCounts {
    fn between(&self, from: Instant, to: Instant) -> Vec<LogKind> {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .filter(|(at, _)| *at >= from && *at < to)
            .map(|(_, k)| k.clone())
            .collect()
    }
}

struct MessageOf(String);

impl Visit for MessageOf {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

struct CountLayer(Arc<LogCounts>);

impl<S: tracing::Subscriber> Layer<S> for CountLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut m = MessageOf(String::new());
        event.record(&mut m);
        let level = *event.metadata().level();
        let kind = if m.0 == "buffer full; play stopped" {
            LogKind::BufferStop
        } else if m.0 == "plays started" {
            LogKind::PlaysStarted
        } else if level <= tracing::Level::WARN {
            let mut text = m.0;
            text.truncate(100);
            LogKind::Warn(text)
        } else {
            return;
        };
        self.0.lines.lock().unwrap().push((Instant::now(), kind));
    }
}

/// Installs the process's logging: the usual stderr lines (`NVRVIEW_LOG`,
/// default `warn`) plus the counting layer for the engine's lines.
pub fn init_logging() -> Arc<LogCounts> {
    static COUNTS: OnceLock<Arc<LogCounts>> = OnceLock::new();
    if let Some(c) = COUNTS.get() {
        return c.clone();
    }
    let counts = COUNTS
        .get_or_init(|| Arc::new(LogCounts::default()))
        .clone();
    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(
            EnvFilter::try_from_env("NVRVIEW_LOG").unwrap_or_else(|_| EnvFilter::new("warn")),
        );
    let count = CountLayer(counts.clone())
        .with_filter(Targets::new().with_target("viewer_core::playback", tracing::Level::DEBUG));
    let _ = tracing_subscriber::registry()
        .with(fmt)
        .with(count)
        .try_init();
    counts
}

// ---------------------------------------------------------------------------
// The headless consumer of the tiles' actions.

#[derive(Clone, Copy, Debug)]
struct Dec {
    at: Instant,
    pts: u64,
}

#[derive(Default)]
struct Data {
    decodes: HashMap<u64, Vec<Dec>>,
    events: Vec<(Instant, Value)>,
}

/// Answers every `Decode` at once and keeps what happened.
#[derive(Default)]
pub struct Collector {
    core: OnceLock<Weak<Core>>,
    data: Mutex<Data>,
}

/// Events kept at most (a run is capped, this is only a bound).
const EVENTS_KEPT: usize = 50_000;

impl Callbacks for Collector {
    fn on_action(&self, tile: u64, generation: u32, action: &Action<'_>) {
        if let Action::Decode { pts, .. } = *action {
            self.data
                .lock()
                .unwrap()
                .decodes
                .entry(tile)
                .or_default()
                .push(Dec {
                    at: Instant::now(),
                    pts,
                });
            if let Some(core) = self.core.get().and_then(Weak::upgrade) {
                // A tile closed meanwhile is the only expected failure.
                let _ = core.decoded(tile, generation, pts);
            }
        }
    }

    fn on_event(&self, json: &str) {
        let Ok(v) = serde_json::from_str::<Value>(json) else {
            return;
        };
        let mut d = self.data.lock().unwrap();
        if d.events.len() < EVENTS_KEPT {
            d.events.push((Instant::now(), v));
        }
    }
}

impl Collector {
    fn decodes_between(&self, tile: u64, from: Instant, to: Instant) -> Vec<Dec> {
        self.data
            .lock()
            .unwrap()
            .decodes
            .get(&tile)
            .map(|v| {
                v.iter()
                    .filter(|d| d.at >= from && d.at < to)
                    .copied()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn last_decode(&self, tile: u64, before: Instant) -> Option<Dec> {
        self.data
            .lock()
            .unwrap()
            .decodes
            .get(&tile)?
            .iter()
            .rev()
            .find(|d| d.at < before)
            .copied()
    }

    fn events_between(&self, from: Instant, to: Instant) -> Vec<(Instant, Value)> {
        self.data
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|(at, _)| *at >= from && *at < to)
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Choosing the time.

/// A closed interval of seconds.
type Span = (i64, i64);

/// Sorts and merges overlapping or touching spans.
fn merge(mut v: Vec<Span>) -> Vec<Span> {
    v.sort_unstable();
    let mut out: Vec<Span> = Vec::new();
    for (s, e) in v {
        match out.last_mut() {
            Some(last) if s <= last.1 + 1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// The parts every list of spans has in common.
fn intersect(lists: &[Vec<Span>]) -> Vec<Span> {
    let mut cur = lists.first().cloned().unwrap_or_default();
    for next in lists.iter().skip(1) {
        let mut out = Vec::new();
        for &(a, b) in &cur {
            for &(c, d) in next {
                let (s, e) = (a.max(c), b.min(d));
                if s < e {
                    out.push((s, e));
                }
            }
        }
        cur = merge(out);
    }
    cur
}

/// The time to start at: the latest interval all cameras recorded that is
/// at least [`MIN_WINDOW_SECS`] long and ends `MIN_AGE_SECS` before `now`.
/// Returns (start of the playback range, the position to open at, end of
/// the range), seconds. The position is [`SCRIPT_MEDIA_SECS`] before the
/// interval's end when it is long enough, else its start.
pub fn choose_window(per_camera: &[Vec<Span>], now_secs: i64) -> Option<(i64, i64, i64)> {
    let limit = now_secs - MIN_AGE_SECS;
    let clipped: Vec<Vec<Span>> = per_camera
        .iter()
        .map(|spans| {
            merge(
                spans
                    .iter()
                    .filter(|(s, _)| *s < limit)
                    .map(|&(s, e)| (s, e.min(limit)))
                    .collect(),
            )
        })
        .collect();
    let common = intersect(&clipped);
    let (s, e) = common
        .iter()
        .rev()
        .find(|(s, e)| e - s >= MIN_WINDOW_SECS)
        .copied()?;
    let pos = (e - SCRIPT_MEDIA_SECS).max(s + 2);
    Some((s, pos, e))
}

// ---------------------------------------------------------------------------
// Measurements.

/// One camera's numbers for one segment of the script.
#[derive(Clone, Debug, Default)]
pub struct CamSeg {
    /// From the call to the first picture released, ms.
    pub first_ms: Option<u64>,
    /// Pictures released.
    pub frames: usize,
    /// Pictures released per second after the first two seconds.
    pub fps: f64,
    /// Media seconds advanced per wall second over the same stretch.
    pub media_speed: f64,
    /// The longest gap between two releases, ms.
    pub max_gap_ms: u64,
    /// Gaps over the stall limit.
    pub stalls: usize,
    /// `playback_camera` states seen, as `state` or `state(reason)`.
    pub states: Vec<String>,
}

/// One segment of the script.
#[derive(Clone, Debug, Default)]
pub struct Segment {
    pub name: String,
    pub rate: f64,
    pub wall_s: f64,
    /// The call to the seek's `active` event, ms (seeks and the opening).
    pub seek_active_ms: Option<u64>,
    pub cams: Vec<CamSeg>,
    /// Largest and mean spread of the cameras' released media time, ms.
    pub spread_max_ms: Option<i64>,
    pub spread_mean_ms: Option<i64>,
    /// How far the cameras ran behind (positive) or ahead of the clock on
    /// average at the end, ms (only where the cameras are anchored).
    pub live_fps: f64,
    pub buffer_stops: usize,
    pub plays_started: usize,
    pub warnings: Vec<String>,
    pub position_ms: i64,
    pub state: String,
}

struct Sample {
    cams: Vec<Option<Dec>>,
}

/// Spread of the cameras' released media time within one segment, from
/// samples of each camera's latest release. Each camera is anchored at the
/// first sample where all have released in the segment, so this measures
/// how far the cameras' media time drifted apart since then. Returns
/// (max, mean), ms.
fn spread(samples: &[Sample]) -> Option<(i64, i64)> {
    let first = samples
        .iter()
        .position(|s| !s.cams.is_empty() && s.cams.iter().all(Option::is_some))?;
    let base: Vec<u64> = samples[first]
        .cams
        .iter()
        .map(|d| d.expect("checked").pts)
        .collect();
    let mut worst = 0i64;
    let (mut sum, mut n) = (0i64, 0i64);
    for s in &samples[first..] {
        let adv: Vec<i64> = s
            .cams
            .iter()
            .zip(&base)
            .filter_map(|(d, b)| d.map(|d| (d.pts as i64 - *b as i64) / 1000))
            .collect();
        if adv.len() != base.len() {
            continue;
        }
        let sp = adv.iter().max()? - adv.iter().min()?;
        worst = worst.max(sp);
        sum += sp;
        n += 1;
    }
    (n > 0).then(|| (worst, sum / n))
}

/// One camera's numbers from its releases `decs` in a segment that began at
/// `t0`.
fn cam_numbers(decs: &[Dec], t0: Instant) -> CamSeg {
    let mut c = CamSeg {
        frames: decs.len(),
        ..CamSeg::default()
    };
    let Some(first) = decs.first() else {
        return c;
    };
    c.first_ms = Some(first.at.duration_since(t0).as_millis() as u64);
    for w in decs.windows(2) {
        let gap = w[1].at.duration_since(w[0].at);
        c.max_gap_ms = c.max_gap_ms.max(gap.as_millis() as u64);
        if gap > STALL_GAP {
            c.stalls += 1;
        }
    }
    // The first two seconds after the first release hold a seek's burst of
    // pictures before the target; the steady stretch is what follows.
    let steady: Vec<&Dec> = decs
        .iter()
        .filter(|d| d.at.duration_since(first.at) >= Duration::from_secs(2))
        .collect();
    let steady: Vec<&Dec> = if steady.len() >= 2 {
        steady
    } else {
        decs.iter().collect()
    };
    if let (Some(a), Some(b)) = (steady.first(), steady.last()) {
        let wall = b.at.duration_since(a.at).as_secs_f64();
        if wall > 0.0 && steady.len() > 1 {
            c.fps = (steady.len() - 1) as f64 / wall;
            c.media_speed = (b.pts as f64 - a.pts as f64) / 1e6 / wall;
        }
    }
    c
}

// ---------------------------------------------------------------------------
// The run.

/// How the run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every step ran.
    Completed,
    /// The run cap came first.
    TimeLimit,
    /// Interrupted.
    Interrupted,
}

/// The result of a run.
#[derive(Debug)]
pub struct Report {
    pub outcome: Outcome,
    pub segments: Vec<Segment>,
    /// Seconds from the chosen position to the end of the common recording.
    pub window_secs: i64,
    /// Seconds from the end of the common recording to `now`.
    pub age_secs: i64,
    pub index_ms: u64,
    pub live_baseline_fps: f64,
    pub live_first_ms: Option<u64>,
    pub elapsed_s: f64,
    /// The playback and the core (and so the session) were closed.
    pub closed: bool,
}

struct Cleanup {
    core: Arc<Core>,
    playback: Option<u64>,
    live: Option<u64>,
    dir: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(pb) = self.playback.take() {
            let _ = self.core.playback_close(pb);
        }
        if let Some(t) = self.live.take() {
            let _ = self.core.close_tile(t);
        }
        // Closes the one session.
        self.core.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Run<'a> {
    core: Arc<Core>,
    col: Arc<Collector>,
    logs: Arc<LogCounts>,
    stop: Arc<AtomicBool>,
    deadline: Instant,
    out: &'a mut dyn Write,
    /// The playback's tiles, in channel order.
    tiles: Vec<u64>,
    playback: u64,
    live: u64,
}

/// Why a step ended early.
#[derive(Debug)]
enum Halt {
    Stopped,
    TimeLimit,
    Failed(String),
}

impl From<viewer_core::RecError> for Halt {
    fn from(e: viewer_core::RecError) -> Halt {
        Halt::Failed(format!("{e:?}"))
    }
}

impl Run<'_> {
    /// Sleeps `d` in short slices; stops early when interrupted or past the
    /// cap.
    fn nap(&self, d: Duration) -> Result<(), Halt> {
        let end = Instant::now() + d;
        loop {
            self.check()?;
            let now = Instant::now();
            if now >= end {
                return Ok(());
            }
            std::thread::sleep((end - now).min(Duration::from_millis(20)));
        }
    }

    fn check(&self) -> Result<(), Halt> {
        if self.stop.load(Ordering::SeqCst) {
            return Err(Halt::Stopped);
        }
        if Instant::now() >= self.deadline {
            return Err(Halt::TimeLimit);
        }
        Ok(())
    }

    fn seek_active_since(&self, t0: Instant) -> Option<Instant> {
        self.col
            .events_between(t0, Instant::now())
            .into_iter()
            .find(|(_, e)| {
                e["type"] == "playback_seek"
                    && e["playback"] == self.playback
                    && e["state"] == "active"
            })
            .map(|(at, _)| at)
    }

    /// One segment: runs `begin` at once, holds for `hold` sampling the
    /// cameras, and measures.
    fn segment(
        &mut self,
        name: &str,
        rate: f64,
        hold: Duration,
        begin: impl FnOnce(&Core, u64) -> Result<(), viewer_core::RecError>,
    ) -> Result<Segment, Halt> {
        self.check()?;
        let t0 = Instant::now();
        begin(&self.core, self.playback)?;
        let mut samples = Vec::new();
        let end = t0 + hold;
        while Instant::now() < end {
            self.nap(SAMPLE_EVERY.min(end.saturating_duration_since(Instant::now())))?;
            let now = Instant::now();
            samples.push(Sample {
                cams: self
                    .tiles
                    .iter()
                    .map(|t| self.col.last_decode(*t, now).filter(|d| d.at >= t0))
                    .collect(),
            });
        }
        Ok(self.measure(name, rate, t0, Instant::now(), &samples))
    }

    fn measure(
        &self,
        name: &str,
        rate: f64,
        t0: Instant,
        t1: Instant,
        samples: &[Sample],
    ) -> Segment {
        let wall = t1.duration_since(t0);
        let mut seg = Segment {
            name: name.to_owned(),
            rate,
            wall_s: wall.as_secs_f64(),
            ..Segment::default()
        };
        seg.seek_active_ms = self
            .seek_active_since(t0)
            .map(|a| a.duration_since(t0).as_millis() as u64);
        let events = self.col.events_between(t0, t1);
        for &tile in &self.tiles {
            let decs = self.col.decodes_between(tile, t0, t1);
            let mut c = cam_numbers(&decs, t0);
            for (_, e) in &events {
                if e["type"] == "playback_camera" && e["tile"] == tile {
                    let mut s = e["state"].as_str().unwrap_or("?").to_owned();
                    if let Some(r) = e["reason"].as_str() {
                        let _ = write!(s, "({r})");
                    }
                    c.states.push(s);
                }
            }
            seg.cams.push(c);
        }
        if let Some((max, mean)) = spread(samples) {
            seg.spread_max_ms = Some(max);
            seg.spread_mean_ms = Some(mean);
        }
        let live = self.col.decodes_between(self.live, t0, t1).len();
        seg.live_fps = live as f64 / wall.as_secs_f64().max(0.001);
        for k in self.logs.between(t0, t1) {
            match k {
                LogKind::BufferStop => seg.buffer_stops += 1,
                LogKind::PlaysStarted => seg.plays_started += 1,
                LogKind::Warn(w) => seg.warnings.push(w),
            }
        }
        if let Ok((p, s)) = self.core.playback_position(self.playback) {
            seg.position_ms = p;
            seg.state = format!("{s:?}");
        }
        seg
    }
}

/// Spans per camera, in seconds, from `recordings_spans`: those of the sub
/// tier when every camera has some, else all of them.
fn read_spans(
    core: &Core,
    key: &str,
    channels: &[u8],
    from: i64,
    to: i64,
) -> Result<Vec<Vec<Span>>, String> {
    let cams: Vec<Value> = channels
        .iter()
        .map(|c| json!({"device": key, "channel": c}))
        .collect();
    let q = json!({"start": from, "end": to, "cameras": cams, "limit": 100_000});
    let a = core
        .recordings_spans(&q.to_string())
        .map_err(|e| format!("spans: {e:?}"))?;
    let v: Value = serde_json::from_str(&a).map_err(|e| e.to_string())?;
    let lanes = v["spans"].as_array().ok_or("spans: no lanes")?;
    let pick = |only_sub: bool| -> Vec<Vec<Span>> {
        channels
            .iter()
            .map(|c| {
                lanes
                    .iter()
                    .filter(|l| l["camera"]["channel"] == *c)
                    .flat_map(|l| l["spans"].as_array().cloned().unwrap_or_default())
                    .filter(|s| !only_sub || s["tier"] == "sub")
                    .filter_map(|s| Some((s["start"].as_i64()?, s["end"].as_i64()?)))
                    .collect()
            })
            .collect()
    };
    let sub = pick(true);
    Ok(if sub.iter().all(|s| !s.is_empty()) {
        sub
    } else {
        pick(false)
    })
}

fn new_scratch(cfg: &ProbeConfig) -> std::io::Result<PathBuf> {
    let parent = cfg
        .scratch_parent
        .clone()
        .unwrap_or_else(std::env::temp_dir);
    static RUNS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = RUNS.fetch_add(1, Ordering::SeqCst);
    let dir = parent.join(format!("nvrview-playback-probe-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Runs the probe. Blocks; call it from a blocking thread. `stop` is the
/// interrupt flag. Progress goes to `out`.
///
/// # Errors
/// A message for anything that stopped the run before the script (login,
/// index, no common recording, open).
pub fn run(
    cfg: &ProbeConfig,
    password: &str,
    logs: Arc<LogCounts>,
    stop: Arc<AtomicBool>,
    out: &mut dyn Write,
) -> Result<Report, String> {
    let started = Instant::now();
    let deadline = started + cfg.run_limit.min(RUN_LIMIT);
    let dir = new_scratch(cfg).map_err(|e| format!("scratch directory: {e}"))?;
    let col = Arc::new(Collector::default());
    let core = Arc::new(
        Core::new(
            CoreConfig {
                data_dir: Some(dir.clone()),
                ..CoreConfig::default()
            },
            col.clone(),
        )
        .map_err(|e| format!("core: {e:?}"))?,
    );
    let _ = col.core.set(Arc::downgrade(&core));
    let mut cleanup = Cleanup {
        core: core.clone(),
        playback: None,
        live: None,
        dir,
    };
    let r = run_inner(
        cfg,
        password,
        &mut cleanup,
        col,
        logs,
        stop,
        deadline,
        out,
        started,
    );
    // Close everything now so the report can say it was done.
    drop(cleanup);
    let mut report = r?;
    report.closed = true;
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn run_inner(
    cfg: &ProbeConfig,
    password: &str,
    cleanup: &mut Cleanup,
    col: Arc<Collector>,
    logs: Arc<LogCounts>,
    stop: Arc<AtomicBool>,
    deadline: Instant,
    out: &mut dyn Write,
    started: Instant,
) -> Result<Report, String> {
    let core = cleanup.core.clone();
    let say = |out: &mut dyn Write, s: String| {
        let _ = writeln!(out, "{s}");
        let _ = out.flush();
    };
    let now_s = cfg.now.to_naive_secs();
    let day0 = now_s.div_euclid(86_400) * 86_400;

    // Device, recordings, live tile.
    core.recordings_configure("{}")
        .map_err(|e| format!("recordings_configure: {e:?}"))?;
    let device = core
        .add_device(&cfg.host, cfg.port, &cfg.user, password)
        .map_err(|e| format!("add_device: {e:?}"))?;
    let key = core.device_key(device).map_err(|e| format!("{e:?}"))?;
    let chans: Vec<Value> = cfg.channels.iter().map(|c| json!({"channel": c})).collect();
    core.recordings_enable(
        &key,
        Some(&json!({"channels": chans, "utc_offset_s": cfg.utc_offset_s}).to_string()),
    )
    .map_err(|e| format!("recordings_enable: {e:?}"))?;
    let cams: Vec<Value> = cfg
        .channels
        .iter()
        .map(|c| json!({"device": key, "channel": c}))
        .collect();
    // Today, and yesterday when the recording may have started before
    // midnight.
    let mut days = vec![day0];
    if now_s - day0 < 3600 {
        days.push(day0 - 86_400);
    }
    let asked = Instant::now();
    core.recordings_want(&json!({"cameras": cams, "days": days}).to_string())
        .map_err(|e| format!("recordings_want: {e:?}"))?;
    let live_tile = core
        .open_tile(device, cfg.channels[0], StreamTier::Sub)
        .map_err(|e| format!("open_tile: {e:?}"))?;
    cleanup.live = Some(live_tile);

    let interrupted = |stop: &AtomicBool| stop.load(Ordering::SeqCst);

    // Wait for the index.
    let mut spans;
    loop {
        if interrupted(&stop) || Instant::now() >= deadline {
            return Ok(early(Outcome::Interrupted, started));
        }
        if asked.elapsed() > INDEX_WAIT {
            return Err("the recordings index did not finish in time".into());
        }
        let idle = core
            .recordings_status()
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .is_some_and(|v| {
                v["devices"].as_array().is_some_and(|d| {
                    d.iter().any(|x| {
                        x["device"] == key.as_str()
                            && x["state"] == "idle"
                            && x["pending_days"] == 0
                    })
                })
            });
        spans = read_spans(&core, &key, &cfg.channels, day0 - 86_400, day0 + 86_400)?;
        if idle && (spans.iter().all(|s| !s.is_empty()) || asked.elapsed() >= cfg.index_grace) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let index_ms = asked.elapsed().as_millis() as u64;
    let Some((start_s, pos_s, end_s)) = choose_window(&spans, now_s) else {
        return Err(format!(
            "no time at least {MIN_WINDOW_SECS} s long that all {} cameras recorded and that \
             ended {MIN_AGE_SECS} s before now",
            cfg.channels.len()
        ));
    };
    let live_first_ms = col
        .data
        .lock()
        .unwrap()
        .decodes
        .get(&live_tile)
        .and_then(|v| v.first())
        .map(|d| d.at.duration_since(started).as_millis() as u64);
    say(
        out,
        format!(
            "index ready after {index_ms} ms; common recording: {} cameras, {} s long, ends {} s \
             before now; opening {} s before its end",
            cfg.channels.len(),
            end_s - start_s,
            now_s - end_s,
            end_s - pos_s
        ),
    );

    // Open the playback paused at the position.
    let pcams: Vec<Value> = cfg
        .channels
        .iter()
        .map(|c| json!({"camera": {"device": key, "channel": c}, "tier": "sub"}))
        .collect();
    let open_at = Instant::now();
    let answer = core
        .playback_open(
            &json!({"start_ms": start_s * 1000, "end_ms": end_s * 1000,
                    "position_ms": pos_s * 1000, "rate": 1.0, "sync": "wait",
                    "cameras": pcams})
            .to_string(),
        )
        .map_err(|e| format!("playback_open: {e:?}"))?;
    let v: Value = serde_json::from_str(&answer).map_err(|e| e.to_string())?;
    let playback = v["playback"].as_u64().ok_or("no playback id")?;
    cleanup.playback = Some(playback);
    let tiles: Vec<u64> = v["tiles"]
        .as_array()
        .ok_or("no tiles")?
        .iter()
        .filter_map(Value::as_u64)
        .collect();

    // The live tile's baseline: its rate from its first picture to here.
    let mut run = Run {
        core: core.clone(),
        col: col.clone(),
        logs,
        stop,
        deadline,
        out,
        tiles,
        playback,
        live: live_tile,
    };
    let mut report = Report {
        outcome: Outcome::Completed,
        segments: Vec::new(),
        window_secs: end_s - pos_s,
        age_secs: now_s - end_s,
        index_ms,
        live_baseline_fps: 0.0,
        live_first_ms,
        elapsed_s: 0.0,
        closed: false,
    };
    {
        let live = col.decodes_between(live_tile, started, open_at);
        if let (Some(a), Some(b)) = (live.first(), live.last()) {
            let w = b.at.duration_since(a.at).as_secs_f64();
            if w > 0.5 {
                report.live_baseline_fps = (live.len() - 1) as f64 / w;
            }
        }
    }

    // Wait for the opening seek.
    let open_wait = open_at + OPEN_WAIT;
    let mut opened = None;
    let scale = cfg.scale;
    let hold = |secs: f64| Duration::from_secs_f64(secs * scale);
    let result: Result<(), Halt> = (|| {
        while Instant::now() < open_wait {
            run.check()?;
            if let Some(at) = run.seek_active_since(open_at) {
                opened = Some(at);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let Some(at) = opened else {
            return Err(Halt::Failed("the opening seek did not finish".into()));
        };
        // The opening segment: the pictures up to the target, paused.
        let mut seg = run.measure(
            "open (paused, exact seek)",
            1.0,
            open_at,
            Instant::now(),
            &[],
        );
        seg.seek_active_ms = Some(at.duration_since(open_at).as_millis() as u64);
        run.report_segment(&seg);
        run.keep(&mut report, seg);

        let steps: Vec<(&str, f64, f64, u8)> = vec![
            ("play 1x", 1.0, 20.0, 0),
            ("seek +60 s (exact), play", 1.0, 15.0, 1),
            ("rate 4x", 4.0, 15.0, 2),
            ("rate 16x", 16.0, 10.0, 3),
            ("rate 1x again", 1.0, 10.0, 4),
            ("pause", 1.0, 3.0, 5),
            ("resume", 1.0, 5.0, 6),
        ];
        for (name, rate, secs, kind) in steps {
            let seg = run.segment(name, rate, hold(secs), |core, pb| match kind {
                0 => core.playback_play(pb),
                1 => {
                    let (pos, _) = core.playback_position(pb)?;
                    core.playback_seek(pb, pos + 60_000, SeekMode::Exact)?;
                    core.playback_play(pb)
                }
                2..=4 => core.playback_set_rate(pb, rate),
                5 => core.playback_pause(pb),
                _ => core.playback_play(pb),
            })?;
            run.report_segment(&seg);
            run.keep(&mut report, seg);
        }
        Ok(())
    })();
    match result {
        Ok(()) => {}
        Err(Halt::Stopped) => report.outcome = Outcome::Interrupted,
        Err(Halt::TimeLimit) => report.outcome = Outcome::TimeLimit,
        Err(Halt::Failed(m)) => {
            let _ = writeln!(run.out, "step failed: {m}");
            report.outcome = Outcome::Interrupted;
            report.segments.push(Segment {
                name: format!("FAILED: {m}"),
                ..Segment::default()
            });
        }
    }
    report.elapsed_s = started.elapsed().as_secs_f64();
    let state = core.playback_position(playback).map(|(_, s)| s);
    let _ = writeln!(
        run.out,
        "closing: playback state {}",
        state.map_or("unknown".to_owned(), |s: PlaybackState| format!("{s:?}"))
    );
    Ok(report)
}

fn early(outcome: Outcome, started: Instant) -> Report {
    Report {
        outcome,
        segments: Vec::new(),
        window_secs: 0,
        age_secs: 0,
        index_ms: 0,
        live_baseline_fps: 0.0,
        live_first_ms: None,
        elapsed_s: started.elapsed().as_secs_f64(),
        closed: false,
    }
}

impl Run<'_> {
    fn keep(&self, report: &mut Report, seg: Segment) {
        report.segments.push(seg);
    }

    fn report_segment(&mut self, s: &Segment) {
        let _ = write!(self.out, "{}", format_segment(s));
        let _ = self.out.flush();
    }
}

/// One segment as report lines.
pub fn format_segment(s: &Segment) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "== {} (rate {}x, {:.1} s) ==", s.name, s.rate, s.wall_s);
    if let Some(ms) = s.seek_active_ms {
        let _ = writeln!(o, "  seek reported active after {ms} ms");
    }
    for (i, c) in s.cams.iter().enumerate() {
        let first = c.first_ms.map_or("none".to_owned(), |f| format!("{f} ms"));
        let _ = writeln!(
            o,
            "  camera {}: first picture {first}; {} released, {:.1}/s, media {:.2} s/s; \
             max gap {} ms, stalls {}; states [{}]",
            i + 1,
            c.frames,
            c.fps,
            c.media_speed,
            c.max_gap_ms,
            c.stalls,
            c.states.join(", ")
        );
    }
    if let (Some(max), Some(mean)) = (s.spread_max_ms, s.spread_mean_ms) {
        let _ = writeln!(
            o,
            "  sync spread (media time drift between cameras): max {max} ms, mean {mean} ms"
        );
    }
    let _ = writeln!(
        o,
        "  live tile {:.1}/s; engine: buffer stops {}, play starts {}, warnings {}{}; \
         end position state {}",
        s.live_fps,
        s.buffer_stops,
        s.plays_started,
        s.warnings.len(),
        if s.warnings.is_empty() {
            String::new()
        } else {
            format!(" ({})", s.warnings.join("; "))
        },
        s.state
    );
    o
}

/// The closing summary.
pub fn format_summary(r: &Report) -> String {
    let mut o = String::new();
    let _ = writeln!(o, "== summary ==");
    let _ = writeln!(
        o,
        "outcome: {:?} after {:.1} s; index ready after {} ms; recording ends {} s before now, \
         {} s of it ahead of the start position",
        r.outcome, r.elapsed_s, r.index_ms, r.age_secs, r.window_secs
    );
    let _ = writeln!(
        o,
        "live tile: first picture {} ms after the run began, {:.1}/s before the playback opened",
        r.live_first_ms.map_or("none".to_owned(), |m| m.to_string()),
        r.live_baseline_fps
    );
    let live: Vec<f64> = r.segments.iter().map(|s| s.live_fps).collect();
    if let (Some(lo), Some(hi)) = (
        live.iter().copied().reduce(f64::min),
        live.iter().copied().reduce(f64::max),
    ) {
        let _ = writeln!(o, "live tile during the playback: {lo:.1}/s to {hi:.1}/s");
    }
    let stops: usize = r.segments.iter().map(|s| s.buffer_stops).sum();
    let starts: usize = r.segments.iter().map(|s| s.plays_started).sum();
    let stalls: usize = r
        .segments
        .iter()
        .flat_map(|s| &s.cams)
        .map(|c| c.stalls)
        .sum();
    let bad: usize = r
        .segments
        .iter()
        .flat_map(|s| &s.cams)
        .flat_map(|c| &c.states)
        .filter(|st| {
            st.starts_with("late") || st.starts_with("no_recording") || st.starts_with("failed")
        })
        .count();
    let _ = writeln!(
        o,
        "buffer stops {stops}, play starts {starts}, stalls {stalls}, late/no_recording/failed \
         states {bad}"
    );
    let _ = writeln!(
        o,
        "playback closed and session closed: {}",
        if r.closed { "yes" } else { "no" }
    );
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_are_one_to_four_distinct() {
        assert_eq!(parse_channels("0,1,2,3"), Some(vec![0, 1, 2, 3]));
        assert_eq!(parse_channels("7"), Some(vec![7]));
        assert_eq!(parse_channels("1,1"), None);
        assert_eq!(parse_channels("0,1,2,3,4"), None);
        assert_eq!(parse_channels(""), None);
        assert_eq!(parse_channels("1,x"), None);
        assert_eq!(parse_channels("256"), None);
    }

    #[test]
    fn utc_offset_is_bounded() {
        assert_eq!(parse_utc_offset("-14400"), Some(-14400));
        assert_eq!(parse_utc_offset("3600"), Some(3600));
        assert_eq!(parse_utc_offset("90000"), None);
        assert_eq!(parse_utc_offset("four"), None);
    }

    #[test]
    fn window_is_the_latest_common_recording_old_enough() {
        let now = 100_000;
        // One camera has a hole at 50000; a file ending after the age
        // limit is clipped.
        let a = vec![(1_000, 99_900)];
        let b = vec![(1_000, 50_000), (50_010, 99_950)];
        let (start, pos, end) = choose_window(&[a, b], now).unwrap();
        assert_eq!(end, now - MIN_AGE_SECS);
        assert_eq!(start, 50_010);
        assert_eq!(pos, end - SCRIPT_MEDIA_SECS);
    }

    #[test]
    fn window_needs_a_long_enough_common_recording() {
        let a = vec![(1_000, 5_000)];
        let b = vec![(4_950, 9_000)];
        assert_eq!(choose_window(&[a, b], 100_000), None);
        // Recorded only within the age limit.
        let c = vec![(99_700, 99_900)];
        assert_eq!(choose_window(&[c], 100_000), None);
        // A short common window starts near its start.
        let d = vec![(10_000, 10_200)];
        let (s, p, e) = choose_window(&[d], 100_000).unwrap();
        assert_eq!((s, p, e), (10_000, 10_002, 10_200));
    }

    #[test]
    fn spread_is_the_drift_between_cameras() {
        let t = Instant::now();
        let d = |ms: u64| {
            Some(Dec {
                at: t,
                pts: ms * 1000,
            })
        };
        let samples = vec![
            Sample {
                cams: vec![None, d(0)],
            },
            Sample {
                cams: vec![d(5_000), d(900)],
            },
            Sample {
                cams: vec![d(6_000), d(1_950)],
            },
        ];
        // Anchored at the second sample: the cameras advance 1000 and 1050.
        assert_eq!(spread(&samples), Some((50, 25)));
        assert_eq!(spread(&samples[..1]), None);
    }

    #[test]
    fn camera_numbers_measure_rate_and_stalls() {
        let t0 = Instant::now();
        // 30 pictures per second at 1x, with one 1.5 s gap.
        let mut decs = Vec::new();
        for i in 0..90u64 {
            let gap = if i >= 45 { 1_500 } else { 0 };
            decs.push(Dec {
                at: t0 + Duration::from_millis(200 + i * 33 + gap),
                pts: 5_000_000 + i * 33_000 + gap * 1000,
            });
        }
        let c = cam_numbers(&decs, t0);
        assert_eq!(c.first_ms, Some(200));
        assert_eq!(c.stalls, 1);
        assert!(c.max_gap_ms >= 1_500);
        assert!((c.media_speed - 1.0).abs() < 0.05, "{}", c.media_speed);
        assert_eq!(cam_numbers(&[], t0).first_ms, None);
    }
}
