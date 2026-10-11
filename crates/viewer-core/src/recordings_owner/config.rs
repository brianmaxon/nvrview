//! The JSON the app passes to configure recordings and to enable a device
//! (docs/FFI-RECORDINGS.md, section 1), parsed and checked. Unknown members
//! are ignored; a member of the wrong type or out of range refuses the whole
//! call ([`RecError::InvalidArgument`]).

use super::RecError;
use recordings::index::IndexConfig;
use recordings::naming::NamePattern;
use recordings::thumbs::ThumbCacheConfig;
use recordings::{DeviceCamera, Secs};
use serde_json::{Map, Value};
use std::path::PathBuf;
use std::time::Duration;

/// `nv_recordings_configure`'s settings, with the defaults of section 1.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Settings {
    /// Where downloads go; without it, downloads are not available.
    pub downloads_dir: Option<PathBuf>,
    /// Index limits per device.
    pub index: IndexConfig,
    /// How old a held today may get before it is searched again.
    pub refresh_today_s: Secs,
    /// Grouping gap.
    pub gap_s: Secs,
    /// Grouping margin.
    pub margin_s: Secs,
    /// Thumbnail cache limits.
    pub thumbnails: ThumbCacheConfig,
    /// Download settings.
    pub downloads: DownloadSettings,
}

/// The `downloads` and `naming` members: what the scheduler is made with.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DownloadSettings {
    pub chunk_s: Secs,
    pub per_device: usize,
    pub max_attempts: u32,
    pub naming: NamePattern,
}

/// Which cameras of a device may be searched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Channels {
    /// Every connected channel the device lists, first lens only.
    All,
    /// These cameras.
    List(Vec<DeviceCamera>),
}

/// The optional background fill of one device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Background {
    pub days_back: u32,
    pub pass_interval: Duration,
}

/// `nv_recordings_enable`'s JSON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Enable {
    pub channels: Channels,
    /// Seconds east of UTC (only used to know the device's "today").
    pub utc_offset_s: i64,
    pub background: Option<Background>,
}

/// Most days a background block may cover (the index keeps 90 by default;
/// a longer fill would only be pruned again).
const MAX_DAYS_BACK: u64 = 366;

fn object(json: &str) -> Result<Map<String, Value>, RecError> {
    match serde_json::from_str::<Value>(json) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(RecError::InvalidArgument),
    }
}

fn section<'a>(
    m: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a Map<String, Value>>, RecError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(o)) => Ok(Some(o)),
        Some(_) => Err(RecError::InvalidArgument),
    }
}

/// A whole number in `min..=max`, or `default` when absent.
fn uint(
    m: &Map<String, Value>,
    key: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, RecError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| (min..=max).contains(n))
            .ok_or(RecError::InvalidArgument),
    }
}

fn text(m: &Map<String, Value>, key: &str, default: &str) -> Result<String, RecError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(default.to_owned()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(RecError::InvalidArgument),
    }
}

/// Parses `nv_recordings_configure`'s JSON.
pub(super) fn parse_settings(json: &str) -> Result<Settings, RecError> {
    let m = object(json)?;
    let downloads_dir = match m.get("downloads_dir") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => {
            let p = PathBuf::from(s);
            if !p.is_absolute() {
                return Err(RecError::InvalidArgument);
            }
            Some(p)
        }
        Some(_) => return Err(RecError::InvalidArgument),
    };
    let empty = Map::new();
    let index = section(&m, "index")?.unwrap_or(&empty);
    let d = IndexConfig::default();
    let index_cfg = IndexConfig {
        max_age_days: uint(index, "max_age_days", d.max_age_days.into(), 1, 3650)? as u32,
        max_bytes: uint(index, "max_bytes", d.max_bytes, 1 << 20, 1 << 40)?,
    };
    let refresh_today_s = uint(index, "refresh_today_s", 60, 5, 86_400)? as Secs;
    let grouping = section(&m, "grouping")?.unwrap_or(&empty);
    let gap_s = uint(grouping, "gap_s", 10, 0, 3600)? as Secs;
    let margin_s = uint(grouping, "margin_s", 5, 0, 3600)? as Secs;
    let thumbs = section(&m, "thumbnails")?.unwrap_or(&empty);
    let t = ThumbCacheConfig::default();
    let thumbnails = ThumbCacheConfig {
        max_entries: uint(thumbs, "max_entries", t.max_entries as u64, 1, 1 << 20)? as usize,
        max_bytes: uint(thumbs, "max_bytes", t.max_bytes, 1 << 16, 1 << 40)?,
    };
    let dl = section(&m, "downloads")?.unwrap_or(&empty);
    let naming = section(&m, "naming")?.unwrap_or(&empty);
    let n = NamePattern::default();
    let pattern = NamePattern {
        separator: text(naming, "separator", &n.separator)?,
        extension: text(naming, "extension", &n.extension)?,
    };
    if pattern.separator.len() > 16
        || pattern.extension.is_empty()
        || pattern.extension.len() > 8
        || !pattern.extension.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(RecError::InvalidArgument);
    }
    let downloads = DownloadSettings {
        chunk_s: uint(dl, "chunk_s", 60, 5, 3600)? as Secs,
        per_device: uint(dl, "per_device", 1, 1, 4)? as usize,
        max_attempts: uint(dl, "max_attempts", 8, 1, 100)? as u32,
        naming: pattern,
    };
    // `playback` is read by playback (C10); only its types are checked here.
    if let Some(p) = section(&m, "playback")? {
        uint(p, "max_plays_per_device", 2, 1, 4)?;
        uint(p, "buffer_s", 8, 1, 120)?;
        uint(p, "buffer_bytes", 32 << 20, 1 << 20, 1 << 32)?;
    }
    Ok(Settings {
        downloads_dir,
        index: index_cfg,
        refresh_today_s,
        gap_s,
        margin_s,
        thumbnails,
        downloads,
    })
}

/// A camera object: `{"device": "...", "channel": 3, "lens": 0}` (lens
/// optional). Returns the device key and the camera.
pub(super) fn camera(v: &Value) -> Result<(String, DeviceCamera), RecError> {
    let o = v.as_object().ok_or(RecError::InvalidArgument)?;
    let device = match o.get("device") {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Err(RecError::InvalidArgument),
    };
    let channel = o
        .get("channel")
        .and_then(Value::as_u64)
        .filter(|c| *c <= u64::from(u8::MAX))
        .ok_or(RecError::InvalidArgument)? as u8;
    let lens = uint(o, "lens", 0, 0, 1)? as u8;
    Ok((device, DeviceCamera::new(channel, lens)))
}

/// A camera object given alone as JSON text.
pub(super) fn camera_text(json: &str) -> Result<(String, DeviceCamera), RecError> {
    let v: Value = serde_json::from_str(json).map_err(|_| RecError::InvalidArgument)?;
    camera(&v)
}

/// A list of camera objects (`"cameras": [...]`; empty when absent).
pub(super) fn cameras(m: &Map<String, Value>) -> Result<Vec<(String, DeviceCamera)>, RecError> {
    match m.get("cameras") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a.iter().map(camera).collect(),
        Some(_) => Err(RecError::InvalidArgument),
    }
}

/// Parses `nv_recordings_enable`'s JSON.
pub(super) fn parse_enable(json: &str) -> Result<Enable, RecError> {
    let m = object(json)?;
    let channels = match m.get("channels") {
        None | Some(Value::Null) => Channels::All,
        Some(Value::String(s)) if s == "all" => Channels::All,
        Some(Value::Array(a)) => {
            let mut v = Vec::new();
            for c in a {
                let o = c.as_object().ok_or(RecError::InvalidArgument)?;
                let channel = o
                    .get("channel")
                    .and_then(Value::as_u64)
                    .filter(|c| *c <= u64::from(u8::MAX))
                    .ok_or(RecError::InvalidArgument)? as u8;
                let lens = uint(o, "lens", 0, 0, 1)? as u8;
                let cam = DeviceCamera::new(channel, lens);
                if !v.contains(&cam) {
                    v.push(cam);
                }
            }
            if v.len() > 512 {
                return Err(RecError::Limit);
            }
            Channels::List(v)
        }
        Some(_) => return Err(RecError::InvalidArgument),
    };
    let utc_offset_s = match m.get("utc_offset_s") {
        None | Some(Value::Null) => 0,
        Some(v) => v
            .as_i64()
            .filter(|o| o.abs() <= 18 * 3600)
            .ok_or(RecError::InvalidArgument)?,
    };
    let background = match section(&m, "background")? {
        None => None,
        Some(b) => Some(Background {
            days_back: uint(b, "days_back", 14, 1, MAX_DAYS_BACK)? as u32,
            pass_interval: Duration::from_secs(uint(b, "pass_interval_s", 60, 10, 86_400)?),
        }),
    };
    Ok(Enable {
        channels,
        utc_offset_s,
        background,
    })
}

/// The JSON object of a request or query (anything else is refused).
pub(super) fn request(json: &str) -> Result<Map<String, Value>, RecError> {
    object(json)
}

/// A required integer member (seconds).
pub(super) fn secs(m: &Map<String, Value>, key: &str) -> Result<Secs, RecError> {
    m.get(key)
        .and_then(Value::as_i64)
        .filter(|s| (0..=253_402_300_799).contains(s))
        .ok_or(RecError::InvalidArgument)
}

/// An optional boolean member.
pub(super) fn flag(m: &Map<String, Value>, key: &str) -> Result<bool, RecError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(RecError::InvalidArgument),
    }
}

/// An optional list of strings (`kinds`).
pub(super) fn strings(m: &Map<String, Value>, key: &str) -> Result<Vec<String>, RecError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or(RecError::InvalidArgument)
            })
            .collect(),
        Some(_) => Err(RecError::InvalidArgument),
    }
}

/// `limit` of a query: default 5000, at most 100000.
pub(super) fn limit(m: &Map<String, Value>) -> Result<usize, RecError> {
    Ok(uint(m, "limit", 5000, 1, 100_000)? as usize)
}

/// A tier name (`main` or `sub`), or `default` when absent.
pub(super) fn tier(
    m: &Map<String, Value>,
    default: recordings::Tier,
) -> Result<recordings::Tier, RecError> {
    match m.get("tier") {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(s)) if s == "main" => Ok(recordings::Tier::Main),
        Some(Value::String(s)) if s == "sub" => Ok(recordings::Tier::Sub),
        Some(_) => Err(RecError::InvalidArgument),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_refusals() {
        let s = parse_settings("{}").unwrap();
        assert_eq!(s.downloads_dir, None);
        assert_eq!(s.index, IndexConfig::default());
        assert_eq!(s.refresh_today_s, 60);
        assert_eq!((s.gap_s, s.margin_s), (10, 5));
        assert_eq!(s.downloads.chunk_s, 60);
        assert_eq!(s.thumbnails, ThumbCacheConfig::default());
        assert!(parse_settings("[]").is_err());
        assert!(parse_settings(r#"{"downloads_dir":"relative"}"#).is_err());
        assert!(parse_settings(r#"{"index":{"max_age_days":0}}"#).is_err());
        assert!(parse_settings(r#"{"index":{"max_age_days":"9"}}"#).is_err());
        assert!(parse_settings(r#"{"naming":{"extension":"m/4"}}"#).is_err());
        let s = parse_settings(r#"{"grouping":{"gap_s":30},"future":{"x":1}}"#).unwrap();
        assert_eq!(s.gap_s, 30);

        let e = parse_enable("{}").unwrap();
        assert_eq!(e.channels, Channels::All);
        assert_eq!(e.background, None, "no background fill by default");
        let e = parse_enable(
            r#"{"channels":[{"channel":0},{"channel":3,"lens":1}],"utc_offset_s":7200,
                "background":{"days_back":3}}"#,
        )
        .unwrap();
        assert_eq!(
            e.channels,
            Channels::List(vec![DeviceCamera::new(0, 0), DeviceCamera::new(3, 1)])
        );
        assert_eq!(e.utc_offset_s, 7200);
        assert_eq!(e.background.unwrap().days_back, 3);
        assert!(parse_enable(r#"{"channels":[{"channel":0,"lens":2}]}"#).is_err());
        assert!(parse_enable(r#"{"channels":"some"}"#).is_err());
    }

    #[test]
    fn cameras() {
        let v: Value = serde_json::from_str(r#"{"device":"d-1","channel":3}"#).unwrap();
        assert_eq!(camera(&v).unwrap(), ("d-1".into(), DeviceCamera::new(3, 0)));
        assert!(camera_text(r#"{"device":"","channel":3}"#).is_err());
        assert!(camera_text(r#"{"device":"d","channel":300}"#).is_err());
        assert!(camera_text(r#"{"device":"d","channel":1,"lens":1}"#).is_ok());
    }
}
