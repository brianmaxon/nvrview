//! Downloads (docs/FFI-RECORDINGS.md, section 5): the scheduler, its
//! progress as `download` and `download_removed` events, and the calls
//! that control it.
//!
//! **Coalescing.** The scheduler publishes its whole progress list on every
//! change. A task per scheduler turns that into events: a download whose
//! derived state changed (its own state or paused flag, or any camera's
//! state or paused flag) is reported at once; one whose progress alone
//! changed (bytes, chunks, estimate) at most every 250 ms, with the newest
//! progress winning. A download that leaves the list is reported removed.
//!
//! **States** are derived here, not stored (section 5): a camera is
//! `waiting_device` while its job is queued or running but its device is
//! not added or not connected, `queued`, `running`, `finalizing` (chunks
//! complete, joining), `done`, or `failed`; a download is `done` when every
//! camera is, `failed` when some camera failed and none is left to work,
//! `paused` when it, or every camera left to work, is paused, `running`
//! while a camera downloads, `finalizing` while one joins, else `queued`.

use super::{Hub, RecError, camera_name, config, lock, query};
use crate::recordings_owner::config::DownloadSettings;
use bc_session::DeviceSession;
use recordings::finalize::Mp4Finalizer;
use recordings::groups::GroupStore;
use recordings::incident::{CameraId, Incident};
use recordings::scheduler::{
    CameraProgress, CameraSource, CameraState, EnqueueError, IncidentDownload, IncidentProgress,
    Scheduler, SchedulerConfig,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{info, warn};

/// Least time between two progress events of one download.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// The scheduler and its event task.
pub(super) struct Downloads {
    pub scheduler: Scheduler,
    dir: PathBuf,
    settings: DownloadSettings,
    pump: JoinHandle<()>,
}

impl Downloads {
    /// What it was made with (a change makes a new one).
    pub fn key(&self) -> (PathBuf, DownloadSettings) {
        (self.dir.clone(), self.settings.clone())
    }

    /// A scheduler writing under `dir`, serving the devices added now, its
    /// event task, and (once `old`, if any, has stopped) the resumption of
    /// unfinished downloads of earlier runs, off the caller's thread.
    pub fn start(
        h: &Arc<Hub>,
        dir: PathBuf,
        settings: DownloadSettings,
        sessions: &[(String, Arc<DeviceSession>)],
        old: Option<Downloads>,
    ) -> Downloads {
        let mut cfg = SchedulerConfig::new(&dir);
        cfg.chunk_secs = settings.chunk_s;
        cfg.per_device = settings.per_device;
        cfg.max_attempts = settings.max_attempts;
        cfg.pattern = settings.naming.clone();
        let scheduler = Scheduler::new(cfg, Some(Arc::new(Mp4Finalizer::default())));
        {
            let _rt = h.rt.enter();
            for (key, s) in sessions {
                scheduler.add_device(key, s.clone());
            }
        }
        let pump = h.rt.spawn(pump(Arc::downgrade(h), scheduler.clone()));
        let s = scheduler.clone();
        h.rt.spawn(async move {
            if let Some(old) = old {
                old.stop().await;
            }
            match tokio::task::spawn_blocking(move || s.resume_all()).await {
                Ok(Ok(0)) => {}
                Ok(Ok(n)) => info!(downloads = n, "unfinished downloads resumed"),
                Ok(Err(e)) => warn!(error = %e, "cannot resume downloads"),
                Err(e) => warn!(error = %e, "cannot resume downloads"),
            }
        });
        Downloads {
            scheduler,
            dir,
            settings,
            pump,
        }
    }

    /// Stops every download (job files keep the progress) and the events.
    pub async fn stop(self) {
        self.scheduler.shutdown().await;
        self.pump.abort();
    }
}

/// What was last reported of one download.
struct Sent {
    /// Derived states (a change is sent at once).
    states: String,
    /// The newest event body (sent, or waiting while `pending`).
    body: Value,
    last: Instant,
    pending: bool,
}

fn camera_state(c: &CameraProgress) -> &'static str {
    match &c.state {
        CameraState::Queued | CameraState::Running if c.waiting_device => "waiting_device",
        CameraState::Queued => "queued",
        CameraState::Running => "running",
        CameraState::ChunksComplete => "finalizing",
        CameraState::Done => "done",
        CameraState::Failed(_) => "failed",
    }
}

fn download_state(p: &IncidentProgress) -> &'static str {
    if p.cameras.iter().all(|c| c.state == CameraState::Done) {
        return "done";
    }
    let left: Vec<&CameraProgress> = p
        .cameras
        .iter()
        .filter(|c| !matches!(c.state, CameraState::Done | CameraState::Failed(_)))
        .collect();
    if left.is_empty() {
        "failed"
    } else if p.paused || left.iter().all(|c| c.paused) {
        "paused"
    } else if left
        .iter()
        .any(|c| c.state == CameraState::Running && !c.waiting_device)
    {
        "running"
    } else if left.iter().any(|c| c.state == CameraState::ChunksComplete) {
        "finalizing"
    } else {
        "queued"
    }
}

/// A download as `download` events and `nv_downloads_get` show it (without
/// `type`), and the derived states that make a change urgent.
fn download_value(p: &IncidentProgress) -> (Value, String) {
    let state = download_state(p);
    let mut states = format!("{state}:{}", p.paused);
    let cameras: Vec<Value> = p
        .cameras
        .iter()
        .map(|c| {
            let cs = camera_state(c);
            states.push_str(&format!("|{cs}:{}", c.paused));
            let mut v = json!({
                "camera": super::camera_json(&c.camera.device, c.camera.local()),
                "name": c.name, "state": cs, "paused": c.paused,
                "chunks_done": c.chunks_done, "chunks_total": c.chunks_total,
                "bytes": c.bytes, "bytes_estimate": c.bytes_estimate,
                "output": (c.state == CameraState::Done)
                    .then(|| c.output.to_string_lossy().into_owned()),
            });
            if let CameraState::Failed(f) = &c.state {
                v["reason"] = Value::from(f.reason.name());
                v["detail"] = Value::from(f.detail.clone());
            }
            v
        })
        .collect();
    let v = json!({
        "download": p.id, "incident": p.key, "folder": p.folder.to_string_lossy(),
        "state": state, "paused": p.paused, "cameras": cameras,
    });
    (v, states)
}

fn event(body: &Value) -> Value {
    let mut v = json!({"type": "download"});
    if let (Some(o), Some(b)) = (v.as_object_mut(), body.as_object()) {
        o.extend(b.iter().map(|(k, x)| (k.clone(), x.clone())));
    }
    v
}

impl Hub {
    /// Sends what is due of the progress list; returns when to look again.
    fn report_downloads(
        &self,
        list: &[IncidentProgress],
        sent: &mut HashMap<String, Sent>,
        now: Instant,
    ) -> Option<Instant> {
        let trashing = lock(&self.trashing).clone();
        let mut next: Option<Instant> = None;
        let mut at = |t: Instant| next = Some(next.map_or(t, |n: Instant| n.min(t)));
        for p in list {
            if trashing.contains_key(&p.id) {
                continue;
            }
            let (body, states) = download_value(p);
            match sent.get_mut(&p.id) {
                None => {
                    self.send(event(&body));
                    sent.insert(
                        p.id.clone(),
                        Sent {
                            states,
                            body,
                            last: now,
                            pending: false,
                        },
                    );
                }
                Some(s) => {
                    let due = now >= s.last + PROGRESS_INTERVAL;
                    if s.states != states || ((s.pending || s.body != body) && due) {
                        self.send(event(&body));
                        *s = Sent {
                            states,
                            body,
                            last: now,
                            pending: false,
                        };
                    } else if s.pending || s.body != body {
                        s.body = body;
                        s.pending = true;
                        at(s.last + PROGRESS_INTERVAL);
                    }
                }
            }
        }
        let present: HashSet<&str> = list.iter().map(|p| p.id.as_str()).collect();
        let mut gone: Vec<String> = sent
            .keys()
            .filter(|id| !present.contains(id.as_str()))
            .cloned()
            .collect();
        gone.extend(
            trashing
                .keys()
                .filter(|id| !present.contains(id.as_str()) && !sent.contains_key(*id))
                .cloned(),
        );
        for id in gone {
            sent.remove(&id);
            let folder = lock(&self.trashing).remove(&id);
            let mut v = json!({"type": "download_removed", "download": id,
                               "trash": folder.is_some()});
            if let Some(f) = folder {
                v["folder"] = Value::from(f.to_string_lossy().into_owned());
            }
            self.send(v);
        }
        next
    }

    /// The progress of a known download (not one being trashed).
    fn progress_of(&self, sched: &Scheduler, id: &str) -> Option<IncidentProgress> {
        if lock(&self.trashing).contains_key(id) {
            return None;
        }
        sched
            .progress()
            .borrow()
            .iter()
            .find(|p| p.id == id)
            .cloned()
    }
}

/// Turns the scheduler's progress into events (see the module
/// documentation).
async fn pump(hub: Weak<Hub>, sched: Scheduler) {
    let mut rx = sched.progress();
    let mut sent: HashMap<String, Sent> = HashMap::new();
    loop {
        let list = rx.borrow_and_update().clone();
        let Some(h) = hub.upgrade() else {
            return;
        };
        let next = h.report_downloads(&list, &mut sent, Instant::now());
        drop(h);
        tokio::select! {
            r = rx.changed() => if r.is_err() { return },
            _ = tokio::time::sleep_until(next.unwrap_or_else(Instant::now)), if next.is_some() => {}
        }
    }
}

/// The download of `inc`'s `chosen` cameras.
pub(super) fn incident_request(
    groups: &GroupStore,
    inc: Incident,
    chosen: Vec<(CameraId, recordings::Tier)>,
) -> IncidentDownload {
    let cameras = chosen
        .into_iter()
        .map(|(c, tier)| CameraSource {
            name: camera_name(groups, &c),
            uid: None,
            stream: c.local().stream(tier),
            camera: c,
        })
        .collect();
    IncidentDownload {
        incident: inc,
        cameras,
    }
}

pub(super) fn io_error(e: std::io::Error) -> RecError {
    warn!(error = %e, "download control failed");
    RecError::Internal
}

impl crate::Core {
    /// `nv_download_incident`: queues an incident's download
    /// (`{"incident": "<key>", "cameras": [{"camera": {...}, "tier":
    /// "main"}]}`, cameras optional) and returns `{"download": "<id>",
    /// "folder": "<path>", "existing": bool}`. A download of the same
    /// incident key that is not finished is returned instead, and so is a
    /// finished one of the unchanged incident (section 5, "Download IDs").
    ///
    /// # Errors
    /// [`RecError::NotConfigured`] without `downloads_dir`,
    /// [`RecError::UnknownId`] if the key matches no incident,
    /// [`RecError::Limit`] if 1000 downloads are known,
    /// [`RecError::InvalidArgument`] for a camera not in the incident,
    /// [`RecError::Internal`] if the folder cannot be written,
    /// [`RecError::Closed`].
    pub fn download_incident(&self, request: &str) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let m = config::request(request)?;
        let key = m
            .get("incident")
            .and_then(Value::as_str)
            .ok_or(RecError::InvalidArgument)?
            .to_owned();
        let wanted: Option<Vec<(CameraId, recordings::Tier)>> = match m.get("cameras") {
            None | Some(Value::Null) => None,
            Some(Value::Array(a)) => Some(
                a.iter()
                    .map(|c| {
                        let o = c.as_object().ok_or(RecError::InvalidArgument)?;
                        let (d, cam) =
                            config::camera(o.get("camera").ok_or(RecError::InvalidArgument)?)?;
                        let tier = config::tier(o, recordings::Tier::Main)?;
                        Ok((CameraId::with_lens(d, cam.channel, cam.lens), tier))
                    })
                    .collect::<Result<_, RecError>>()?,
            ),
            Some(_) => return Err(RecError::InvalidArgument),
        };
        let (sched, req) = {
            let mut st = h.st();
            let sched = h.scheduler(&st)?;
            h.fresh_incidents(&mut st);
            let inc = query::find(&st, &key).cloned().ok_or(RecError::UnknownId)?;
            let all = inc.cameras();
            let chosen: Vec<(CameraId, recordings::Tier)> = match wanted {
                None => all
                    .into_iter()
                    .map(|c| (c, recordings::Tier::Main))
                    .collect(),
                Some(w) => {
                    if w.iter().any(|(c, _)| !all.contains(c)) {
                        return Err(RecError::InvalidArgument);
                    }
                    w
                }
            };
            let groups = &st.stores.as_ref().expect("configured").groups;
            (sched, incident_request(groups, inc, chosen))
        };
        let r = sched.enqueue_incident(&req).map_err(|e| match e {
            EnqueueError::Limit => RecError::Limit,
            EnqueueError::NoSource => RecError::InvalidArgument,
            EnqueueError::Io(e) => io_error(e),
        })?;
        Ok(
            json!({"download": r.id, "folder": r.folder.to_string_lossy(),
                  "existing": r.existing})
            .to_string(),
        )
    }

    fn rec_download(&self, id: &str) -> Result<(Scheduler, IncidentProgress), RecError> {
        let h = self.rec();
        h.check()?;
        let sched = h.scheduler(&h.st())?;
        let p = h.progress_of(&sched, id).ok_or(RecError::UnknownId)?;
        Ok((sched, p))
    }

    fn rec_camera(p: &IncidentProgress, camera: &str) -> Result<CameraId, RecError> {
        let (d, c) = config::camera_text(camera)?;
        let id = CameraId::with_lens(d, c.channel, c.lens);
        if p.cameras.iter().any(|x| x.camera == id) {
            Ok(id)
        } else {
            Err(RecError::UnknownId)
        }
    }

    /// `nv_download_pause`: pauses or resumes a whole download (`camera`
    /// `None`) or one camera of it (a JSON camera object). A running chunk
    /// is stopped and fetched again on resume. Persisted.
    ///
    /// # Errors
    /// [`RecError::UnknownId`] for an unknown download or a camera not in
    /// it, [`RecError::InvalidArgument`], [`RecError::NotConfigured`],
    /// [`RecError::Internal`], [`RecError::Closed`].
    pub fn download_pause(
        &self,
        download: &str,
        camera: Option<&str>,
        paused: bool,
    ) -> Result<(), RecError> {
        let (sched, p) = self.rec_download(download)?;
        let found = match camera {
            None => sched.set_paused(download, paused),
            Some(c) => sched.set_camera_paused(download, &Self::rec_camera(&p, c)?, paused),
        }
        .map_err(io_error)?;
        if found {
            Ok(())
        } else {
            Err(RecError::UnknownId)
        }
    }

    /// `nv_download_retry`: queues failed camera jobs again with fresh
    /// attempts: every failed camera (`camera` `None`) or one. A camera
    /// that has not failed is left as it is.
    ///
    /// # Errors
    /// As [`crate::Core::download_pause`].
    pub fn download_retry(&self, download: &str, camera: Option<&str>) -> Result<(), RecError> {
        let (sched, p) = self.rec_download(download)?;
        match camera {
            None => sched.retry(download).map(|_| ()),
            Some(c) => sched
                .retry_camera(download, &Self::rec_camera(&p, c)?)
                .map(|_| ()),
        }
        .map_err(io_error)
    }

    /// `nv_download_forget`: forgets a download with no camera job running
    /// (pause it first). With `delete_partial`, its `.parts` folder is
    /// removed; finished files and `incident.json` are never deleted.
    /// `download_removed` follows.
    ///
    /// # Errors
    /// [`RecError::UnknownId`] for an unknown download,
    /// [`RecError::InvalidArgument`] while a camera job of it is running,
    /// [`RecError::NotConfigured`], [`RecError::Internal`],
    /// [`RecError::Closed`].
    pub fn download_forget(&self, download: &str, delete_partial: bool) -> Result<(), RecError> {
        let (sched, _) = self.rec_download(download)?;
        match sched.forget(download, delete_partial) {
            Ok(true) => Ok(()),
            Ok(false) => Err(RecError::InvalidArgument),
            Err(e) => Err(io_error(e)),
        }
    }

    /// `nv_download_trash`: stops a download's camera jobs and forgets it,
    /// and returns `{"folder": "<path>"}`. The core deletes nothing: the
    /// app moves the folder to the Trash after `download_removed` (with
    /// `"trash": true`), which follows once nothing writes into it.
    ///
    /// # Errors
    /// [`RecError::UnknownId`] for an unknown download,
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn download_trash(&self, download: &str) -> Result<String, RecError> {
        let (sched, p) = self.rec_download(download)?;
        let h = self.rec();
        lock(&h.trashing).insert(download.to_owned(), p.folder.clone());
        let id = download.to_owned();
        h.rt.spawn(async move {
            if let Err(e) = sched.trash(&id).await {
                warn!(error = %e, "cannot end a download for the Trash");
            }
        });
        Ok(json!({"folder": p.folder.to_string_lossy()}).to_string())
    }

    /// `nv_downloads_get`: every known download, `{"downloads": [...]}` as
    /// in the `download` event.
    ///
    /// # Errors
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn downloads_get(&self) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let sched = h.scheduler(&h.st())?;
        let trashing = lock(&h.trashing).clone();
        let list: Vec<Value> = sched
            .progress()
            .borrow()
            .iter()
            .filter(|p| !trashing.contains_key(&p.id))
            .map(|p| download_value(p).0)
            .collect();
        Ok(json!({"downloads": list}).to_string())
    }
}
