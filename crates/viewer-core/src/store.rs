//! What the core keeps between runs, in the app's data directory: layouts
//! (split trees, docs/FFI.md "Layouts") and the stable key of each device
//! the app has added. (Second-lens probe answers live in `lenses.json`,
//! written the same way by `crate::lens`.)
//!
//! Each is one small JSON file, rewritten whole and atomically (a temporary
//! file in the same directory, flushed, then renamed over the old one), so
//! a crash leaves either the old or the new file, never a torn one. A file
//! that cannot be read is moved aside (`*.unreadable`) and the store starts
//! empty. Without a data directory both stores live in memory only.

use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use tracing::warn;

/// Most layouts stored.
pub const MAX_LAYOUTS: usize = 500;
/// Largest layout, as JSON text.
pub const MAX_LAYOUT_BYTES: usize = 64 * 1024;
/// Deepest split tree (splits from the root to a leaf).
pub const MAX_DEPTH: usize = 16;
/// Most leaves (tiles) in one layout.
pub const MAX_LEAVES: usize = 64;
/// Longest layout ID, name, or device key, in bytes.
pub const MAX_TEXT: usize = 256;
/// Smallest and largest split ratio.
pub const RATIO_RANGE: std::ops::RangeInclusive<f64> = 0.02..=0.98;

const LAYOUTS_FILE: &str = "layouts.json";
const DEVICES_FILE: &str = "devices.json";
const FORMAT: u64 = 1;

/// Why a layout was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutError {
    /// Not a valid layout; the text says what is wrong.
    Invalid(String),
    /// [`MAX_LAYOUTS`] are stored already.
    TooMany,
    /// No layout has this ID.
    Unknown,
    /// The file could not be written; nothing changed.
    Io,
}

fn invalid<T>(what: impl Into<String>) -> Result<T, LayoutError> {
    Err(LayoutError::Invalid(what.into()))
}

fn text<'a>(o: &'a Map<String, Value>, key: &str, empty_ok: bool) -> Result<&'a str, LayoutError> {
    match o.get(key) {
        Some(Value::String(s)) if s.len() <= MAX_TEXT && (empty_ok || !s.is_empty()) => Ok(s),
        Some(Value::String(_)) => invalid(format!("\"{key}\" is empty or too long")),
        _ => invalid(format!("\"{key}\" must be a string")),
    }
}

/// Checks one node of a split tree: a leaf `{"camera": null}` or
/// `{"camera": {"device": key, "channel": n, "lens": l}}` (`lens` optional:
/// 0, the default, is a camera's first lens, 1 the second lens of a
/// multi-lens camera), or a split
/// `{"split": "row"|"col", "ratio": r, "a": node, "b": node}`. Other
/// members are kept as they are (apps may store their own settings).
fn check_node(v: &Value, depth: usize, leaves: &mut usize) -> Result<(), LayoutError> {
    let Value::Object(o) = v else {
        return invalid("a node must be an object");
    };
    match (o.get("camera"), o.get("split")) {
        (Some(_), Some(_)) => invalid("a node is either a camera or a split"),
        (Some(cam), None) => {
            *leaves += 1;
            if *leaves > MAX_LEAVES {
                return invalid(format!("more than {MAX_LEAVES} tiles"));
            }
            match cam {
                Value::Null => Ok(()),
                Value::Object(c) => {
                    text(c, "device", false)?;
                    match c.get("channel").and_then(Value::as_u64) {
                        Some(ch) if ch <= u64::from(u8::MAX) => {}
                        _ => return invalid("\"channel\" must be an integer from 0 to 255"),
                    }
                    match c.get("lens") {
                        None => Ok(()),
                        Some(l) if l.as_u64().is_some_and(|l| l <= 1) => Ok(()),
                        Some(_) => invalid("\"lens\" must be 0 or 1"),
                    }
                }
                _ => invalid("\"camera\" must be null or an object"),
            }
        }
        (None, Some(split)) => {
            if depth >= MAX_DEPTH {
                return invalid(format!("splits nested deeper than {MAX_DEPTH}"));
            }
            if split != "row" && split != "col" {
                return invalid("\"split\" must be \"row\" or \"col\"");
            }
            match o.get("ratio").and_then(Value::as_f64) {
                Some(r) if RATIO_RANGE.contains(&r) => {}
                _ => return invalid("\"ratio\" must be a number from 0.02 to 0.98"),
            }
            for side in ["a", "b"] {
                match o.get(side) {
                    Some(n) => check_node(n, depth + 1, leaves)?,
                    None => return invalid(format!("a split needs \"{side}\"")),
                }
            }
            Ok(())
        }
        (None, None) => invalid("a node needs \"camera\" or \"split\""),
    }
}

/// Validates a layout and returns its ID. `version` is the core's and is
/// ignored here.
pub fn validate(v: &Value) -> Result<String, LayoutError> {
    let Value::Object(o) = v else {
        return invalid("a layout must be an object");
    };
    let id = text(o, "id", false)?.to_owned();
    text(o, "name", true)?;
    let Some(root) = o.get("root") else {
        return invalid("a layout needs \"root\"");
    };
    check_node(root, 0, &mut 0)?;
    Ok(id)
}

/// Writes `value` to `dir/name` atomically.
pub(crate) fn write_atomic(dir: &Path, name: &str, value: &Value) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{name}.tmp"));
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dir.join(name))
}

/// Reads `dir/name`; `None` when missing. An unreadable file is moved
/// aside so it is neither lost nor read again.
pub(crate) fn read(dir: &Path, name: &str) -> Option<Value> {
    let path = dir.join(name);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!(file = name, error = %e, "cannot read stored data");
            return None;
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(v) => Some(v),
        Err(e) => {
            warn!(file = name, error = %e, "stored data is unreadable; moved aside");
            let _ = fs::rename(&path, dir.join(format!("{name}.unreadable")));
            None
        }
    }
}

/// The layouts, in the order they were first stored.
#[derive(Debug)]
pub struct Layouts {
    dir: Option<PathBuf>,
    layouts: Vec<Value>,
    /// Bumped on every change.
    revision: u64,
}

impl Layouts {
    /// Loads the stored layouts (invalid entries are dropped with a warning).
    pub fn open(dir: Option<PathBuf>) -> Layouts {
        let mut layouts = Vec::new();
        let mut revision = 0;
        if let Some(Value::Object(o)) = dir.as_deref().and_then(|d| read(d, LAYOUTS_FILE)) {
            revision = o.get("revision").and_then(Value::as_u64).unwrap_or(0);
            if let Some(Value::Array(list)) = o.get("layouts") {
                for l in list {
                    match validate(l) {
                        Ok(id) if !layouts.iter().any(|x: &Value| x["id"] == id.as_str()) => {
                            layouts.push(l.clone());
                        }
                        Ok(_) => warn!("duplicate stored layout dropped"),
                        Err(e) => warn!(?e, "invalid stored layout dropped"),
                    }
                    if layouts.len() == MAX_LAYOUTS {
                        break;
                    }
                }
            }
        }
        Layouts {
            dir,
            layouts,
            revision,
        }
    }

    /// Every layout as `{"revision": n, "layouts": [...]}`.
    pub fn to_json(&self) -> String {
        json!({"revision": self.revision, "layouts": self.layouts}).to_string()
    }

    /// The store's revision (bumped on every change).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn save(&self, layouts: &[Value], revision: u64) -> Result<(), LayoutError> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let doc = json!({"format": FORMAT, "revision": revision, "layouts": layouts});
        write_atomic(dir, LAYOUTS_FILE, &doc).map_err(|e| {
            warn!(error = %e, "cannot store layouts");
            LayoutError::Io
        })
    }

    /// Inserts or replaces (by `id`) a layout given as JSON text. The core
    /// sets `version`: 1 for a new layout, one more on each replacement.
    /// Returns the stored version.
    pub fn put(&mut self, text: &str) -> Result<u64, LayoutError> {
        if text.len() > MAX_LAYOUT_BYTES {
            return invalid(format!("larger than {MAX_LAYOUT_BYTES} bytes"));
        }
        let mut v: Value =
            serde_json::from_str(text).map_err(|e| LayoutError::Invalid(e.to_string()))?;
        let id = validate(&v)?;
        let mut next = self.layouts.clone();
        let version = match next.iter().position(|l| l["id"] == id.as_str()) {
            Some(i) => {
                let version = next[i]["version"].as_u64().unwrap_or(0) + 1;
                v["version"] = json!(version);
                next[i] = v;
                version
            }
            None => {
                if next.len() >= MAX_LAYOUTS {
                    return Err(LayoutError::TooMany);
                }
                v["version"] = json!(1);
                next.push(v);
                1
            }
        };
        self.save(&next, self.revision + 1)?;
        self.layouts = next;
        self.revision += 1;
        Ok(version)
    }

    /// Deletes a layout by ID.
    pub fn delete(&mut self, id: &str) -> Result<(), LayoutError> {
        let mut next = self.layouts.clone();
        let before = next.len();
        next.retain(|l| l["id"] != id);
        if next.len() == before {
            return Err(LayoutError::Unknown);
        }
        self.save(&next, self.revision + 1)?;
        self.layouts = next;
        self.revision += 1;
        Ok(())
    }
}

/// Stable device keys: one per device address (host name or address, any
/// case, and port), assigned the first time a device is added and kept in
/// `devices.json`.
#[derive(Debug)]
pub struct DeviceKeys {
    dir: Option<PathBuf>,
    keys: HashMap<String, String>,
}

/// How a device address is looked up.
fn address(host: &str, port: u16) -> String {
    format!("{}:{port}", host.trim().to_ascii_lowercase())
}

/// A new random key such as `d-3f9a0c1b7e2d4a65`.
fn new_key(taken: &HashMap<String, String>) -> String {
    use std::hash::{BuildHasher, Hasher};
    loop {
        // RandomState is seeded randomly per process; mixing in the time
        // and a counter gives distinct keys across runs and calls.
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        h.write_usize(taken.len());
        let key = format!("d-{:016x}", h.finish());
        if !taken.values().any(|k| *k == key) {
            return key;
        }
    }
}

impl DeviceKeys {
    /// Loads the stored keys.
    pub fn open(dir: Option<PathBuf>) -> DeviceKeys {
        let mut keys = HashMap::new();
        if let Some(Value::Object(o)) = dir.as_deref().and_then(|d| read(d, DEVICES_FILE))
            && let Some(Value::Array(list)) = o.get("devices")
        {
            for d in list {
                let (Some(host), Some(port), Some(key)) = (
                    d["host"].as_str(),
                    d["port"].as_u64().and_then(|p| u16::try_from(p).ok()),
                    d["key"].as_str(),
                ) else {
                    continue;
                };
                if !key.is_empty() && key.len() <= MAX_TEXT {
                    keys.insert(address(host, port), key.to_owned());
                }
            }
        }
        DeviceKeys { dir, keys }
    }

    /// The key of the device at `host` and `port`, assigned (and stored)
    /// on first use. A key that cannot be stored still holds for this run.
    pub fn key_for(&mut self, host: &str, port: u16) -> String {
        let addr = address(host, port);
        if let Some(k) = self.keys.get(&addr) {
            return k.clone();
        }
        let key = new_key(&self.keys);
        self.keys.insert(addr, key.clone());
        if let Some(dir) = &self.dir {
            let mut list: Vec<(&String, &String)> = self.keys.iter().collect();
            list.sort();
            let devices: Vec<Value> = list
                .into_iter()
                .filter_map(|(addr, key)| {
                    let (host, port) = addr.rsplit_once(':')?;
                    Some(json!({"host": host, "port": port.parse::<u16>().ok()?, "key": key}))
                })
                .collect();
            let doc = json!({"format": FORMAT, "devices": devices});
            if let Err(e) = write_atomic(dir, DEVICES_FILE, &doc) {
                warn!(error = %e, "cannot store device keys");
            }
        }
        key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTRANCES: &str = r#"{"id":"entrances","name":"Entrances","version":7,
        "root":{"split":"row","ratio":0.62,
          "a":{"camera":{"device":"d-1","channel":3}},
          "b":{"split":"col","ratio":0.34,"a":{"camera":null},
               "b":{"camera":{"device":"d-1","channel":9},"fill":true}}}}"#;

    #[test]
    fn layouts_validate_version_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = Layouts::open(Some(dir.path().to_owned()));
        assert_eq!(l.put(ENTRANCES), Ok(1));
        assert_eq!(l.put(ENTRANCES), Ok(2), "replaced by id");
        let mut two: Value = serde_json::from_str(ENTRANCES).unwrap();
        two["id"] = json!("two");
        assert_eq!(l.put(&two.to_string()), Ok(1));
        assert_eq!(l.revision(), 3);

        let again = Layouts::open(Some(dir.path().to_owned()));
        let v: Value = serde_json::from_str(&again.to_json()).unwrap();
        assert_eq!(v["revision"], 3);
        assert_eq!(v["layouts"][0]["id"], "entrances");
        assert_eq!(v["layouts"][0]["version"], 2);
        // Members the core does not know are kept.
        assert_eq!(v["layouts"][0]["root"]["b"]["b"]["fill"], true);
        assert_eq!(v["layouts"][1]["id"], "two");

        let mut again = again;
        assert_eq!(again.delete("two"), Ok(()));
        assert_eq!(again.delete("two"), Err(LayoutError::Unknown));
        let v: Value =
            serde_json::from_str(&Layouts::open(Some(dir.path().to_owned())).to_json()).unwrap();
        assert_eq!(v["layouts"].as_array().unwrap().len(), 1);
        assert!(!dir.path().join("layouts.json.tmp").exists());
    }

    #[test]
    fn bad_layouts_are_refused() {
        let mut l = Layouts::open(None);
        for bad in [
            "not json",
            "[]",
            r#"{"name":"x","root":{"camera":null}}"#,
            r#"{"id":"","name":"x","root":{"camera":null}}"#,
            r#"{"id":"x","name":"x"}"#,
            r#"{"id":"x","name":"x","root":{}}"#,
            r#"{"id":"x","name":"x","root":{"camera":null,"split":"row"}}"#,
            r#"{"id":"x","name":"x","root":{"camera":{"device":"d","channel":256}}}"#,
            r#"{"id":"x","name":"x","root":{"camera":{"channel":1}}}"#,
            r#"{"id":"x","name":"x","root":{"camera":{"device":"d","channel":1,"lens":2}}}"#,
            r#"{"id":"x","name":"x","root":{"camera":{"device":"d","channel":1,"lens":"1"}}}"#,
            r#"{"id":"x","name":"x","root":{"split":"diagonal","ratio":0.5,"a":{"camera":null},"b":{"camera":null}}}"#,
            r#"{"id":"x","name":"x","root":{"split":"row","ratio":1.5,"a":{"camera":null},"b":{"camera":null}}}"#,
            r#"{"id":"x","name":"x","root":{"split":"row","ratio":0.5,"a":{"camera":null}}}"#,
        ] {
            assert!(
                matches!(l.put(bad), Err(LayoutError::Invalid(_))),
                "accepted {bad}"
            );
        }
        // Too deep.
        let mut node = json!({"camera": null});
        for _ in 0..=MAX_DEPTH {
            node = json!({"split": "row", "ratio": 0.5, "a": node, "b": {"camera": null}});
        }
        let deep = json!({"id": "deep", "name": "", "root": node});
        assert!(matches!(
            l.put(&deep.to_string()),
            Err(LayoutError::Invalid(_))
        ));
        assert_eq!(l.revision(), 0);
        assert_eq!(l.to_json(), r#"{"layouts":[],"revision":0}"#);
        // The second lens of a multi-lens camera.
        assert!(
            l.put(r#"{"id":"x","name":"x","root":{"camera":{"device":"d","channel":1,"lens":1}}}"#)
                .is_ok()
        );
    }

    #[test]
    fn the_layout_count_is_capped() {
        let mut l = Layouts::open(None);
        for i in 0..MAX_LAYOUTS {
            let v = json!({"id": format!("l{i}"), "name": "", "root": {"camera": null}});
            l.put(&v.to_string()).unwrap();
        }
        let v = json!({"id": "one-more", "name": "", "root": {"camera": null}});
        assert_eq!(l.put(&v.to_string()), Err(LayoutError::TooMany));
        // Replacing still works.
        let v = json!({"id": "l0", "name": "renamed", "root": {"camera": null}});
        assert_eq!(l.put(&v.to_string()), Ok(2));
    }

    #[test]
    fn an_unreadable_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("layouts.json"), b"{ torn").unwrap();
        let l = Layouts::open(Some(dir.path().to_owned()));
        assert_eq!(l.revision(), 0);
        assert!(dir.path().join("layouts.json.unreadable").exists());
    }

    #[test]
    fn device_keys_are_stable_per_address() {
        let dir = tempfile::tempdir().unwrap();
        let mut k = DeviceKeys::open(Some(dir.path().to_owned()));
        let a = k.key_for("192.0.2.10", 9000);
        let b = k.key_for("nvr.example", 9000);
        assert_ne!(a, b);
        assert!(a.starts_with("d-") && a.len() == 18);
        assert_eq!(k.key_for("192.0.2.10", 9000), a);
        assert_eq!(k.key_for("NVR.example ", 9000), b, "any case");
        assert_ne!(k.key_for("192.0.2.10", 9001), a, "another port");
        let mut again = DeviceKeys::open(Some(dir.path().to_owned()));
        assert_eq!(again.key_for("192.0.2.10", 9000), a);
        assert_eq!(again.key_for("nvr.example", 9000), b);
        // In memory only, still stable within the run.
        let mut mem = DeviceKeys::open(None);
        let m = mem.key_for("192.0.2.10", 9000);
        assert_eq!(mem.key_for("192.0.2.10", 9000), m);
    }
}
