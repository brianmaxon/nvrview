//! The trim planner (docs/FFI-RECORDINGS.md, section 7a, "Trim"): one output
//! per camera, cut to an in and an out point inside a downloaded incident or
//! range download.
//!
//! The planner reuses the edit-list types of [`crate::edl`]: a camera's
//! finished file is a [`SourceFile`] (keyframes from
//! [`crate::sync_samples`]), times are device-local milliseconds, and a
//! source's picture at file position `p` is at
//! `file_start_ms + offset_ms + p`. The output is a plan, not a file; the
//! app copies or re-encodes the samples.
//!
//! # Rules
//!
//! - `in_ms >= out_ms` is [`ExportError::InvalidRange`].
//! - **Cameras.** `cameras` absent: every distinct camera among the sources.
//!   Present: those listed, in that order, each once; a listed camera with
//!   no source is skipped, and an empty list is [`ExportError::NoCameras`].
//!   Output order is that order, which also fixes share-safe names.
//! - **Clipping.** Each camera is cut to the part of `[in_ms, out_ms)`
//!   its file covers (`[0, duration_ms)` in file positions). A camera whose
//!   file does not overlap the range is left out; if none overlaps the
//!   error is [`ExportError::OutsideFile`] ("outside every chosen camera's
//!   file").
//! - **`keyframe` (fast).** The in point is the last keyframe at or before
//!   the clipped in position, so the moment asked for is included. If the
//!   file has no keyframe that early (it starts between keyframes), the
//!   first keyframe is used instead, since a copy has to start on one; if
//!   that is not before the out point the camera is left out. A file with
//!   no keyframes cannot be cut this way (left out; [`ExportError::NoKeyframes`]
//!   if that leaves nothing). `copy_possible` is true.
//! - **`exact`.** The in point is the clipped in position itself, and
//!   `copy_possible` is false: the app re-encodes at least the group of
//!   pictures that contains it (it may re-encode everything).
//! - **Out point.** Exact in both cuts: the clipped out position
//!   (exclusive), because a copy can end on any picture. The contract's
//!   "snap out to the frame before the next keyframe" therefore only
//!   applies to the app's choice of the last sample to write, not to this
//!   position.
//! - `start_ms`/`end_ms` of the plan are the earliest output start and the
//!   latest output end, after the keyframe move, in device-local time.
//!
//! # Names
//!
//! Each output is `<dir>/<YYYY-MM-DD HH.MM.SS> <name> trim.mp4`, stamped
//! with that output's own start (after the keyframe move), where `<dir>` is
//! `dest` or, without it, the directory of the camera's finished file. With
//! `share_safe` the name is `Camera A`, `Camera B`, ... instead of the
//! camera's name. Names that collide get ` (2)` and so on.

use super::{ExportError, MAX_SOURCES, is_false, make_unique, parent_dir, place, validate_dest};
use crate::edl::{PLAN_VERSION, SourceFile};
use crate::incident::CameraId;
use crate::naming::{NamePattern, camera_file_name, neutral_camera_name};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// How the in point is cut.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrimCut {
    /// Back to the keyframe at or before: the samples can be copied.
    #[default]
    #[serde(alias = "frame")]
    Keyframe,
    /// Exactly at the picture asked for: needs a re-encode.
    Exact,
}

/// A trim request (the `trim` kind of `nv_edit_compute`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrimRequest {
    /// The download the points are in.
    pub download: String,
    /// Cameras to cut; absent for every camera with a finished file.
    #[serde(default)]
    pub cameras: Option<Vec<CameraId>>,
    /// In point, device-local milliseconds.
    pub in_ms: i64,
    /// Out point (exclusive), device-local milliseconds.
    pub out_ms: i64,
    /// How the in point is cut (default `keyframe`).
    #[serde(default)]
    pub cut: TrimCut,
    /// An absolute directory for the outputs.
    #[serde(default)]
    pub dest: Option<String>,
    /// Leave out names that identify the installation.
    #[serde(default)]
    pub share_safe: bool,
}

/// One camera's output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrimOutput {
    /// Index of the output.
    pub source: usize,
    /// The camera.
    pub camera: CameraId,
    /// Its name (neutral with `share_safe`).
    pub name: String,
    /// The finished file to read.
    pub file: String,
    /// Where in that file the output starts, in milliseconds.
    pub source_in_ms: i64,
    /// Where it ends (exclusive), in milliseconds.
    pub source_out_ms: i64,
    /// Whether the samples can be copied without re-encoding.
    pub copy_possible: bool,
    /// A path for the output.
    pub suggested_file: String,
}

/// A trim plan.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrimPlan {
    /// [`PLAN_VERSION`].
    pub version: u32,
    /// `trim`.
    pub kind: String,
    /// The download this was computed for.
    pub download: String,
    /// Earliest output start (device-local milliseconds).
    pub start_ms: i64,
    /// Latest output end.
    pub end_ms: i64,
    /// One output per camera.
    pub outputs: Vec<TrimOutput>,
    /// Echo of the request: the writer adds no metadata of its own (see
    /// [`super::mp4_metadata`]). Written only when true.
    #[serde(default, skip_serializing_if = "is_false")]
    pub share_safe: bool,
}

/// The first picture's time of a source plus the user's correction.
fn base_ms(f: &SourceFile) -> i64 {
    f.file_start_ms.saturating_add(f.offset_ms)
}

/// The keyframe positions of a source inside its file, sorted.
pub(super) fn keyframes(f: &SourceFile) -> Vec<i64> {
    let mut keys: Vec<i64> = f
        .keyframes_ms
        .iter()
        .copied()
        .filter(|&k| k >= 0 && k < f.duration_ms)
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

/// The last keyframe at or before `pos`, else the first one.
pub(super) fn keyframe_at_or_before(keys: &[i64], pos: i64) -> Option<i64> {
    let i = keys.partition_point(|&k| k <= pos);
    if i > 0 { keys.get(i - 1) } else { keys.first() }.copied()
}

/// The sources a request chooses, in output order.
pub(super) fn choose<'a>(
    cameras: Option<&[CameraId]>,
    sources: &'a [SourceFile],
) -> Result<Vec<&'a SourceFile>, ExportError> {
    if sources.len() > MAX_SOURCES {
        return Err(ExportError::TooMany);
    }
    let usable = |f: &&SourceFile| f.duration_ms > 0;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    match cameras {
        None => {
            for f in sources.iter().filter(usable) {
                if seen.insert(&f.camera) {
                    out.push(f);
                }
            }
        }
        Some([]) => return Err(ExportError::NoCameras),
        Some(list) => {
            for c in list {
                if !seen.insert(c) {
                    continue;
                }
                if let Some(f) = sources.iter().filter(usable).find(|f| f.camera == *c) {
                    out.push(f);
                }
            }
        }
    }
    if out.is_empty() {
        return Err(ExportError::NoSources);
    }
    Ok(out)
}

/// Plans a trim. See the module documentation for the rules.
pub fn plan_trim(req: &TrimRequest, sources: &[SourceFile]) -> Result<TrimPlan, ExportError> {
    if req.in_ms >= req.out_ms {
        return Err(ExportError::InvalidRange);
    }
    let dest = req.dest.as_deref().map(validate_dest).transpose()?;
    let chosen = choose(req.cameras.as_deref(), sources)?;

    let mut outside = 0usize;
    let mut keyless = 0usize;
    let mut cuts: Vec<(&SourceFile, i64, i64)> = Vec::new();
    for f in chosen {
        let base = base_ms(f);
        let lo = req.in_ms.saturating_sub(base).max(0);
        let hi = req.out_ms.saturating_sub(base).min(f.duration_ms);
        if lo >= hi {
            outside += 1;
            continue;
        }
        let start = match req.cut {
            TrimCut::Exact => lo,
            TrimCut::Keyframe => match keyframe_at_or_before(&keyframes(f), lo) {
                Some(k) if k < hi => k,
                Some(_) => {
                    outside += 1;
                    continue;
                }
                None => {
                    keyless += 1;
                    continue;
                }
            },
        };
        cuts.push((f, start, hi));
    }
    if cuts.is_empty() {
        return Err(if outside == 0 && keyless > 0 {
            ExportError::NoKeyframes
        } else {
            ExportError::OutsideFile
        });
    }

    let pattern = NamePattern::default();
    let mut outputs = Vec::with_capacity(cuts.len());
    let mut files = Vec::with_capacity(cuts.len());
    for (n, (f, start, end)) in cuts.iter().enumerate() {
        let name = if req.share_safe {
            neutral_camera_name(n)
        } else {
            f.name.clone()
        };
        let stamp = base_ms(f).saturating_add(*start).div_euclid(1000);
        files.push(camera_file_name(stamp, &format!("{name} trim"), &pattern));
        outputs.push(TrimOutput {
            source: n,
            camera: f.camera.clone(),
            name,
            file: f.file.clone(),
            source_in_ms: *start,
            source_out_ms: *end,
            copy_possible: req.cut == TrimCut::Keyframe,
            suggested_file: String::new(),
        });
    }
    make_unique(&mut files);
    for ((o, name), (f, _, _)) in outputs.iter_mut().zip(&files).zip(&cuts) {
        let dir = dest.or_else(|| parent_dir(&f.file));
        o.suggested_file = place(dir, name);
    }
    let start_ms = cuts
        .iter()
        .map(|(f, s, _)| base_ms(f).saturating_add(*s))
        .min()
        .unwrap_or(0);
    let end_ms = cuts
        .iter()
        .map(|(f, _, e)| base_ms(f).saturating_add(*e))
        .max()
        .unwrap_or(0);
    Ok(TrimPlan {
        version: PLAN_VERSION,
        kind: "trim".to_owned(),
        download: req.download.clone(),
        start_ms,
        end_ms,
        outputs,
        share_safe: req.share_safe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secs;
    use bc_protocol::archive::DateTime;

    /// 2026-10-10 07.42.00 in milliseconds.
    fn t0() -> i64 {
        secs(DateTime::new(2026, 10, 10, 7, 42, 0).unwrap()) * 1000
    }

    fn cam(n: u8) -> CameraId {
        CameraId::new("device-a", n)
    }

    /// A file starting `start` ms after t0, `dur` long, a keyframe every
    /// `gop` ms from its first picture.
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

    fn req(in_ms: i64, out_ms: i64, cut: TrimCut) -> TrimRequest {
        TrimRequest {
            download: "dl-1".into(),
            cameras: None,
            in_ms: t0() + in_ms,
            out_ms: t0() + out_ms,
            cut,
            dest: None,
            share_safe: false,
        }
    }

    #[test]
    fn keyframe_cut_moves_in_back_and_keeps_out_exact() {
        let f = [file(1, 0, 30_000, 4_000)];
        let p = plan_trim(&req(9_000, 17_500, TrimCut::Keyframe), &f).unwrap();
        let o = &p.outputs[0];
        assert_eq!((o.source_in_ms, o.source_out_ms), (8_000, 17_500));
        assert!(o.copy_possible);
        assert_eq!((p.start_ms, p.end_ms), (t0() + 8_000, t0() + 17_500));
        assert_eq!(p.kind, "trim");
        assert_eq!(p.version, 1);
        // On a keyframe: stays.
        let p = plan_trim(&req(8_000, 9_000, TrimCut::Keyframe), &f).unwrap();
        assert_eq!(p.outputs[0].source_in_ms, 8_000);
        // One millisecond after: back to it.
        let p = plan_trim(&req(8_001, 9_000, TrimCut::Keyframe), &f).unwrap();
        assert_eq!(p.outputs[0].source_in_ms, 8_000);
    }

    #[test]
    fn exact_cut_starts_where_asked_and_flags_a_re_encode() {
        let f = [file(1, 0, 30_000, 4_000)];
        let p = plan_trim(&req(9_000, 17_500, TrimCut::Exact), &f).unwrap();
        let o = &p.outputs[0];
        assert_eq!((o.source_in_ms, o.source_out_ms), (9_000, 17_500));
        assert!(!o.copy_possible);
        assert_eq!(p.start_ms, t0() + 9_000);
    }

    #[test]
    fn points_outside_the_file_are_clipped_or_refused() {
        let f = [file(1, 5_000, 20_000, 4_000)];
        // In before the file starts: begins at its first picture.
        let p = plan_trim(&req(0, 10_000, TrimCut::Keyframe), &f).unwrap();
        assert_eq!(
            (p.outputs[0].source_in_ms, p.outputs[0].source_out_ms),
            (0, 5_000)
        );
        let p = plan_trim(&req(0, 10_000, TrimCut::Exact), &f).unwrap();
        assert_eq!(p.outputs[0].source_in_ms, 0);
        // Out after the end: ends with the file.
        let p = plan_trim(&req(20_000, 99_000, TrimCut::Exact), &f).unwrap();
        assert_eq!(p.outputs[0].source_out_ms, 20_000);
        // Entirely before, after, or touching an edge.
        for (a, b) in [(0, 5_000), (25_000, 30_000), (40_000, 50_000)] {
            assert_eq!(
                plan_trim(&req(a, b, TrimCut::Exact), &f),
                Err(ExportError::OutsideFile),
                "{a}..{b}"
            );
        }
    }

    #[test]
    fn in_must_be_before_out() {
        let f = [file(1, 0, 10_000, 2_000)];
        for (a, b) in [(5_000, 5_000), (6_000, 5_000)] {
            assert_eq!(
                plan_trim(&req(a, b, TrimCut::Keyframe), &f),
                Err(ExportError::InvalidRange)
            );
        }
        let mut r = req(0, 1, TrimCut::Exact);
        (r.in_ms, r.out_ms) = (i64::MAX, i64::MIN);
        assert_eq!(plan_trim(&r, &f), Err(ExportError::InvalidRange));
    }

    #[test]
    fn extreme_points_do_not_overflow() {
        let f = [file(1, 0, 10_000, 2_000)];
        let mut r = req(0, 1, TrimCut::Exact);
        r.in_ms = i64::MIN;
        r.out_ms = i64::MAX;
        let p = plan_trim(&r, &f).unwrap();
        assert_eq!(
            (p.outputs[0].source_in_ms, p.outputs[0].source_out_ms),
            (0, 10_000)
        );
        let mut r = req(0, 1, TrimCut::Keyframe);
        r.in_ms = i64::MAX - 1;
        r.out_ms = i64::MAX;
        assert_eq!(plan_trim(&r, &f), Err(ExportError::OutsideFile));
    }

    #[test]
    fn files_without_keyframes_cannot_be_copied() {
        let mut f = file(1, 0, 10_000, 2_000);
        f.keyframes_ms.clear();
        let one = [f.clone()];
        assert_eq!(
            plan_trim(&req(1_000, 5_000, TrimCut::Keyframe), &one),
            Err(ExportError::NoKeyframes)
        );
        // An exact cut does not need them.
        assert!(plan_trim(&req(1_000, 5_000, TrimCut::Exact), &one).is_ok());
        // Another camera still gets its output.
        let two = [f, file(2, 0, 10_000, 2_000)];
        let p = plan_trim(&req(1_000, 5_000, TrimCut::Keyframe), &two).unwrap();
        assert_eq!(p.outputs.len(), 1);
        assert_eq!(p.outputs[0].camera, cam(2));
    }

    #[test]
    fn a_file_whose_first_keyframe_is_late_starts_on_it() {
        let mut f = file(1, 0, 10_000, 2_000);
        f.keyframes_ms = vec![3_000, 6_000];
        let one = [f.clone()];
        let p = plan_trim(&req(1_000, 5_000, TrimCut::Keyframe), &one).unwrap();
        assert_eq!(p.outputs[0].source_in_ms, 3_000);
        // The first keyframe is not before the out point: left out.
        assert_eq!(
            plan_trim(&req(1_000, 3_000, TrimCut::Keyframe), &one),
            Err(ExportError::OutsideFile)
        );
    }

    #[test]
    fn offsets_shift_the_file_on_the_clock() {
        let mut f = file(1, 0, 20_000, 4_000);
        f.offset_ms = -400;
        let one = [f];
        // The picture at file position 0 is at t0 - 400.
        let p = plan_trim(&req(8_000, 12_000, TrimCut::Keyframe), &one).unwrap();
        // Position of in = 8_400, keyframe at 8_000, which is at t0 + 7_600.
        assert_eq!(p.outputs[0].source_in_ms, 8_000);
        assert_eq!(p.start_ms, t0() + 7_600);
        assert_eq!(p.outputs[0].source_out_ms, 12_400);
    }

    #[test]
    fn cameras_are_chosen_in_request_order() {
        let f = [
            file(1, 0, 10_000, 2_000),
            file(2, 0, 10_000, 2_000),
            file(3, 0, 10_000, 2_000),
        ];
        let mut r = req(1_000, 5_000, TrimCut::Exact);
        r.cameras = Some(vec![cam(3), cam(1), cam(3), cam(9)]);
        let p = plan_trim(&r, &f).unwrap();
        let cams: Vec<_> = p.outputs.iter().map(|o| o.camera.clone()).collect();
        assert_eq!(cams, [cam(3), cam(1)]);
        assert_eq!(p.outputs[1].source, 1);
        r.cameras = Some(vec![]);
        assert_eq!(plan_trim(&r, &f), Err(ExportError::NoCameras));
        r.cameras = Some(vec![cam(9)]);
        assert_eq!(plan_trim(&r, &f), Err(ExportError::NoSources));
        r.cameras = None;
        assert_eq!(plan_trim(&r, &f).unwrap().outputs.len(), 3);
        assert_eq!(
            plan_trim(&r, &[]),
            Err(ExportError::NoSources),
            "no sources"
        );
    }

    #[test]
    fn a_camera_outside_the_range_is_left_out_not_fatal() {
        let f = [file(1, 0, 10_000, 2_000), file(2, 50_000, 10_000, 2_000)];
        let p = plan_trim(&req(1_000, 5_000, TrimCut::Exact), &f).unwrap();
        assert_eq!(p.outputs.len(), 1);
        assert_eq!(p.outputs[0].camera, cam(1));
    }

    #[test]
    fn names_follow_the_destination_and_share_safe_rules() {
        let f = [file(1, 0, 20_000, 4_000), file(2, 1_000, 20_000, 4_000)];
        let p = plan_trim(&req(9_000, 12_000, TrimCut::Keyframe), &f).unwrap();
        assert_eq!(p.outputs[0].name, "Door 1");
        assert_eq!(
            p.outputs[0].suggested_file,
            "/dl/folder/2026-10-10 07.42.08 Door 1 trim.mp4"
        );
        // Camera 2's file starts 1 s later; its keyframe at 8_000 is at 9 s.
        assert_eq!(
            p.outputs[1].suggested_file,
            "/dl/folder/2026-10-10 07.42.09 Door 2 trim.mp4"
        );

        let mut r = req(9_000, 12_000, TrimCut::Keyframe);
        r.dest = Some("C:\\Out".into());
        r.share_safe = true;
        let p = plan_trim(&r, &f).unwrap();
        assert!(p.share_safe);
        assert_eq!(p.outputs[0].name, "Camera A");
        assert_eq!(p.outputs[1].name, "Camera B");
        assert_eq!(
            p.outputs[0].suggested_file,
            "C:\\Out\\2026-10-10 07.42.08 Camera A trim.mp4"
        );
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("Door"), "{json}");
    }

    #[test]
    fn unsafe_and_repeated_names_are_made_safe_and_unique() {
        let mut a = file(1, 0, 20_000, 4_000);
        a.name = "Front: door/yard?".into();
        let mut b = file(2, 0, 20_000, 4_000);
        b.name = "Front: door/yard?".into();
        let p = plan_trim(&req(9_000, 12_000, TrimCut::Exact), &[a, b]).unwrap();
        assert_eq!(
            p.outputs[0].suggested_file,
            "/dl/folder/2026-10-10 07.42.09 Front- door-yard- trim.mp4"
        );
        assert_eq!(
            p.outputs[1].suggested_file,
            "/dl/folder/2026-10-10 07.42.09 Front- door-yard- trim (2).mp4"
        );
    }

    #[test]
    fn a_bad_destination_is_refused() {
        let f = [file(1, 0, 10_000, 2_000)];
        let mut r = req(1_000, 5_000, TrimCut::Exact);
        r.dest = Some("relative".into());
        assert_eq!(plan_trim(&r, &f), Err(ExportError::InvalidDest));
    }

    #[test]
    fn the_plan_is_deterministic_and_has_the_documented_json_shape() {
        let f = [file(1, 0, 30_000, 4_000)];
        let r = req(9_000, 17_500, TrimCut::Keyframe);
        let a = serde_json::to_value(plan_trim(&r, &f).unwrap()).unwrap();
        let b = serde_json::to_value(plan_trim(&r, &f).unwrap()).unwrap();
        assert_eq!(a, b);
        let o = &a["outputs"][0];
        for key in [
            "source",
            "camera",
            "name",
            "file",
            "source_in_ms",
            "source_out_ms",
            "copy_possible",
            "suggested_file",
        ] {
            assert!(o.get(key).is_some(), "{key}");
        }
        assert_eq!(o["camera"]["lens"], 0);
        assert_eq!(a["version"], 1);
        assert_eq!(a["kind"], "trim");
        assert_eq!(a["download"], "dl-1");
        assert!(a.get("share_safe").is_none());
    }

    #[test]
    fn requests_parse_from_the_documented_json() {
        let j = r#"{"kind": "trim", "download": "1791618125-9c1e5a2b7d40f316",
            "cameras": [{"device": "d-example", "channel": 3}],
            "in_ms": 1791618128250, "out_ms": 1791618139500, "cut": "keyframe",
            "dest": "/tmp/out", "share_safe": false}"#;
        let r: TrimRequest = serde_json::from_str(j).unwrap();
        assert_eq!(r.cameras.unwrap()[0].lens, 0);
        assert_eq!(r.cut, TrimCut::Keyframe);
        let r: TrimRequest =
            serde_json::from_str(r#"{"download":"x","in_ms":1,"out_ms":2,"cut":"exact"}"#).unwrap();
        assert_eq!(r.cut, TrimCut::Exact);
        assert!(r.dest.is_none() && !r.share_safe && r.cameras.is_none());
    }

    #[test]
    fn too_many_sources_are_a_limit() {
        let many: Vec<SourceFile> = (0..=MAX_SOURCES)
            .map(|i| file((i % 200) as u8, 0, 10_000, 2_000))
            .collect();
        let e = plan_trim(&req(1_000, 5_000, TrimCut::Exact), &many).unwrap_err();
        assert!(e.is_limit());
    }
}
