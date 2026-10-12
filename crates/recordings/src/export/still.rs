//! The still image planner (docs/FFI-RECORDINGS.md, section 7a, "Still
//! images"): which picture to save, and what to call it. The app encodes
//! PNG or JPEG (ImageIO, WIC) and writes it; the core plans from memory.
//!
//! # From a file
//!
//! [`plan_file_still`]: a position in a camera's finished file. File
//! positions follow [`crate::edl`]: the picture at position `p` is at
//! `file_start_ms + offset_ms + p`.
//!
//! - `at_ms` must fall inside the file's video, `[0, duration_ms)` in file
//!   positions, else [`ExportError::OutsideFile`].
//! - **`exact`** (the default): `source_in_ms` is the position of `at_ms`
//!   itself, and `position_ms` is `at_ms`. The picture is decodable only
//!   from a keyframe, so `keyframe_in_ms` names where the app starts
//!   decoding (the keyframe at or before the position, else the first one);
//!   it is absent for a file without keyframes (decode from the start).
//! - **Not exact**: the nearest decodable picture, which is the keyframe at
//!   or before `at_ms` (the first keyframe when none is that early), so one
//!   picture is decoded. `source_in_ms` is that keyframe and `position_ms`
//!   is its time. A file without keyframes is
//!   [`ExportError::NoKeyframes`].
//!
//! # From a playback
//!
//! [`plan_playback_stills`]: the picture each playback tile shows now. The
//! caller (the playback owner, C10/C11) describes each tile as a
//! [`ShownPicture`]: its newest answered picture's recorded time and
//! generation (`None` when it has answered none). The plan names that
//! picture; the app saves the one it holds for that tile and generation.
//!
//! - `tiles` absent: every tile that has shown a picture, in the order given
//!   (tiles that have not are skipped). Present: those tiles, in that order,
//!   each once; an unknown tile is [`ExportError::UnknownTile`] and a listed
//!   tile that has shown nothing is [`ExportError::NothingShown`].
//! - Nothing to save at all is [`ExportError::NothingShown`].
//! - `label` is the optional caption the app burns in: the time as
//!   `YYYY-MM-DD HH.MM.SS.mmm` and the name, which `share_safe` leaves out.
//!
//! # Names
//!
//! `<dir>/<YYYY-MM-DD HH.MM.SS.mmm> <camera name>.<png|jpg>`, in `dest`;
//! without it, a still from a file goes next to the file and a still from a
//! playback into the default directory the caller passes (the downloads
//! directory), or is a bare name when there is none. With `share_safe` the
//! camera name is `Camera A`, `Camera B`, ... in the order of the stills.
//! Names that collide get ` (2)` and so on.

use super::trim::{keyframe_at_or_before, keyframes};
use super::{
    ExportError, MAX_SOURCES, StillFormat, is_false, make_unique, parent_dir, place, validate_dest,
};
use crate::edl::SourceFile;
use crate::incident::CameraId;
use crate::naming::{millis_stamp, neutral_camera_name, sanitize};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Most tiles planned in one call.
pub const MAX_TILES: usize = 64;

fn yes() -> bool {
    true
}

/// A request for a still from a downloaded file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStillRequest {
    /// The download the file belongs to.
    pub download: String,
    /// The camera.
    pub camera: CameraId,
    /// The time wanted, device-local milliseconds.
    pub at_ms: i64,
    /// The picture at exactly that time (default), or the nearest keyframe
    /// at or before it.
    #[serde(default = "yes")]
    pub exact: bool,
    /// `png` (default) or `jpeg`.
    #[serde(default)]
    pub format: StillFormat,
    /// An absolute directory for the image.
    #[serde(default)]
    pub dest: Option<String>,
    /// Leave out names that identify the installation.
    #[serde(default)]
    pub share_safe: bool,
}

/// A request for the pictures playback tiles show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackStillRequest {
    /// The tiles; absent for every tile that has shown a picture.
    #[serde(default)]
    pub tiles: Option<Vec<u32>>,
    /// `png` (default) or `jpeg`.
    #[serde(default)]
    pub format: StillFormat,
    /// An absolute directory for the images.
    #[serde(default)]
    pub dest: Option<String>,
    /// Leave out names that identify the installation.
    #[serde(default)]
    pub share_safe: bool,
}

/// One playback tile as the playback owner reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownPicture {
    /// The tile.
    pub tile: u32,
    /// Its camera.
    pub camera: CameraId,
    /// The camera's name.
    pub name: String,
    /// The generation of the newest picture the app answered with
    /// `nv_tile_decoded`.
    pub generation: u64,
    /// That picture's recorded time (device-local milliseconds), or `None`
    /// when no picture has been answered yet.
    pub position_ms: Option<i64>,
}

/// A still from a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStill {
    /// The camera.
    pub camera: CameraId,
    /// Its name (neutral with `share_safe`).
    pub name: String,
    /// The finished file to decode from.
    pub file: String,
    /// The picture's position in the file, in milliseconds.
    pub source_in_ms: i64,
    /// Where decoding starts for an exact picture (the keyframe at or
    /// before it); absent when `source_in_ms` is itself a keyframe or the
    /// file has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyframe_in_ms: Option<i64>,
    /// The picture's time, device-local milliseconds.
    pub position_ms: i64,
    /// A path for the image.
    pub suggested_file: String,
}

/// The plan for a still from a file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStillPlan {
    /// The stills (one).
    pub stills: Vec<FileStill>,
    /// Echo of the request; written only when true.
    #[serde(default, skip_serializing_if = "is_false")]
    pub share_safe: bool,
}

/// The caption the app may burn in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StillLabel {
    /// The camera's name; absent with `share_safe`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The picture's time, `YYYY-MM-DD HH.MM.SS.mmm`.
    pub time: String,
}

/// A still of a playback tile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackStill {
    /// The tile.
    pub tile: u32,
    /// The camera.
    pub camera: CameraId,
    /// Its name (neutral with `share_safe`).
    pub name: String,
    /// The generation of the picture to save.
    pub generation: u64,
    /// That picture's recorded time, device-local milliseconds.
    pub position_ms: i64,
    /// The optional caption.
    pub label: StillLabel,
    /// A path for the image.
    pub suggested_file: String,
}

/// The plan for stills of playback tiles.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackStillPlan {
    /// One per tile.
    pub stills: Vec<PlaybackStill>,
    /// Echo of the request; written only when true.
    #[serde(default, skip_serializing_if = "is_false")]
    pub share_safe: bool,
}

/// A still image's file name: the millisecond stamp, a space, and the camera
/// name, sanitized, with the format's extension (also the rule of live
/// pictures, docs/CAPTURE.md "Save picture").
pub fn still_name(position_ms: i64, name: &str, format: StillFormat) -> String {
    format!(
        "{}.{}",
        sanitize(&format!("{} {name}", millis_stamp(position_ms))),
        format.extension()
    )
}

/// Plans a still from a finished file. See the module documentation.
pub fn plan_file_still(
    req: &FileStillRequest,
    sources: &[SourceFile],
) -> Result<FileStillPlan, ExportError> {
    let dest = req.dest.as_deref().map(validate_dest).transpose()?;
    if sources.len() > MAX_SOURCES {
        return Err(ExportError::TooMany);
    }
    let f = sources
        .iter()
        .find(|f| f.camera == req.camera && f.duration_ms > 0)
        .ok_or(ExportError::NoSources)?;
    let base = f.file_start_ms.saturating_add(f.offset_ms);
    let pos = req.at_ms.saturating_sub(base);
    if pos < 0 || pos >= f.duration_ms {
        return Err(ExportError::OutsideFile);
    }
    let keys = keyframes(f);
    let key = keyframe_at_or_before(&keys, pos);
    let (source_in_ms, keyframe_in_ms) = if req.exact {
        (pos, key.filter(|&k| k != pos))
    } else {
        (key.ok_or(ExportError::NoKeyframes)?, None)
    };
    let position_ms = base.saturating_add(source_in_ms);
    let name = if req.share_safe {
        neutral_camera_name(0)
    } else {
        f.name.clone()
    };
    let file_name = still_name(position_ms, &name, req.format);
    let dir = dest.or_else(|| parent_dir(&f.file));
    Ok(FileStillPlan {
        stills: vec![FileStill {
            camera: f.camera.clone(),
            suggested_file: place(dir, &file_name),
            name,
            file: f.file.clone(),
            source_in_ms,
            keyframe_in_ms,
            position_ms,
        }],
        share_safe: req.share_safe,
    })
}

/// Plans stills of the pictures playback tiles show. `shown` is every tile
/// of the playback; `default_dir` is where the images go without `dest`
/// (the downloads directory), or `None` for bare names.
pub fn plan_playback_stills(
    req: &PlaybackStillRequest,
    shown: &[ShownPicture],
    default_dir: Option<&str>,
) -> Result<PlaybackStillPlan, ExportError> {
    let dest = req.dest.as_deref().map(validate_dest).transpose()?;
    let default_dir = default_dir.map(validate_dest).transpose()?;
    if shown.len() > MAX_TILES || req.tiles.as_ref().is_some_and(|t| t.len() > MAX_TILES) {
        return Err(ExportError::TooMany);
    }
    let chosen: Vec<(&ShownPicture, i64)> = match &req.tiles {
        None => shown
            .iter()
            .filter_map(|s| s.position_ms.map(|p| (s, p)))
            .collect(),
        Some(list) => {
            let mut seen = BTreeSet::new();
            let mut out = Vec::new();
            for &t in list {
                if !seen.insert(t) {
                    continue;
                }
                let s = shown
                    .iter()
                    .find(|s| s.tile == t)
                    .ok_or(ExportError::UnknownTile(t))?;
                out.push((s, s.position_ms.ok_or(ExportError::NothingShown)?));
            }
            out
        }
    };
    if chosen.is_empty() {
        return Err(ExportError::NothingShown);
    }
    let dir = dest.or(default_dir);
    let mut stills: Vec<PlaybackStill> = chosen
        .iter()
        .enumerate()
        .map(|(n, (s, position_ms))| {
            let name = if req.share_safe {
                neutral_camera_name(n)
            } else {
                s.name.clone()
            };
            PlaybackStill {
                tile: s.tile,
                camera: s.camera.clone(),
                label: StillLabel {
                    name: (!req.share_safe).then(|| name.clone()),
                    time: millis_stamp(*position_ms),
                },
                suggested_file: still_name(*position_ms, &name, req.format),
                name,
                generation: s.generation,
                position_ms: *position_ms,
            }
        })
        .collect();
    let mut files: Vec<String> = stills.iter().map(|s| s.suggested_file.clone()).collect();
    make_unique(&mut files);
    for (s, f) in stills.iter_mut().zip(files) {
        s.suggested_file = place(dir, &f);
    }
    Ok(PlaybackStillPlan {
        stills,
        share_safe: req.share_safe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secs;
    use bc_protocol::archive::DateTime;

    fn t0() -> i64 {
        secs(DateTime::new(2026, 10, 10, 7, 42, 0).unwrap()) * 1000
    }

    fn cam(n: u8) -> CameraId {
        CameraId::new("device-a", n)
    }

    fn file(n: u8, start: i64, dur: i64, gop: i64) -> SourceFile {
        SourceFile {
            camera: cam(n),
            name: format!("Door {n}"),
            file: format!("/dl/folder/cam{n}.mp4"),
            file_start_ms: t0() + start,
            duration_ms: dur,
            keyframes_ms: (0..dur).step_by(gop as usize).collect(),
            codec: "h264".into(),
            width: 1920,
            height: 1080,
            offset_ms: 0,
        }
    }

    fn req(at: i64, exact: bool) -> FileStillRequest {
        FileStillRequest {
            download: "dl-1".into(),
            camera: cam(1),
            at_ms: t0() + at,
            exact,
            format: StillFormat::Png,
            dest: None,
            share_safe: false,
        }
    }

    #[test]
    fn an_exact_still_is_at_the_time_and_names_where_to_start_decoding() {
        let f = [file(1, 1_000, 30_000, 4_000)];
        let p = plan_file_still(&req(12_250, true), &f).unwrap();
        let s = &p.stills[0];
        assert_eq!(s.source_in_ms, 11_250);
        assert_eq!(s.keyframe_in_ms, Some(8_000));
        assert_eq!(s.position_ms, t0() + 12_250);
        assert_eq!(
            s.suggested_file,
            "/dl/folder/2026-10-10 07.42.12.250 Door 1.png"
        );
        // On a keyframe there is nothing earlier to decode.
        let p = plan_file_still(&req(5_000, true), &f).unwrap();
        assert_eq!(p.stills[0].source_in_ms, 4_000);
        assert_eq!(p.stills[0].keyframe_in_ms, None);
    }

    #[test]
    fn a_keyframe_still_is_the_nearest_decodable_picture_at_or_before() {
        let f = [file(1, 1_000, 30_000, 4_000)];
        let p = plan_file_still(&req(12_250, false), &f).unwrap();
        let s = &p.stills[0];
        assert_eq!(s.source_in_ms, 8_000);
        assert_eq!(s.position_ms, t0() + 9_000);
        assert_eq!(s.keyframe_in_ms, None);
        assert!(s.suggested_file.ends_with("07.42.09.000 Door 1.png"));
        // First keyframe later than the position: use it.
        let mut late = file(1, 0, 10_000, 2_000);
        late.keyframes_ms = vec![3_000];
        let p = plan_file_still(&req(1_000, false), &[late]).unwrap();
        assert_eq!(p.stills[0].source_in_ms, 3_000);
    }

    #[test]
    fn times_outside_the_file_and_missing_keyframes() {
        let f = [file(1, 1_000, 10_000, 2_000)];
        for at in [0, 999, 11_000, 50_000] {
            assert_eq!(
                plan_file_still(&req(at, true), &f),
                Err(ExportError::OutsideFile),
                "{at}"
            );
        }
        assert!(plan_file_still(&req(1_000, true), &f).is_ok());
        assert!(plan_file_still(&req(10_999, true), &f).is_ok());
        let mut r = req(5_000, true);
        r.camera = cam(9);
        assert_eq!(plan_file_still(&r, &f), Err(ExportError::NoSources));
        let mut bare = file(1, 0, 10_000, 2_000);
        bare.keyframes_ms.clear();
        assert_eq!(
            plan_file_still(&req(5_000, false), &[bare.clone()]),
            Err(ExportError::NoKeyframes)
        );
        let p = plan_file_still(&req(5_000, true), &[bare]).unwrap();
        assert_eq!(p.stills[0].keyframe_in_ms, None);
        let mut r = req(0, true);
        r.at_ms = i64::MIN;
        assert_eq!(plan_file_still(&r, &f), Err(ExportError::OutsideFile));
        r.at_ms = i64::MAX;
        assert_eq!(plan_file_still(&r, &f), Err(ExportError::OutsideFile));
    }

    #[test]
    fn file_still_names_follow_format_dest_and_share_safe() {
        let f = [file(1, 0, 30_000, 4_000)];
        let mut r = req(5_000, true);
        r.format = StillFormat::Jpeg;
        r.dest = Some("/srv/pics".into());
        r.share_safe = true;
        let p = plan_file_still(&r, &f).unwrap();
        assert_eq!(
            p.stills[0].suggested_file,
            "/srv/pics/2026-10-10 07.42.05.000 Camera A.jpg"
        );
        assert_eq!(p.stills[0].name, "Camera A");
        assert!(p.share_safe);
        r.dest = Some("nope".into());
        assert_eq!(plan_file_still(&r, &f), Err(ExportError::InvalidDest));
    }

    #[test]
    fn file_still_json_shape() {
        let f = [file(1, 0, 30_000, 4_000)];
        let a = serde_json::to_value(plan_file_still(&req(5_000, true), &f).unwrap()).unwrap();
        let s = &a["stills"][0];
        for key in [
            "camera",
            "name",
            "file",
            "source_in_ms",
            "position_ms",
            "suggested_file",
        ] {
            assert!(s.get(key).is_some(), "{key}");
        }
        assert!(a.get("share_safe").is_none());
        let r: FileStillRequest = serde_json::from_str(
            r#"{"download": "x", "camera": {"device": "d", "channel": 3},
                "at_ms": 5, "exact": true, "format": "jpeg"}"#,
        )
        .unwrap();
        assert_eq!(r.format, StillFormat::Jpeg);
        let r: FileStillRequest = serde_json::from_str(
            r#"{"download": "x", "camera": {"device": "d", "channel": 3}, "at_ms": 5}"#,
        )
        .unwrap();
        assert!(r.exact);
    }

    fn tile(t: u32, n: u8, generation: u64, pos: Option<i64>) -> ShownPicture {
        ShownPicture {
            tile: t,
            camera: cam(n),
            name: format!("Door {n}"),
            generation,
            position_ms: pos.map(|p| t0() + p),
        }
    }

    fn tiles() -> Vec<ShownPicture> {
        vec![
            tile(11, 1, 4, Some(11_240)),
            tile(12, 2, 3, None),
            tile(13, 3, 4, Some(11_240)),
        ]
    }

    #[test]
    fn playback_stills_name_each_tiles_newest_answered_picture() {
        let p = plan_playback_stills(&PlaybackStillRequest::default(), &tiles(), Some("/srv/dl"))
            .unwrap();
        assert_eq!(p.stills.len(), 2, "the tile without a picture is skipped");
        let s = &p.stills[0];
        assert_eq!((s.tile, s.generation), (11, 4));
        assert_eq!(s.position_ms, t0() + 11_240);
        assert_eq!(s.label.time, "2026-10-10 07.42.11.240");
        assert_eq!(s.label.name.as_deref(), Some("Door 1"));
        assert_eq!(
            s.suggested_file,
            "/srv/dl/2026-10-10 07.42.11.240 Door 1.png"
        );
        assert_eq!(
            p.stills[1].suggested_file,
            "/srv/dl/2026-10-10 07.42.11.240 Door 3.png"
        );
    }

    #[test]
    fn playback_tile_choice_errors_and_order() {
        let mut r = PlaybackStillRequest {
            tiles: Some(vec![13, 11, 13]),
            ..Default::default()
        };
        let p = plan_playback_stills(&r, &tiles(), None).unwrap();
        let ids: Vec<_> = p.stills.iter().map(|s| s.tile).collect();
        assert_eq!(ids, [13, 11]);
        assert_eq!(
            p.stills[0].suggested_file,
            "2026-10-10 07.42.11.240 Door 3.png"
        );
        r.tiles = Some(vec![99]);
        let e = plan_playback_stills(&r, &tiles(), None).unwrap_err();
        assert_eq!(e, ExportError::UnknownTile(99));
        assert!(e.is_unknown());
        r.tiles = Some(vec![12]);
        assert_eq!(
            plan_playback_stills(&r, &tiles(), None),
            Err(ExportError::NothingShown)
        );
        r.tiles = Some(vec![]);
        assert_eq!(
            plan_playback_stills(&r, &tiles(), None),
            Err(ExportError::NothingShown)
        );
        r.tiles = None;
        assert_eq!(
            plan_playback_stills(&r, &[tile(1, 1, 1, None)], None),
            Err(ExportError::NothingShown)
        );
        assert_eq!(
            plan_playback_stills(&r, &[], None),
            Err(ExportError::NothingShown)
        );
    }

    #[test]
    fn playback_share_safe_and_collisions() {
        let r = PlaybackStillRequest {
            share_safe: true,
            format: StillFormat::Jpeg,
            dest: Some("C:\\Pics".into()),
            ..Default::default()
        };
        let p = plan_playback_stills(&r, &tiles(), Some("/srv/dl")).unwrap();
        assert_eq!(p.stills[0].name, "Camera A");
        assert_eq!(p.stills[1].name, "Camera B");
        assert_eq!(p.stills[0].label.name, None);
        assert_eq!(
            p.stills[0].suggested_file,
            "C:\\Pics\\2026-10-10 07.42.11.240 Camera A.jpg"
        );
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("Door"), "{json}");
        assert!(json.contains("\"share_safe\":true"));

        // Same name and time twice: counters keep the files apart.
        let twins = vec![tile(1, 1, 1, Some(5)), tile(2, 1, 1, Some(5))];
        let r = PlaybackStillRequest::default();
        let p = plan_playback_stills(&r, &twins, None).unwrap();
        assert_eq!(
            p.stills[0].suggested_file,
            "2026-10-10 07.42.00.005 Door 1.png"
        );
        assert_eq!(
            p.stills[1].suggested_file,
            "2026-10-10 07.42.00.005 Door 1 (2).png"
        );
        // Unsafe characters in a name are replaced.
        let mut odd = tile(1, 1, 1, Some(5));
        odd.name = "Back: yard/gate?".into();
        let p = plan_playback_stills(&r, &[odd], None).unwrap();
        assert_eq!(
            p.stills[0].suggested_file,
            "2026-10-10 07.42.00.005 Back- yard-gate-.png"
        );
    }

    #[test]
    fn playback_json_shape_and_bounds() {
        let p = plan_playback_stills(&PlaybackStillRequest::default(), &tiles(), None).unwrap();
        let v = serde_json::to_value(&p).unwrap();
        let s = &v["stills"][0];
        for key in [
            "tile",
            "camera",
            "name",
            "generation",
            "position_ms",
            "label",
            "suggested_file",
        ] {
            assert!(s.get(key).is_some(), "{key}");
        }
        assert_eq!(s["label"]["time"], "2026-10-10 07.42.11.240");
        let many: Vec<ShownPicture> = (0..=MAX_TILES as u32)
            .map(|i| tile(i, 1, 1, Some(1)))
            .collect();
        let e = plan_playback_stills(&PlaybackStillRequest::default(), &many, None).unwrap_err();
        assert!(e.is_limit());
        assert_eq!(
            plan_playback_stills(&PlaybackStillRequest::default(), &tiles(), Some("rel")),
            Err(ExportError::InvalidDest)
        );
        // Deterministic.
        assert_eq!(
            p,
            plan_playback_stills(&PlaybackStillRequest::default(), &tiles(), None).unwrap()
        );
    }
}
