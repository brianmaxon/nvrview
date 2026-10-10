//! Output names (docs/PLAYBACK.md, "Output"): one folder per incident,
//! named so it sorts by time and reads well, one file per camera, and an
//! `incident.json` for tools and search.
//!
//! ```text
//! 2026-10-10 07.42 vehicle — Front, Side, Gate/
//!   2026-10-10 07.42.05 Front.mp4
//!   2026-10-10 07.42.11 Side.mp4
//!   incident.json
//! ```
//!
//! Names are made safe for every file system the apps run on: characters
//! Windows forbids become `-`, trailing dots and spaces go, and length is
//! capped.

use crate::incident::{CameraId, Incident};
use crate::{Secs, date_time};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Longest folder or file name produced, in characters.
pub const MAX_NAME_CHARS: usize = 120;

/// The pieces of the names, so apps can change the pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamePattern {
    /// Separator between the time and types and the camera names.
    pub separator: String,
    /// Extension of the per-camera files.
    pub extension: String,
}

impl Default for NamePattern {
    fn default() -> Self {
        NamePattern {
            separator: " \u{2014} ".into(),
            extension: "mp4".into(),
        }
    }
}

/// Replaces characters that are not allowed in file names on macOS,
/// Windows, or Linux, collapses whitespace, and trims what Windows rejects
/// at the end of a name.
pub fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        let c = match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        };
        if c == ' ' && out.ends_with(' ') {
            continue;
        }
        out.push(c);
    }
    let mut out: String = out.trim().chars().take(MAX_NAME_CHARS).collect();
    while out.ends_with('.') || out.ends_with(' ') {
        out.pop();
    }
    if out.is_empty() { "_".into() } else { out }
}

fn minute_stamp(t: Secs) -> String {
    let d = date_time(t);
    format!(
        "{:04}-{:02}-{:02} {:02}.{:02}",
        d.year, d.month, d.day, d.hour, d.minute
    )
}

fn second_stamp(t: Secs) -> String {
    let d = date_time(t);
    format!(
        "{:04}-{:02}-{:02} {:02}.{:02}.{:02}",
        d.year, d.month, d.day, d.hour, d.minute, d.second
    )
}

/// The incident's folder name: start time to the minute, the types (or the
/// user's name for the incident), and the camera names.
pub fn folder_name(inc: &Incident, camera_names: &[String], pattern: &NamePattern) -> String {
    let what = match &inc.name {
        Some(n) => n.clone(),
        None => inc.kinds().into_iter().collect::<Vec<_>>().join(", "),
    };
    let mut s = minute_stamp(inc.start());
    if !what.is_empty() {
        s.push(' ');
        s.push_str(&what);
    }
    if !camera_names.is_empty() {
        s.push_str(&pattern.separator);
        s.push_str(&camera_names.join(", "));
    }
    sanitize(&s)
}

/// One camera's file name: its range start to the second and its name.
pub fn camera_file_name(start: Secs, camera_name: &str, pattern: &NamePattern) -> String {
    let base = sanitize(&format!("{} {camera_name}", second_stamp(start)));
    format!("{base}.{}", pattern.extension)
}

/// A time as `incident.json` writes it: device-local, no zone.
pub fn json_time(t: Secs) -> String {
    date_time(t).to_string()
}

/// One camera in `incident.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentCamera {
    /// Device identifier.
    pub device: String,
    /// Channel.
    pub channel: u8,
    /// The user's name for the camera.
    pub name: String,
    /// Start of the downloaded range.
    pub start: String,
    /// End of the downloaded range.
    pub end: String,
    /// File name in the folder.
    pub file: String,
}

/// One event in `incident.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentEventJson {
    /// Device identifier.
    pub device: String,
    /// Channel.
    pub channel: u8,
    /// Type as the device names it.
    #[serde(rename = "type")]
    pub kind: String,
    /// Start.
    pub start: String,
    /// End.
    pub end: String,
}

/// The contents of `incident.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentJson {
    /// Format version (1).
    pub version: u32,
    /// Incident ID.
    pub id: String,
    /// The user's name, if any.
    pub name: Option<String>,
    /// Camera group, if any.
    pub group: Option<String>,
    /// Event types.
    pub types: Vec<String>,
    /// First event start (device-local time, no zone).
    pub start: String,
    /// Last event end.
    pub end: String,
    /// Cameras with their files.
    pub cameras: Vec<IncidentCamera>,
    /// Events.
    pub events: Vec<IncidentEventJson>,
}

/// Builds `incident.json` for an incident and its per-camera ranges and
/// names.
pub fn incident_json(
    inc: &Incident,
    ranges: &BTreeMap<CameraId, (Secs, Secs)>,
    names: &BTreeMap<CameraId, String>,
    pattern: &NamePattern,
) -> IncidentJson {
    IncidentJson {
        version: 1,
        id: inc.id.clone(),
        name: inc.name.clone(),
        group: inc.group.clone(),
        types: inc.kinds().into_iter().collect(),
        start: json_time(inc.start()),
        end: json_time(inc.end()),
        cameras: ranges
            .iter()
            .map(|(c, (s, e))| {
                let name = names
                    .get(c)
                    .cloned()
                    .unwrap_or_else(|| format!("Channel {}", u32::from(c.channel) + 1));
                IncidentCamera {
                    device: c.device.clone(),
                    channel: c.channel,
                    file: camera_file_name(*s, &name, pattern),
                    name,
                    start: json_time(*s),
                    end: json_time(*e),
                }
            })
            .collect(),
        events: inc
            .events
            .iter()
            .map(|e| IncidentEventJson {
                device: e.camera.device.clone(),
                channel: e.camera.channel,
                kind: e.kind.clone(),
                start: json_time(e.start),
                end: json_time(e.end),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::incident::{CameraEvent, GroupingConfig, group};
    use crate::secs;
    use bc_protocol::archive::DateTime;

    #[test]
    fn names_read_and_sort_well() {
        let t = secs(DateTime::new(2026, 10, 10, 7, 42, 5).unwrap());
        let evs = vec![CameraEvent {
            start: t,
            end: t + 20,
            camera: CameraId::new("d", 0),
            kind: "vehicle".into(),
        }];
        let inc = &group(&evs, &GroupingConfig::default())[0];
        let p = NamePattern::default();
        let names = vec!["Front".to_string(), "Side".into(), "Gate".into()];
        assert_eq!(
            folder_name(inc, &names, &p),
            "2026-10-10 07.42 vehicle \u{2014} Front, Side, Gate"
        );
        assert_eq!(
            camera_file_name(t, "Front", &p),
            "2026-10-10 07.42.05 Front.mp4"
        );
        let mut renamed = inc.clone();
        renamed.name = Some("Delivery: van/truck?".into());
        assert_eq!(
            folder_name(&renamed, &[], &p),
            "2026-10-10 07.42 Delivery- van-truck-"
        );
    }

    #[test]
    fn sanitize_is_safe_everywhere() {
        assert_eq!(sanitize("a<b>c:d\"e|f?g*h\\i/j"), "a-b-c-d-e-f-g-h-i-j");
        assert_eq!(sanitize("  name.  "), "name");
        assert_eq!(sanitize("tab\there\nnew"), "tab here new");
        assert_eq!(sanitize("..."), "_");
        assert_eq!(sanitize(&"x".repeat(500)).chars().count(), MAX_NAME_CHARS);
    }
}
