//! Verification evidence: the frames a check judged and every method's
//! detections, kept for a short time so a person can review why an alert was
//! or was not raised (docs/NOTIFICATIONS.md, "Evidence").
//!
//! Storage and expiry are the core's; the app writes the frames and the
//! detections (it runs the detector). A set is keyed by trigger ID, holds up
//! to [`MAX_FRAMES`] JPEG frames and a JSON document (the decision, the
//! camera AI's claim, our detector's boxes, anything else that ran). Sets
//! expire after the retention time (30 minutes to 24 hours, default 2
//! hours), and are also capped at [`MAX_SETS`] sets and [`MAX_BYTES`] bytes,
//! oldest first. Every function takes the time as an argument, so tests run
//! on a simulated clock.
//!
//! A pinned set leaves the short-lived store for `pinned/` beside it, where
//! it keeps the per-alert snapshot limits (7 days, 256 sets, 64 MiB).
//! Everything is local and is deleted when it expires; nothing here is sent
//! anywhere.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Frames per set.
pub(crate) const MAX_FRAMES: usize = 8;
/// One frame, in bytes.
pub(crate) const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// The detections document of a set, in bytes.
const MAX_DOC_BYTES: usize = 256 * 1024;
/// Sets kept.
pub(crate) const MAX_SETS: usize = 300;
/// All short-lived sets together.
pub(crate) const MAX_BYTES: u64 = 200 * 1024 * 1024;
pub(crate) const DEFAULT_RETENTION_MIN: u32 = 120;
const MIN_RETENTION_MIN: u32 = 30;
const MAX_RETENTION_MIN: u32 = 24 * 60;
const PIN_RETENTION_MS: i64 = 7 * 24 * 3600 * 1000;
const PIN_SETS: usize = 256;
const PIN_BYTES: u64 = 64 * 1024 * 1024;
/// Without a directory (tests, a core with no data directory), frames stay
/// in memory and the same bounds apply.
const META: &str = "meta.json";

struct Set {
    created_ms: i64,
    bytes: u64,
    pinned: bool,
    /// Frame index to (at_ms, bytes); the bytes live on disk or in `memory`.
    frames: BTreeMap<u32, (i64, u64)>,
    /// The document: `decision`, `methods`, and what the app put.
    doc: Value,
    memory: BTreeMap<u32, Vec<u8>>,
}

pub(crate) struct EvidenceStore {
    dir: Option<PathBuf>,
    retention_ms: i64,
    /// The caps on the short-lived sets (fields so tests can use small ones).
    max_sets: usize,
    max_bytes: u64,
    sets: BTreeMap<u64, Set>,
}

fn frame_name(i: u32) -> String {
    format!("{i}.jpg")
}

/// Clamps the Settings value (minutes) to what the store accepts.
pub(crate) fn clamp_retention_min(m: i64) -> u32 {
    u32::try_from(m.clamp(i64::from(MIN_RETENTION_MIN), i64::from(MAX_RETENTION_MIN)))
        .unwrap_or(DEFAULT_RETENTION_MIN)
}

/// Merges `patch` into `into`: objects member by member (so a method's
/// boxes for another frame add to the earlier ones), anything else replaced.
fn merge(into: &mut Value, patch: &Value) {
    match (into.as_object_mut(), patch.as_object()) {
        (Some(a), Some(b)) => {
            for (k, v) in b {
                match a.get_mut(k) {
                    Some(existing) => merge(existing, v),
                    None => {
                        a.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        _ => *into = patch.clone(),
    }
}

impl EvidenceStore {
    /// Opens (and prunes) the store under `dir/evidence` and
    /// `dir/evidence/pinned`; `None` keeps everything in memory.
    pub(crate) fn open(dir: Option<PathBuf>, now_ms: i64) -> EvidenceStore {
        let dir = dir.map(|d| d.join("evidence"));
        let dir = dir.filter(|d| fs::create_dir_all(d.join("pinned")).is_ok());
        let mut s = EvidenceStore {
            dir,
            retention_ms: i64::from(DEFAULT_RETENTION_MIN) * 60_000,
            max_sets: MAX_SETS,
            max_bytes: MAX_BYTES,
            sets: BTreeMap::new(),
        };
        s.scan(false);
        s.scan(true);
        s.prune(now_ms);
        s
    }

    fn set_dir(&self, id: u64, pinned: bool) -> Option<PathBuf> {
        let d = self.dir.as_ref()?;
        Some(if pinned {
            d.join("pinned").join(id.to_string())
        } else {
            d.join(id.to_string())
        })
    }

    fn scan(&mut self, pinned: bool) {
        let Some(root) = self.dir.clone() else { return };
        let root = if pinned { root.join("pinned") } else { root };
        let Ok(rd) = fs::read_dir(&root) else { return };
        for e in rd.filter_map(Result::ok) {
            let Some(id) = e.file_name().to_string_lossy().parse::<u64>().ok() else {
                continue;
            };
            let path = e.path();
            let doc: Value = fs::read_to_string(path.join(META))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(Value::Null);
            let created_ms = doc["created_ms"].as_i64().unwrap_or(0);
            let mut frames = BTreeMap::new();
            let mut bytes = 0;
            if let Ok(files) = fs::read_dir(&path) {
                for f in files.filter_map(Result::ok) {
                    let name = f.file_name().to_string_lossy().into_owned();
                    if let Some(i) = name
                        .strip_suffix(".jpg")
                        .and_then(|s| s.parse::<u32>().ok())
                    {
                        let len = f.metadata().map_or(0, |m| m.len());
                        let at = doc["frames"][i.to_string()].as_i64().unwrap_or(created_ms);
                        frames.insert(i, (at, len));
                        bytes += len;
                    }
                }
            }
            if doc.is_null() && frames.is_empty() {
                let _ = fs::remove_dir_all(&path);
                continue;
            }
            self.sets.insert(
                id,
                Set {
                    created_ms,
                    bytes,
                    pinned,
                    frames,
                    doc,
                    memory: BTreeMap::new(),
                },
            );
        }
    }

    /// Sets the retention (minutes, clamped) and prunes.
    pub(crate) fn set_retention_min(&mut self, minutes: u32, now_ms: i64) {
        self.retention_ms = i64::from(clamp_retention_min(i64::from(minutes))) * 60_000;
        self.prune(now_ms);
    }

    pub(crate) fn retention_min(&self) -> u32 {
        u32::try_from(self.retention_ms / 60_000).unwrap_or(DEFAULT_RETENTION_MIN)
    }

    fn entry(&mut self, id: u64, now_ms: i64) -> &mut Set {
        self.sets.entry(id).or_insert_with(|| Set {
            created_ms: now_ms,
            bytes: 0,
            pinned: false,
            frames: BTreeMap::new(),
            doc: json!({"trigger_id": id, "created_ms": now_ms}),
            memory: BTreeMap::new(),
        })
    }

    /// Stores one frame (replacing the same index). False when it cannot be
    /// written or the set already has [`MAX_FRAMES`] other frames.
    pub(crate) fn put_frame(
        &mut self,
        id: u64,
        index: u32,
        at_ms: i64,
        jpeg: &[u8],
        now_ms: i64,
    ) -> bool {
        self.prune(now_ms);
        let dir_of = self.set_dir(id, false);
        let set = self.entry(id, now_ms);
        if !set.frames.contains_key(&index) && set.frames.len() >= MAX_FRAMES {
            return false;
        }
        let old = set.frames.get(&index).map_or(0, |f| f.1);
        let dir = if set.pinned {
            return false;
        } else {
            dir_of
        };
        match &dir {
            Some(d) => {
                if fs::create_dir_all(d).is_err()
                    || fs::write(d.join(frame_name(index)), jpeg).is_err()
                {
                    return false;
                }
            }
            None => {
                set.memory.insert(index, jpeg.to_vec());
            }
        }
        set.bytes = set.bytes.saturating_sub(old) + jpeg.len() as u64;
        set.frames.insert(index, (at_ms, jpeg.len() as u64));
        set.doc["frames"][index.to_string()] = json!(at_ms);
        self.write_doc(id);
        self.prune(now_ms);
        true
    }

    /// Merges the members of `patch` (an object) into the set's document,
    /// creating the set if needed. False for a document that is too large,
    /// not an object, or for a pinned set.
    pub(crate) fn merge_doc(&mut self, id: u64, patch: &Value, now_ms: i64) -> bool {
        let Some(obj) = patch.as_object() else {
            return false;
        };
        if patch.to_string().len() > MAX_DOC_BYTES {
            return false;
        }
        self.prune(now_ms);
        let set = self.entry(id, now_ms);
        if set.pinned {
            return false;
        }
        for (k, v) in obj {
            // The store's own members are not the app's to set.
            if matches!(
                k.as_str(),
                "trigger_id" | "created_ms" | "frames" | "pinned"
            ) {
                continue;
            }
            merge(&mut set.doc[k], v);
        }
        self.write_doc(id);
        true
    }

    /// Whether a set exists for the trigger.
    pub(crate) fn has(&self, id: u64) -> bool {
        self.sets.contains_key(&id)
    }

    fn write_doc(&self, id: u64) {
        let Some(set) = self.sets.get(&id) else {
            return;
        };
        let Some(d) = self.set_dir(id, set.pinned) else {
            return;
        };
        if fs::create_dir_all(&d).is_ok() {
            let _ = fs::write(d.join(META), set.doc.to_string());
        }
    }

    /// The set as JSON: the document with `created_ms`, `expires_ms`
    /// (`null` when pinned), `pinned`, `bytes`, and `frames` (an array of
    /// `{index, at_ms, bytes}`).
    pub(crate) fn get(&mut self, id: u64, now_ms: i64) -> Option<Value> {
        self.prune(now_ms);
        let set = self.sets.get(&id)?;
        Some(self.describe(id, set))
    }

    fn describe(&self, id: u64, set: &Set) -> Value {
        let mut v = set.doc.clone();
        v["trigger_id"] = json!(id);
        v["created_ms"] = json!(set.created_ms);
        v["pinned"] = json!(set.pinned);
        v["bytes"] = json!(set.bytes);
        v["expires_ms"] = if set.pinned {
            json!(set.created_ms + PIN_RETENTION_MS)
        } else {
            json!(set.created_ms + self.retention_ms)
        };
        v["frames"] = Value::Array(
            set.frames
                .iter()
                .map(|(i, (at, n))| json!({"index": i, "at_ms": at, "bytes": n}))
                .collect(),
        );
        v
    }

    /// All sets, newest first, optionally only those created at or after
    /// `since_ms`.
    pub(crate) fn list(&mut self, since_ms: Option<i64>, now_ms: i64) -> Vec<Value> {
        self.prune(now_ms);
        let mut out: Vec<(i64, Value)> = self
            .sets
            .iter()
            .filter(|(_, s)| since_ms.is_none_or(|t| s.created_ms >= t))
            .map(|(id, s)| (s.created_ms, self.describe(*id, s)))
            .collect();
        out.sort_by_key(|e| std::cmp::Reverse(e.0));
        out.into_iter().map(|e| e.1).collect()
    }

    pub(crate) fn frame(&mut self, id: u64, index: u32, now_ms: i64) -> Option<Vec<u8>> {
        self.prune(now_ms);
        let set = self.sets.get(&id)?;
        set.frames.get(&index)?;
        match self.set_dir(id, set.pinned) {
            Some(d) => fs::read(d.join(frame_name(index))).ok(),
            None => set.memory.get(&index).cloned(),
        }
    }

    /// Pins a set: it leaves the short-lived store, with the snapshot
    /// limits (7 days from the pin, 256 pinned sets, 64 MiB). Returns false
    /// for an unknown set.
    pub(crate) fn pin(&mut self, id: u64, now_ms: i64) -> bool {
        self.prune(now_ms);
        let Some(set) = self.sets.get(&id) else {
            return false;
        };
        if set.pinned {
            return true;
        }
        let from = self.set_dir(id, false);
        let to = self.set_dir(id, true);
        if let (Some(from), Some(to)) = (&from, &to) {
            let _ = fs::remove_dir_all(to);
            if fs::rename(from, to).is_err() {
                return false;
            }
        }
        if let Some(set) = self.sets.get_mut(&id) {
            set.pinned = true;
            // The seven days count from the pin.
            set.created_ms = now_ms;
            set.doc["created_ms"] = json!(now_ms);
        }
        self.write_doc(id);
        self.prune(now_ms);
        true
    }

    fn remove(&mut self, id: u64) {
        if let Some(set) = self.sets.remove(&id)
            && let Some(d) = self.set_dir(id, set.pinned)
        {
            let _ = fs::remove_dir_all(d);
        }
    }

    /// Drops what expired, then what is over the count and size caps,
    /// oldest first.
    pub(crate) fn prune(&mut self, now_ms: i64) {
        let retention = self.retention_ms;
        let (cap_sets, cap_bytes) = (self.max_sets, self.max_bytes);
        let expired: Vec<u64> = self
            .sets
            .iter()
            .filter(|(_, s)| {
                now_ms - s.created_ms
                    > if s.pinned {
                        PIN_RETENTION_MS
                    } else {
                        retention
                    }
            })
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.remove(id);
        }
        for (pinned, max_sets, max_bytes) in
            [(false, cap_sets, cap_bytes), (true, PIN_SETS, PIN_BYTES)]
        {
            let mut order: Vec<(i64, u64, u64)> = self
                .sets
                .iter()
                .filter(|(_, s)| s.pinned == pinned)
                .map(|(id, s)| (s.created_ms, *id, s.bytes))
                .collect();
            order.sort_unstable();
            let mut count = order.len();
            let mut total: u64 = order.iter().map(|e| e.2).sum();
            for (_, id, bytes) in order {
                if count <= max_sets && total <= max_bytes {
                    break;
                }
                self.remove(id);
                count -= 1;
                total = total.saturating_sub(bytes);
            }
        }
    }

    /// Sets held now (for the status).
    pub(crate) fn counts(&self) -> (usize, usize, u64) {
        let pinned = self.sets.values().filter(|s| s.pinned).count();
        (
            self.sets.len() - pinned,
            pinned,
            self.sets.values().map(|s| s.bytes).sum(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_000_000_000_000;
    const MIN: i64 = 60_000;

    #[test]
    fn sets_expire_after_the_retention_time() {
        let mut s = EvidenceStore::open(None, T0);
        assert!(s.put_frame(1, 0, T0, b"jpeg", T0));
        assert!(s.merge_doc(1, &json!({"decision": {"decision": "quiet"}}), T0));
        assert_eq!(
            s.get(1, T0 + 119 * MIN).unwrap()["decision"]["decision"],
            "quiet"
        );
        assert_eq!(s.frame(1, 0, T0 + 119 * MIN).as_deref(), Some(&b"jpeg"[..]));
        assert!(s.get(1, T0 + 121 * MIN).is_none());
        assert!(s.frame(1, 0, T0 + 121 * MIN).is_none());
    }

    #[test]
    fn retention_is_a_setting_from_30_minutes_to_24_hours() {
        let mut s = EvidenceStore::open(None, T0);
        s.set_retention_min(1, T0);
        assert_eq!(s.retention_min(), 30);
        s.set_retention_min(100_000, T0);
        assert_eq!(s.retention_min(), 24 * 60);
        s.set_retention_min(30, T0);
        s.merge_doc(7, &json!({"a": 1}), T0);
        assert!(s.get(7, T0 + 29 * MIN).is_some());
        assert!(s.get(7, T0 + 31 * MIN).is_none());
        // Shortening the retention drops sets that are now too old.
        s.set_retention_min(120, T0);
        s.merge_doc(8, &json!({"a": 1}), T0);
        s.set_retention_min(30, T0 + 40 * MIN);
        assert!(s.get(8, T0 + 40 * MIN).is_none());
    }

    #[test]
    fn the_count_cap_drops_the_oldest_first() {
        let mut s = EvidenceStore::open(None, T0);
        for id in 0..(MAX_SETS as u64 + 5) {
            assert!(s.merge_doc(id, &json!({"n": id}), T0 + id as i64));
        }
        assert!(s.get(0, T0 + 1000).is_none());
        assert!(s.get(4, T0 + 1000).is_none());
        assert!(s.get(5, T0 + 1000).is_some());
        assert_eq!(s.list(None, T0 + 1000).len(), MAX_SETS);
        // Newest first.
        assert_eq!(
            s.list(None, T0 + 1000)[0]["trigger_id"],
            MAX_SETS as u64 + 4
        );
    }

    #[test]
    fn the_size_cap_drops_the_oldest_first() {
        let mut s = EvidenceStore::open(None, T0);
        s.max_bytes = 1000;
        let frame = vec![0u8; 100];
        // Eleven sets of 100 bytes against a 1000 byte cap: the first goes.
        for id in 0..11u64 {
            assert!(s.put_frame(id, 0, T0, &frame, T0 + id as i64));
        }
        assert!(s.get(0, T0 + 1000).is_none());
        assert!(s.get(1, T0 + 1000).is_some());
        assert!(s.counts().2 <= 1000);
    }

    #[test]
    fn a_set_holds_at_most_eight_frames() {
        let mut s = EvidenceStore::open(None, T0);
        for i in 0..MAX_FRAMES as u32 {
            assert!(s.put_frame(1, i, T0, b"x", T0));
        }
        assert!(!s.put_frame(1, 99, T0, b"x", T0));
        // Replacing an index is fine.
        assert!(s.put_frame(1, 3, T0, b"yy", T0));
        assert_eq!(s.get(1, T0).unwrap()["frames"].as_array().unwrap().len(), 8);
    }

    #[test]
    fn pinning_keeps_a_set_for_seven_days_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = EvidenceStore::open(Some(dir.path().to_path_buf()), T0);
        s.put_frame(5, 0, T0, b"jpeg", T0);
        s.merge_doc(
            5,
            &json!({"methods": {"camera_ai": {"claimed": "person"}}}),
            T0,
        );
        s.put_frame(6, 0, T0, b"other", T0);
        assert!(!s.pin(99, T0));
        assert!(s.pin(5, T0 + MIN));
        // Long past the short retention: the unpinned set is gone, the pinned stays.
        let later = T0 + 3 * 60 * MIN;
        assert!(s.get(6, later).is_none());
        let kept = s.get(5, later).unwrap();
        assert_eq!(kept["pinned"], true);
        assert_eq!(kept["methods"]["camera_ai"]["claimed"], "person");
        assert_eq!(s.frame(5, 0, later).as_deref(), Some(&b"jpeg"[..]));
        // It survives a restart, and expires after seven days from the pin.
        let mut again = EvidenceStore::open(Some(dir.path().to_path_buf()), later);
        assert!(again.get(5, later).is_some());
        assert!(again.get(5, T0 + MIN + PIN_RETENTION_MS + 1).is_none());
        assert!(!dir.path().join("evidence/pinned/5").exists());
    }

    #[test]
    fn documents_merge_deeply_so_each_frame_adds_its_boxes() {
        let mut s = EvidenceStore::open(None, T0);
        let first = json!({"methods": {"detector": {"frames": {"0": {"boxes": [1]}}}}});
        let second = json!({"methods": {"detector": {"frames": {"1": {"boxes": [2]}}},
                                         "camera_ai": {"claimed": "person"}}});
        s.merge_doc(1, &first, T0);
        s.merge_doc(1, &second, T0);
        let v = s.get(1, T0).unwrap();
        assert_eq!(v["methods"]["detector"]["frames"]["0"]["boxes"][0], 1);
        assert_eq!(v["methods"]["detector"]["frames"]["1"]["boxes"][0], 2);
        assert_eq!(v["methods"]["camera_ai"]["claimed"], "person");
    }

    #[test]
    fn app_documents_cannot_overwrite_the_stores_members() {
        let mut s = EvidenceStore::open(None, T0);
        s.merge_doc(1, &json!({"pinned": true, "created_ms": 5, "x": 1}), T0);
        let v = s.get(1, T0).unwrap();
        assert_eq!(
            (v["pinned"].clone(), v["created_ms"].clone(), v["x"].clone()),
            (json!(false), json!(T0), json!(1))
        );
        assert!(!s.merge_doc(1, &json!([1]), T0));
    }
}
