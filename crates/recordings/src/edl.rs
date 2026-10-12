//! Edit decision lists over a downloaded incident (docs/FFI-RECORDINGS.md,
//! section 7): the follow cut, and 2-up and 4-up grids.
//!
//! The planners are pure: the same incident, sources, and request always
//! give the same plan, with no clock and no I/O. The caller describes each
//! finished file as a [`SourceFile`] (its keyframes come from
//! [`crate::sync_samples`]); the plan holds no pixels, only which source is
//! shown when, so the apps can render and encode it.
//!
//! # Time
//!
//! All times are device-local milliseconds, the units of the index and of
//! the incident's events (seconds there). A source's picture at file
//! position `p` is at `file_start_ms + offset_ms + p`; `offset_ms` is the
//! user's correction for clocks that disagree.
//!
//! **Seam (B7):** `file_start_ms` must be on the same clock as the events.
//! Whether the keyframes' wall-clock seconds are the device's local time or
//! UTC is not established ([`crate::recorded_time`]), so the planners do not
//! convert; the caller passes [`fallback_file_start_ms`] (the download
//! range's start, which is on the index's local clock) until that is
//! settled, or the first picture's time once it is.
//!
//! # Follow cut
//!
//! Section 7 gives: the camera "with the action" at each moment, overlaps
//! resolved by `priority` and then by the event that started last, no shot
//! shorter than `min_hold_ms`, each switch `pre_roll_ms` earlier, and with
//! `cut` `keyframe` every switch on the incoming source's next keyframe.
//! Where it is silent, these rules apply:
//!
//! - **Range.** From the earliest to the latest event of cameras that have
//!   a source, each event clipped to its source's footage. The download
//!   margin is not included.
//! - **No usable event** (none, or none inside any file's footage) is an
//!   error, not a guess. Cameras added to an incident without events never
//!   take the action; they only appear in grids.
//! - **Footage.** A camera can be chosen only where its file has video, so
//!   a file that starts late is not chosen before it starts.
//! - **Gaps.** Where no event is active (events of one incident may be up
//!   to the grouping gap apart), the previous shot continues if its source
//!   has video there; otherwise the plan has a gap (no segment; the app
//!   shows black). Segments are in time order and may leave gaps.
//! - **Minimum hold.** A shot shorter than `min_hold_ms` is absorbed, the
//!   shortest first, into the shot before it, or into the one after it if
//!   it is the first, provided the absorbing source has video over the
//!   span; a shot that cannot be absorbed stays. A range shorter than the
//!   minimum is one short shot.
//! - **Pre-roll.** A switch moves earlier by `pre_roll_ms`, but never so
//!   far that the shot before it is shorter than `min_hold_ms`, and never
//!   before the incoming file has video.
//! - **Keyframe cuts.** The start of every shot (including the first, so
//!   the plan's start can be later than the first event) moves to the first
//!   keyframe of its source at or after it; the shot before it extends to
//!   there. A shot with no keyframe before its end is dropped and the shot
//!   before it continues. A source with no keyframes is not used at all
//!   with `keyframe` cuts (with `frame` cuts it is).
//! - **`copy_possible`** is true for `keyframe` cuts when every source used
//!   has the same codec and picture size. (Equal parameter sets are not
//!   compared here; the apps check them when they open the files.)
//!
//! # Grids
//!
//! `2up` is two cells side by side (stacked when the canvas is taller than
//! wide), `4up` is two by two, and `auto` is one cell for one camera, 2up
//! for two, and 4up for three or four. Cameras are taken in the order of
//! the request (else the incident's), skipping those without a usable
//! file, and any beyond the cells are left out. Cells tile the canvas
//! exactly, with even widths and heights where the canvas allows. The
//! source rectangle inside a cell comes from the same layout code as live
//! view in the app (`fit` says how); the cell is the whole available area.
//! The range is the incident with its download margin, limited to where
//! some chosen file has video; `present` lists where each file does.

use crate::Secs;
use crate::incident::{CameraId, Incident};
use crate::naming::{NamePattern, camera_file_name};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

/// The version of the plan JSON.
pub const PLAN_VERSION: u32 = 1;
/// Largest canvas side accepted.
pub const MAX_CANVAS: u32 = 16_384;

/// Why no plan could be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdlError {
    /// No source file was given.
    NoSources,
    /// The incident has no events.
    NoEvents,
    /// Sources exist but none has a keyframe, and `keyframe` cuts need one.
    NoKeyframes,
    /// No event or requested time falls inside any source's footage.
    NoCoverage,
    /// The canvas is empty or beyond [`MAX_CANVAS`].
    InvalidCanvas,
    /// `in_ms` is not before `out_ms`.
    InvalidWindow,
}

impl fmt::Display for EdlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EdlError::NoSources => "no camera has a finished file",
            EdlError::NoEvents => "the incident has no events",
            EdlError::NoKeyframes => "no source has a keyframe to cut on",
            EdlError::NoCoverage => "no source has video where the incident happened",
            EdlError::InvalidCanvas => "the canvas size is not usable",
            EdlError::InvalidWindow => "the in point is not before the out point",
        })
    }
}

impl std::error::Error for EdlError {}

/// One camera's finished file, as the planners need it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    /// The camera.
    pub camera: CameraId,
    /// Its display name.
    pub name: String,
    /// The file (as the app should open it).
    pub file: String,
    /// Device-local milliseconds of the file's first picture (see the
    /// module documentation, "Time").
    pub file_start_ms: i64,
    /// Length of the file's video in milliseconds.
    pub duration_ms: i64,
    /// Keyframe times as milliseconds from the file's first picture (from
    /// [`crate::sync_samples::SyncSamples::keyframes_ms`]).
    pub keyframes_ms: Vec<i64>,
    /// Video codec name, such as `h264`.
    pub codec: String,
    /// Picture width.
    pub width: u32,
    /// Picture height.
    pub height: u32,
    /// The user's clock correction for this camera, in milliseconds.
    #[serde(default)]
    pub offset_ms: i64,
}

/// The start of a camera's file when its first picture's time is not
/// known: the start of the incident's download range for the camera, in
/// milliseconds on the index's clock (see the module documentation, "Time").
pub fn fallback_file_start_ms(inc: &Incident, camera: &CameraId) -> Option<i64> {
    inc.download_ranges()
        .get(camera)
        .map(|r| r.0.saturating_mul(1000))
}

/// How a switch between sources is cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cut {
    /// On the incoming source's next keyframe (can be written without
    /// re-encoding when the sources agree).
    #[default]
    Keyframe,
    /// Exactly where planned (needs a re-encode).
    Frame,
}

/// Which captions the app burns in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Labels {
    /// The camera's name.
    #[serde(default)]
    pub name: bool,
    /// The time of the picture.
    #[serde(default)]
    pub time: bool,
}

impl Default for Labels {
    fn default() -> Self {
        Labels {
            name: true,
            time: true,
        }
    }
}

fn default_min_hold() -> i64 {
    2000
}

fn default_pre_roll() -> i64 {
    1000
}

/// A follow-cut request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowRequest {
    /// No shot shorter than this (milliseconds; default 2000).
    #[serde(default = "default_min_hold")]
    pub min_hold_ms: i64,
    /// Each switch happens this much earlier (milliseconds; default 1000).
    #[serde(default = "default_pre_roll")]
    pub pre_roll_ms: i64,
    /// How switches are cut (default `keyframe`).
    #[serde(default)]
    pub cut: Cut,
    /// Cameras that win an overlap, first first.
    #[serde(default)]
    pub priority: Vec<CameraId>,
    /// Captions.
    #[serde(default)]
    pub labels: Labels,
    /// Trim the cut to start here (device-local milliseconds). With a
    /// `keyframe` cut the start moves back to the keyframe at or before it
    /// (never before the shot's own start).
    #[serde(default)]
    pub in_ms: Option<i64>,
    /// Trim the cut to end here (exclusive; exact in both cuts).
    #[serde(default)]
    pub out_ms: Option<i64>,
}

impl Default for FollowRequest {
    fn default() -> Self {
        FollowRequest {
            min_hold_ms: default_min_hold(),
            pre_roll_ms: default_pre_roll(),
            cut: Cut::Keyframe,
            priority: Vec::new(),
            labels: Labels::default(),
            in_ms: None,
            out_ms: None,
        }
    }
}

/// The grid layouts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GridKind {
    /// One cell for one camera, two for two, four for three or four.
    #[default]
    #[serde(rename = "auto")]
    Auto,
    /// Two cells.
    #[serde(rename = "2up")]
    TwoUp,
    /// Four cells, two by two.
    #[serde(rename = "4up")]
    FourUp,
}

/// How a picture sits in its cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// The whole picture, with bars where the shapes differ.
    #[default]
    Fit,
    /// The cell filled, cropping the picture.
    Fill,
}

/// The output size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Canvas {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas {
            width: 1920,
            height: 1080,
        }
    }
}

/// A user's correction for one camera's clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraOffset {
    /// The camera.
    pub camera: CameraId,
    /// Milliseconds added to its recorded times.
    pub offset_ms: i64,
}

/// A grid request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridRequest {
    /// The layout.
    #[serde(default)]
    pub layout: GridKind,
    /// The output size.
    #[serde(default)]
    pub canvas: Canvas,
    /// How pictures sit in cells.
    #[serde(default)]
    pub fit: Fit,
    /// The cameras in cell order; the incident's when empty.
    #[serde(default)]
    pub cameras: Vec<CameraId>,
    /// Clock corrections; they replace the sources' own `offset_ms`.
    #[serde(default)]
    pub offsets_ms: Vec<CameraOffset>,
    /// Trim the grid to start here (device-local milliseconds).
    #[serde(default)]
    pub in_ms: Option<i64>,
    /// Trim the grid to end here (exclusive).
    #[serde(default)]
    pub out_ms: Option<i64>,
}

/// A source as the plan lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSource {
    /// Index used by segments, cells, and labels.
    pub source: usize,
    /// The camera.
    pub camera: CameraId,
    /// Its display name.
    pub name: String,
    /// The file.
    pub file: String,
    /// Device-local milliseconds of the file's first picture.
    pub file_start_ms: i64,
    /// The clock correction applied.
    pub offset_ms: i64,
    /// Video codec name.
    pub codec: String,
    /// Picture width.
    pub width: u32,
    /// Picture height.
    pub height: u32,
}

/// One stretch of a follow cut.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    /// Start (device-local milliseconds).
    pub start_ms: i64,
    /// End (device-local milliseconds).
    pub end_ms: i64,
    /// The source shown.
    pub source: usize,
    /// Where in the source file the stretch starts, in milliseconds.
    pub source_in_ms: i64,
}

/// Captions for one source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanLabel {
    /// The source.
    pub source: usize,
    /// The name to show; absent when names are off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Show the time.
    pub time: bool,
}

/// A follow cut.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowPlan {
    /// [`PLAN_VERSION`].
    pub version: u32,
    /// `follow`.
    pub kind: String,
    /// The download this was computed for.
    pub download: String,
    /// Start of the first segment.
    pub start_ms: i64,
    /// End of the last segment.
    pub end_ms: i64,
    /// A file name for the output (the app puts it in the incident's
    /// folder).
    pub suggested_file: String,
    /// Whether the edit can be written without re-encoding.
    pub copy_possible: bool,
    /// The sources used, in order of first use.
    pub sources: Vec<PlanSource>,
    /// What is shown, in time order (gaps possible).
    pub segments: Vec<Segment>,
    /// Captions.
    pub labels: Vec<PlanLabel>,
}

/// A rectangle in canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    /// Left.
    pub x: u32,
    /// Top.
    pub y: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// One cell of a grid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// Where the cell is.
    pub rect: Rect,
    /// The source in it.
    pub source: usize,
    /// How its picture sits.
    pub fit: Fit,
    /// Where the source has video, as `[start_ms, end_ms]` pairs (black
    /// elsewhere).
    pub present: Vec<[i64; 2]>,
}

/// A 2-up or 4-up grid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GridPlan {
    /// [`PLAN_VERSION`].
    pub version: u32,
    /// `grid`.
    pub kind: String,
    /// The layout used: `1up` (a single camera under `auto`), `2up`, or
    /// `4up`.
    pub layout: String,
    /// The download this was computed for.
    pub download: String,
    /// Start of the range.
    pub start_ms: i64,
    /// End of the range.
    pub end_ms: i64,
    /// A file name for the output.
    pub suggested_file: String,
    /// Always false: a grid is re-encoded.
    pub copy_possible: bool,
    /// The sources used.
    pub sources: Vec<PlanSource>,
    /// The output size.
    pub canvas: Canvas,
    /// The cells; each runs for the whole range.
    pub cells: Vec<Cell>,
}

/// A source with its keyframes sorted, ready to plan with.
struct Src<'a> {
    file: &'a SourceFile,
    start: i64,
    end: i64,
    keys: Vec<i64>,
}

impl Src<'_> {
    fn covers(&self, a: i64, b: i64) -> bool {
        self.start <= a && b <= self.end
    }

    /// The first keyframe at or after `t` (device-local milliseconds).
    fn next_key(&self, t: i64) -> Option<i64> {
        let i = self.keys.partition_point(|&k| k < t);
        self.keys.get(i).copied()
    }
}

fn prepare<'a>(
    sources: &'a [SourceFile],
    keyframes_needed: bool,
    offsets: &[CameraOffset],
) -> Result<Vec<Src<'a>>, EdlError> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut without_keys = false;
    for f in sources {
        if f.duration_ms <= 0 || !seen.insert(f.camera.clone()) {
            continue;
        }
        let offset = offsets
            .iter()
            .rev()
            .find(|o| o.camera == f.camera)
            .map_or(f.offset_ms, |o| o.offset_ms);
        let base = f.file_start_ms.saturating_add(offset);
        let mut keys: Vec<i64> = f
            .keyframes_ms
            .iter()
            .filter(|&&k| k >= 0 && k <= f.duration_ms)
            .map(|&k| base.saturating_add(k))
            .collect();
        keys.sort_unstable();
        keys.dedup();
        if keyframes_needed && keys.is_empty() {
            without_keys = true;
            continue;
        }
        out.push(Src {
            file: f,
            start: base,
            end: base.saturating_add(f.duration_ms),
            keys,
        });
    }
    if out.is_empty() {
        return Err(if without_keys {
            EdlError::NoKeyframes
        } else {
            EdlError::NoSources
        });
    }
    Ok(out)
}

/// A shot in progress: a source (or a gap) over `[a, b)`.
#[derive(Clone, Copy, Debug)]
struct Shot {
    src: Option<usize>,
    a: i64,
    b: i64,
}

/// Joins neighbors that continue the same source.
fn merge_same(shots: &mut Vec<Shot>) {
    let mut out: Vec<Shot> = Vec::with_capacity(shots.len());
    for s in shots.drain(..) {
        match out.last_mut() {
            Some(p) if p.src == s.src && p.b == s.a => p.b = s.b,
            _ => out.push(s),
        }
    }
    *shots = out;
}

struct EventSpan {
    src: usize,
    a: i64,
    b: i64,
    started: i64,
}

/// Plans a follow cut of an incident. `download` is echoed into the plan.
/// See the module documentation for the rules.
pub fn plan_follow(
    download: &str,
    inc: &Incident,
    sources: &[SourceFile],
    req: &FollowRequest,
) -> Result<FollowPlan, EdlError> {
    let keyed = req.cut == Cut::Keyframe;
    let window = window(req.in_ms, req.out_ms)?;
    let srcs = prepare(sources, keyed, &[])?;
    if inc.events.is_empty() {
        return Err(EdlError::NoEvents);
    }
    let min_hold = req.min_hold_ms.max(0);
    let pre_roll = req.pre_roll_ms.max(0);

    // Each event, clipped to the footage of its camera's file.
    let mut spans: Vec<EventSpan> = Vec::new();
    for e in &inc.events {
        let Some(i) = srcs.iter().position(|s| s.file.camera == e.camera) else {
            continue;
        };
        let a = e.start.saturating_mul(1000).max(srcs[i].start);
        let b = e.end.saturating_mul(1000).min(srcs[i].end);
        if a < b {
            spans.push(EventSpan {
                src: i,
                a,
                b,
                started: e.start,
            });
        }
    }
    if spans.is_empty() {
        return Err(EdlError::NoCoverage);
    }

    // Slice time at every event edge and choose a source for each slice.
    let mut edges: Vec<i64> = spans.iter().flat_map(|s| [s.a, s.b]).collect();
    edges.sort_unstable();
    edges.dedup();
    let rank = |i: usize| {
        req.priority
            .iter()
            .position(|c| *c == srcs[i].file.camera)
            .unwrap_or(usize::MAX)
    };
    let mut shots: Vec<Shot> = Vec::new();
    for w in edges.windows(2) {
        let (a, b) = (w[0], w[1]);
        let best = spans
            .iter()
            .filter(|s| s.a <= a && b <= s.b)
            .min_by_key(|s| (rank(s.src), std::cmp::Reverse(s.started), s.src))
            .map(|s| s.src);
        let src = best.or_else(|| {
            // A gap: the previous shot goes on where its file has video.
            shots
                .last()
                .and_then(|p| p.src)
                .filter(|&p| srcs[p].covers(a, b))
        });
        shots.push(Shot { src, a, b });
    }
    merge_same(&mut shots);

    absorb_short_shots(&mut shots, &srcs, min_hold);
    pre_roll_switches(&mut shots, &srcs, min_hold, pre_roll);
    if keyed {
        snap_to_keyframes(&mut shots, &srcs);
    }
    shots.retain(|s| s.src.is_some() && s.a < s.b);
    clip_shots(&mut shots, &srcs, window, keyed);
    if shots.is_empty() {
        return Err(EdlError::NoCoverage);
    }

    // The sources used, in order of first use.
    let mut order: Vec<usize> = Vec::new();
    for s in &shots {
        if let Some(i) = s.src
            && !order.contains(&i)
        {
            order.push(i);
        }
    }
    let index_of = |i: usize| order.iter().position(|&o| o == i).unwrap_or(0);
    let segments: Vec<Segment> = shots
        .iter()
        .filter_map(|s| {
            let i = s.src?;
            Some(Segment {
                start_ms: s.a,
                end_ms: s.b,
                source: index_of(i),
                source_in_ms: s.a - srcs[i].start,
            })
        })
        .collect();
    let used: Vec<&Src> = order.iter().map(|&i| &srcs[i]).collect();
    let copy_possible = keyed
        && used.iter().all(|s| {
            s.file.codec == used[0].file.codec
                && s.file.width == used[0].file.width
                && s.file.height == used[0].file.height
        });
    let (start_ms, end_ms) = (segments[0].start_ms, segments[segments.len() - 1].end_ms);
    Ok(FollowPlan {
        version: PLAN_VERSION,
        kind: "follow".to_owned(),
        download: download.to_owned(),
        start_ms,
        end_ms,
        suggested_file: suggested_file(start_ms, "follow"),
        copy_possible,
        labels: used
            .iter()
            .enumerate()
            .map(|(n, s)| PlanLabel {
                source: n,
                name: req.labels.name.then(|| s.file.name.clone()),
                time: req.labels.time,
            })
            .collect(),
        sources: plan_sources(&used),
        segments,
    })
}

/// The `[in, out)` window of a request, `i64::MIN..i64::MAX` when open.
fn window(in_ms: Option<i64>, out_ms: Option<i64>) -> Result<(i64, i64), EdlError> {
    let (a, b) = (in_ms.unwrap_or(i64::MIN), out_ms.unwrap_or(i64::MAX));
    if a >= b {
        return Err(EdlError::InvalidWindow);
    }
    Ok((a, b))
}

/// Cuts shots to the window. A `keyed` shot whose start is cut moves back
/// to the last keyframe of its source at or before the window's start,
/// but never before the shot's own start (which is itself a keyframe).
fn clip_shots(shots: &mut Vec<Shot>, srcs: &[Src], (lo, hi): (i64, i64), keyed: bool) {
    if lo == i64::MIN && hi == i64::MAX {
        return;
    }
    shots.retain_mut(|s| {
        let mut a = s.a.max(lo);
        if keyed
            && a > s.a
            && let Some(i) = s.src
        {
            let keys = &srcs[i].keys;
            let k = keys.partition_point(|&k| k <= a);
            if let Some(&key) = k.checked_sub(1).and_then(|k| keys.get(k)) {
                a = key.max(s.a);
            }
        }
        let b = s.b.min(hi);
        s.a = a;
        s.b = b;
        a < b
    });
}

fn plan_sources(used: &[&Src]) -> Vec<PlanSource> {
    used.iter()
        .enumerate()
        .map(|(n, s)| PlanSource {
            source: n,
            camera: s.file.camera.clone(),
            name: s.file.name.clone(),
            file: s.file.file.clone(),
            file_start_ms: s.file.file_start_ms,
            offset_ms: s.start - s.file.file_start_ms,
            codec: s.file.codec.clone(),
            width: s.file.width,
            height: s.file.height,
        })
        .collect()
}

fn suggested_file(start_ms: i64, what: &str) -> String {
    let secs: Secs = start_ms.div_euclid(1000);
    camera_file_name(secs, what, &NamePattern::default())
}

/// Absorbs shots shorter than `min_hold`, shortest first, into a neighbor
/// whose source has video over the span.
fn absorb_short_shots(shots: &mut Vec<Shot>, srcs: &[Src], min_hold: i64) {
    loop {
        let mut short: Vec<usize> = (0..shots.len())
            .filter(|&i| shots[i].src.is_some() && shots[i].b - shots[i].a < min_hold)
            .collect();
        short.sort_by_key(|&i| (shots[i].b - shots[i].a, i));
        let mut done = false;
        for i in short {
            let s = shots[i];
            let before = i.checked_sub(1).map(|p| shots[p]);
            let after = shots.get(i + 1).copied();
            if let Some(p) =
                before.filter(|p| p.b == s.a && p.src.is_some_and(|x| srcs[x].covers(p.a, s.b)))
            {
                shots[i - 1].b = s.b;
                let _ = p;
            } else if let Some(n) =
                after.filter(|n| n.a == s.b && n.src.is_some_and(|x| srcs[x].covers(s.a, n.b)))
            {
                shots[i + 1].a = s.a;
                let _ = n;
            } else {
                continue;
            }
            shots.remove(i);
            done = true;
            break;
        }
        if !done {
            return;
        }
        merge_same(shots);
    }
}

/// Moves each switch earlier by `pre_roll`, keeping the earlier shot at
/// least `min_hold` long and inside the incoming file's footage.
fn pre_roll_switches(shots: &mut [Shot], srcs: &[Src], min_hold: i64, pre_roll: i64) {
    for i in 1..shots.len() {
        let (prev, cur) = (shots[i - 1], shots[i]);
        let Some(c) = cur.src else {
            continue;
        };
        if prev.b != cur.a {
            continue;
        }
        // A gap before the switch has no minimum length.
        let hold = if prev.src.is_some() { min_hold } else { 0 };
        let moved = (cur.a - pre_roll)
            .max(prev.a + hold)
            .max(srcs[c].start)
            .min(cur.a);
        shots[i - 1].b = moved;
        shots[i].a = moved;
    }
}

/// Moves the start of every shot to its source's next keyframe.
fn snap_to_keyframes(shots: &mut Vec<Shot>, srcs: &[Src]) {
    let mut out: Vec<Shot> = Vec::with_capacity(shots.len());
    for sh in shots.drain(..) {
        let Some(i) = sh.src else {
            out.push(sh);
            continue;
        };
        let contiguous = out.last().is_some_and(|p| p.b == sh.a);
        if contiguous && out.last().is_some_and(|p| p.src == Some(i)) {
            if let Some(p) = out.last_mut() {
                p.b = sh.b;
            }
            continue;
        }
        let key = srcs[i].next_key(sh.a).filter(|&k| k < sh.b);
        let prev_ok = |k: i64| {
            out.last()
                .is_none_or(|p| p.b != sh.a || p.src.is_none_or(|x| srcs[x].covers(p.a, k)))
        };
        match key {
            Some(k) if prev_ok(k) => {
                if contiguous && let Some(p) = out.last_mut() {
                    p.b = k;
                }
                out.push(Shot { a: k, ..sh });
            }
            _ => {
                // No keyframe to cut to: the shot before it goes on if it
                // can, otherwise there is a gap.
                if contiguous
                    && let Some(p) = out.last_mut()
                    && p.src.is_some_and(|x| srcs[x].covers(p.a, sh.b))
                {
                    p.b = sh.b;
                }
            }
        }
    }
    *shots = out;
}

fn even(n: u32) -> u32 {
    if n >= 2 { n & !1 } else { n }
}

/// Splits `total` into `parts` pieces that tile it, each but the last
/// even where possible.
fn split(total: u32, parts: u32) -> Vec<(u32, u32)> {
    let each = even(total / parts);
    (0..parts)
        .map(|i| {
            let at = each * i;
            let len = if i + 1 == parts { total - at } else { each };
            (at, len)
        })
        .collect()
}

/// Plans a grid of an incident's cameras. `download` is echoed into the
/// plan. See the module documentation for the rules.
pub fn plan_grid(
    download: &str,
    inc: &Incident,
    sources: &[SourceFile],
    req: &GridRequest,
) -> Result<GridPlan, EdlError> {
    let c = req.canvas;
    if c.width == 0 || c.height == 0 || c.width > MAX_CANVAS || c.height > MAX_CANVAS {
        return Err(EdlError::InvalidCanvas);
    }
    let (win_a, win_b) = window(req.in_ms, req.out_ms)?;
    if inc.events.is_empty() {
        return Err(EdlError::NoEvents);
    }
    let srcs = prepare(sources, false, &req.offsets_ms)?;
    let wanted: Vec<CameraId> = if req.cameras.is_empty() {
        inc.cameras().into_iter().collect()
    } else {
        req.cameras.clone()
    };
    let mut chosen: Vec<&Src> = Vec::new();
    for cam in &wanted {
        if let Some(s) = srcs.iter().find(|s| s.file.camera == *cam)
            && !chosen.iter().any(|x| x.file.camera == *cam)
        {
            chosen.push(s);
        }
    }
    if chosen.is_empty() {
        return Err(EdlError::NoSources);
    }
    let (layout, cols, rows) = match (req.layout, chosen.len()) {
        (GridKind::Auto, 1) => ("1up", 1, 1),
        (GridKind::Auto | GridKind::TwoUp, 1 | 2) | (GridKind::TwoUp, _) => {
            if c.width >= c.height {
                ("2up", 2, 1)
            } else {
                ("2up", 1, 2)
            }
        }
        _ => ("4up", 2, 2),
    };
    chosen.truncate((cols * rows) as usize);

    let margin = inc.margin.max(0);
    let want_a = inc.start().saturating_sub(margin).saturating_mul(1000);
    let want_b = inc.end().saturating_add(margin).saturating_mul(1000);
    let lo = chosen
        .iter()
        .map(|s| s.start)
        .min()
        .unwrap_or(0)
        .max(want_a)
        .max(win_a);
    let hi = chosen
        .iter()
        .map(|s| s.end)
        .max()
        .unwrap_or(0)
        .min(want_b)
        .min(win_b);
    if lo >= hi {
        return Err(EdlError::NoCoverage);
    }

    let xs = split(c.width, cols);
    let ys = split(c.height, rows);
    let cells = chosen
        .iter()
        .enumerate()
        .map(|(n, s)| {
            let (col, row) = (n as u32 % cols, n as u32 / cols);
            let (a, b) = (s.start.max(lo), s.end.min(hi));
            Cell {
                rect: Rect {
                    x: xs[col as usize].0,
                    y: ys[row as usize].0,
                    width: xs[col as usize].1,
                    height: ys[row as usize].1,
                },
                source: n,
                fit: req.fit,
                present: if a < b { vec![[a, b]] } else { Vec::new() },
            }
        })
        .collect();
    Ok(GridPlan {
        version: PLAN_VERSION,
        kind: "grid".to_owned(),
        layout: layout.to_owned(),
        download: download.to_owned(),
        start_ms: lo,
        end_ms: hi,
        suggested_file: suggested_file(lo, layout),
        copy_possible: false,
        sources: plan_sources(&chosen),
        canvas: c,
        cells,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::CameraEvent;
    use std::collections::BTreeSet;

    /// Event times are seconds past this (naive seconds).
    const T: Secs = 1_000_000;
    /// The same in milliseconds.
    const TM: i64 = T * 1000;

    fn cam(n: u8) -> CameraId {
        CameraId::new("device-a", n)
    }

    fn ev(c: u8, start: Secs, end: Secs) -> CameraEvent {
        CameraEvent {
            start: T + start,
            end: T + end,
            camera: cam(c),
            kind: "motion".into(),
        }
    }

    fn incident(events: Vec<CameraEvent>) -> Incident {
        Incident {
            id: "inc".into(),
            name: None,
            group: None,
            margin: 5,
            events,
            extra_cameras: BTreeSet::new(),
        }
    }

    /// A file that starts `start` ms after `T`, lasts `dur` ms, and has a
    /// keyframe every `gop` ms from its first picture.
    fn file(c: u8, start: i64, dur: i64, gop: i64) -> SourceFile {
        SourceFile {
            camera: cam(c),
            name: format!("Camera {c}"),
            file: format!("cam{c}.mp4"),
            file_start_ms: TM + start,
            duration_ms: dur,
            keyframes_ms: (0..=dur / gop).map(|i| i * gop).collect(),
            codec: "h264".into(),
            width: 1920,
            height: 1080,
            offset_ms: 0,
        }
    }

    fn follow(min_hold: i64, pre_roll: i64) -> FollowRequest {
        FollowRequest {
            min_hold_ms: min_hold,
            pre_roll_ms: pre_roll,
            ..FollowRequest::default()
        }
    }

    fn seg(p: &FollowPlan) -> Vec<(i64, i64, usize)> {
        p.segments
            .iter()
            .map(|s| (s.start_ms - TM, s.end_ms - TM, s.source))
            .collect()
    }

    #[test]
    fn no_events_and_no_sources_are_errors() {
        let inc = incident(vec![]);
        assert_eq!(
            plan_follow("d", &inc, &[file(0, 0, 10_000, 1000)], &follow(0, 0)).unwrap_err(),
            EdlError::NoEvents
        );
        let inc = incident(vec![ev(0, 0, 5)]);
        assert_eq!(
            plan_follow("d", &inc, &[], &follow(0, 0)).unwrap_err(),
            EdlError::NoSources
        );
        // A camera added without events never takes the action.
        let mut inc = incident(vec![ev(0, 0, 5)]);
        inc.extra_cameras.insert(cam(1));
        let p = plan_follow(
            "d",
            &inc,
            &[file(0, -1000, 20_000, 1000), file(1, -1000, 20_000, 1000)],
            &follow(0, 0),
        )
        .unwrap();
        assert_eq!(p.sources.len(), 1);
        // Events outside every file's footage.
        let inc = incident(vec![ev(0, 100, 105)]);
        assert_eq!(
            plan_follow("d", &inc, &[file(0, 0, 10_000, 1000)], &follow(0, 0)).unwrap_err(),
            EdlError::NoCoverage
        );
    }

    #[test]
    fn one_camera_is_one_segment_cut_on_a_keyframe() {
        let inc = incident(vec![ev(0, 2, 9), ev(0, 10, 14)]);
        // File starts 600 ms before T: keyframes at T-600, T+1400, ...
        let f = file(0, -600, 20_000, 2000);
        let p = plan_follow("dl-1", &inc, &[f], &follow(2000, 1000)).unwrap();
        // The start moves from T+2000 to the next keyframe, T+3400; the gap
        // between the events is held.
        assert_eq!(seg(&p), vec![(3400, 14_000, 0)]);
        assert_eq!(p.segments[0].source_in_ms, 4000);
        assert_eq!(p.download, "dl-1");
        assert_eq!((p.start_ms, p.end_ms), (TM + 3400, TM + 14_000));
        assert!(p.copy_possible);
        assert_eq!(p.kind, "follow");
        assert_eq!(p.sources[0].file_start_ms, TM - 600);
        assert_eq!(
            p.labels,
            vec![PlanLabel {
                source: 0,
                name: Some("Camera 0".into()),
                time: true
            }]
        );
        assert!(
            p.suggested_file.ends_with(" follow.mp4"),
            "{}",
            p.suggested_file
        );
    }

    #[test]
    fn a_later_event_takes_the_action_and_the_switch_moves_earlier() {
        // Camera 0 from 0 to 10 s, camera 1 from 5 s to 15 s.
        let inc = incident(vec![ev(0, 0, 10), ev(1, 5, 15)]);
        let files = [file(0, 0, 30_000, 1000), file(1, 0, 30_000, 1000)];
        let p = plan_follow("d", &inc, &files, &follow(2000, 1000)).unwrap();
        // The switch at 5 s moves to 4 s, a keyframe of camera 1 already.
        assert_eq!(seg(&p), vec![(0, 4000, 0), (4000, 15_000, 1)]);
        assert_eq!(p.segments[1].source_in_ms, 4000);
        assert_eq!(p.sources.len(), 2);
        assert_eq!(p.sources[1].camera, cam(1));
    }

    #[test]
    fn keyframe_cuts_wait_for_the_next_keyframe_frame_cuts_do_not() {
        let inc = incident(vec![ev(0, 0, 10), ev(1, 5, 15)]);
        let files = [file(0, 0, 30_000, 1000), file(1, 300, 30_000, 2000)];
        // Camera 1 has keyframes at T+300, 2300, 4300, 6300: a switch at 4 s
        // waits for 4300.
        let p = plan_follow("d", &inc, &files, &follow(2000, 1000)).unwrap();
        assert_eq!(seg(&p), vec![(0, 4300, 0), (4300, 15_000, 1)]);
        let mut req = follow(2000, 1000);
        req.cut = Cut::Frame;
        let p = plan_follow("d", &inc, &files, &req).unwrap();
        assert_eq!(seg(&p), vec![(0, 4000, 0), (4000, 15_000, 1)]);
        assert_eq!(p.segments[1].source_in_ms, 3700);
        assert!(!p.copy_possible);
    }

    #[test]
    fn priority_wins_an_overlap_before_the_latest_start() {
        let inc = incident(vec![ev(0, 0, 10), ev(1, 5, 15)]);
        let files = [file(0, 0, 30_000, 1000), file(1, 0, 30_000, 1000)];
        let mut req = follow(2000, 1000);
        req.priority = vec![cam(0)];
        let p = plan_follow("d", &inc, &files, &req).unwrap();
        // Camera 0 keeps the action through 10 s, then camera 1 takes the
        // rest; the switch at 10 s moves to 9 s.
        assert_eq!(seg(&p), vec![(0, 9000, 0), (9000, 15_000, 1)]);
    }

    #[test]
    fn a_blip_shorter_than_the_minimum_is_absorbed() {
        let inc = incident(vec![ev(0, 0, 10), ev(1, 4, 5)]);
        let files = [file(0, 0, 30_000, 1000), file(1, 0, 30_000, 1000)];
        let p = plan_follow("d", &inc, &files, &follow(2000, 1000)).unwrap();
        assert_eq!(seg(&p), vec![(0, 10_000, 0)]);
        assert_eq!(p.sources.len(), 1);
        // With no minimum the blip is its own shot; both of its switches
        // move earlier by the pre-roll.
        let p = plan_follow("d", &inc, &files, &follow(0, 1000)).unwrap();
        assert_eq!(
            seg(&p),
            vec![(0, 3000, 0), (3000, 4000, 1), (4000, 10_000, 0)]
        );
    }

    #[test]
    fn pre_roll_never_shortens_the_shot_before_below_the_minimum() {
        let inc = incident(vec![ev(0, 0, 3), ev(1, 3, 10)]);
        let files = [file(0, 0, 30_000, 500), file(1, 0, 30_000, 500)];
        let p = plan_follow("d", &inc, &files, &follow(2500, 2000)).unwrap();
        // 3000 - 2000 would leave 1000 ms; the shot keeps 2500.
        assert_eq!(seg(&p), vec![(0, 2500, 0), (2500, 10_000, 1)]);
    }

    #[test]
    fn a_gap_holds_the_previous_shot_if_its_file_has_video() {
        let inc = incident(vec![ev(0, 0, 4), ev(1, 8, 12)]);
        let files = [file(0, 0, 30_000, 1000), file(1, 0, 30_000, 1000)];
        let p = plan_follow("d", &inc, &files, &follow(1000, 1000)).unwrap();
        assert_eq!(seg(&p), vec![(0, 7000, 0), (7000, 12_000, 1)]);
        // Camera 0's file ends at 5 s: the rest of the gap is black.
        let files = [file(0, 0, 5000, 1000), file(1, 0, 30_000, 1000)];
        let p = plan_follow("d", &inc, &files, &follow(1000, 1000)).unwrap();
        assert_eq!(seg(&p), vec![(0, 4000, 0), (7000, 12_000, 1)]);
    }

    #[test]
    fn a_file_that_starts_late_is_not_chosen_before_it_starts() {
        // Camera 1 is the later event, but its file only starts at 6 s.
        let inc = incident(vec![ev(0, 0, 10), ev(1, 4, 12)]);
        let files = [file(0, 0, 30_000, 1000), file(1, 6000, 30_000, 1000)];
        let p = plan_follow("d", &inc, &files, &follow(1000, 1000)).unwrap();
        // The event is clipped to start at 6 s; the pre-roll cannot go
        // before the file starts.
        assert_eq!(seg(&p), vec![(0, 6000, 0), (6000, 12_000, 1)]);
        assert_eq!(p.segments[1].source_in_ms, 0);
    }

    #[test]
    fn a_source_without_keyframes_is_skipped_for_keyframe_cuts() {
        let inc = incident(vec![ev(0, 0, 10), ev(1, 4, 12)]);
        let mut bare = file(1, 0, 30_000, 1000);
        bare.keyframes_ms.clear();
        let files = [file(0, 0, 30_000, 1000), bare.clone()];
        let p = plan_follow("d", &inc, &files, &follow(1000, 1000)).unwrap();
        assert_eq!(seg(&p), vec![(0, 10_000, 0)]);
        // Frame cuts need none.
        let mut req = follow(1000, 1000);
        req.cut = Cut::Frame;
        let p = plan_follow("d", &inc, &files, &req).unwrap();
        assert_eq!(seg(&p), vec![(0, 3000, 0), (3000, 12_000, 1)]);
        // Only keyframe-less sources.
        assert_eq!(
            plan_follow("d", &inc, &[bare], &follow(1000, 1000)).unwrap_err(),
            EdlError::NoKeyframes
        );
    }

    #[test]
    fn a_shot_with_no_keyframe_before_its_end_is_dropped() {
        let inc = incident(vec![ev(0, 0, 10), ev(1, 4, 6), ev(0, 7, 12)]);
        let mut sparse = file(1, 0, 30_000, 1000);
        // Camera 1's only keyframe is far from its shot.
        sparse.keyframes_ms = vec![0, 20_000];
        let files = [file(0, 0, 30_000, 1000), sparse];
        let p = plan_follow("d", &inc, &files, &follow(1000, 0)).unwrap();
        assert_eq!(seg(&p), vec![(0, 12_000, 0)]);
    }

    #[test]
    fn copy_needs_matching_codec_and_size() {
        let inc = incident(vec![ev(0, 0, 10), ev(1, 5, 15)]);
        let mut other = file(1, 0, 30_000, 1000);
        let same = [file(0, 0, 30_000, 1000), other.clone()];
        assert!(
            plan_follow("d", &inc, &same, &follow(1000, 1000))
                .unwrap()
                .copy_possible
        );
        other.codec = "h265".into();
        let files = [file(0, 0, 30_000, 1000), other.clone()];
        assert!(
            !plan_follow("d", &inc, &files, &follow(1000, 1000))
                .unwrap()
                .copy_possible
        );
        other.codec = "h264".into();
        other.width = 1280;
        let files = [file(0, 0, 30_000, 1000), other];
        assert!(
            !plan_follow("d", &inc, &files, &follow(1000, 1000))
                .unwrap()
                .copy_possible
        );
    }

    #[test]
    fn offsets_move_a_source_on_the_timeline_and_plans_are_deterministic() {
        let inc = incident(vec![ev(0, 0, 10), ev(1, 5, 15)]);
        let mut late = file(1, 0, 30_000, 1000);
        late.offset_ms = 500;
        let files = [file(0, 0, 30_000, 1000), late];
        let a = plan_follow("d", &inc, &files, &follow(1000, 1000)).unwrap();
        let b = plan_follow("d", &inc, &files, &follow(1000, 1000)).unwrap();
        assert_eq!(a, b);
        // Camera 1's keyframes are at 500, 1500, ... so 4 s waits for 4500.
        assert_eq!(seg(&a), vec![(0, 4500, 0), (4500, 15_000, 1)]);
        assert_eq!(a.segments[1].source_in_ms, 4000);
        assert_eq!(a.sources[1].offset_ms, 500);
    }

    #[test]
    fn a_range_shorter_than_the_minimum_is_one_short_shot() {
        let inc = incident(vec![ev(0, 0, 1)]);
        let p = plan_follow("d", &inc, &[file(0, 0, 30_000, 500)], &follow(5000, 1000)).unwrap();
        assert_eq!(seg(&p), vec![(0, 1000, 0)]);
    }

    #[test]
    fn the_plan_serializes_as_documented() {
        let inc = incident(vec![ev(0, 0, 10)]);
        let p = plan_follow("dl", &inc, &[file(0, 0, 30_000, 1000)], &follow(0, 0)).unwrap();
        let v = serde_json::to_value(&p).unwrap();
        for key in [
            "version",
            "kind",
            "download",
            "start_ms",
            "end_ms",
            "suggested_file",
            "copy_possible",
            "sources",
            "segments",
            "labels",
        ] {
            assert!(v.get(key).is_some(), "{key}");
        }
        assert_eq!(v["sources"][0]["camera"]["lens"], 0);
        let req: FollowRequest = serde_json::from_str(r#"{"priority":[]}"#).unwrap();
        assert_eq!(req, FollowRequest::default());
        let g: GridRequest = serde_json::from_str(r#"{"layout":"4up","fit":"fill"}"#).unwrap();
        assert_eq!((g.layout, g.fit), (GridKind::FourUp, Fit::Fill));
    }

    fn four_cameras() -> (Incident, Vec<SourceFile>) {
        let inc = incident(vec![ev(0, 0, 10), ev(1, 2, 8), ev(2, 3, 9), ev(3, 4, 6)]);
        let files = (0..4).map(|n| file(n, -5000, 30_000, 1000)).collect();
        (inc, files)
    }

    fn rects(p: &GridPlan) -> Vec<(u32, u32, u32, u32)> {
        p.cells
            .iter()
            .map(|c| (c.rect.x, c.rect.y, c.rect.width, c.rect.height))
            .collect()
    }

    #[test]
    fn auto_picks_the_layout_from_the_camera_count() {
        let (inc, files) = four_cameras();
        let g = |n: usize| {
            plan_grid("d", &inc, &files[..n], &GridRequest::default())
                .unwrap()
                .layout
        };
        assert_eq!(g(1), "1up");
        assert_eq!(g(2), "2up");
        assert_eq!(g(3), "4up");
        assert_eq!(g(4), "4up");
        let p = plan_grid("d", &inc, &files[..3], &GridRequest::default()).unwrap();
        assert_eq!(p.cells.len(), 3);
        assert_eq!(
            rects(&p),
            vec![(0, 0, 960, 540), (960, 0, 960, 540), (0, 540, 960, 540)]
        );
        assert!(!p.copy_possible);
    }

    #[test]
    fn two_up_sits_side_by_side_or_stacked_and_tiles_the_canvas() {
        let (inc, files) = four_cameras();
        let mut req = GridRequest {
            layout: GridKind::TwoUp,
            ..GridRequest::default()
        };
        let p = plan_grid("d", &inc, &files, &req).unwrap();
        assert_eq!(p.cells.len(), 2);
        assert_eq!(rects(&p), vec![(0, 0, 960, 1080), (960, 0, 960, 1080)]);
        req.canvas = Canvas {
            width: 1080,
            height: 1921,
        };
        let p = plan_grid("d", &inc, &files, &req).unwrap();
        assert_eq!(rects(&p), vec![(0, 0, 1080, 960), (0, 960, 1080, 961)]);
        // Odd canvas: the last cell takes the remainder.
        req.layout = GridKind::FourUp;
        req.canvas = Canvas {
            width: 1001,
            height: 601,
        };
        let p = plan_grid("d", &inc, &files, &req).unwrap();
        assert_eq!(p.cells[3].rect.x + p.cells[3].rect.width, 1001);
        assert_eq!(p.cells[3].rect.y + p.cells[3].rect.height, 601);
        assert_eq!(p.cells[0].rect.width % 2, 0);
    }

    #[test]
    fn four_up_with_more_cameras_keeps_the_first_four_in_request_order() {
        let events: Vec<CameraEvent> = (0..6).map(|n| ev(n, 0, 10)).collect();
        let inc = incident(events);
        let files: Vec<SourceFile> = (0..6).map(|n| file(n, 0, 30_000, 1000)).collect();
        let req = GridRequest {
            layout: GridKind::FourUp,
            cameras: vec![cam(5), cam(4), cam(5), cam(3), cam(2), cam(1)],
            ..GridRequest::default()
        };
        let p = plan_grid("d", &inc, &files, &req).unwrap();
        let cams: Vec<u8> = p.sources.iter().map(|s| s.camera.channel).collect();
        assert_eq!(cams, vec![5, 4, 3, 2]);
        // Without a request order: the incident's.
        let p = plan_grid(
            "d",
            &inc,
            &files,
            &GridRequest {
                layout: GridKind::FourUp,
                ..GridRequest::default()
            },
        )
        .unwrap();
        let cams: Vec<u8> = p.sources.iter().map(|s| s.camera.channel).collect();
        assert_eq!(cams, vec![0, 1, 2, 3]);
    }

    #[test]
    fn cells_say_where_their_source_has_video() {
        let inc = incident(vec![ev(0, 0, 10), ev(1, 0, 10)]);
        // The incident's range with margin is -5 s to 15 s.
        let files = [file(0, -5000, 30_000, 1000), file(1, 3000, 4000, 1000)];
        let p = plan_grid("d", &inc, &files, &GridRequest::default()).unwrap();
        assert_eq!((p.start_ms - TM, p.end_ms - TM), (-5000, 15_000));
        assert_eq!(p.cells[0].present, vec![[TM - 5000, TM + 15_000]]);
        assert_eq!(p.cells[1].present, vec![[TM + 3000, TM + 7000]]);
        // A user's offset moves it.
        let req = GridRequest {
            offsets_ms: vec![CameraOffset {
                camera: cam(1),
                offset_ms: -1000,
            }],
            ..GridRequest::default()
        };
        let p = plan_grid("d", &inc, &files, &req).unwrap();
        assert_eq!(p.cells[1].present, vec![[TM + 2000, TM + 6000]]);
        assert_eq!(p.sources[1].offset_ms, -1000);
    }

    #[test]
    fn a_cell_outside_the_range_is_black() {
        let inc = incident(vec![ev(0, 0, 10), ev(1, 0, 10)]);
        let files = [file(0, -5000, 30_000, 1000), file(1, 100_000, 4000, 1000)];
        let p = plan_grid("d", &inc, &files, &GridRequest::default()).unwrap();
        assert!(p.cells[1].present.is_empty());
        assert_eq!(p.cells.len(), 2);
    }

    #[test]
    fn grid_errors() {
        let (inc, files) = four_cameras();
        let bad = |w, h| GridRequest {
            canvas: Canvas {
                width: w,
                height: h,
            },
            ..GridRequest::default()
        };
        assert_eq!(
            plan_grid("d", &inc, &files, &bad(0, 1080)).unwrap_err(),
            EdlError::InvalidCanvas
        );
        assert_eq!(
            plan_grid("d", &inc, &files, &bad(1920, MAX_CANVAS + 1)).unwrap_err(),
            EdlError::InvalidCanvas
        );
        assert_eq!(
            plan_grid("d", &inc, &[], &GridRequest::default()).unwrap_err(),
            EdlError::NoSources
        );
        assert_eq!(
            plan_grid("d", &incident(vec![]), &files, &GridRequest::default()).unwrap_err(),
            EdlError::NoEvents
        );
        let far = [file(0, 1_000_000, 1000, 500)];
        assert_eq!(
            plan_grid("d", &inc, &far, &GridRequest::default()).unwrap_err(),
            EdlError::NoCoverage
        );
        // Requested cameras without files.
        let req = GridRequest {
            cameras: vec![cam(9)],
            ..GridRequest::default()
        };
        assert_eq!(
            plan_grid("d", &inc, &files, &req).unwrap_err(),
            EdlError::NoSources
        );
    }

    #[test]
    fn the_fallback_start_is_the_download_range() {
        let inc = incident(vec![ev(0, 0, 10)]);
        assert_eq!(fallback_file_start_ms(&inc, &cam(0)), Some(TM - 5000));
        assert_eq!(fallback_file_start_ms(&inc, &cam(1)), None);
    }

    #[test]
    fn in_and_out_trim_a_follow_cut() {
        // Camera 0 from 0 to 10 s, camera 1 from 5 s to 15 s: shots
        // 0..4 s (camera 0) and 4..15 s (camera 1), keyframes every second.
        let inc = incident(vec![ev(0, 0, 10), ev(1, 5, 15)]);
        let files = [file(0, 0, 30_000, 1000), file(1, 0, 30_000, 1000)];
        let mut req = follow(2000, 1000);
        // Out only: exact, and camera 1 is dropped when the cut ends first.
        req.out_ms = Some(TM + 3500);
        let p = plan_follow("d", &inc, &files, &req).unwrap();
        assert_eq!(seg(&p), vec![(0, 3500, 0)]);
        assert_eq!(p.sources.len(), 1);
        // In inside the second shot: a keyframe cut starts on the keyframe
        // at or before it, not before the shot began; the first source is
        // gone.
        req.out_ms = None;
        req.in_ms = Some(TM + 6400);
        let p = plan_follow("d", &inc, &files, &req).unwrap();
        assert_eq!(seg(&p), vec![(6000, 15_000, 0)]);
        assert_eq!(p.sources[0].camera, cam(1));
        assert_eq!(p.segments[0].source_in_ms, 6000);
        assert!(p.copy_possible);
        // A frame cut starts exactly at the in point.
        req.cut = Cut::Frame;
        let p = plan_follow("d", &inc, &files, &req).unwrap();
        assert_eq!(seg(&p), vec![(6400, 15_000, 0)]);
        // A window across the switch keeps both shots, clipped.
        req.cut = Cut::Keyframe;
        req.in_ms = Some(TM + 2500);
        req.out_ms = Some(TM + 8000);
        let p = plan_follow("d", &inc, &files, &req).unwrap();
        assert_eq!(seg(&p), vec![(2000, 4000, 0), (4000, 8000, 1)]);
    }

    #[test]
    fn a_window_outside_the_cut_or_backwards_is_an_error() {
        let inc = incident(vec![ev(0, 0, 10)]);
        let files = [file(0, 0, 30_000, 1000)];
        let mut req = follow(2000, 1000);
        req.in_ms = Some(TM + 20_000);
        assert_eq!(
            plan_follow("d", &inc, &files, &req).unwrap_err(),
            EdlError::NoCoverage
        );
        req.in_ms = Some(TM + 5000);
        req.out_ms = Some(TM + 5000);
        assert_eq!(
            plan_follow("d", &inc, &files, &req).unwrap_err(),
            EdlError::InvalidWindow
        );
        let g = GridRequest {
            in_ms: Some(TM + 9),
            out_ms: Some(TM + 3),
            ..GridRequest::default()
        };
        assert_eq!(
            plan_grid("d", &inc, &files, &g).unwrap_err(),
            EdlError::InvalidWindow
        );
    }

    #[test]
    fn in_and_out_trim_a_grid() {
        let inc = incident(vec![ev(0, 0, 10), ev(1, 0, 10)]);
        let files = [file(0, 0, 30_000, 1000), file(1, 0, 30_000, 1000)];
        let full = plan_grid("d", &inc, &files, &GridRequest::default()).unwrap();
        let g = GridRequest {
            in_ms: Some(TM + 2000),
            out_ms: Some(TM + 6000),
            ..GridRequest::default()
        };
        let p = plan_grid("d", &inc, &files, &g).unwrap();
        assert_eq!((p.start_ms, p.end_ms), (TM + 2000, TM + 6000));
        assert!(p.start_ms > full.start_ms && p.end_ms < full.end_ms);
        assert!(
            p.cells
                .iter()
                .all(|c| c.present == vec![[TM + 2000, TM + 6000]])
        );
        let g = GridRequest {
            in_ms: Some(TM + 100_000),
            ..GridRequest::default()
        };
        assert_eq!(
            plan_grid("d", &inc, &files, &g).unwrap_err(),
            EdlError::NoCoverage
        );
    }
}
