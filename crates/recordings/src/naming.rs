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

pub(crate) fn minute_stamp(t: Secs) -> String {
    let d = date_time(t);
    format!(
        "{:04}-{:02}-{:02} {:02}.{:02}",
        d.year, d.month, d.day, d.hour, d.minute
    )
}

pub(crate) fn second_stamp(t: Secs) -> String {
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

/// A time in milliseconds as a file name stamp, `2026-10-10 07.42.11.250`
/// (device-local; used for still images, which can differ by less than a
/// second).
pub fn millis_stamp(ms: i64) -> String {
    format!(
        "{}.{:03}",
        second_stamp(ms.div_euclid(1000)),
        ms.rem_euclid(1000)
    )
}

/// The neutral name of the camera at `index` for share-safe output:
/// `Camera A`, `Camera B`, ... `Camera Z`, `Camera AA`, ... It stands in for
/// the user's name for the camera, in the order the request lists cameras.
pub fn neutral_camera_name(index: usize) -> String {
    let mut letters = Vec::new();
    let mut n = index;
    loop {
        letters.push(b'A' + (n % 26) as u8);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    letters.reverse();
    format!("Camera {}", String::from_utf8_lossy(&letters))
}

/// The folder name of a range download (docs/FFI-RECORDINGS.md, section
/// 7a): `2026-10-10 07.42\u{2013}07.52 range \u{2014} Front, Side`, or
/// `2026-10-10 07.42\u{2013}07.52 <name>` when the user named it. A range
/// that ends on another day writes that day's date before its end time. The
/// end is the minute that contains `end`.
pub fn range_folder_name(
    start: Secs,
    end: Secs,
    name: Option<&str>,
    camera_names: &[String],
    pattern: &NamePattern,
) -> String {
    let (s, e) = (date_time(start), date_time(end));
    let mut out = minute_stamp(start);
    out.push('\u{2013}');
    if (s.year, s.month, s.day) == (e.year, e.month, e.day) {
        out.push_str(&format!("{:02}.{:02}", e.hour, e.minute));
    } else {
        out.push_str(&minute_stamp(end));
    }
    match name {
        Some(n) => {
            out.push(' ');
            out.push_str(n);
        }
        None => {
            out.push_str(" range");
            if !camera_names.is_empty() {
                out.push_str(&pattern.separator);
                out.push_str(&camera_names.join(", "));
            }
        }
    }
    sanitize(&out)
}

/// A time as `incident.json` writes it: device-local, no zone.
pub fn json_time(t: Secs) -> String {
    date_time(t).to_string()
}

fn is_zero(n: &u8) -> bool {
    *n == 0
}

/// One camera in `incident.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncidentCamera {
    /// Device identifier.
    pub device: String,
    /// Channel.
    pub channel: u8,
    /// Lens of a multi-lens camera (written only for a second lens).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub lens: u8,
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
    /// Lens of a multi-lens camera (written only for a second lens).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub lens: u8,
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
                    lens: c.lens,
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
                lens: e.camera.lens,
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
    fn neutral_names_count_like_spreadsheet_columns() {
        assert_eq!(neutral_camera_name(0), "Camera A");
        assert_eq!(neutral_camera_name(1), "Camera B");
        assert_eq!(neutral_camera_name(25), "Camera Z");
        assert_eq!(neutral_camera_name(26), "Camera AA");
        assert_eq!(neutral_camera_name(27), "Camera AB");
        assert_eq!(neutral_camera_name(51), "Camera AZ");
        assert_eq!(neutral_camera_name(52), "Camera BA");
        assert_eq!(neutral_camera_name(701), "Camera ZZ");
        assert_eq!(neutral_camera_name(702), "Camera AAA");
    }

    #[test]
    fn millisecond_stamps_pad_and_handle_negatives() {
        let t = secs(DateTime::new(2026, 10, 10, 7, 42, 11).unwrap());
        assert_eq!(millis_stamp(t * 1000 + 250), "2026-10-10 07.42.11.250");
        assert_eq!(millis_stamp(t * 1000 + 5), "2026-10-10 07.42.11.005");
        assert_eq!(millis_stamp(t * 1000 - 1), "2026-10-10 07.42.10.999");
    }

    #[test]
    fn range_folders() {
        let p = NamePattern::default();
        let s = secs(DateTime::new(2026, 10, 10, 7, 42, 5).unwrap());
        let names = vec!["Front".to_string(), "Side/Gate".into()];
        assert_eq!(
            range_folder_name(s, s + 600, None, &names, &p),
            "2026-10-10 07.42\u{2013}07.52 range \u{2014} Front, Side-Gate"
        );
        assert_eq!(
            range_folder_name(s, s + 600, Some("Parcel: late?"), &names, &p),
            "2026-10-10 07.42\u{2013}07.52 Parcel- late-"
        );
        // Across midnight the end carries its date.
        assert_eq!(
            range_folder_name(s, s + 86_400, None, &[], &p),
            "2026-10-10 07.42\u{2013}2026-10-11 07.42 range"
        );
        // Long names are capped like incident folders.
        let long = vec!["x".repeat(300)];
        assert_eq!(
            range_folder_name(s, s + 60, None, &long, &p)
                .chars()
                .count(),
            MAX_NAME_CHARS
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
