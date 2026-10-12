//! The partials registry (docs/CAPTURE.md, "Crash safety"): the partial
//! files in progress, so a crash leaves them recoverable or removable and
//! nothing else is ever touched.
//!
//! The registry is the file `data_dir/capture/partials.json`:
//!
//! ```json
//! {"version":1,"entries":[{"id":12,"kind":"live",
//!   "partial":"<dir>/<stem>.partial.mp4","target":"<dir>/<stem>.mp4"}]}
//! ```
//!
//! # Rules
//!
//! - **Bounded.** At most [`MAX_ENTRIES`] entries (4 live, 2 call, and room
//!   for recordings that are finalizing); paths at most
//!   [`MAX_PATH_CHARS`]; the file is read only up to 16 MiB. A registry
//!   that is missing or unreadable is empty (and reported corrupt), never
//!   an error that blocks a recording.
//! - **Atomic.** Saved through `recordings::store::write_atomic` (a
//!   sibling temporary file, synced, renamed), so a crash leaves the old or
//!   the new registry. The caller saves after adding an entry (before
//!   writing the partial file) and after removing one (after the final
//!   file is in place).
//! - **Only listed, only shaped files.** An entry is valid only when the
//!   partial path ends in `.partial.mp4`, the target is the same path with
//!   `.mp4` instead, there is no control character, and the path is
//!   absolute. Invalid entries are dropped on load. Recovery never touches
//!   a path that is not a valid entry.
//! - **Recovery plan.** [`plan_recovery`] reads sizes through a callback
//!   and decides per entry: a missing partial is forgotten; a partial under
//!   [`MIN_RECOVERABLE_BYTES`] (no complete fragment) is deleted; a partial
//!   whose target already exists is the leftover of a crash between the
//!   rename and the delete, so only the partial is deleted; anything else
//!   is recovered into its target.

use recordings::export::validate_dest;
use recordings::store::write_json;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;

pub use crate::caps::RecordingKind;

/// Most entries kept.
pub const MAX_ENTRIES: usize = 32;
/// Longest path accepted, in characters.
pub const MAX_PATH_CHARS: usize = 1024;
/// A partial smaller than this holds no complete fragment.
pub const MIN_RECOVERABLE_BYTES: u64 = 4096;
/// The registry's format version.
pub const VERSION: u32 = 1;

/// One partial file in progress.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialEntry {
    /// The recording's ID.
    pub id: u64,
    /// Live or call.
    pub kind: RecordingKind,
    /// The file being written (`<stem>.partial.mp4`).
    pub partial: String,
    /// The file it becomes (`<stem>.mp4`).
    pub target: String,
}

impl PartialEntry {
    /// An entry for `target` (`<stem>.mp4`), whose partial is
    /// `<stem>.partial.mp4` beside it.
    pub fn for_target(id: u64, kind: RecordingKind, target: &str) -> Option<PartialEntry> {
        let stem = target.strip_suffix(".mp4")?;
        let e = PartialEntry {
            id,
            kind,
            partial: format!("{stem}.partial.mp4"),
            target: target.to_owned(),
        };
        e.is_valid().then_some(e)
    }

    /// Whether both paths have the required shape.
    pub fn is_valid(&self) -> bool {
        let ok_path = |p: &str| {
            p.chars().count() <= MAX_PATH_CHARS
                && validate_dest(p).is_ok()
                && !p.ends_with(['/', '\\'])
        };
        let Some(stem) = self.partial.strip_suffix(".partial.mp4") else {
            return false;
        };
        !stem.is_empty()
            && !stem.ends_with(['/', '\\'])
            && self.target.strip_suffix(".mp4") == Some(stem)
            && ok_path(&self.partial)
            && ok_path(&self.target)
    }
}

/// Why an entry was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddError {
    /// [`MAX_ENTRIES`] entries already.
    Full,
    /// A path has the wrong shape.
    Invalid,
    /// An entry with this ID or partial path exists.
    Duplicate,
}

/// The registry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    version: u32,
    entries: Vec<PartialEntry>,
}

/// A load's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Loaded {
    /// The registry (empty if the file was missing or unreadable).
    pub registry: Registry,
    /// The file existed but could not be used as it was: unreadable,
    /// wrong version, or entries dropped.
    pub corrupt: bool,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Registry {
        Registry {
            version: VERSION,
            entries: Vec::new(),
        }
    }

    /// The entries.
    pub fn entries(&self) -> &[PartialEntry] {
        &self.entries
    }

    /// Adds an entry.
    pub fn add(&mut self, e: PartialEntry) -> Result<(), AddError> {
        if !e.is_valid() {
            return Err(AddError::Invalid);
        }
        if self
            .entries
            .iter()
            .any(|x| x.id == e.id || x.partial == e.partial)
        {
            return Err(AddError::Duplicate);
        }
        if self.entries.len() >= MAX_ENTRIES {
            return Err(AddError::Full);
        }
        self.entries.push(e);
        Ok(())
    }

    /// Removes the entry with this ID; true if there was one.
    pub fn remove(&mut self, id: u64) -> bool {
        let n = self.entries.len();
        self.entries.retain(|e| e.id != id);
        self.entries.len() != n
    }

    /// Serializes.
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).unwrap_or_default()
    }

    /// Parses; invalid or surplus entries are dropped (`corrupt`), and
    /// unparseable bytes give an empty registry (`corrupt`).
    pub fn from_bytes(bytes: &[u8]) -> Loaded {
        let corrupt = |registry| Loaded {
            registry,
            corrupt: true,
        };
        if bytes.len() as u64 > recordings::store::MAX_JSON_BYTES {
            return corrupt(Registry::new());
        }
        let Ok(mut r) = serde_json::from_slice::<Registry>(bytes) else {
            return corrupt(Registry::new());
        };
        if r.version != VERSION {
            return corrupt(Registry::new());
        }
        let before = r.entries.len();
        let mut seen: Vec<PartialEntry> = Vec::new();
        for e in r.entries.drain(..) {
            if e.is_valid()
                && seen.len() < MAX_ENTRIES
                && !seen.iter().any(|x| x.id == e.id || x.partial == e.partial)
            {
                seen.push(e);
            }
        }
        r.entries = seen;
        Loaded {
            corrupt: r.entries.len() != before,
            registry: r,
        }
    }

    /// Reads the registry at `path`.
    pub fn load(path: &Path) -> Loaded {
        let empty = |corrupt| Loaded {
            registry: Registry::new(),
            corrupt,
        };
        match std::fs::metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => empty(false),
            Err(_) => empty(true),
            Ok(m) if m.len() > recordings::store::MAX_JSON_BYTES => empty(true),
            Ok(_) => match std::fs::read(path) {
                Ok(b) => Registry::from_bytes(&b),
                Err(_) => empty(true),
            },
        }
    }

    /// Writes the registry atomically.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_json(path, self)
    }
}

/// What to do with one registry entry after a crash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    /// The partial file is gone: drop the entry.
    Forget {
        /// The entry's recording ID.
        id: u64,
    },
    /// Delete the partial file (too small to hold a fragment, or its
    /// target is already in place), then drop the entry.
    Delete {
        /// The entry's recording ID.
        id: u64,
        /// The partial file to delete.
        partial: String,
        /// Why.
        why: DeleteWhy,
    },
    /// Finalize the partial into `target`, delete it, drop the entry.
    Recover {
        /// The entry's recording ID.
        id: u64,
        /// Live or call.
        kind: RecordingKind,
        /// The partial file to finalize.
        partial: String,
        /// The final file to create (never an existing file).
        target: String,
    },
}

/// Why a partial is deleted rather than recovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteWhy {
    /// Under [`MIN_RECOVERABLE_BYTES`].
    TooSmall,
    /// The final file already exists (a crash after the rename).
    AlreadyFinalized,
}

/// Plans the recovery of every entry. `size_of` answers a file's size, or
/// `None` if it does not exist; it is the only file-system access, and is
/// asked about listed paths only.
pub fn plan_recovery(reg: &Registry, size_of: impl Fn(&str) -> Option<u64>) -> Vec<RecoveryAction> {
    reg.entries
        .iter()
        .filter(|e| e.is_valid())
        .map(|e| {
            let Some(size) = size_of(&e.partial) else {
                return RecoveryAction::Forget { id: e.id };
            };
            if size_of(&e.target).is_some_and(|t| t > 0) {
                return RecoveryAction::Delete {
                    id: e.id,
                    partial: e.partial.clone(),
                    why: DeleteWhy::AlreadyFinalized,
                };
            }
            if size < MIN_RECOVERABLE_BYTES {
                return RecoveryAction::Delete {
                    id: e.id,
                    partial: e.partial.clone(),
                    why: DeleteWhy::TooSmall,
                };
            }
            RecoveryAction::Recover {
                id: e.id,
                kind: e.kind,
                partial: e.partial.clone(),
                target: e.target.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, stem: &str) -> PartialEntry {
        PartialEntry::for_target(id, RecordingKind::Live, &format!("/rec/{stem}.mp4")).unwrap()
    }

    #[test]
    fn entries_must_have_the_partial_shape() {
        let ok = entry(1, "a b");
        assert_eq!(ok.partial, "/rec/a b.partial.mp4");
        assert!(ok.is_valid());
        let bad = [
            PartialEntry {
                partial: "/rec/a.mp4".into(),
                target: "/rec/a.mp4".into(),
                ..ok.clone()
            },
            PartialEntry {
                partial: "/rec/a.partial.mp4".into(),
                target: "/rec/b.mp4".into(),
                ..ok.clone()
            },
            PartialEntry {
                partial: "rec/a.partial.mp4".into(),
                target: "rec/a.mp4".into(),
                ..ok.clone()
            },
            PartialEntry {
                partial: "/rec/../a.partial.mp4".into(),
                target: "/rec/../a.mp4".into(),
                ..ok.clone()
            },
            PartialEntry {
                partial: "/rec/.partial.mp4".into(),
                target: "/rec/.mp4".into(),
                ..ok.clone()
            },
            PartialEntry {
                partial: "/rec/a\n.partial.mp4".into(),
                target: "/rec/a\n.mp4".into(),
                ..ok.clone()
            },
            PartialEntry {
                partial: format!("/{}.partial.mp4", "x".repeat(2000)),
                target: format!("/{}.mp4", "x".repeat(2000)),
                ..ok.clone()
            },
        ];
        for b in bad {
            assert!(!b.is_valid(), "{b:?}");
        }
        assert_eq!(
            PartialEntry::for_target(1, RecordingKind::Call, "/rec/a.txt"),
            None
        );
        // Windows paths are accepted.
        assert!(PartialEntry::for_target(2, RecordingKind::Call, "C:\\Rec\\a.mp4").is_some());
    }

    #[test]
    fn add_remove_and_bounds() {
        let mut r = Registry::new();
        r.add(entry(1, "a")).unwrap();
        assert_eq!(r.add(entry(1, "b")), Err(AddError::Duplicate));
        assert_eq!(r.add(entry(2, "a")), Err(AddError::Duplicate));
        assert_eq!(
            r.add(PartialEntry {
                partial: "x".into(),
                ..entry(3, "c")
            }),
            Err(AddError::Invalid)
        );
        for i in 10..(10 + MAX_ENTRIES as u64 - 1) {
            r.add(entry(i, &format!("f{i}"))).unwrap();
        }
        assert_eq!(r.entries().len(), MAX_ENTRIES);
        assert_eq!(r.add(entry(99, "z")), Err(AddError::Full));
        assert!(r.remove(1));
        assert!(!r.remove(1));
        assert!(r.add(entry(99, "z")).is_ok());
    }

    #[test]
    fn bytes_round_trip_and_bad_input_is_empty_and_flagged() {
        let mut r = Registry::new();
        r.add(entry(1, "a")).unwrap();
        r.add(PartialEntry::for_target(2, RecordingKind::Call, "/c/b.mp4").unwrap())
            .unwrap();
        let l = Registry::from_bytes(&r.to_bytes());
        assert!(!l.corrupt);
        assert_eq!(l.registry, r);
        for bad in [
            &b"{not json"[..],
            b"",
            b"{\"version\":9,\"entries\":[]}",
            b"[]",
        ] {
            let l = Registry::from_bytes(bad);
            assert!(l.corrupt, "{:?}", String::from_utf8_lossy(bad));
            assert!(l.registry.entries().is_empty());
        }
    }

    #[test]
    fn invalid_duplicate_and_surplus_entries_are_dropped_on_load() {
        let json = serde_json::json!({"version": 1, "entries": [
            {"id":1,"kind":"live","partial":"/r/a.partial.mp4","target":"/r/a.mp4"},
            {"id":2,"kind":"live","partial":"/etc/passwd","target":"/etc/passwd"},
            {"id":1,"kind":"live","partial":"/r/b.partial.mp4","target":"/r/b.mp4"},
            {"id":3,"kind":"call","partial":"/r/a.partial.mp4","target":"/r/a.mp4"},
        ]});
        let l = Registry::from_bytes(json.to_string().as_bytes());
        assert!(l.corrupt);
        assert_eq!(l.registry.entries().len(), 1);
        assert_eq!(l.registry.entries()[0].id, 1);
        // More than the bound: the first MAX_ENTRIES stay.
        let many: Vec<_> = (0..100)
            .map(|i| {
                serde_json::json!({"id":i,"kind":"live",
                "partial":format!("/r/{i}.partial.mp4"),"target":format!("/r/{i}.mp4")})
            })
            .collect();
        let json = serde_json::json!({"version":1,"entries":many});
        let l = Registry::from_bytes(json.to_string().as_bytes());
        assert_eq!(l.registry.entries().len(), MAX_ENTRIES);
        assert!(l.corrupt);
    }

    #[test]
    fn save_and_load_are_atomic_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("capture").join("partials.json");
        let l = Registry::load(&path);
        assert!(!l.corrupt && l.registry.entries().is_empty());
        let mut r = Registry::new();
        r.add(entry(5, "q")).unwrap();
        r.save(&path).unwrap();
        r.remove(5);
        r.add(entry(6, "w")).unwrap();
        r.save(&path).unwrap();
        assert!(!path.with_extension("json.tmp").exists());
        let l = Registry::load(&path);
        assert!(!l.corrupt);
        assert_eq!(l.registry.entries()[0].id, 6);
        std::fs::write(&path, b"garbage").unwrap();
        let l = Registry::load(&path);
        assert!(l.corrupt && l.registry.entries().is_empty());
    }

    #[test]
    fn recovery_plan_table() {
        let mut r = Registry::new();
        for (id, stem) in [
            (1, "gone"),
            (2, "tiny"),
            (3, "done"),
            (4, "good"),
            (5, "empty_target"),
        ] {
            r.add(entry(id, stem)).unwrap();
        }
        let size = |p: &str| match p {
            "/rec/tiny.partial.mp4" => Some(100),
            "/rec/done.partial.mp4" => Some(1 << 20),
            "/rec/done.mp4" => Some(1 << 20),
            "/rec/good.partial.mp4" => Some(1 << 20),
            "/rec/empty_target.partial.mp4" => Some(1 << 20),
            "/rec/empty_target.mp4" => Some(0),
            _ => None,
        };
        let plan = plan_recovery(&r, size);
        assert_eq!(
            plan,
            vec![
                RecoveryAction::Forget { id: 1 },
                RecoveryAction::Delete {
                    id: 2,
                    partial: "/rec/tiny.partial.mp4".into(),
                    why: DeleteWhy::TooSmall
                },
                RecoveryAction::Delete {
                    id: 3,
                    partial: "/rec/done.partial.mp4".into(),
                    why: DeleteWhy::AlreadyFinalized
                },
                RecoveryAction::Recover {
                    id: 4,
                    kind: RecordingKind::Live,
                    partial: "/rec/good.partial.mp4".into(),
                    target: "/rec/good.mp4".into()
                },
                RecoveryAction::Recover {
                    id: 5,
                    kind: RecordingKind::Live,
                    partial: "/rec/empty_target.partial.mp4".into(),
                    target: "/rec/empty_target.mp4".into()
                },
            ]
        );
    }

    #[test]
    fn recovery_asks_only_about_listed_paths() {
        let mut r = Registry::new();
        r.add(entry(1, "a")).unwrap();
        let asked = std::cell::RefCell::new(Vec::new());
        plan_recovery(&r, |p| {
            asked.borrow_mut().push(p.to_owned());
            None
        });
        assert!(asked.borrow().iter().all(|p| p.starts_with("/rec/a")));
    }
}
