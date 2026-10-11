//! The user's camera groups and camera names, stored like layouts
//! (docs/FFI-RECORDINGS.md, "Camera groups").
//!
//! A group is cameras the user treats as covering one area; incidents form
//! within a group ([`crate::incident::group`]). Camera names override the
//! channel names the devices report, in incidents and output names. The
//! whole list is replaced at once ([`GroupStore::put`]) and written
//! atomically; every replacement raises the revision.

use crate::incident::{CameraGroup, CameraId, GroupingConfig};
use crate::store::{read_json, write_json};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Most groups.
pub const MAX_GROUPS: usize = 64;
/// Most cameras in one group.
pub const MAX_GROUP_CAMERAS: usize = 64;
/// Most camera name overrides.
pub const MAX_CAMERA_NAMES: usize = 4096;
/// Longest group or camera name, in bytes of UTF-8.
pub const MAX_NAME_BYTES: usize = 256;
const VERSION: u32 = 1;

/// The user's name for a camera.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraName {
    /// The camera.
    pub camera: CameraId,
    /// Its name.
    pub name: String,
}

/// Camera groups and names, as the C interface exchanges them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraGroups {
    /// Groups, in the user's order. A camera in several groups belongs to
    /// the first.
    #[serde(default)]
    pub groups: Vec<CameraGroup>,
    /// Camera name overrides.
    #[serde(default)]
    pub camera_names: Vec<CameraName>,
}

/// Why a list of groups was refused.
#[derive(Debug)]
pub enum GroupsError {
    /// Beyond a limit or malformed (English reason, for logs).
    Invalid(&'static str),
    /// It could not be written.
    Io(io::Error),
}

impl fmt::Display for GroupsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GroupsError::Invalid(why) => write!(f, "camera groups refused: {why}"),
            GroupsError::Io(e) => write!(f, "cannot store the camera groups: {e}"),
        }
    }
}

impl std::error::Error for GroupsError {}

impl CameraGroups {
    /// Checks the limits.
    pub fn validate(&self) -> Result<(), GroupsError> {
        let bad = |s: &str| s.trim().is_empty() || s.len() > MAX_NAME_BYTES;
        if self.groups.len() > MAX_GROUPS {
            return Err(GroupsError::Invalid("too many groups"));
        }
        let mut names = BTreeSet::new();
        for g in &self.groups {
            if bad(&g.name) {
                return Err(GroupsError::Invalid("a group name is empty or too long"));
            }
            if !names.insert(g.name.as_str()) {
                return Err(GroupsError::Invalid("two groups have the same name"));
            }
            if g.cameras.len() > MAX_GROUP_CAMERAS {
                return Err(GroupsError::Invalid("too many cameras in a group"));
            }
        }
        if self.camera_names.len() > MAX_CAMERA_NAMES {
            return Err(GroupsError::Invalid("too many camera names"));
        }
        if self.camera_names.iter().any(|n| bad(&n.name)) {
            return Err(GroupsError::Invalid("a camera name is empty or too long"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct GroupFile {
    version: u32,
    revision: u64,
    #[serde(flatten)]
    value: CameraGroups,
}

/// The stored camera groups.
#[derive(Debug)]
pub struct GroupStore {
    path: Option<PathBuf>,
    revision: u64,
    value: CameraGroups,
}

impl GroupStore {
    /// No groups, kept in memory only (no data directory).
    pub fn in_memory() -> GroupStore {
        GroupStore {
            path: None,
            revision: 0,
            value: CameraGroups::default(),
        }
    }

    /// Opens the groups stored at `path`. An unreadable or invalid file is
    /// set aside as `<name>.bad` and the store starts empty.
    pub fn open(path: &Path) -> GroupStore {
        let mut store = GroupStore {
            path: Some(path.to_owned()),
            ..GroupStore::in_memory()
        };
        match read_json::<GroupFile>(path) {
            Ok(Some(f)) if f.version == VERSION && f.value.validate().is_ok() => {
                store.revision = f.revision;
                store.value = f.value;
            }
            Ok(None) => {}
            Ok(Some(_)) | Err(_) => {
                warn!("camera groups unreadable; set aside");
                let mut bad = path.as_os_str().to_owned();
                bad.push(".bad");
                let _ = std::fs::rename(path, PathBuf::from(bad));
            }
        }
        store
    }

    /// The revision (raised by every [`GroupStore::put`]).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The groups and names.
    pub fn get(&self) -> &CameraGroups {
        &self.value
    }

    /// Replaces the whole list; returns the new revision.
    pub fn put(&mut self, value: CameraGroups) -> Result<u64, GroupsError> {
        value.validate()?;
        let revision = self.revision + 1;
        if let Some(p) = &self.path {
            write_json(
                p,
                &GroupFile {
                    version: VERSION,
                    revision,
                    value: value.clone(),
                },
            )
            .map_err(GroupsError::Io)?;
        }
        self.revision = revision;
        self.value = value;
        Ok(revision)
    }

    /// The grouping settings with these groups.
    pub fn grouping(&self, gap: crate::Secs, margin: crate::Secs) -> GroupingConfig {
        GroupingConfig {
            gap,
            margin,
            groups: self.value.groups.clone(),
        }
    }

    /// The user's name for a camera, if set.
    pub fn name_of(&self, camera: &CameraId) -> Option<&str> {
        self.value
            .camera_names
            .iter()
            .find(|n| &n.camera == camera)
            .map(|n| n.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CameraGroups {
        serde_json::from_str(
            r#"{"groups": [{"name": "Group 1", "cameras": [
                 {"device": "d-1", "channel": 3}, {"device": "d-1", "channel": 5, "lens": 1}]}],
               "camera_names": [{"camera": {"device": "d-1", "channel": 3}, "name": "Camera 1"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn stored_reloaded_and_used_for_grouping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groups.json");
        let mut s = GroupStore::open(&path);
        assert_eq!(s.revision(), 0);
        assert_eq!(s.put(sample()).unwrap(), 1);
        let s = GroupStore::open(&path);
        assert_eq!(s.revision(), 1);
        assert_eq!(s.get(), &sample());
        assert_eq!(
            s.get().groups[0].cameras[1],
            CameraId::with_lens("d-1", 5, 1)
        );
        assert_eq!(s.name_of(&CameraId::new("d-1", 3)), Some("Camera 1"));
        assert_eq!(s.name_of(&CameraId::new("d-1", 4)), None);
        let g = s.grouping(10, 5);
        assert_eq!((g.gap, g.margin, g.groups.len()), (10, 5, 1));
    }

    #[test]
    fn limits_are_enforced_and_nothing_is_stored_then() {
        let mut s = GroupStore::in_memory();
        let mut v = sample();
        v.groups.push(v.groups[0].clone());
        assert!(s.put(v).is_err());
        let mut v = sample();
        v.groups[0].name = " ".into();
        assert!(s.put(v).is_err());
        let mut v = sample();
        v.groups[0].cameras = (0..65).map(|c| CameraId::new("d", c)).collect();
        assert!(s.put(v).is_err());
        let mut v = sample();
        v.groups = (0..65)
            .map(|n| CameraGroup {
                name: format!("g{n}"),
                cameras: vec![],
            })
            .collect();
        assert!(s.put(v).is_err());
        let mut v = sample();
        v.camera_names[0].name = "x".repeat(MAX_NAME_BYTES + 1);
        assert!(s.put(v).is_err());
        assert_eq!(s.revision(), 0);
        assert_eq!(s.get(), &CameraGroups::default());
    }

    #[test]
    fn a_damaged_file_is_set_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("groups.json");
        std::fs::write(&path, b"[]").unwrap();
        let s = GroupStore::open(&path);
        assert_eq!(s.get(), &CameraGroups::default());
        assert!(dir.path().join("groups.json.bad").exists());
    }
}
