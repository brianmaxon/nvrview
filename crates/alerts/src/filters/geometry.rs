//! Box geometry: overlap (intersection over union), moving versus parked
//! between two pictures, and the set match behind repeat suppression.

use std::collections::BTreeMap;

use crate::model::CameraId;
use crate::time::Millis;
use crate::verify::Detection;
use crate::zone::BBox;

/// Intersection over union of two boxes (0 when either is empty or not
/// finite).
pub fn iou(a: &BBox, b: &BBox) -> f32 {
    let vals = [a.x, a.y, a.w, a.h, b.x, b.y, b.w, b.h];
    if vals.iter().any(|v| !v.is_finite()) || a.w <= 0.0 || a.h <= 0.0 || b.w <= 0.0 || b.h <= 0.0 {
        return 0.0;
    }
    let ix = ((a.x + a.w).min(b.x + b.w) - a.x.max(b.x)).max(0.0);
    let iy = ((a.y + a.h).min(b.y + b.h) - a.y.max(b.y)).max(0.0);
    let inter = ix * iy;
    let union = a.w * a.h + b.w * b.h - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// The best overlap of `d` with a box of the same class in `others`, or
/// `None` when `others` has no box of that class (nothing to compare with:
/// a missed detection is not evidence of motion).
pub fn best_same_class(d: &Detection, others: &[Detection]) -> Option<f32> {
    others
        .iter()
        .filter(|o| o.class == d.class)
        .map(|o| iou(&d.bbox, &o.bbox))
        .fold(None, |best, v| Some(best.map_or(v, |b: f32| b.max(v))))
}

/// Whether two detection sets are the same scene: the same classes with the
/// same counts, and every box of `a` paired (greedily, best first) with a
/// distinct box of `b` of its class at an IoU of at least `min_iou`.
pub fn same_set(a: &[Detection], b: &[Detection], min_iou: f32) -> bool {
    if a.is_empty() || a.len() != b.len() {
        return false;
    }
    let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
    for (i, da) in a.iter().enumerate() {
        for (j, db) in b.iter().enumerate() {
            if da.class == db.class {
                pairs.push((iou(&da.bbox, &db.bbox), i, j));
            }
        }
    }
    pairs.sort_by(|x, y| y.0.total_cmp(&x.0));
    let mut used_a = vec![false; a.len()];
    let mut used_b = vec![false; b.len()];
    let mut matched = 0;
    for (v, i, j) in pairs {
        if v < min_iou {
            break;
        }
        if !used_a[i] && !used_b[j] {
            used_a[i] = true;
            used_b[j] = true;
            matched += 1;
        }
    }
    matched == a.len()
}

/// The last alerted detection set of one camera.
#[derive(Clone, Debug, PartialEq)]
pub struct LastAlerted {
    pub alert_id: u64,
    /// When it alerted.
    pub alert_ms: Millis,
    /// When it (or a repeat of it) was last seen; the window counts from
    /// here.
    pub seen_ms: Millis,
    pub boxes: Vec<Detection>,
}

/// The most cameras remembered.
const MAX_CAMERAS: usize = 512;
/// The most boxes kept per set.
const MAX_BOXES: usize = 16;

/// The last alerted set per camera, for repeat suppression. In memory
/// only: after a restart the first alert is never a repeat.
#[derive(Clone, Debug, Default)]
pub struct RepeatMemory {
    last: BTreeMap<CameraId, LastAlerted>,
}

impl RepeatMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, camera: CameraId) -> Option<&LastAlerted> {
        self.last.get(&camera)
    }

    /// The remembered set that `boxes` repeats, if any (inside the window).
    pub fn matching(
        &self,
        camera: CameraId,
        boxes: &[Detection],
        min_iou: f32,
        window_ms: Millis,
        now_ms: Millis,
    ) -> Option<&LastAlerted> {
        self.last
            .get(&camera)
            .filter(|l| now_ms - l.seen_ms <= window_ms && same_set(boxes, &l.boxes, min_iou))
    }

    /// A repeat was seen: the window starts again.
    pub fn seen(&mut self, camera: CameraId, now_ms: Millis) {
        if let Some(l) = self.last.get_mut(&camera) {
            l.seen_ms = l.seen_ms.max(now_ms);
        }
    }

    /// An alert went out with these boxes.
    pub fn alerted(
        &mut self,
        camera: CameraId,
        alert_id: u64,
        boxes: &[Detection],
        now_ms: Millis,
    ) {
        if boxes.is_empty() {
            return;
        }
        if !self.last.contains_key(&camera) && self.last.len() >= MAX_CAMERAS {
            let oldest = self
                .last
                .iter()
                .min_by_key(|(_, l)| l.seen_ms)
                .map(|(c, _)| *c);
            if let Some(c) = oldest {
                self.last.remove(&c);
            }
        }
        self.last.insert(
            camera,
            LastAlerted {
                alert_id,
                alert_ms: now_ms,
                seen_ms: now_ms,
                boxes: boxes.iter().take(MAX_BOXES).cloned().collect(),
            },
        );
    }

    pub fn forget(&mut self, camera: CameraId) {
        self.last.remove(&camera);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Class;

    fn det(class: Class, x: f32, y: f32) -> Detection {
        Detection {
            class,
            score: 0.6,
            bbox: BBox::new(x, y, 0.1, 0.2),
        }
    }

    #[test]
    fn overlap() {
        let a = BBox::new(0.0, 0.0, 0.2, 0.2);
        assert!((iou(&a, &a) - 1.0).abs() < 1e-6);
        assert_eq!(iou(&a, &BBox::new(0.5, 0.5, 0.1, 0.1)), 0.0);
        let half = iou(&a, &BBox::new(0.1, 0.0, 0.2, 0.2));
        assert!((half - 1.0 / 3.0).abs() < 1e-5, "{half}");
        assert_eq!(iou(&a, &BBox::new(f32::NAN, 0.0, 0.1, 0.1)), 0.0);
        assert_eq!(iou(&a, &BBox::new(0.0, 0.0, 0.0, 0.1)), 0.0);
    }

    #[test]
    fn same_class_match() {
        let p = det(Class::Person, 0.1, 0.1);
        assert_eq!(best_same_class(&p, &[det(Class::Vehicle, 0.1, 0.1)]), None);
        let v = best_same_class(&p, &[det(Class::Person, 0.5, 0.5), p.clone()]).unwrap();
        assert!((v - 1.0).abs() < 1e-6);
    }

    #[test]
    fn sets() {
        let a = vec![det(Class::Person, 0.1, 0.1), det(Class::Person, 0.5, 0.1)];
        let b = vec![det(Class::Person, 0.51, 0.1), det(Class::Person, 0.1, 0.11)];
        assert!(same_set(&a, &b, 0.6));
        assert!(!same_set(&a, &b[..1], 0.6), "counts differ");
        let c = vec![det(Class::Person, 0.1, 0.1), det(Class::Vehicle, 0.5, 0.1)];
        assert!(!same_set(&a, &c, 0.6), "classes differ");
        let moved = vec![det(Class::Person, 0.1, 0.1), det(Class::Person, 0.7, 0.1)];
        assert!(!same_set(&a, &moved, 0.6));
        assert!(!same_set(&[], &[], 0.6));
    }

    #[test]
    fn repeat_window() {
        let cam = CameraId::new(1, 0, 0);
        let set = vec![det(Class::Person, 0.1, 0.1)];
        let mut m = RepeatMemory::new();
        assert!(m.matching(cam, &set, 0.6, 60_000, 0).is_none());
        m.alerted(cam, 7, &set, 1_000);
        assert_eq!(
            m.matching(cam, &set, 0.6, 60_000, 30_000).unwrap().alert_id,
            7
        );
        m.seen(cam, 50_000);
        assert!(m.matching(cam, &set, 0.6, 60_000, 100_000).is_some());
        assert!(m.matching(cam, &set, 0.6, 60_000, 120_000).is_none());
        m.forget(cam);
        assert!(m.get(cam).is_none());
    }
}
