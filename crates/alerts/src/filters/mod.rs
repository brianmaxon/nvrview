//! The cheap filters that run on a check's boxes before the rules alert
//! (docs/ARCHITECTURE.md, "Static-scene memory and repeat suppression"):
//!
//! - [`scene`]: static-scene memory. Per camera and class, a coarse grid
//!   that warms where boxes of that class keep appearing and cools by time
//!   otherwise; a box mostly over warm cells is stationary (a statue, a
//!   parked car, an object the detector keeps mistaking for a person) and does not alert.
//! - [`geometry`]: box overlap, moving versus parked across the pictures of
//!   a check, and the match used by repeat suppression.
//! - [`book`]: what one check saw, picture by picture, and the verdict the
//!   engine acts on.
//!
//! Everything here is sans-I/O: times come from the host, and the memory
//! is saved and loaded as bytes by the host.

use serde::{Deserialize, Serialize};

use crate::settings::{Issue, clamp_f32, clamp_u32};
use crate::time::{MINUTE_MS, Millis, SECOND_MS};

pub mod book;
pub mod geometry;
pub mod scene;

pub use book::{FilterBook, FilterVerdict};
pub use geometry::{RepeatMemory, iou};
pub use scene::SceneMemory;

/// The filters' settings, with defaults chosen by replaying kept evidence
/// (docs/NOTIFICATIONS.md, "Cheap filters").
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterSettings {
    /// Static-scene memory on or off.
    pub scene_memory: bool,
    /// Minutes for a cell's warmth to halve when nothing covers it.
    pub scene_half_life_min: u32,
    /// How far one learned picture moves a covered cell toward 1.
    pub scene_rise: f32,
    /// A cell at or above this warmth is "hot".
    pub scene_hot: f32,
    /// A box is stationary when at least this share of its cells is hot.
    pub scene_cover: f32,
    /// One camera and class learns at most once per this many seconds, so
    /// warmth measures time spent in place, not pictures taken.
    pub scene_learn_spacing_s: u32,
    /// Boxes at or above this score are learned (lower than the alert floor:
    /// the detector's weak, repeated guesses at one spot are evidence too).
    pub scene_learn_min_score: f32,
    /// A box whose match in the other picture overlaps it at least this much
    /// (IoU) did not move: it is parked.
    pub parked_iou: f32,
    /// A parked box cannot make the "person on an outside camera at night"
    /// rule apply by itself.
    pub parked_blocks_always: bool,
    /// Repeat suppression on or off.
    pub repeat: bool,
    /// Boxes match the last alerted set at or above this IoU.
    pub repeat_iou: f32,
    /// How long the last alerted set is remembered after it was last seen.
    pub repeat_window_min: u32,
}

impl Default for FilterSettings {
    fn default() -> Self {
        FilterSettings {
            scene_memory: true,
            scene_half_life_min: 30,
            scene_rise: 0.35,
            scene_hot: 0.7,
            scene_cover: 0.6,
            scene_learn_spacing_s: 120,
            scene_learn_min_score: 0.3,
            parked_iou: 0.8,
            parked_blocks_always: true,
            repeat: true,
            repeat_iou: 0.6,
            repeat_window_min: 30,
        }
    }
}

impl FilterSettings {
    pub fn half_life_ms(&self) -> Millis {
        Millis::from(self.scene_half_life_min) * MINUTE_MS
    }

    pub fn learn_spacing_ms(&self) -> Millis {
        Millis::from(self.scene_learn_spacing_s) * SECOND_MS
    }

    pub fn repeat_window_ms(&self) -> Millis {
        Millis::from(self.repeat_window_min) * MINUTE_MS
    }

    /// Clamps every value into its allowed range.
    pub(crate) fn validate(&mut self, issues: &mut Vec<Issue>) {
        clamp_u32(
            &mut self.scene_half_life_min,
            1,
            24 * 60,
            "scene memory half-life",
            issues,
        );
        clamp_f32(&mut self.scene_rise, 0.01, 1.0, "scene memory rise", issues);
        clamp_f32(
            &mut self.scene_hot,
            0.05,
            0.99,
            "scene memory hot level",
            issues,
        );
        clamp_f32(
            &mut self.scene_cover,
            0.05,
            1.0,
            "scene memory cover",
            issues,
        );
        clamp_u32(
            &mut self.scene_learn_spacing_s,
            0,
            3600,
            "scene memory spacing",
            issues,
        );
        clamp_f32(
            &mut self.scene_learn_min_score,
            0.05,
            0.99,
            "scene memory learn score",
            issues,
        );
        clamp_f32(&mut self.parked_iou, 0.3, 0.99, "parked overlap", issues);
        clamp_f32(&mut self.repeat_iou, 0.3, 0.99, "repeat overlap", issues);
        clamp_u32(
            &mut self.repeat_window_min,
            1,
            24 * 60,
            "repeat window",
            issues,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_clamps_and_round_trips() {
        let mut s = FilterSettings {
            scene_rise: f32::NAN,
            scene_hot: 2.0,
            parked_iou: 0.0,
            scene_half_life_min: 0,
            repeat_window_min: 100_000,
            ..FilterSettings::default()
        };
        let mut issues = Vec::new();
        s.validate(&mut issues);
        assert_eq!(issues.len(), 5);
        assert_eq!(s.scene_rise, 0.01);
        assert_eq!(s.scene_hot, 0.99);
        assert_eq!(s.parked_iou, 0.3);
        assert_eq!(s.scene_half_life_min, 1);
        assert_eq!(s.repeat_window_min, 1440);
        let mut d = FilterSettings::default();
        let mut none = Vec::new();
        d.validate(&mut none);
        assert!(none.is_empty(), "defaults are in range");
        let back: FilterSettings = serde_json::from_str("{\"repeat\":false}").unwrap();
        assert!(!back.repeat && back.scene_memory);
    }
}
