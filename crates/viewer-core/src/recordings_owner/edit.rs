//! Edit decision lists over a finished download (docs/FFI-RECORDINGS.md,
//! section 7): the owner side of `nv_edit_compute`. The planners are
//! [`recordings::edl`]; this reads what they need from the download (the
//! incident it was made for, each finished file's keyframes, codec, and
//! size) and returns the plan as JSON.
//!
//! - **The incident** is the current incident with the download's key
//!   (edits to it apply); a download whose incident no longer exists is
//!   [`RecError::UnknownId`].
//! - **Finished files**: cameras whose job is `done` and whose file can be
//!   read. A camera whose file cannot be read is left out, as one without
//!   a file; if none remains the answer is [`RecError::InvalidArgument`].
//! - **Time seam (B7)**: `file_start_ms` is
//!   [`recordings::edl::fallback_file_start_ms`] (the download range's
//!   start) until the first picture's keyframe-clock time replaces it.
//! - **Not in this call yet (F4)**: `in_ms`/`out_ms` and the `trim` kind
//!   answer [`RecError::Unsupported`].

use super::{RecError, config, query};
use recordings::edl::{self, EdlError, FollowRequest, GridRequest, SourceFile};
use recordings::scheduler::CameraState;
use recordings::sync_samples::read_sync_samples;
use serde_json::Value;

/// Most sources one plan reads files for.
const MAX_SOURCES: usize = 16;

fn plan_error(_: EdlError) -> RecError {
    RecError::InvalidArgument
}

impl crate::Core {
    /// `nv_edit_compute`: an edit list for a finished download, as JSON
    /// (section 7). `request` is `{"kind": "follow" | "grid", "download":
    /// "<id>", ...}` with the members of [`FollowRequest`] or
    /// [`GridRequest`]. Pure computation over the incident and its files;
    /// no device traffic. The files' headers are read on the caller's
    /// thread (bounded: only each file's `moov`).
    ///
    /// # Errors
    /// [`RecError::UnknownId`] for an unknown download,
    /// [`RecError::InvalidArgument`] for invalid JSON or values, an unknown
    /// `kind`, or when no camera has a finished, readable file or the plan
    /// has nothing to show, [`RecError::Unsupported`] for `in_ms`, `out_ms`,
    /// and the `trim` kind (export, F4), [`RecError::NotConfigured`],
    /// [`RecError::Closed`].
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
        match kind.as_str() {
            "follow" | "grid" => {}
            "trim" => return Err(RecError::Unsupported),
            _ => return Err(RecError::InvalidArgument),
        }
        if m.contains_key("in_ms") || m.contains_key("out_ms") {
            return Err(RecError::Unsupported);
        }
        let body = Value::Object(m);
        let follow: Option<FollowRequest> = (kind == "follow")
            .then(|| serde_json::from_value(body.clone()))
            .transpose()
            .map_err(|_| RecError::InvalidArgument)?;
        let grid: Option<GridRequest> = (kind == "grid")
            .then(|| serde_json::from_value(body))
            .transpose()
            .map_err(|_| RecError::InvalidArgument)?;

        // The state lock is released before any file is read.
        let (p, inc) = {
            let mut st = h.st();
            let sched = h.scheduler(&st)?;
            let p = h
                .progress_of(&sched, &download)
                .ok_or(RecError::UnknownId)?;
            h.fresh_incidents(&mut st);
            let inc = query::find(&st, &p.key)
                .cloned()
                .ok_or(RecError::UnknownId)?;
            (p, inc)
        };

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
            let Some(start) = edl::fallback_file_start_ms(&inc, &c.camera) else {
                continue;
            };
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
        if sources.is_empty() {
            return Err(RecError::InvalidArgument);
        }
        let out = if let Some(r) = follow {
            let plan = edl::plan_follow(&download, &inc, &sources, &r).map_err(plan_error)?;
            serde_json::to_string(&plan)
        } else {
            let r = grid.expect("one of the two kinds");
            let plan = edl::plan_grid(&download, &inc, &sources, &r).map_err(plan_error)?;
            serde_json::to_string(&plan)
        };
        out.map_err(|_| RecError::Internal)
    }
}
