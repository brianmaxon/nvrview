//! Replays kept verification evidence through the cheap filters, to choose
//! their defaults with numbers (docs/NOTIFICATIONS.md, "Cheap filters").
//!
//! ```text
//! cargo run -p alerts --example replay -- EVIDENCE_DIR [LOG_DIR]
//! ```
//!
//! `EVIDENCE_DIR` holds the core's evidence sets (`<trigger id>/meta.json`:
//! the decision and the detector's boxes per picture); `LOG_DIR` the alert
//! log (`*.jsonl`), whose traces say which boxes were hits and what the
//! rules decided. Sets are replayed in time order. Every set teaches the
//! scene memory (quiet ones included); each object alert is then judged by
//! one filter at a time, with each setting, and by all filters at their
//! defaults. The table counts object alerts kept and removed (critical ones
//! apart) and, of the removed ones, those with a box that moved between the
//! pictures (a sign the filter may have removed something real). With
//! `REPLAY_LIST=1` each removed alert is listed on stderr (trigger ID,
//! channel, best score, moving, which filter), to open its pictures.
//!
//! The evidence is private (pictures of a home): run this on it locally and
//! keep the output out of the repository.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use alerts::filters::{FilterBook, FilterSettings, RepeatMemory, SceneMemory};
use alerts::model::{CameraId, Class};
use alerts::time::Millis;
use alerts::verify::Detection;
use alerts::zone::BBox;
use serde_json::Value;

fn class_of(label: &str) -> Option<Class> {
    Some(match label {
        "person" | "people" => Class::Person,
        "vehicle" | "car" | "truck" | "bus" | "motorcycle" => Class::Vehicle,
        "animal" | "dog" | "cat" | "dog_cat" => Class::Animal,
        _ => return None,
    })
}

fn class_name(c: Class) -> &'static str {
    c.lower()
}

/// One picture of a kept set.
struct Picture {
    at_ms: Millis,
    boxes: Vec<Detection>,
    /// Indexes of the boxes the check counted as hits.
    hits: Vec<usize>,
}

/// One kept set, with what the engine decided.
struct Set {
    id: u64,
    created_ms: Millis,
    camera: CameraId,
    pictures: Vec<Picture>,
    alert: bool,
    class: Option<Class>,
    critical: bool,
    always: bool,
    night: bool,
    /// Pictures with a hit the check counted (k of n).
    k_needed: f32,
}

fn f32_of(v: &Value) -> f32 {
    v.as_f64().unwrap_or(0.0) as f32
}

fn camera_of(v: &Value) -> Option<CameraId> {
    Some(CameraId::new(
        u32::try_from(v["device"].as_u64()?).ok()?,
        u16::try_from(v["channel"].as_u64()?).ok()?,
        u8::try_from(v["lens"].as_u64().unwrap_or(0)).ok()?,
    ))
}

fn load_log(dir: Option<&Path>) -> BTreeMap<u64, Value> {
    let mut out = BTreeMap::new();
    let Some(dir) = dir else { return out };
    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&p) else {
            continue;
        };
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line)
                && let Some(id) = v["decision"]["trigger_id"].as_u64()
            {
                out.insert(id, v["decision"].clone());
            }
        }
    }
    out
}

fn load_sets(dir: &Path, log: &BTreeMap<u64, Value>) -> Vec<Set> {
    let mut sets = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return sets;
    };
    for e in rd.flatten() {
        let Ok(text) = fs::read_to_string(e.path().join("meta.json")) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let id = meta["trigger_id"].as_u64().unwrap_or(0);
        let dec = log.get(&id).unwrap_or(&meta["decision"]);
        let Some(camera) = camera_of(&dec["camera"]) else {
            continue;
        };
        // Pictures in time order.
        let mut frames: Vec<(u64, Millis)> = meta["frames"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_i64()?)))
                    .collect()
            })
            .unwrap_or_default();
        frames.sort_by_key(|f| f.1);
        let steps: Vec<&Value> = dec["trace"]["steps"]
            .as_array()
            .map(|a| a.iter().collect())
            .unwrap_or_default();
        let judged: Vec<&Value> = steps
            .iter()
            .filter(|s| s["step"] == "frame_sampled")
            .copied()
            .collect();
        let class = dec["class"].as_str().and_then(class_of);
        let mut pictures = Vec::new();
        for (n, (k, at_ms)) in frames.iter().enumerate() {
            let boxes: Vec<Detection> =
                meta["methods"]["detector"]["frames"][k.to_string()]["boxes"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|b| {
                                let class = class_of(b["label"].as_str()?)?;
                                let bb = &b["box"];
                                Some(Detection {
                                    class,
                                    score: f32_of(&b["score"]),
                                    bbox: BBox::new(
                                        f32_of(&bb["x"]),
                                        f32_of(&bb["y"]),
                                        f32_of(&bb["w"]),
                                        f32_of(&bb["h"]),
                                    ),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
            // Hits: the trace's judged detections, matched by class and
            // score; without a trace, boxes of the decided class at 0.5.
            let mut hits = Vec::new();
            match judged.get(n).and_then(|s| s["detections"].as_array()) {
                Some(js) => {
                    for j in js.iter().filter(|j| j["hit"] == true) {
                        let c = j["class"].as_str().and_then(class_of);
                        let s = f32_of(&j["score"]);
                        if let Some(i) = boxes.iter().enumerate().position(|(i, b)| {
                            Some(b.class) == c && (b.score - s).abs() < 1e-5 && !hits.contains(&i)
                        }) {
                            hits.push(i);
                        }
                    }
                }
                None => {
                    for (i, b) in boxes.iter().enumerate() {
                        if Some(b.class) == class && b.score >= 0.5 {
                            hits.push(i);
                        }
                    }
                }
            }
            pictures.push(Picture {
                at_ms: *at_ms,
                boxes,
                hits,
            });
        }
        let rules = steps.iter().find(|s| s["step"] == "rules");
        let sampling = steps.iter().find(|s| s["step"] == "sampling");
        sets.push(Set {
            id,
            created_ms: meta["created_ms"].as_i64().unwrap_or(0),
            camera,
            pictures,
            alert: dec["decision"] == "alert",
            class,
            critical: dec["severity"] == "critical",
            always: dec.get("always").is_some_and(|a| !a.is_null()),
            night: rules.is_some_and(|r| r["night"] == true),
            k_needed: sampling.map_or(2.0, |s| f32_of(&s["k"])),
        });
    }
    sets.sort_by_key(|s| s.created_ms);
    sets
}

/// Which filter a run tries.
#[derive(Clone, Copy)]
enum Try {
    None,
    Scene,
    Parked,
    Repeat,
    NightFloor(f32),
    All(f32),
}

#[derive(Default)]
struct Tally {
    alerts: u32,
    removed: u32,
    critical: u32,
    critical_removed: u32,
    /// Critical alerts that stayed alerts at a lower severity.
    critical_lowered: u32,
    removed_moving: u32,
}

fn object_alert(s: &Set) -> Option<Class> {
    s.class.filter(|c| c.is_detector_class() && s.alert)
}

fn replay(sets: &[Set], fs: &FilterSettings, t: Try) -> Tally {
    let mut scene = SceneMemory::new();
    let mut repeats = RepeatMemory::new();
    let mut tally = Tally::default();
    for s in sets {
        // Place this check's hits in the memory as it was, then learn.
        let mut book = FilterBook::new();
        for p in &s.pictures {
            let placed: Vec<_> = p
                .hits
                .iter()
                .map(|&i| {
                    let b = &p.boxes[i];
                    (i, scene.place(s.camera, b.class, &b.bbox, p.at_ms, fs))
                })
                .collect();
            book.add(s.camera, p.at_ms, &p.boxes, &placed);
            scene.learn(s.camera, &p.boxes, p.at_ms, fs);
        }
        let Some(class) = object_alert(s) else {
            continue;
        };
        let now = s.pictures.last().map_or(s.created_ms, |p| p.at_ms);
        let v = book.verdict(class, fs);
        tally.alerts += 1;
        tally.critical += u32::from(s.critical);
        let floor_cut = |f: f32| {
            let hits = s
                .pictures
                .iter()
                .filter(|p| {
                    p.hits
                        .iter()
                        .any(|&i| p.boxes[i].class == class && p.boxes[i].score >= f)
                })
                .count() as f32;
            s.night && hits + 1e-6 < s.k_needed.min(s.pictures.len() as f32)
        };
        let scene_cut = fs.scene_memory && v.all_stationary();
        let repeat_cut = fs.repeat
            && repeats
                .matching(s.camera, &v.set, fs.repeat_iou, fs.repeat_window_ms(), now)
                .is_some();
        let parked = s.critical && s.always && class == Class::Person && v.moving == Some(false);
        let (removed, lowered) = match t {
            Try::None => (false, false),
            Try::Scene => (scene_cut, false),
            Try::Repeat => (repeat_cut, false),
            Try::Parked => (false, parked),
            Try::NightFloor(f) => (floor_cut(f), false),
            Try::All(f) => (scene_cut || repeat_cut || floor_cut(f), parked),
        };
        if repeat_cut {
            repeats.seen(s.camera, now);
        }
        if removed && std::env::var_os("REPLAY_LIST").is_some() {
            let best = v.set.iter().map(|d| d.score).fold(0.0f32, f32::max);
            eprintln!(
                "removed {} ch{} score {best:.2} moving {:?} scene {scene_cut} repeat {repeat_cut} critical {}",
                s.id, s.camera.channel, v.moving, s.critical
            );
        }
        if removed {
            tally.removed += 1;
            tally.critical_removed += u32::from(s.critical);
            tally.removed_moving += u32::from(v.moving == Some(true));
        } else {
            if lowered {
                tally.critical_lowered += 1;
            }
            repeats.alerted(s.camera, 0, &v.set, now);
        }
    }
    tally
}

fn row(name: &str, t: &Tally) {
    let pct = |a: u32, b: u32| {
        if b == 0 {
            0.0
        } else {
            100.0 * a as f64 / b as f64
        }
    };
    println!(
        "| {name} | {} | {} ({:.0}%) | {} | {} | {} | {} |",
        t.alerts - t.removed,
        t.removed,
        pct(t.removed, t.alerts),
        t.critical - t.critical_removed - t.critical_lowered,
        t.critical_removed,
        t.critical_lowered,
        t.removed_moving,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(ev) = args.first() else {
        eprintln!("usage: replay EVIDENCE_DIR [LOG_DIR]");
        std::process::exit(2);
    };
    let log = load_log(args.get(1).map(Path::new));
    let sets = load_sets(Path::new(ev), &log);
    let with_trace = sets
        .iter()
        .filter(|s| s.pictures.iter().any(|p| !p.hits.is_empty()))
        .count();
    let span = sets
        .last()
        .zip(sets.first())
        .map_or(0, |(l, f)| (l.created_ms - f.created_ms) / 60_000);
    let mut per_class: BTreeMap<&str, u32> = BTreeMap::new();
    for s in &sets {
        if let Some(c) = object_alert(s) {
            *per_class.entry(class_name(c)).or_default() += 1;
        }
    }
    println!(
        "{} sets over {} min ({} with hits); object alerts by class: {:?}\n",
        sets.len(),
        span,
        with_trace,
        per_class
    );
    println!(
        "| filter and setting | alerts kept | removed | critical kept | critical removed | critical lowered | removed with a moving box |"
    );
    println!("|---|---|---|---|---|---|---|");
    let d = FilterSettings::default();
    row("none (as decided)", &replay(&sets, &d, Try::None));
    for hl in [10, 30, 60, 120] {
        for hot in [0.5, 0.7, 0.85] {
            let fs = FilterSettings {
                scene_half_life_min: hl,
                scene_hot: hot,
                ..d
            };
            row(
                &format!("scene memory: half-life {hl} min, hot {hot}"),
                &replay(&sets, &fs, Try::Scene),
            );
        }
    }
    for sp in [0, 60, 120, 300] {
        let fs = FilterSettings {
            scene_learn_spacing_s: sp,
            ..d
        };
        row(
            &format!("scene memory: spacing {sp} s"),
            &replay(&sets, &fs, Try::Scene),
        );
    }
    for lm in [0.2, 0.3, 0.5] {
        let fs = FilterSettings {
            scene_learn_min_score: lm,
            ..d
        };
        row(
            &format!("scene memory: learn score {lm}"),
            &replay(&sets, &fs, Try::Scene),
        );
    }
    for cover in [0.4, 0.6, 0.8] {
        let fs = FilterSettings {
            scene_cover: cover,
            ..d
        };
        row(
            &format!("scene memory: cover {cover}"),
            &replay(&sets, &fs, Try::Scene),
        );
    }
    for iou in [0.6, 0.7, 0.8, 0.9] {
        let fs = FilterSettings {
            parked_iou: iou,
            ..d
        };
        row(
            &format!("parked (no always-alert): IoU {iou}"),
            &replay(&sets, &fs, Try::Parked),
        );
    }
    for iou in [0.5, 0.6, 0.7] {
        for w in [10, 30, 60] {
            let fs = FilterSettings {
                repeat_iou: iou,
                repeat_window_min: w,
                ..d
            };
            row(
                &format!("repeat: IoU {iou}, window {w} min"),
                &replay(&sets, &fs, Try::Repeat),
            );
        }
    }
    for f in [0.55, 0.6, 0.65, 0.7] {
        row(
            &format!("night floor {f}"),
            &replay(&sets, &d, Try::NightFloor(f)),
        );
    }
    for f in [0.5, 0.6] {
        row(
            &format!("all at defaults, night floor {f}"),
            &replay(&sets, &d, Try::All(f)),
        );
    }
}
