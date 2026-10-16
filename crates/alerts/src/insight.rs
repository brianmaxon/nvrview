//! Insight layers: what the engine has learned about one camera, in a
//! generic form an app can draw without knowing the technique behind it.
//!
//! A layer has an id, a title, units, a value range, a short description,
//! a provenance note, and one of three kinds of data: a `grid` (a heatmap of
//! `rows` by `cols` values, row by row from the top left), `boxes` (boxes in
//! picture coordinates), or `scalar` (named values such as thresholds). A
//! new technique appears in the app by adding a layer here.

use serde::{Deserialize, Serialize};

use crate::engine::Engine;
use crate::filters::scene::{COLS, ROWS};
use crate::model::{CameraId, Class};
use crate::time::Millis;
use crate::verify::Detection;

/// The layers of one camera.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Insights {
    pub camera: CameraId,
    pub at_ms: Millis,
    /// Whether the night thresholds are in force now.
    pub night: bool,
    pub layers: Vec<Layer>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    pub id: String,
    pub title: String,
    pub description: String,
    /// Where the method comes from.
    pub provenance: String,
    pub units: String,
    /// The values' range, for a color scale.
    pub range: [f32; 2],
    #[serde(flatten)]
    pub data: LayerData,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayerData {
    Grid {
        rows: u32,
        cols: u32,
        /// `rows * cols` values, row by row from the top left.
        values: Vec<f32>,
        /// The class the grid is about, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class: Option<Class>,
        /// A level the method treats specially (drawn as a contour or noted
        /// in the legend).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_ms: Option<Millis>,
    },
    Boxes {
        boxes: Vec<Detection>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_ms: Option<Millis>,
    },
    Scalar {
        items: Vec<ScalarItem>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScalarItem {
    pub id: String,
    pub label: String,
    pub value: f32,
    pub units: String,
}

fn round3(v: f32) -> f32 {
    (v * 1000.0).round() / 1000.0
}

fn item(id: &str, label: &str, value: f32, units: &str) -> ScalarItem {
    ScalarItem {
        id: id.into(),
        label: label.into(),
        value: round3(value),
        units: units.into(),
    }
}

/// The layers for `camera` at `now_ms`; `night` says which score floor is in
/// force.
pub fn insights(engine: &Engine, camera: CameraId, now_ms: Millis, night: bool) -> Insights {
    let settings = engine.settings();
    let fs = settings.filters;
    let mut layers = Vec::new();
    for g in engine.scene().view(camera, now_ms, &fs) {
        layers.push(Layer {
            id: format!("scene.{}", g.class.lower()),
            title: format!("Scene memory: {}", g.class.lower()),
            description: format!(
                "Where {} boxes have stayed. Cells warm each time a box covers them \
                 (at most once per {} s) and halve every {} min; a box with {}% of \
                 its cells at or above {:.2} is treated as stationary and does not alert.",
                g.class.lower(),
                fs.scene_learn_spacing_s,
                fs.scene_half_life_min,
                (fs.scene_cover * 100.0).round(),
                fs.scene_hot
            ),
            provenance: "A per-cell running average with exponential forgetting, as in \
                         adaptive background models (Stauffer and Grimson, 1999), kept \
                         over detector boxes per class instead of pixels."
                .into(),
            units: "warmth".into(),
            range: [0.0, 1.0],
            data: LayerData::Grid {
                rows: ROWS as u32,
                cols: COLS as u32,
                values: g.values.into_iter().map(round3).collect(),
                class: Some(g.class),
                threshold: Some(fs.scene_hot),
                updated_ms: g.learned_ms,
            },
        });
    }
    if let Some(last) = engine.repeats().get(camera) {
        layers.push(Layer {
            id: "repeat.last".into(),
            title: "Last alerted boxes".into(),
            description: format!(
                "The boxes of this camera's last alert. A later check with the same \
                 classes and count, each box overlapping by IoU {:.2} or more, within \
                 {} min of the last time they were seen, does not alert again.",
                fs.repeat_iou, fs.repeat_window_min
            ),
            provenance: "Greedy box matching by intersection over union, the overlap \
                         measure of the PASCAL VOC evaluation (Everingham et al., 2010)."
                .into(),
            units: "score".into(),
            range: [0.0, 1.0],
            data: LayerData::Boxes {
                boxes: last.boxes.clone(),
                at_ms: Some(last.alert_ms),
            },
        });
    }
    let day = settings.score_floor(camera, false);
    let night_floor = settings.score_floor(camera, true);
    layers.push(Layer {
        id: "thresholds".into(),
        title: "Thresholds".into(),
        description: "The values the filters and the check use on this camera now.".into(),
        provenance: "Settings (defaults chosen by replaying kept evidence).".into(),
        units: String::new(),
        range: [0.0, 1.0],
        data: LayerData::Scalar {
            items: vec![
                item("floor.day", "Score floor by day", day, "score"),
                item("floor.night", "Score floor at night", night_floor, "score"),
                item(
                    "scene.hot",
                    "Scene memory hot level",
                    fs.scene_hot,
                    "warmth",
                ),
                item(
                    "scene.cover",
                    "Share of hot cells for stationary",
                    fs.scene_cover,
                    "share",
                ),
                item(
                    "scene.half_life",
                    "Scene memory half-life",
                    fs.scene_half_life_min as f32,
                    "min",
                ),
                item("parked.iou", "Parked overlap", fs.parked_iou, "IoU"),
                item("repeat.iou", "Repeat overlap", fs.repeat_iou, "IoU"),
                item(
                    "repeat.window",
                    "Repeat window",
                    fs.repeat_window_min as f32,
                    "min",
                ),
            ],
        },
    });
    Insights {
        camera,
        at_ms: now_ms,
        night,
        layers,
    }
}
