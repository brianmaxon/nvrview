//! Export (docs/FFI-RECORDINGS.md, section 7a): the owner side of
//! `nv_download_range` and `nv_still_plan`. The planners are
//! [`recordings::export`]; the scheduler runs the range jobs
//! ([`recordings::scheduler::Scheduler::enqueue_range`]).
//!
//! - **Range download.** The cameras must be allowed by
//!   `recordings_enable` ([`RecError::InvalidArgument`] otherwise). Camera
//!   names are the user's (group names, else reported names); the events
//!   inside the range come from the indexes of the devices, none for days
//!   not indexed. A `dest` that is not the downloads directory needs a
//!   data directory for the registry ([`RecError::NotConfigured`]).
//! - **Stills from a playback** use the newest picture of each tile's shown
//!   generation that the app answered with `nv_tile_decoded`; a generation
//!   that is not shown yet (a seek in progress) has none.
//! - **Stills from a file** read the finished file's headers on the
//!   caller's thread, as `nv_edit_compute` does.

use super::edit::{export_error, finished_sources};
use super::{RecError, camera_name, config, downloads, sound};
use recordings::export::range::{RangeRequest, plan_range};
use recordings::export::still::{
    FileStillRequest, PlaybackStillRequest, ShownPicture, plan_file_still, plan_playback_stills,
};
use recordings::incident::{CameraEvent, CameraId};
use recordings::index::EventQuery;
use serde_json::{Value, json};
use std::collections::BTreeMap;

impl crate::Core {
    /// `nv_download_range`: queues the download of a time range of chosen
    /// cameras (section 7a, "Range download") and returns
    /// `{"download": "<id>", "folder": "<path>", "existing": bool}`. The
    /// same cameras, tiers, and range queue nothing new.
    ///
    /// # Errors
    /// [`RecError::NotConfigured`] without `downloads_dir` (or without a
    /// data directory when `dest` is elsewhere), [`RecError::Limit`] beyond
    /// 16 cameras, 24 hours, or 1000 known downloads,
    /// [`RecError::InvalidArgument`] for invalid JSON or values, an empty
    /// range, or a camera not allowed by `recordings_enable`,
    /// [`RecError::Internal`] if the folder cannot be written,
    /// [`RecError::Closed`].
    pub fn download_range(&self, request: &str) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        config::request(request)?;
        let req: RangeRequest =
            serde_json::from_str(request).map_err(|_| RecError::InvalidArgument)?;
        let (sched, job, events) = {
            let st = h.st();
            let sched = h.scheduler(&st)?;
            let stores = st.stores.as_ref().expect("configured");
            let reported = h.reported();
            let mut names: BTreeMap<CameraId, String> = BTreeMap::new();
            for c in &req.cameras {
                sound::check_camera(&st, &c.camera.device, c.camera.local())?;
                names.insert(
                    c.camera.clone(),
                    camera_name(&stores.groups, &reported, &c.camera),
                );
            }
            let job = plan_range(&req, &names, None).map_err(export_error)?;
            let mut events: Vec<CameraEvent> = Vec::new();
            for key in job
                .cameras
                .iter()
                .map(|c| c.camera.device.as_str())
                .collect::<std::collections::BTreeSet<_>>()
            {
                let Some(dev) = st.devices.get(key) else {
                    continue;
                };
                let q = EventQuery {
                    cameras: job
                        .cameras
                        .iter()
                        .filter(|c| c.camera.device == key)
                        .map(|c| c.camera.local())
                        .collect(),
                    kinds: Vec::new(),
                    start: job.start,
                    end: job.end,
                };
                events.extend(super::lock(&dev.index).events(&q).into_iter().map(|e| {
                    CameraEvent {
                        start: e.start,
                        end: e.end,
                        camera: CameraId::with_lens(key, e.channel, e.lens),
                        kind: e.kind,
                    }
                }));
            }
            (sched, job, events)
        };
        let r = sched
            .enqueue_range(&job, &events)
            .map_err(downloads::enqueue_error)?;
        Ok(
            json!({"download": r.id, "folder": r.folder.to_string_lossy(),
                  "existing": r.existing})
            .to_string(),
        )
    }

    /// `nv_still_plan`: which picture to save and what to call it
    /// (section 7a, "Still images"). `{"playback": <id>, "tiles": [...],
    /// "format", "dest", "share_safe"}` plans the pictures playback tiles
    /// show; `{"download": "<id>", "camera": {...}, "at_ms", "exact",
    /// "format", "dest", "share_safe"}` plans one from a finished file.
    /// Answers from memory (a file's headers are read).
    ///
    /// # Errors
    /// [`RecError::UnknownId`] for an unknown playback, tile, or download,
    /// [`RecError::InvalidArgument`] for invalid JSON or values, a time
    /// outside the file, a tile that has shown nothing, or no finished file,
    /// [`RecError::NotConfigured`] (a file still needs the downloads),
    /// [`RecError::Closed`].
    pub fn still_plan(&self, request: &str) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let m = config::request(request)?;
        let bad = |_| RecError::InvalidArgument;
        match (m.contains_key("playback"), m.contains_key("download")) {
            (true, false) => {
                let playback = m
                    .get("playback")
                    .and_then(Value::as_u64)
                    .ok_or(RecError::InvalidArgument)?;
                let req: PlaybackStillRequest =
                    serde_json::from_value(Value::Object(m)).map_err(bad)?;
                let tiles = self.playback_shown(playback)?;
                let shown: Vec<ShownPicture> = {
                    let st = h.st();
                    let reported = h.reported();
                    tiles
                        .into_iter()
                        .map(|t| {
                            let camera =
                                CameraId::with_lens(&t.device, t.camera.channel, t.camera.lens);
                            let name = st.stores.as_ref().map_or_else(
                                || format!("Camera {}", u32::from(t.camera.channel) + 1),
                                |s| camera_name(&s.groups, &reported, &camera),
                            );
                            ShownPicture {
                                tile: u32::try_from(t.tile).unwrap_or(u32::MAX),
                                camera,
                                name,
                                generation: u64::from(t.generation),
                                position_ms: t.position_ms,
                            }
                        })
                        .collect()
                };
                let default_dir = {
                    let st = h.st();
                    st.stores
                        .as_ref()
                        .and_then(|s| s.settings.downloads_dir.as_ref())
                        .map(|d| d.to_string_lossy().into_owned())
                };
                let plan = plan_playback_stills(&req, &shown, default_dir.as_deref())
                    .map_err(export_error)?;
                serde_json::to_string(&plan).map_err(|_| RecError::Internal)
            }
            (false, true) => {
                let req: FileStillRequest =
                    serde_json::from_value(Value::Object(m)).map_err(bad)?;
                let p = {
                    let st = h.st();
                    let sched = h.scheduler(&st)?;
                    h.progress_of(&sched, &req.download)
                        .ok_or(RecError::UnknownId)?
                };
                let sources = finished_sources(&p, |c| Some(c.start.saturating_mul(1000)));
                let plan = plan_file_still(&req, &sources).map_err(export_error)?;
                serde_json::to_string(&plan).map_err(|_| RecError::Internal)
            }
            _ => Err(RecError::InvalidArgument),
        }
    }
}
