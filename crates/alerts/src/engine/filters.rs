//! The engine's use of the cheap filters (crate::filters): each picture's
//! hits are placed in the static-scene memory and the picture is learned;
//! at the decision, the filters' verdict may quiet the event (stationary,
//! a repeat) or keep a parked box from making an always-alert case.

use super::Engine;
use crate::decision::QuietReason;
use crate::filters::book::FilterKind;
use crate::filters::scene::Placement;
use crate::filters::{FilterBook, FilterSettings, FilterVerdict, RepeatMemory, SceneMemory};
use crate::model::{CameraId, Class};
use crate::rules::{RuleInput, RuleOutcome, Support, evaluate};
use crate::settings::AlwaysAlertSettings;
use crate::time::Millis;
use crate::trace::StepKind;
use crate::verify::{Judged, Sample};

use super::Now;

/// Places a picture's hits in the memory (marking them in `judged`), keeps
/// the picture in the check's book, then learns it when `learn`.
pub(super) fn note_picture(
    scene: &mut SceneMemory,
    fs: &FilterSettings,
    book: &mut FilterBook,
    sample: &Sample,
    judged: &mut [Judged],
    learn: bool,
    now_ms: Millis,
) {
    let mut hits: Vec<(usize, Placement)> = Vec::new();
    for (i, (j, d)) in judged.iter_mut().zip(&sample.detections).enumerate() {
        if j.hit {
            let p = scene.place(sample.camera, d.class, &d.bbox, now_ms, fs);
            j.stationary = fs.scene_memory && p.stationary;
            hits.push((i, p));
        }
    }
    book.add(sample.camera, sample.at_ms, &sample.detections, &hits);
    if learn {
        scene.learn(sample.camera, &sample.detections, now_ms, fs);
    }
}

/// The filters' verdict on one check, and what it changes.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Filtered {
    pub verdict: FilterVerdict,
    /// The alert these boxes repeat, and how long ago it alerted.
    pub repeat_of: Option<(u64, Millis)>,
    pub acted: Option<FilterKind>,
}

impl Filtered {
    pub fn step(&self) -> StepKind {
        StepKind::Filters {
            class: self.verdict.class,
            hits: self.verdict.hits,
            stationary: self.verdict.stationary,
            stationary_for_ms: self.verdict.stationary_for_ms,
            moving: self.verdict.moving,
            repeat_of: self.repeat_of.map(|r| r.0),
            acted: self.acted,
        }
    }

    /// The quiet-log reason when a filter holds the alert back.
    pub fn quiet_reason(&self) -> Option<QuietReason> {
        match self.acted? {
            FilterKind::SceneMemory => Some(QuietReason::Stationary {
                for_ms: self.verdict.stationary_for_ms.unwrap_or(0),
            }),
            FilterKind::Repeat => self
                .repeat_of
                .map(|(alert_id, since_ms)| QuietReason::Repeat { alert_id, since_ms }),
            FilterKind::Parked => None,
        }
    }

    fn holds_back(&self) -> bool {
        matches!(
            self.acted,
            Some(FilterKind::SceneMemory | FilterKind::Repeat)
        )
    }
}

/// The rules, with the "person outside at night" rule off when a filter
/// holds the event back or the person did not move; returns whether being
/// parked is what turned the rule off.
pub(super) fn rules_with(
    input: &RuleInput,
    always: &AlwaysAlertSettings,
    filtered: Option<&Filtered>,
    fs: &FilterSettings,
) -> (RuleOutcome, bool) {
    let full = evaluate(input, always);
    let Some(f) = filtered else {
        return (full, false);
    };
    let parked = fs.parked_blocks_always && f.verdict.moving == Some(false);
    if !(parked || f.holds_back()) || input.class != Class::Person {
        return (full, false);
    }
    let mut off = *always;
    off.night_outside_person = false;
    let rules = evaluate(input, &off);
    let parked_acted = parked && !f.holds_back() && rules.always != full.always;
    (rules, parked_acted)
}

impl Engine {
    /// The filters' verdict for a check decided on `camera`.
    pub(super) fn judge_filters(
        &mut self,
        verdict: FilterVerdict,
        camera: CameraId,
        support: Support,
        test: bool,
        now: Now,
    ) -> Filtered {
        let fs = self.settings.filters;
        let ours = matches!(support, Support::Verified | Support::Weak);
        let repeat_of = if fs.repeat && ours {
            self.repeats
                .matching(
                    camera,
                    &verdict.set,
                    fs.repeat_iou,
                    fs.repeat_window_ms(),
                    now.ms,
                )
                .map(|l| (l.alert_id, (now.ms - l.alert_ms).max(0)))
        } else {
            None
        };
        // A simulated trigger is shown what the filters found but is never
        // held back by them (real activity never quiets a test).
        let acted = if test || !ours {
            None
        } else if fs.scene_memory && verdict.all_stationary() {
            Some(FilterKind::SceneMemory)
        } else if repeat_of.is_some() {
            Some(FilterKind::Repeat)
        } else {
            None
        };
        if !test && repeat_of.is_some() {
            self.repeats.seen(camera, now.ms);
        }
        Filtered {
            verdict,
            repeat_of,
            acted,
        }
    }

    /// Learns a picture taken outside a check (a "still present" re-check):
    /// quiet pictures are where most of the stationary evidence is.
    pub fn learn(&mut self, sample: &Sample, now: Now) -> bool {
        self.scene.learn(
            sample.camera,
            &sample.detections,
            now.ms,
            &self.settings.filters,
        )
    }

    pub fn scene(&self) -> &SceneMemory {
        &self.scene
    }

    /// Replaces the scene memory (the host loads its saved copy).
    pub fn set_scene(&mut self, scene: SceneMemory) {
        self.scene = scene;
    }

    /// The memory changed since the host last saved it; the host then saves
    /// [`SceneMemory::to_bytes`] and calls [`Engine::scene_saved`].
    pub fn scene_dirty(&self) -> bool {
        self.scene.is_dirty()
    }

    pub fn scene_saved(&mut self) {
        self.scene.mark_saved();
    }

    pub fn repeats(&self) -> &RepeatMemory {
        &self.repeats
    }

    /// Forgets what the filters learned about one camera (a local data
    /// action; nothing is sent to the camera).
    pub fn reset_scene(&mut self, camera: CameraId) -> bool {
        let had = self.repeats.get(camera).is_some();
        self.repeats.forget(camera);
        self.scene.reset(camera) || had
    }
}
