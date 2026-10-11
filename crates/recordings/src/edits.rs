//! The user's edits to incidents, stored and replayed (docs/FFI-RECORDINGS.md,
//! "Incidents and the user's edits").
//!
//! Incidents are derived, not stored: [`crate::incident::group`] makes them
//! from the index's events whenever the events or the settings change. The
//! user's merges, splits, renames, and added or removed cameras are stored
//! here in the order they were made, refer to incidents by their stable
//! [`Incident::key`], and are replayed over every fresh grouping
//! ([`EditLog::apply`]).
//!
//! - An edit whose incidents are not there (yet) is skipped and kept: its
//!   events may not be indexed yet. It is dropped once its time is older
//!   than the index keeps ([`EditLog::expire`]).
//! - A new edit that cannot apply (unknown key, a split that would leave a
//!   part empty, removing the last camera) is refused and nothing is stored.
//! - [`Edit::Revert`] is not stored: it removes every stored edit that
//!   shaped the incident.
//! - At most [`MAX_EDITS`] edits; names up to [`MAX_NAME_BYTES`] bytes.
//!
//! The JSON form of [`Edit`] is the C interface's (`{"op": "merge", ...}`).

use crate::Secs;
use crate::incident::{CameraId, Incident, merge, split};
use crate::store::{read_json, write_json};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Most edits stored.
pub const MAX_EDITS: usize = 10_000;
/// Longest incident name, in bytes of UTF-8.
pub const MAX_NAME_BYTES: usize = 256;
/// Most incidents one merge takes.
const MAX_MERGE: usize = 1000;
const VERSION: u32 = 1;

/// One edit by the user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Edit {
    /// These incidents are one event.
    Merge {
        /// Two or more incident keys.
        keys: Vec<String>,
    },
    /// Events that start before `at` and the rest are two events.
    Split {
        /// Incident key.
        key: String,
        /// Where to split (naive seconds).
        at: Secs,
    },
    /// Names the incident; `None` clears the name.
    Rename {
        /// Incident key.
        key: String,
        /// The name.
        name: Option<String>,
    },
    /// A camera without events of its own saw it too.
    AddCamera {
        /// Incident key.
        key: String,
        /// The camera.
        camera: CameraId,
    },
    /// A camera and its events do not belong to it.
    RemoveCamera {
        /// Incident key.
        key: String,
        /// The camera.
        camera: CameraId,
    },
    /// Drops every stored edit that shaped the incident (never stored).
    Revert {
        /// Incident key.
        key: String,
    },
}

impl Edit {
    /// The incident keys the edit refers to.
    pub fn keys(&self) -> Vec<&str> {
        match self {
            Edit::Merge { keys } => keys.iter().map(String::as_str).collect(),
            Edit::Split { key, .. }
            | Edit::Rename { key, .. }
            | Edit::AddCamera { key, .. }
            | Edit::RemoveCamera { key, .. }
            | Edit::Revert { key } => vec![key.as_str()],
        }
    }

    /// The edit's time for expiry: the earliest start in its keys (keys
    /// begin with `i-<start>-`); `None` when no key has one.
    pub fn time(&self) -> Option<Secs> {
        self.keys().iter().filter_map(|k| key_start(k)).min()
    }
}

/// The start time inside an incident key (`i-<start>-...`).
pub fn key_start(key: &str) -> Option<Secs> {
    let rest = key.strip_prefix("i-")?;
    let end = rest
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || (*i == 0 && *c == '-')))
        .map_or(rest.len(), |(i, _)| i);
    rest[..end].parse().ok()
}

/// Why an edit was refused.
#[derive(Debug)]
pub enum EditError {
    /// No incident has this key now.
    UnknownKey(String),
    /// The edit cannot apply (English reason, for logs).
    Invalid(&'static str),
    /// [`MAX_EDITS`] edits are stored already.
    Full,
    /// The edit log could not be written.
    Io(io::Error),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::UnknownKey(k) => write!(f, "no incident has the key {k}"),
            EditError::Invalid(why) => write!(f, "edit cannot apply: {why}"),
            EditError::Full => write!(f, "too many stored edits"),
            EditError::Io(e) => write!(f, "cannot store the edit: {e}"),
        }
    }
}

impl std::error::Error for EditError {}

/// Incidents after the edits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edited {
    /// The incidents, newest first, as [`crate::incident::group`] orders
    /// them.
    pub incidents: Vec<Incident>,
    /// Keys of the incidents a stored edit shaped.
    pub edited: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct EditFile {
    version: u32,
    edits: Vec<Edit>,
}

/// The stored edits, mirrored to one JSON file written atomically.
#[derive(Debug)]
pub struct EditLog {
    path: Option<PathBuf>,
    edits: Vec<Edit>,
}

/// An incident during replay, with the stored edits that shaped it.
struct Shaped {
    inc: Incident,
    by: BTreeSet<usize>,
}

/// Applies one edit to `list`. `Err` when it cannot apply; `list` is then
/// unchanged.
fn apply_one(list: &mut Vec<Shaped>, edit: &Edit, n: usize) -> Result<(), EditError> {
    let find = |list: &Vec<Shaped>, key: &str| list.iter().position(|s| s.inc.key() == key);
    let one = |list: &Vec<Shaped>, key: &str| {
        find(list, key).ok_or_else(|| EditError::UnknownKey(key.to_owned()))
    };
    match edit {
        Edit::Merge { keys } => {
            let unique: BTreeSet<&String> = keys.iter().collect();
            if unique.len() < 2 || keys.len() > MAX_MERGE {
                return Err(EditError::Invalid("a merge takes two or more incidents"));
            }
            let mut at = Vec::new();
            for k in &unique {
                at.push(one(list, k)?);
            }
            // In the order the user named them, so the first name survives.
            let mut parts: Vec<Shaped> = Vec::new();
            at.sort_unstable();
            for i in at.into_iter().rev() {
                parts.push(list.remove(i));
            }
            parts.sort_by_key(|p| keys.iter().position(|k| *k == p.inc.key()));
            let mut it = parts.into_iter();
            let mut acc = it.next().expect("two or more");
            for p in it {
                acc.inc = merge(&acc.inc, &p.inc);
                acc.by.extend(p.by);
            }
            acc.by.insert(n);
            list.push(acc);
        }
        Edit::Split { key, at } => {
            let i = one(list, key)?;
            let (a, b) =
                split(&list[i].inc, *at).ok_or(EditError::Invalid("a part would be empty"))?;
            let mut by = list.remove(i).by;
            by.insert(n);
            list.push(Shaped {
                inc: a,
                by: by.clone(),
            });
            list.push(Shaped { inc: b, by });
        }
        Edit::Rename { key, name } => {
            if name.as_ref().is_some_and(|s| s.len() > MAX_NAME_BYTES) {
                return Err(EditError::Invalid("name too long"));
            }
            let i = one(list, key)?;
            list[i].inc.name = name.clone().filter(|s| !s.is_empty());
            list[i].by.insert(n);
        }
        Edit::AddCamera { key, camera } => {
            let i = one(list, key)?;
            if list[i].inc.cameras().contains(camera) {
                return Err(EditError::Invalid("the camera is already in the incident"));
            }
            let s = &mut list[i];
            s.inc.add_camera(camera.clone());
            s.inc.id = s.inc.compute_id();
            s.by.insert(n);
        }
        Edit::RemoveCamera { key, camera } => {
            let i = one(list, key)?;
            let s = &mut list[i];
            if !s.inc.cameras().contains(camera) {
                return Err(EditError::Invalid("the camera is not in the incident"));
            }
            // The key may change (its earliest event can go), so check that
            // something with events is left.
            let mut copy = s.inc.clone();
            if !copy.remove_camera(camera) || copy.events.is_empty() {
                return Err(EditError::Invalid("the incident would have no camera left"));
            }
            copy.id = copy.compute_id();
            s.inc = copy;
            s.by.insert(n);
        }
        Edit::Revert { .. } => return Err(EditError::Invalid("revert is not replayed")),
    }
    Ok(())
}

fn sort_newest_first(v: &mut [Incident]) {
    v.sort_by(|a, b| b.start().cmp(&a.start()).then_with(|| a.id.cmp(&b.id)));
}

impl EditLog {
    /// An empty log kept in memory only (no data directory).
    pub fn in_memory() -> EditLog {
        EditLog {
            path: None,
            edits: Vec::new(),
        }
    }

    /// Opens the log stored at `path` (created on the first edit). An
    /// unreadable file is set aside as `<name>.bad` and the log starts
    /// empty; edits are the user's work, so they are not deleted.
    pub fn open(path: &Path) -> EditLog {
        let edits = match read_json::<EditFile>(path) {
            Ok(Some(f)) if f.version == VERSION => f.edits,
            Ok(None) => Vec::new(),
            Ok(Some(f)) => {
                warn!(
                    version = f.version,
                    "incident edits of an unknown format ignored"
                );
                Vec::new()
            }
            Err(e) => {
                warn!(error = %e, "incident edits unreadable; set aside");
                let mut bad = path.as_os_str().to_owned();
                bad.push(".bad");
                let _ = std::fs::rename(path, PathBuf::from(bad));
                Vec::new()
            }
        };
        EditLog {
            path: Some(path.to_owned()),
            edits: edits
                .into_iter()
                .filter(|e| !matches!(e, Edit::Revert { .. }))
                .take(MAX_EDITS)
                .collect(),
        }
    }

    /// The stored edits, oldest first.
    pub fn edits(&self) -> &[Edit] {
        &self.edits
    }

    fn save(&self) -> io::Result<()> {
        match &self.path {
            Some(p) => write_json(
                p,
                &EditFile {
                    version: VERSION,
                    edits: self.edits.clone(),
                },
            ),
            None => Ok(()),
        }
    }

    fn replay(&self, base: &[Incident]) -> Vec<Shaped> {
        let mut list: Vec<Shaped> = base
            .iter()
            .map(|i| Shaped {
                inc: i.clone(),
                by: BTreeSet::new(),
            })
            .collect();
        for (n, e) in self.edits.iter().enumerate() {
            // Not applicable now (events not indexed yet, or already
            // changed): skipped, kept.
            let _ = apply_one(&mut list, e, n);
        }
        list
    }

    /// Replays the stored edits over freshly grouped incidents.
    pub fn apply(&self, base: &[Incident]) -> Edited {
        let list = self.replay(base);
        let edited = list
            .iter()
            .filter(|s| !s.by.is_empty())
            .map(|s| s.inc.key())
            .collect();
        let mut incidents: Vec<Incident> = list.into_iter().map(|s| s.inc).collect();
        sort_newest_first(&mut incidents);
        Edited { incidents, edited }
    }

    /// Applies a new edit on top of the stored ones (over `base`, the
    /// freshly grouped incidents), stores it, and returns the keys of the
    /// incidents it produced. Refused edits store nothing.
    pub fn push(&mut self, base: &[Incident], edit: Edit) -> Result<Vec<String>, EditError> {
        let mut list = self.replay(base);
        if let Edit::Revert { key } = &edit {
            let s = list
                .iter()
                .find(|s| s.inc.key() == *key)
                .ok_or_else(|| EditError::UnknownKey(key.clone()))?;
            let drop = s.by.clone();
            let before = std::mem::take(&mut self.edits);
            self.edits = before
                .iter()
                .enumerate()
                .filter(|(n, _)| !drop.contains(n))
                .map(|(_, e)| e.clone())
                .collect();
            if let Err(e) = self.save() {
                self.edits = before;
                return Err(EditError::Io(e));
            }
            // What the reverted incident's events form now.
            let firsts: BTreeSet<(Secs, CameraId)> = s
                .inc
                .events
                .iter()
                .map(|e| (e.start, e.camera.clone()))
                .collect();
            let after = self.apply(base);
            return Ok(after
                .incidents
                .iter()
                .filter(|i| {
                    i.events
                        .iter()
                        .any(|e| firsts.contains(&(e.start, e.camera.clone())))
                })
                .map(Incident::key)
                .collect());
        }
        if self.edits.len() >= MAX_EDITS {
            return Err(EditError::Full);
        }
        let n = self.edits.len();
        let before: BTreeMap<String, ()> = list.iter().map(|s| (s.inc.key(), ())).collect();
        apply_one(&mut list, &edit, n)?;
        self.edits.push(edit);
        if let Err(e) = self.save() {
            self.edits.pop();
            return Err(EditError::Io(e));
        }
        let mut keys: Vec<String> = list
            .iter()
            .filter(|s| s.by.contains(&n))
            .map(|s| s.inc.key())
            .collect();
        keys.sort_by_key(|k| before.contains_key(k));
        Ok(keys)
    }

    /// Drops edits older than `oldest` (naive seconds; the index's age
    /// limit): their incidents' events are no longer indexed. Returns how
    /// many were dropped.
    pub fn expire(&mut self, oldest: Secs) -> io::Result<usize> {
        let before = self.edits.len();
        self.edits.retain(|e| e.time().is_none_or(|t| t >= oldest));
        let dropped = before - self.edits.len();
        if dropped > 0 {
            info!(dropped, "expired incident edits dropped");
            self.save()?;
        }
        Ok(dropped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::{CameraEvent, GroupingConfig, group};

    fn ev(dev: &str, ch: u8, kind: &str, start: Secs, end: Secs) -> CameraEvent {
        CameraEvent {
            start,
            end,
            camera: CameraId::new(dev, ch),
            kind: kind.into(),
        }
    }

    /// Three incidents: 1000–1030 (two cameras), 2000–2010, 5000–5020.
    fn base() -> Vec<Incident> {
        group(
            &[
                ev("nvr-a", 0, "vehicle", 1000, 1015),
                ev("nvr-a", 1, "vehicle", 1012, 1030),
                ev("nvr-a", 0, "md", 2000, 2010),
                ev("nvr-b", 0, "people", 5000, 5020),
            ],
            &GroupingConfig::default(),
        )
    }

    fn key_at(v: &[Incident], start: Secs) -> String {
        v.iter().find(|i| i.start() == start).unwrap().key()
    }

    #[test]
    fn json_is_the_c_interface_form() {
        let e: Edit = serde_json::from_str(
            r#"{"op":"add_camera","key":"i-1000-nvr-a-0-0","camera":{"device":"nvr-b","channel":5}}"#,
        )
        .unwrap();
        assert_eq!(
            e,
            Edit::AddCamera {
                key: "i-1000-nvr-a-0-0".into(),
                camera: CameraId::new("nvr-b", 5)
            }
        );
        let e: Edit = serde_json::from_str(r#"{"op":"rename","key":"k","name":null}"#).unwrap();
        assert_eq!(
            e,
            Edit::Rename {
                key: "k".into(),
                name: None
            }
        );
        assert_eq!(key_start("i-1000-nvr-a-0-0"), Some(1000));
        assert_eq!(key_start("i--5-x"), Some(-5));
        assert_eq!(key_start("x"), None);
    }

    #[test]
    fn edits_are_stored_replayed_and_reverted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incident-edits.json");
        let base = base();
        let mut log = EditLog::open(&path);

        // Merge the first two; rename the result.
        let (k1, k2) = (key_at(&base, 1000), key_at(&base, 2000));
        let keys = log
            .push(
                &base,
                Edit::Merge {
                    keys: vec![k1.clone(), k2.clone()],
                },
            )
            .unwrap();
        assert_eq!(keys, vec![k1.clone()]);
        log.push(
            &base,
            Edit::Rename {
                key: k1.clone(),
                name: Some("Delivery".into()),
            },
        )
        .unwrap();
        log.push(
            &base,
            Edit::AddCamera {
                key: k1.clone(),
                camera: CameraId::new("nvr-b", 3),
            },
        )
        .unwrap();

        // Replayed from the file over a fresh grouping.
        let log = EditLog::open(&path);
        assert_eq!(log.edits().len(), 3);
        let out = log.apply(&base);
        assert_eq!(out.incidents.len(), 2);
        let m = out.incidents.iter().find(|i| i.key() == k1).unwrap();
        assert_eq!((m.start(), m.end()), (1000, 2010));
        assert_eq!(m.name.as_deref(), Some("Delivery"));
        assert!(m.cameras().contains(&CameraId::new("nvr-b", 3)));
        assert_eq!(m.id, m.compute_id());
        assert_eq!(out.edited, BTreeSet::from([k1.clone()]));
        // Newest first.
        assert_eq!(out.incidents[0].start(), 5000);

        // Revert drops all three; the original two come back.
        let mut log = log;
        let keys = log.push(&base, Edit::Revert { key: k1.clone() }).unwrap();
        assert_eq!(keys, vec![k2.clone(), k1.clone()]);
        assert!(log.edits().is_empty());
        assert_eq!(log.apply(&base).incidents, base);
        assert!(EditLog::open(&path).edits().is_empty());
    }

    #[test]
    fn edits_that_cannot_apply_are_refused_and_not_stored() {
        let base = base();
        let mut log = EditLog::in_memory();
        let k = key_at(&base, 1000);
        let solo = key_at(&base, 5000);
        assert!(matches!(
            log.push(
                &base,
                Edit::Rename {
                    key: "i-9-x-0-0".into(),
                    name: None
                }
            ),
            Err(EditError::UnknownKey(_))
        ));
        assert!(matches!(
            log.push(
                &base,
                Edit::Split {
                    key: k.clone(),
                    at: 900
                }
            ),
            Err(EditError::Invalid(_))
        ));
        assert!(matches!(
            log.push(
                &base,
                Edit::Merge {
                    keys: vec![k.clone(), k.clone()]
                }
            ),
            Err(EditError::Invalid(_))
        ));
        assert!(matches!(
            log.push(
                &base,
                Edit::RemoveCamera {
                    key: solo,
                    camera: CameraId::new("nvr-b", 0)
                }
            ),
            Err(EditError::Invalid(_))
        ));
        assert!(matches!(
            log.push(
                &base,
                Edit::Rename {
                    key: k.clone(),
                    name: Some("x".repeat(MAX_NAME_BYTES + 1))
                }
            ),
            Err(EditError::Invalid(_))
        ));
        assert!(log.edits().is_empty());

        // A split that works gives two keys, the new one first.
        let keys = log
            .push(
                &base,
                Edit::Split {
                    key: k.clone(),
                    at: 1010,
                },
            )
            .unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[1], k);
        assert_eq!(keys[0], "i-1012-nvr-a-1-0");

        // Removing the camera of the earliest event changes the key.
        let mut log = EditLog::in_memory();
        let keys = log
            .push(
                &base,
                Edit::RemoveCamera {
                    key: k.clone(),
                    camera: CameraId::new("nvr-a", 0),
                },
            )
            .unwrap();
        assert_eq!(keys, vec!["i-1012-nvr-a-1-0".to_string()]);
        assert!(log.apply(&base).incidents.iter().all(|i| i.key() != k));
    }

    #[test]
    fn edits_wait_for_their_incidents_and_expire() {
        let base = base();
        let mut log = EditLog::in_memory();
        let k = key_at(&base, 1000);
        log.push(
            &base,
            Edit::Rename {
                key: k.clone(),
                name: Some("Gate".into()),
            },
        )
        .unwrap();
        // The incident's events are not indexed in this grouping: the edit
        // is skipped and kept.
        let partial: Vec<Incident> = base.iter().filter(|i| i.key() != k).cloned().collect();
        let out = log.apply(&partial);
        assert_eq!(out.incidents, partial);
        assert!(out.edited.is_empty());
        assert_eq!(log.edits().len(), 1);
        // And applies once they are.
        let out = log.apply(&base);
        assert_eq!(out.edited.len(), 1);
        // Older than the index keeps: dropped.
        assert_eq!(log.expire(999).unwrap(), 0);
        assert_eq!(log.expire(1001).unwrap(), 1);
        assert!(log.edits().is_empty());
    }

    #[test]
    fn a_damaged_file_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incident-edits.json");
        std::fs::write(&path, b"{nope").unwrap();
        let log = EditLog::open(&path);
        assert!(log.edits().is_empty());
        assert!(dir.path().join("incident-edits.json.bad").exists());
    }
}
