//! The range download planner (docs/FFI-RECORDINGS.md, section 7a, "Range
//! download"): validates and normalizes a request for any time range of
//! chosen cameras and describes the job the scheduler takes.
//!
//! # Rules
//!
//! - Times are device-local seconds ([`crate::Secs`]), as for incidents.
//!   `end <= start` or a negative start is [`ExportError::InvalidRange`];
//!   a range longer than [`MAX_RANGE_SECS`] (exactly 24 hours is allowed)
//!   is [`ExportError::RangeTooLong`]; no cameras is
//!   [`ExportError::NoCameras`]; more than [`MAX_CAMERAS`] entries (counted
//!   as listed, before removing repeats) is [`ExportError::TooManyCameras`].
//! - A camera listed twice with the same tier counts once (the first
//!   entry wins its place); the same camera with two tiers is
//!   [`ExportError::DuplicateCamera`], because both would write the same
//!   file name.
//! - **ID.** `r-<start>-<hash>`: the hash is FNV-1a over the range and the
//!   cameras with their tiers in a canonical order, so the same request
//!   names the same download whatever order the cameras were listed in. The
//!   user's name, `dest`, and `share_safe` are not part of it; queuing the
//!   same cameras, tiers, and range again returns the existing download
//!   (the scheduler's rule, C15). Overlapping but different ranges are
//!   different downloads.
//! - **Folder.** [`crate::naming::range_folder_name`]; with `share_safe`
//!   the user's name is left out and cameras are `Camera A`, `Camera B`, ...
//!   in request order. [`numbered_folder`] adds ` (2)` and so on when the
//!   name is taken (the caller knows what is on disk).
//! - **Files.** `<date time to the second> <camera name>.mp4`, stamped with
//!   the range start, as in an incident folder, made unique when two cameras
//!   share a name.
//! - **`incident.json`.** [`range_incident_json`] builds the file of
//!   `"kind": "range"`; with `share_safe` there is none.
//!
//! # Seam: the scheduler (C11, C15)
//!
//! [`RangeJob`] is what the scheduler takes to queue a range: one camera job
//! per [`RangeJobCamera`] over `[start, end)` at its tier, output
//! `<folder>/<file>`. Nothing here calls the scheduler. C15 owns turning
//! the job into camera jobs, the `existing` answer for a known `id`, the
//! `downloads_dir`/`data_dir` rules and the registry for a `dest` outside
//! `downloads_dir`, and the 1000-downloads limit. [`RangeJob::folder`] is
//! `None` when neither `dest` nor a default directory was given; the
//! scheduler then joins [`RangeJob::folder_name`] onto its `downloads_dir`.

use super::{ExportError, is_false, make_unique, place, validate_dest};
use crate::incident::{CameraEvent, CameraId};
use crate::naming::{
    IncidentCamera, IncidentEventJson, NamePattern, camera_file_name, json_time,
    neutral_camera_name, range_folder_name,
};
use crate::{Secs, Tier};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Most cameras in one range download.
pub const MAX_CAMERAS: usize = 16;
/// Longest range in seconds (24 hours).
pub const MAX_RANGE_SECS: i64 = 24 * 3600;
/// Most events written to `incident.json`.
pub const MAX_JSON_EVENTS: usize = 10_000;
/// Most ` (n)` folder names tried by [`numbered_folder`].
pub const MAX_FOLDER_TRIES: u32 = 1000;

fn main_tier() -> Tier {
    Tier::Main
}

/// One camera of a range request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeCameraRequest {
    /// The camera.
    pub camera: CameraId,
    /// Its quality tier (default `main`).
    #[serde(default = "main_tier")]
    pub tier: Tier,
}

/// A range download request (`nv_download_range`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeRequest {
    /// Start, device-local seconds.
    pub start: Secs,
    /// End (exclusive), device-local seconds.
    pub end: Secs,
    /// The cameras and tiers, in the order names are given out.
    pub cameras: Vec<RangeCameraRequest>,
    /// The user's name for the download, if any.
    #[serde(default)]
    pub name: Option<String>,
    /// An absolute directory for the download's folder.
    #[serde(default)]
    pub dest: Option<String>,
    /// Leave out names that identify the installation.
    #[serde(default)]
    pub share_safe: bool,
}

/// One camera job of a range.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeJobCamera {
    /// The camera.
    pub camera: CameraId,
    /// Its tier.
    pub tier: Tier,
    /// Its name (neutral with `share_safe`).
    pub name: String,
    /// The file name inside the folder.
    pub file: String,
}

/// What the scheduler takes to queue a range download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeJob {
    /// The opaque download ID, `r-<start>-<hash>`.
    pub id: String,
    /// Start, device-local seconds.
    pub start: Secs,
    /// End (exclusive), device-local seconds.
    pub end: Secs,
    /// One job per camera, in request order.
    pub cameras: Vec<RangeJobCamera>,
    /// The folder's name (before any ` (2)`; see [`numbered_folder`]).
    pub folder_name: String,
    /// The folder's full path when the directory is known (`dest`, else the
    /// default directory the caller passed).
    pub folder: Option<String>,
    /// The validated `dest`, if one was given.
    pub dest: Option<String>,
    /// The user's name for the download (absent with `share_safe`).
    pub name: Option<String>,
    /// Whether the download is share-safe: no `incident.json`, neutral
    /// names, and zero MP4 times (see [`super::mp4_metadata`]).
    #[serde(default, skip_serializing_if = "is_false")]
    pub share_safe: bool,
}

impl RangeJob {
    /// Whether an `incident.json` is written (never with `share_safe`).
    pub fn writes_incident_json(&self) -> bool {
        !self.share_safe
    }
}

/// FNV-1a, for short stable IDs (not for security).
fn fnv(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain([0xff]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn tier_str(t: Tier) -> &'static str {
    match t {
        Tier::Main => "main",
        Tier::Sub => "sub",
    }
}

/// The download ID for a range of cameras and tiers: independent of the
/// order the cameras are listed in.
pub fn range_id(start: Secs, end: Secs, cameras: &[RangeCameraRequest]) -> String {
    let mut keys: Vec<(&CameraId, &'static str)> = cameras
        .iter()
        .map(|c| (&c.camera, tier_str(c.tier)))
        .collect();
    keys.sort();
    keys.dedup();
    let (s, e) = (start.to_string(), end.to_string());
    let strings: Vec<String> = keys
        .iter()
        .flat_map(|(c, t)| {
            [
                c.device.clone(),
                c.channel.to_string(),
                c.lens.to_string(),
                (*t).to_owned(),
            ]
        })
        .collect();
    let mut parts: Vec<&str> = vec![&s, &e];
    parts.extend(strings.iter().map(String::as_str));
    format!("r-{start}-{:016x}", fnv(&parts))
}

/// Checks the shape of a request and returns its cameras without repeats,
/// in request order.
fn normalize(req: &RangeRequest) -> Result<Vec<RangeCameraRequest>, ExportError> {
    if req.start < 0 || req.end <= req.start {
        return Err(ExportError::InvalidRange);
    }
    if req.end.saturating_sub(req.start) > MAX_RANGE_SECS {
        return Err(ExportError::RangeTooLong);
    }
    if req.cameras.is_empty() {
        return Err(ExportError::NoCameras);
    }
    if req.cameras.len() > MAX_CAMERAS {
        return Err(ExportError::TooManyCameras);
    }
    let mut out: Vec<RangeCameraRequest> = Vec::with_capacity(req.cameras.len());
    let mut tiers: BTreeMap<&CameraId, Tier> = BTreeMap::new();
    for c in &req.cameras {
        match tiers.get(&c.camera) {
            Some(&t) if t == c.tier => continue,
            Some(_) => return Err(ExportError::DuplicateCamera),
            None => {
                tiers.insert(&c.camera, c.tier);
                out.push(c.clone());
            }
        }
    }
    Ok(out)
}

/// Plans a range download. `names` holds the user's camera names (a camera
/// without one is called `Channel N`, from 1); `default_dir` is the
/// directory used when the request has no `dest` (the downloads directory),
/// or `None` to leave the folder's location to the scheduler.
pub fn plan_range(
    req: &RangeRequest,
    names: &BTreeMap<CameraId, String>,
    default_dir: Option<&str>,
) -> Result<RangeJob, ExportError> {
    let cameras = normalize(req)?;
    let dest = req.dest.as_deref().map(validate_dest).transpose()?;
    let default_dir = default_dir.map(validate_dest).transpose()?;
    let pattern = NamePattern::default();

    let display: Vec<String> = cameras
        .iter()
        .enumerate()
        .map(|(n, c)| {
            if req.share_safe {
                neutral_camera_name(n)
            } else {
                names.get(&c.camera).cloned().unwrap_or_else(|| {
                    let base = format!("Channel {}", u32::from(c.camera.channel) + 1);
                    if c.camera.lens == 0 {
                        base
                    } else {
                        format!("{base} lens {}", u32::from(c.camera.lens) + 1)
                    }
                })
            }
        })
        .collect();
    let user_name = if req.share_safe {
        None
    } else {
        req.name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
    };
    let folder_name =
        range_folder_name(req.start, req.end, user_name.as_deref(), &display, &pattern);
    let mut files: Vec<String> = display
        .iter()
        .map(|n| camera_file_name(req.start, n, &pattern))
        .collect();
    make_unique(&mut files);
    let folder = dest.or(default_dir).map(|d| place(Some(d), &folder_name));
    Ok(RangeJob {
        id: range_id(req.start, req.end, &cameras),
        start: req.start,
        end: req.end,
        cameras: cameras
            .into_iter()
            .zip(display)
            .zip(files)
            .map(|((c, name), file)| RangeJobCamera {
                camera: c.camera,
                tier: c.tier,
                name,
                file,
            })
            .collect(),
        folder_name,
        folder,
        dest: dest.map(str::to_owned),
        name: user_name,
        share_safe: req.share_safe,
    })
}

/// The first of `base`, `base (2)`, `base (3)`, ... that `taken` does not
/// report as existing. [`ExportError::TooMany`] after
/// [`MAX_FOLDER_TRIES`] names.
pub fn numbered_folder(base: &str, taken: impl Fn(&str) -> bool) -> Result<String, ExportError> {
    if !taken(base) {
        return Ok(base.to_owned());
    }
    (2..=MAX_FOLDER_TRIES)
        .map(|n| format!("{base} ({n})"))
        .find(|c| !taken(c))
        .ok_or(ExportError::TooMany)
}

/// The `incident.json` of a range download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeIncidentJson {
    /// Format version (1).
    pub version: u32,
    /// `range`.
    pub kind: String,
    /// The download ID.
    pub id: String,
    /// The user's name, if any.
    pub name: Option<String>,
    /// Start (device-local time, no zone).
    pub start: String,
    /// End.
    pub end: String,
    /// Cameras with their files.
    pub cameras: Vec<IncidentCamera>,
    /// The indexed events inside the range (none if the days are not
    /// indexed), at most [`MAX_JSON_EVENTS`].
    pub events: Vec<IncidentEventJson>,
}

/// Builds `incident.json` for a range job, or `None` when the job is
/// share-safe. `events` are whatever the index holds for the days; those of
/// other cameras or outside the range are skipped, and the rest are written
/// in time order.
pub fn range_incident_json(job: &RangeJob, events: &[CameraEvent]) -> Option<RangeIncidentJson> {
    if !job.writes_incident_json() {
        return None;
    }
    let wanted: BTreeSet<&CameraId> = job.cameras.iter().map(|c| &c.camera).collect();
    let mut inside: Vec<&CameraEvent> = events
        .iter()
        .filter(|e| wanted.contains(&e.camera) && e.start < job.end && e.end > job.start)
        .collect();
    inside.sort();
    inside.dedup();
    inside.truncate(MAX_JSON_EVENTS);
    Some(RangeIncidentJson {
        version: 1,
        kind: "range".to_owned(),
        id: job.id.clone(),
        name: job.name.clone(),
        start: json_time(job.start),
        end: json_time(job.end),
        cameras: job
            .cameras
            .iter()
            .map(|c| IncidentCamera {
                device: c.camera.device.clone(),
                channel: c.camera.channel,
                lens: c.camera.lens,
                name: c.name.clone(),
                start: json_time(job.start),
                end: json_time(job.end),
                file: c.file.clone(),
            })
            .collect(),
        events: inside
            .into_iter()
            .map(|e| IncidentEventJson {
                device: e.camera.device.clone(),
                channel: e.camera.channel,
                lens: e.camera.lens,
                kind: e.kind.clone(),
                start: json_time(e.start),
                end: json_time(e.end),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secs;
    use bc_protocol::archive::DateTime;

    fn t0() -> Secs {
        secs(DateTime::new(2026, 10, 10, 7, 42, 5).unwrap())
    }

    fn cam(n: u8) -> CameraId {
        CameraId::new("device-a", n)
    }

    fn rc(n: u8, tier: Tier) -> RangeCameraRequest {
        RangeCameraRequest {
            camera: cam(n),
            tier,
        }
    }

    fn request(cams: &[u8]) -> RangeRequest {
        RangeRequest {
            start: t0(),
            end: t0() + 600,
            cameras: cams.iter().map(|&n| rc(n, Tier::Main)).collect(),
            name: None,
            dest: None,
            share_safe: false,
        }
    }

    fn names() -> BTreeMap<CameraId, String> {
        [(cam(1), "Front"), (cam(2), "Side/Gate")]
            .into_iter()
            .map(|(c, n)| (c, n.to_owned()))
            .collect()
    }

    #[test]
    fn plans_ids_folders_and_files() {
        let job = plan_range(&request(&[1, 2]), &names(), None).unwrap();
        assert!(job.id.starts_with(&format!("r-{}-", t0())), "{}", job.id);
        assert_eq!(job.id.len(), format!("r-{}-", t0()).len() + 16);
        assert_eq!(
            job.folder_name,
            "2026-10-10 07.42\u{2013}07.52 range \u{2014} Front, Side-Gate"
        );
        assert_eq!(job.cameras[0].file, "2026-10-10 07.42.05 Front.mp4");
        assert_eq!(job.cameras[1].file, "2026-10-10 07.42.05 Side-Gate.mp4");
        assert_eq!(job.cameras[1].name, "Side/Gate");
        assert_eq!(job.folder, None);
        assert!(job.writes_incident_json());
    }

    #[test]
    fn a_user_name_replaces_the_word_range() {
        let mut r = request(&[1]);
        r.name = Some("  Delivery window  ".into());
        let job = plan_range(&r, &names(), None).unwrap();
        assert_eq!(
            job.folder_name,
            "2026-10-10 07.42\u{2013}07.52 Delivery window"
        );
        assert_eq!(job.name.as_deref(), Some("Delivery window"));
        r.name = Some("   ".into());
        let job = plan_range(&r, &names(), None).unwrap();
        assert!(job.folder_name.contains(" range "));
        assert_eq!(job.name, None);
    }

    #[test]
    fn the_id_ignores_order_name_dest_and_share_safe_but_not_range_or_tier() {
        let base = plan_range(&request(&[1, 2]), &names(), None).unwrap();
        let swapped = plan_range(&request(&[2, 1]), &names(), None).unwrap();
        assert_eq!(base.id, swapped.id);
        let mut r = request(&[1, 2]);
        r.name = Some("x".into());
        r.dest = Some("/srv/out".into());
        r.share_safe = true;
        assert_eq!(plan_range(&r, &names(), None).unwrap().id, base.id);
        let mut r = request(&[1, 2]);
        r.end += 1;
        assert_ne!(plan_range(&r, &names(), None).unwrap().id, base.id);
        let mut r = request(&[1, 2]);
        r.cameras[1].tier = Tier::Sub;
        assert_ne!(plan_range(&r, &names(), None).unwrap().id, base.id);
        let r = request(&[1, 3]);
        assert_ne!(plan_range(&r, &names(), None).unwrap().id, base.id);
        // A second lens is a different camera.
        let mut r = request(&[1]);
        r.cameras[0].camera = CameraId::with_lens("device-a", 1, 1);
        assert_ne!(
            plan_range(&r, &names(), None).unwrap().id,
            plan_range(&request(&[1]), &names(), None).unwrap().id
        );
    }

    #[test]
    fn the_same_request_gives_the_same_job() {
        let a = plan_range(&request(&[1, 2]), &names(), Some("/srv/dl")).unwrap();
        let b = plan_range(&request(&[1, 2]), &names(), Some("/srv/dl")).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
    }

    #[test]
    fn camera_and_length_limits() {
        let ok: Vec<u8> = (0..16).collect();
        assert!(plan_range(&request(&ok), &names(), None).is_ok());
        let too_many: Vec<u8> = (0..17).collect();
        let e = plan_range(&request(&too_many), &names(), None).unwrap_err();
        assert_eq!(e, ExportError::TooManyCameras);
        assert!(e.is_limit());
        // Repeats are counted as listed.
        let repeats = vec![1u8; 17];
        assert_eq!(
            plan_range(&request(&repeats), &names(), None),
            Err(ExportError::TooManyCameras)
        );
        assert_eq!(
            plan_range(&request(&[]), &names(), None),
            Err(ExportError::NoCameras)
        );

        let mut r = request(&[1]);
        r.end = r.start + 86_400;
        assert!(plan_range(&r, &names(), None).is_ok(), "exactly 24 h");
        r.end += 1;
        let e = plan_range(&r, &names(), None).unwrap_err();
        assert_eq!(e, ExportError::RangeTooLong);
        assert!(e.is_limit());
        r.end = i64::MAX;
        assert_eq!(
            plan_range(&r, &names(), None),
            Err(ExportError::RangeTooLong)
        );
    }

    #[test]
    fn empty_and_backwards_ranges_are_invalid() {
        let mut r = request(&[1]);
        r.end = r.start;
        assert_eq!(
            plan_range(&r, &names(), None),
            Err(ExportError::InvalidRange)
        );
        r.end = r.start - 10;
        assert_eq!(
            plan_range(&r, &names(), None),
            Err(ExportError::InvalidRange)
        );
        r.start = -5;
        r.end = 10;
        assert_eq!(
            plan_range(&r, &names(), None),
            Err(ExportError::InvalidRange)
        );
        assert!(!ExportError::InvalidRange.is_limit());
    }

    #[test]
    fn repeats_collapse_but_tier_conflicts_are_refused() {
        let mut r = request(&[1, 2, 1]);
        let job = plan_range(&r, &names(), None).unwrap();
        assert_eq!(job.cameras.len(), 2);
        r.cameras.push(rc(2, Tier::Sub));
        assert_eq!(
            plan_range(&r, &names(), None),
            Err(ExportError::DuplicateCamera)
        );
    }

    #[test]
    fn missing_names_fall_back_to_channels_and_repeats_get_counters() {
        let mut r = request(&[7]);
        r.cameras.push(RangeCameraRequest {
            camera: CameraId::with_lens("device-a", 7, 1),
            tier: Tier::Main,
        });
        let job = plan_range(&r, &BTreeMap::new(), None).unwrap();
        assert_eq!(job.cameras[0].name, "Channel 8");
        assert_eq!(job.cameras[1].name, "Channel 8 lens 2");
        // Two cameras with the same user name get distinct files.
        let same: BTreeMap<_, _> =
            [(cam(1), "Door".to_owned()), (cam(2), "Door".to_owned())].into();
        let job = plan_range(&request(&[1, 2]), &same, None).unwrap();
        assert_eq!(job.cameras[0].file, "2026-10-10 07.42.05 Door.mp4");
        assert_eq!(job.cameras[1].file, "2026-10-10 07.42.05 Door (2).mp4");
    }

    #[test]
    fn destination_and_default_directory() {
        let mut r = request(&[1]);
        r.dest = Some("C:\\Exports".into());
        let job = plan_range(&r, &names(), Some("/srv/dl")).unwrap();
        assert_eq!(job.dest.as_deref(), Some("C:\\Exports"));
        assert_eq!(
            job.folder.as_deref(),
            Some("C:\\Exports\\2026-10-10 07.42\u{2013}07.52 range \u{2014} Front")
        );
        r.dest = None;
        let job = plan_range(&r, &names(), Some("/srv/dl/")).unwrap();
        assert_eq!(
            job.folder.as_deref(),
            Some("/srv/dl/2026-10-10 07.42\u{2013}07.52 range \u{2014} Front")
        );
        assert_eq!(job.dest, None);
        r.dest = Some("not/absolute".into());
        assert_eq!(
            plan_range(&r, &names(), None),
            Err(ExportError::InvalidDest)
        );
        r.dest = None;
        assert_eq!(
            plan_range(&r, &names(), Some("relative")),
            Err(ExportError::InvalidDest)
        );
    }

    #[test]
    fn share_safe_hides_names_and_the_incident_file() {
        let mut r = request(&[2, 1]);
        r.share_safe = true;
        r.name = Some("Break-in at the shed".into());
        let job = plan_range(&r, &names(), None).unwrap();
        assert_eq!(
            job.folder_name,
            "2026-10-10 07.42\u{2013}07.52 range \u{2014} Camera A, Camera B"
        );
        assert_eq!(job.cameras[0].camera, cam(2));
        assert_eq!(job.cameras[0].name, "Camera A");
        assert_eq!(job.cameras[0].file, "2026-10-10 07.42.05 Camera A.mp4");
        assert_eq!(job.name, None);
        assert!(!job.writes_incident_json());
        assert_eq!(range_incident_json(&job, &[]), None);
        let json = serde_json::to_string(&job).unwrap();
        for private in ["Front", "Side", "Break-in", "shed"] {
            assert!(!json.contains(private), "{private}: {json}");
        }
        assert!(job.share_safe);
    }

    #[test]
    fn folders_get_numbers_when_taken() {
        assert_eq!(numbered_folder("a", |_| false).unwrap(), "a");
        assert_eq!(numbered_folder("a", |n| n == "a").unwrap(), "a (2)");
        assert_eq!(
            numbered_folder("a", |n| n == "a" || n == "a (2)").unwrap(),
            "a (3)"
        );
        assert_eq!(numbered_folder("a", |_| true), Err(ExportError::TooMany));
    }

    #[test]
    fn incident_json_lists_cameras_and_events_inside_the_range() {
        let mut r = request(&[1, 2]);
        r.name = Some("Parcel".into());
        let job = plan_range(&r, &names(), None).unwrap();
        let ev = |c: u8, s: Secs, e: Secs, k: &str| CameraEvent {
            start: t0() + s,
            end: t0() + e,
            camera: cam(c),
            kind: k.to_owned(),
        };
        let events = [
            ev(2, 100, 120, "vehicle"),
            ev(1, 10, 20, "motion"),
            ev(1, 10, 20, "motion"),
            ev(9, 10, 20, "motion"),
            ev(1, -50, -10, "motion"),
            ev(1, 600, 700, "motion"),
            ev(1, -5, 5, "person"),
        ];
        let j = range_incident_json(&job, &events).unwrap();
        assert_eq!((j.version, j.kind.as_str()), (1, "range"));
        assert_eq!(j.id, job.id);
        assert_eq!(j.name.as_deref(), Some("Parcel"));
        assert_eq!(j.start, "2026-10-10 07:42:05");
        assert_eq!(j.cameras.len(), 2);
        assert_eq!(j.cameras[0].file, job.cameras[0].file);
        let kinds: Vec<_> = j
            .events
            .iter()
            .map(|e| (e.channel, e.kind.as_str()))
            .collect();
        assert_eq!(kinds, [(1, "person"), (1, "motion"), (2, "vehicle")]);
        let v = serde_json::to_value(&j).unwrap();
        assert_eq!(v["kind"], "range");
        assert!(range_incident_json(&job, &[]).unwrap().events.is_empty());
    }

    #[test]
    fn requests_parse_from_the_documented_json() {
        let j = r#"{"start": 1791618100, "end": 1791618400,
            "cameras": [{"camera": {"device": "d-example", "channel": 3}, "tier": "main"},
                        {"camera": {"device": "d-example", "channel": 5}}],
            "name": null, "dest": "/srv/out", "share_safe": false}"#;
        let r: RangeRequest = serde_json::from_str(j).unwrap();
        assert_eq!(r.cameras[1].tier, Tier::Main);
        assert_eq!(r.cameras[0].camera.lens, 0);
        assert_eq!(r.dest.as_deref(), Some("/srv/out"));
    }
}
