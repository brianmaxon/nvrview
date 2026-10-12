//! Edit decision lists and trims over a finished download
//! (docs/FFI-RECORDINGS.md, sections 7 and 7a): the owner side of
//! `nv_edit_compute`. The planners are [`recordings::edl`] and
//! [`recordings::export::trim`]; this reads what they need from the
//! download (the incident it was made for, each finished file's keyframes,
//! codec, and size) and returns the plan as JSON.
//!
//! - **The incident** (`follow` and `grid`) is the current incident with the
//!   download's key (edits to it apply); a download whose incident no longer
//!   exists is [`RecError::UnknownId`], and a range download has none
//!   ([`RecError::InvalidArgument`]). A `trim` needs no incident: it works
//!   on incident and range downloads alike.
//! - **Finished files**: cameras whose job is `done` and whose file can be
//!   read. A camera whose file cannot be read is left out, as one without
//!   a file; if none remains the answer is [`RecError::InvalidArgument`].
//! - **Time seam (B7)**: `file_start_ms` is the start of the camera's
//!   download range (for `follow` and `grid`,
//!   [`recordings::edl::fallback_file_start_ms`] of the current incident)
//!   until the first picture's keyframe-clock time replaces it.
//! - **`dest` and `share_safe`** apply to every kind. `dest` is where the
//!   suggested file goes (default: the download's folder); `share_safe`
//!   names sources `Camera A`, `Camera B`, ... in the order of the plan's
//!   sources and leaves the camera name out of labels. The plan itself keeps
//!   the files' paths, which the app needs to read them.

use super::{RecError, config, query};
use recordings::edl::{self, EdlError, FollowRequest, GridRequest, SourceFile};
use recordings::export::trim::{TrimRequest, plan_trim};
use recordings::export::{ExportError, join_dest, validate_dest};
use recordings::naming::neutral_camera_name;
use recordings::scheduler::{CameraProgress, CameraState, IncidentProgress};
use recordings::sync_samples::read_sync_samples;
use serde_json::Value;

/// Most sources one plan reads files for.
const MAX_SOURCES: usize = 16;

fn plan_error(_: EdlError) -> RecError {
    RecError::InvalidArgument
}

/// How an export planner's error is told to the caller.
pub(super) fn export_error(e: ExportError) -> RecError {
    if e.is_limit() {
        RecError::Limit
    } else if e.is_unknown() {
        RecError::UnknownId
    } else {
        RecError::InvalidArgument
    }
}

/// The finished, readable files of a download as planner sources;
/// `start_ms` says when a camera's file starts (`None` leaves it out).
pub(super) fn finished_sources(
    p: &IncidentProgress,
    start_ms: impl Fn(&CameraProgress) -> Option<i64>,
) -> Vec<SourceFile> {
    let mut sources: Vec<SourceFile> = Vec::new();
    for c in p
        .cameras
        .iter()
        .filter(|c| c.state == CameraState::Done)
        .take(MAX_SOURCES)
    {
        let Ok(s) = read_sync_samples(&c.output) else {
            continue;
        };
        let Some(v) = s.video else { continue };
        let Some(start) = start_ms(c) else { continue };
        sources.push(SourceFile {
            camera: c.camera.clone(),
            name: c.name.clone(),
            file: c.output.to_string_lossy().into_owned(),
            file_start_ms: start,
            duration_ms: i64::try_from(s.duration_ms).unwrap_or(i64::MAX),
            keyframes_ms: s
                .keyframes_ms
                .iter()
                .map(|&k| i64::try_from(k).unwrap_or(i64::MAX))
                .collect(),
            codec: v.codec.to_owned(),
            width: v.width,
            height: v.height,
            offset_ms: 0,
        });
    }
    sources
}

/// Applies `dest` and `share_safe` to a follow or grid plan.
fn finish(mut plan: Value, folder: &str, dest: Option<&str>, share_safe: bool) -> Value {
    if let Some(Value::String(f)) = plan.get("suggested_file") {
        let name = f.rsplit(['/', '\\']).next().unwrap_or(f).to_owned();
        plan["suggested_file"] = Value::from(join_dest(dest.unwrap_or(folder), &name));
    }
    if share_safe {
        plan["share_safe"] = Value::from(true);
        let count = plan["sources"].as_array().map_or(0, Vec::len);
        let names: Vec<String> = (0..count).map(neutral_camera_name).collect();
        if let Some(sources) = plan["sources"].as_array_mut() {
            for (s, n) in sources.iter_mut().zip(&names) {
                s["name"] = Value::from(n.as_str());
            }
        }
        if let Some(labels) = plan["labels"].as_array_mut() {
            for l in labels {
                let i = usize::try_from(l["source"].as_u64().unwrap_or(0)).unwrap_or(0);
                if !l["name"].is_null() {
                    l["name"] = Value::from(names.get(i).cloned().unwrap_or_default());
                }
            }
        }
    }
    plan
}

impl crate::Core {
    /// `nv_edit_compute`: an edit list or trim for a finished download, as
    /// JSON (sections 7 and 7a). `request` is `{"kind": "follow" | "grid" |
    /// "trim", "download": "<id>", ...}` with the members of
    /// [`FollowRequest`], [`GridRequest`], or [`TrimRequest`]; `in_ms` and
    /// `out_ms` trim a follow cut or a grid, and every kind takes `dest` and
    /// `share_safe`. Pure computation over the incident and its files; no
    /// device traffic. The files' headers are read on the caller's thread
    /// (bounded: only each file's `moov`).
    ///
    /// # Errors
    /// [`RecError::UnknownId`] for an unknown download,
    /// [`RecError::InvalidArgument`] for invalid JSON or values, an unknown
    /// `kind`, a `dest` that is not an absolute directory, a `follow` or
    /// `grid` of a range download, or when no camera has a finished,
    /// readable file or the plan has nothing to show,
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn edit_compute(&self, request: &str) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let m = config::request(request)?;
        let kind = m
            .get("kind")
            .and_then(Value::as_str)
            .ok_or(RecError::InvalidArgument)?
            .to_owned();
        let download = m
            .get("download")
            .and_then(Value::as_str)
            .ok_or(RecError::InvalidArgument)?
            .to_owned();
        if !matches!(kind.as_str(), "follow" | "grid" | "trim") {
            return Err(RecError::InvalidArgument);
        }
        let dest: Option<String> = match m.get("dest") {
            None | Some(Value::Null) => None,
            Some(Value::String(d)) => Some(
                validate_dest(d)
                    .map_err(|_| RecError::InvalidArgument)?
                    .to_owned(),
            ),
            Some(_) => return Err(RecError::InvalidArgument),
        };
        let share_safe = config::flag(&m, "share_safe")?;
        let body = Value::Object(m);
        let bad = |_| RecError::InvalidArgument;
        let follow: Option<FollowRequest> = (kind == "follow")
            .then(|| serde_json::from_value(body.clone()))
            .transpose()
            .map_err(bad)?;
        let grid: Option<GridRequest> = (kind == "grid")
            .then(|| serde_json::from_value(body.clone()))
            .transpose()
            .map_err(bad)?;
        let trim: Option<TrimRequest> = (kind == "trim")
            .then(|| serde_json::from_value(body))
            .transpose()
            .map_err(bad)?;

        // The state lock is released before any file is read.
        let (p, inc) = {
            let mut st = h.st();
            let sched = h.scheduler(&st)?;
            let p = h
                .progress_of(&sched, &download)
                .ok_or(RecError::UnknownId)?;
            if trim.is_some() {
                (p, None)
            } else if p.range.is_some() {
                return Err(RecError::InvalidArgument);
            } else {
                h.fresh_incidents(&mut st);
                let inc = query::find(&st, &p.key)
                    .cloned()
                    .ok_or(RecError::UnknownId)?;
                (p, Some(inc))
            }
        };

        let sources = finished_sources(&p, |c| match &inc {
            Some(inc) => edl::fallback_file_start_ms(inc, &c.camera),
            None => Some(c.start.saturating_mul(1000)),
        });
        if sources.is_empty() {
            return Err(RecError::InvalidArgument);
        }
        let folder = p.folder.to_string_lossy().into_owned();
        let out = if let Some(t) = trim {
            let plan = plan_trim(&t, &sources).map_err(export_error)?;
            serde_json::to_string(&plan)
        } else {
            let inc = inc.expect("follow and grid have an incident");
            let plan = if let Some(r) = follow {
                let plan = edl::plan_follow(&download, &inc, &sources, &r).map_err(plan_error)?;
                serde_json::to_value(plan)
            } else {
                let r = grid.expect("one of the three kinds");
                let plan = edl::plan_grid(&download, &inc, &sources, &r).map_err(plan_error)?;
                serde_json::to_value(plan)
            }
            .map_err(|_| RecError::Internal)?;
            serde_json::to_string(&finish(plan, &folder, dest.as_deref(), share_safe))
        };
        out.map_err(|_| RecError::Internal)
    }
}
