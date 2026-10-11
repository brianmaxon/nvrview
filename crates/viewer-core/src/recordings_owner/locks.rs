//! Locked incidents in the owner (docs/FFI-RECORDINGS.md, section 3,
//! "Locked incidents"; docs/MILESTONE-P.md, slice C13).
//!
//! The lock store ([`LockStore`]) lives with the edits and groups. After
//! every regroup and edit replay the owner matches it to the incidents
//! ([`Hub::follow_locks`]) and passes what the locks keep to the stores
//! that expire and evict: the days of every device's index
//! ([`recordings::index::DeviceIndex::set_kept_days`]) and the thumbnail
//! cache's pins. Edits of locked incidents never expire
//! ([`expire_edits`]), and a camera group change rewrites the keys of
//! locked incidents in the stored edits and locks ([`rekey`]).
//!
//! **Downloads.** Locking queues the incident's download as
//! `nv_download_incident` would (every camera, main tier). A growing
//! incident's key is marked in its lock instead; the regroup that finds it
//! no longer growing queues it (on a blocking task, as the scheduler writes
//! the job file). The pump looks again when the earliest of them can stop
//! growing ([`Hub::settle_timer`]), so nothing waits for new events.

use super::{Hub, RecError, State, Stores, downloads, lock, query};
use recordings::Secs;
use recordings::edits::{Edit, Edited};
use recordings::incident::{GroupingConfig, Incident, rekey as rekey_one};
use recordings::locks::{LockError, LockStore};
use recordings::scheduler::{EnqueueError, Scheduler};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{info, warn};

/// What the last matching found.
#[derive(Default)]
pub(super) struct Matched {
    /// Locked incidents' keys to their lock's id.
    pub locked: BTreeMap<String, u64>,
    /// The store revision whose kept days and pins were handed out.
    applied: Option<u64>,
    /// When a waiting download's incident may stop growing.
    settle_at: Option<Instant>,
}

/// The lock store under `path`, or in memory.
pub(super) fn open(path: Option<PathBuf>) -> LockStore {
    match path {
        Some(p) => LockStore::open(&p),
        None => LockStore::in_memory(),
    }
}

/// Drops expired edits, except those of locked incidents.
pub(super) fn expire_edits(stores: &mut Stores, oldest: Secs) {
    let locks = &stores.locks;
    let keep = |e: &Edit| e.keys().iter().any(|k| locks.covers_key(k));
    if let Err(e) = stores.edits.expire_except(oldest, keep) {
        warn!(error = %e, "cannot store the incident edits");
    }
}

/// After the groups changed from `before`: rewrites the keys of locked
/// incidents in the stored edits and in the locks, so a locked incident
/// keeps its edits.
pub(super) fn rekey(stores: &mut Stores, before: &GroupingConfig) {
    let after = stores
        .groups
        .grouping(stores.settings.gap_s, stores.settings.margin_s);
    if stores.locks.is_empty() {
        return;
    }
    let locks = &stores.locks;
    let in_edits = |k: &str| rekey_one(k, before, &after).filter(|_| locks.covers_key(k));
    match stores.edits.rewrite_keys(in_edits) {
        Ok(0) => {}
        Ok(n) => info!(keys = n, "locked incidents' edits follow their new keys"),
        Err(e) => warn!(error = %e, "cannot store the incident edits"),
    }
    if let Err(e) = stores.locks.rewrite_keys(|k| rekey_one(k, before, &after)) {
        warn!(error = %e, "cannot store the incident locks");
    }
}

/// Refuses a merge that would stretch a lock beyond 31 days.
pub(super) fn check_merge(st: &State, edit: &Edit) -> Result<(), RecError> {
    let Edit::Merge { keys } = edit else {
        return Ok(());
    };
    let stores = st.stores.as_ref().ok_or(RecError::NotConfigured)?;
    let parts: Vec<&Incident> = keys.iter().filter_map(|k| query::find(st, k)).collect();
    if stores.locks.merge_fits(&parts) {
        Ok(())
    } else {
        Err(RecError::Limit)
    }
}

/// Hands one device's kept days to its index (when it is enabled).
pub(super) fn keep_days(st: &State, device: &str) {
    let (Some(stores), Some(dev)) = (st.stores.as_ref(), st.devices.get(device)) else {
        return;
    };
    let days = stores.locks.kept_days().remove(device).unwrap_or_default();
    lock(&dev.index).set_kept_days(days);
}

fn lock_error(e: LockError) -> RecError {
    match e {
        LockError::Full | LockError::TooLong => RecError::Limit,
        LockError::Io(e) => {
            warn!(error = %e, "cannot store the incident locks");
            RecError::Internal
        }
    }
}

/// The device-local offset of the incident's first camera.
fn offset_of(st: &State, inc: &Incident) -> i64 {
    inc.events
        .first()
        .and_then(|e| st.devices.get(&e.camera.device))
        .map_or(0, |d| d.enable.utc_offset_s)
}

/// Every camera of the incident, main tier (what locking downloads).
fn whole(inc: &Incident) -> Vec<(recordings::incident::CameraId, recordings::Tier)> {
    inc.cameras()
        .into_iter()
        .map(|c| (c, recordings::Tier::Main))
        .collect()
}

/// Keys whose lock state differs.
fn changed_keys(a: &BTreeMap<String, u64>, b: &BTreeMap<String, u64>) -> Vec<String> {
    let mut v: Vec<String> = a
        .keys()
        .filter(|k| !b.contains_key(*k))
        .chain(b.keys().filter(|k| !a.contains_key(*k)))
        .cloned()
        .collect();
    v.sort();
    v
}

/// Queues a download off the caller's thread (the scheduler writes its job
/// file), for a locked incident that stopped growing.
fn enqueue_later(h: &Hub, sched: Scheduler, req: recordings::scheduler::IncidentDownload) {
    h.rt.spawn_blocking(move || match sched.enqueue_incident(&req) {
        Ok(r) => info!(
            existing = r.existing,
            "a locked incident's download is queued now that it stopped growing"
        ),
        Err(EnqueueError::Limit) => warn!("a locked incident's download refused: 1000 downloads"),
        Err(e) => warn!(error = ?e, "cannot queue a locked incident's download"),
    });
}

impl Hub {
    /// Matches the locks to the freshly edited incidents, hands what they
    /// keep to the index and thumbnail stores, and queues the downloads of
    /// locked incidents that stopped growing. Returns whether any
    /// incident's lock state changed.
    pub(super) fn follow_locks(&self, st: &mut State, edited: &Edited) -> bool {
        let Some(stores) = st.stores.as_mut() else {
            return false;
        };
        let locked = stores.locks.follow(&edited.incidents);
        let changed = locked != st.incidents.locks.locked;
        st.incidents.locks.locked = locked;
        let revision = stores.locks.revision();
        if st.incidents.locks.applied != Some(revision) {
            st.incidents.locks.applied = Some(revision);
            let days = stores.locks.kept_days();
            for (key, dev) in &st.devices {
                lock(&dev.index).set_kept_days(days.get(key).cloned().unwrap_or_default());
            }
            lock(&stores.thumbs).set_pins(stores.locks.pins());
        }
        self.settle(st, edited);
        changed
    }

    /// Queues the downloads waiting for their incidents to stop growing,
    /// and sets when to look again.
    fn settle(&self, st: &mut State, edited: &Edited) {
        let waiting: Vec<String> = st.stores.as_ref().map_or_else(Vec::new, |s| {
            s.locks
                .locks()
                .iter()
                .flat_map(|l| l.settle.iter().cloned())
                .collect()
        });
        let mut next: Option<Secs> = None;
        let mut ready: Vec<Incident> = Vec::new();
        for key in waiting {
            // Not there now (not indexed in this run): keep waiting.
            let Some(inc) = edited.incidents.iter().find(|i| i.key() == key) else {
                continue;
            };
            let stores = st.stores.as_ref().expect("checked");
            let now = self.device_now(offset_of(st, inc));
            if inc.is_growing(now, stores.settings.gap_s) {
                let left = inc.end() + stores.settings.gap_s + 1 - now;
                next = Some(next.map_or(left, |n| n.min(left)));
            } else {
                ready.push(inc.clone());
            }
        }
        let stores = st.stores.as_mut().expect("checked");
        for inc in ready {
            if let Err(e) = stores.locks.settle(&inc.key(), false) {
                warn!(error = %e, "cannot store the incident locks");
                continue;
            }
            match stores.downloads.as_ref().map(|d| d.scheduler.clone()) {
                Some(sched) => {
                    let req = downloads::incident_request(&stores.groups, inc.clone(), whole(&inc));
                    enqueue_later(self, sched, req);
                }
                None => info!("a locked incident stopped growing, but downloads are off"),
            }
        }
        st.incidents.locks.settle_at = next.map(|s| {
            let _rt = self.rt.enter();
            Instant::now() + Duration::from_secs(s.max(1).unsigned_abs())
        });
        if next.is_some() {
            // The pump sets its timer from this.
            self.wake.notify_one();
        }
    }

    /// For the pump: when a waiting download's incident may have stopped
    /// growing, marks the incidents for a regroup (which queues it);
    /// otherwise returns when to look.
    pub(super) fn settle_timer(&self, now: Instant) -> Option<Instant> {
        let mut st = self.st();
        match st.incidents.locks.settle_at {
            Some(t) if t <= now => {
                st.incidents.locks.settle_at = None;
                st.incidents.dirty = true;
                None
            }
            t => t,
        }
    }
}

impl crate::Core {
    /// `nv_incident_lock`: locks (`locked` true) or unlocks an incident by
    /// key (docs/FFI-RECORDINGS.md, section 3, "Locked incidents"); stores
    /// it, sends `incidents_changed` when an incident's lock state changed,
    /// and returns `{"revision": N, "keys": [...]}` with the keys whose
    /// lock state changed; when locking, also `"download"`, `"folder"`, and
    /// `"download_status"` (`queued`, `existing`, `when_settled`,
    /// `not_configured`, or `limit`). Locking an incident that is locked
    /// already changes no lock and runs the download step again.
    ///
    /// # Errors
    /// [`RecError::UnknownId`] if no incident has the key,
    /// [`RecError::Limit`] when locking beyond 1000 locks or 31 days,
    /// [`RecError::Internal`] if it cannot be stored (or the download's
    /// folder cannot be written; the lock is stored then),
    /// [`RecError::NotConfigured`], [`RecError::Closed`].
    pub fn incident_lock(&self, key: &str, locked: bool) -> Result<String, RecError> {
        let h = self.rec();
        h.check()?;
        let mut st = h.st();
        if st.stores.is_none() {
            return Err(RecError::NotConfigured);
        }
        h.fresh_incidents(&mut st);
        let inc = query::find(&st, key).cloned().ok_or(RecError::UnknownId)?;
        let before = st.incidents.locks.locked.clone();
        let growing = {
            let stores = st.stores.as_ref().expect("checked");
            inc.is_growing(h.device_now(offset_of(&st, &inc)), stores.settings.gap_s)
        };
        let stores = st.stores.as_mut().expect("checked");
        if !locked {
            stores.locks.unlock(&inc).map_err(lock_error)?;
            h.regroup(&mut st);
            let keys = changed_keys(&before, &st.incidents.locks.locked);
            return Ok(json!({"revision": st.incidents.revision, "keys": keys}).to_string());
        }
        stores.locks.lock(&inc).map_err(lock_error)?;
        let mut queue = None;
        let status = match stores.downloads.as_ref().map(|d| d.scheduler.clone()) {
            None => "not_configured",
            Some(_) if growing => {
                stores.locks.settle(key, true).map_err(lock_error)?;
                "when_settled"
            }
            Some(sched) => {
                let req = downloads::incident_request(&stores.groups, inc.clone(), whole(&inc));
                queue = Some((sched, req));
                "queued"
            }
        };
        h.regroup(&mut st);
        let keys = changed_keys(&before, &st.incidents.locks.locked);
        let revision = st.incidents.revision;
        drop(st);
        let mut out = json!({"revision": revision, "keys": keys, "download": null,
                             "folder": null, "download_status": status});
        if let Some((sched, req)) = queue {
            match sched.enqueue_incident(&req) {
                Ok(r) => {
                    out["download"] = Value::from(r.id);
                    out["folder"] = Value::from(r.folder.to_string_lossy().into_owned());
                    out["download_status"] =
                        Value::from(if r.existing { "existing" } else { "queued" });
                }
                Err(EnqueueError::Limit) => out["download_status"] = Value::from("limit"),
                Err(EnqueueError::NoSource) => return Err(RecError::Internal),
                Err(EnqueueError::Io(e)) => return Err(downloads::io_error(e)),
            }
        }
        Ok(out.to_string())
    }
}
