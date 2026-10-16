//! What one check saw, picture by picture, for the filters: every box (for
//! matching across pictures), which boxes were hits, and where each hit
//! sat in the static-scene memory when its picture came in.

use serde::{Deserialize, Serialize};

use crate::filters::FilterSettings;
use crate::filters::geometry::best_same_class;
use crate::filters::scene::Placement;
use crate::model::{CameraId, Class};
use crate::time::Millis;
use crate::verify::Detection;

/// Pictures kept per check.
const MAX_PICTURES: usize = 16;
/// Boxes kept per picture.
const MAX_BOXES: usize = 32;

#[derive(Clone, Debug, PartialEq)]
struct Hit {
    index: usize,
    stationary: bool,
    for_ms: Option<Millis>,
}

#[derive(Clone, Debug, PartialEq)]
struct Picture {
    camera: CameraId,
    at_ms: Millis,
    boxes: Vec<Detection>,
    hits: Vec<Hit>,
}

/// One check's pictures.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FilterBook {
    pictures: Vec<Picture>,
}

/// What the filters found for the class a check settled on.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterVerdict {
    pub class: Class,
    /// Hit boxes of the class, over all pictures.
    pub hits: u32,
    /// Of those, the ones over hot cells of the scene memory.
    pub stationary: u32,
    /// For an all-stationary check: how long the place has been warm (the
    /// shortest over the hits, so "at least this long").
    pub stationary_for_ms: Option<Millis>,
    /// Moved between pictures (`Some(false)`: every hit that could be
    /// compared stayed in place); `None` when nothing could be compared.
    pub moving: Option<bool>,
    /// The hit boxes (every allowed class) of the picture with the most
    /// hits: the set repeat suppression compares.
    pub set: Vec<Detection>,
}

impl FilterVerdict {
    /// Every hit of the class is stationary.
    pub fn all_stationary(&self) -> bool {
        self.hits > 0 && self.stationary == self.hits
    }
}

impl FilterBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one picture: its boxes, and for each hit its index into
    /// `detections` and its placement in the memory.
    pub fn add(
        &mut self,
        camera: CameraId,
        at_ms: Millis,
        detections: &[Detection],
        hits: &[(usize, Placement)],
    ) {
        if self.pictures.len() >= MAX_PICTURES {
            return;
        }
        self.pictures.push(Picture {
            camera,
            at_ms,
            boxes: detections.iter().take(MAX_BOXES).cloned().collect(),
            hits: hits
                .iter()
                .filter(|(i, _)| *i < MAX_BOXES.min(detections.len()))
                .map(|(i, p)| Hit {
                    index: *i,
                    stationary: p.stationary,
                    for_ms: p.for_ms,
                })
                .collect(),
        });
    }

    /// The other picture of the same camera nearest in time to picture `i`.
    fn partner(&self, i: usize) -> Option<&Picture> {
        let p = &self.pictures[i];
        self.pictures
            .iter()
            .enumerate()
            .filter(|(j, q)| *j != i && q.camera == p.camera)
            .min_by_key(|(_, q)| (q.at_ms - p.at_ms).abs())
            .map(|(_, q)| q)
    }

    pub fn verdict(&self, class: Class, s: &FilterSettings) -> FilterVerdict {
        let mut hits = 0u32;
        let mut stationary = 0u32;
        let mut for_ms: Option<Millis> = None;
        let mut moved = false;
        let mut parked = false;
        for (i, p) in self.pictures.iter().enumerate() {
            for h in &p.hits {
                let d = &p.boxes[h.index];
                if d.class != class {
                    continue;
                }
                hits += 1;
                if h.stationary {
                    stationary += 1;
                    if let Some(f) = h.for_ms {
                        for_ms = Some(for_ms.map_or(f, |x| x.min(f)));
                    }
                }
                if let Some(q) = self.partner(i)
                    && let Some(v) = best_same_class(d, &q.boxes)
                {
                    if v >= s.parked_iou {
                        parked = true;
                    } else {
                        moved = true;
                    }
                }
            }
        }
        let moving = if moved {
            Some(true)
        } else if parked {
            Some(false)
        } else {
            None
        };
        let set = self
            .pictures
            .iter()
            .filter(|p| !p.hits.is_empty())
            .max_by(|a, b| {
                a.hits
                    .len()
                    .cmp(&b.hits.len())
                    // Ties: the earlier picture.
                    .then(b.at_ms.cmp(&a.at_ms))
            })
            .map(|p| p.hits.iter().map(|h| p.boxes[h.index].clone()).collect())
            .unwrap_or_default();
        FilterVerdict {
            class,
            hits,
            stationary,
            stationary_for_ms: if hits > 0 && stationary == hits {
                for_ms
            } else {
                None
            },
            moving,
            set,
        }
    }
}

/// Which filter changed a decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterKind {
    /// Static-scene memory: every hit sat where its class has been a while.
    SceneMemory,
    /// The same boxes as the last alert on this camera.
    Repeat,
    /// Nothing moved between the pictures, so the always-alert rule for a
    /// person outside at night did not apply.
    Parked,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zone::BBox;

    const CAM: CameraId = CameraId::new(1, 0, 0);

    fn d(class: Class, x: f32) -> Detection {
        Detection {
            class,
            score: 0.7,
            bbox: BBox::new(x, 0.3, 0.1, 0.3),
        }
    }

    fn place(stationary: bool) -> Placement {
        Placement {
            stationary,
            hot_share: if stationary { 1.0 } else { 0.0 },
            for_ms: stationary.then_some(600_000),
        }
    }

    #[test]
    fn parked_moving_and_unknown() {
        let s = FilterSettings::default();
        // Same place in both pictures: parked.
        let mut b = FilterBook::new();
        b.add(CAM, 0, &[d(Class::Person, 0.2)], &[(0, place(false))]);
        b.add(CAM, 1000, &[d(Class::Person, 0.201)], &[(0, place(false))]);
        let v = b.verdict(Class::Person, &s);
        assert_eq!((v.hits, v.moving), (2, Some(false)));
        // Moved.
        let mut b = FilterBook::new();
        b.add(CAM, 0, &[d(Class::Person, 0.2)], &[(0, place(false))]);
        b.add(CAM, 1000, &[d(Class::Person, 0.26)], &[]);
        assert_eq!(b.verdict(Class::Person, &s).moving, Some(true));
        // One picture, or no box of the class in the other: unknown.
        let mut b = FilterBook::new();
        b.add(CAM, 0, &[d(Class::Person, 0.2)], &[(0, place(false))]);
        assert_eq!(b.verdict(Class::Person, &s).moving, None);
        b.add(CAM, 1000, &[d(Class::Vehicle, 0.2)], &[]);
        assert_eq!(b.verdict(Class::Person, &s).moving, None);
        // Another camera's picture is not a partner.
        let mut b = FilterBook::new();
        b.add(CAM, 0, &[d(Class::Person, 0.2)], &[(0, place(false))]);
        b.add(CameraId::new(1, 1, 0), 500, &[d(Class::Person, 0.6)], &[]);
        assert_eq!(b.verdict(Class::Person, &s).moving, None);
    }

    #[test]
    fn stationary_counts_and_set() {
        let s = FilterSettings::default();
        let mut b = FilterBook::new();
        b.add(
            CAM,
            0,
            &[d(Class::Person, 0.2), d(Class::Person, 0.6)],
            &[(0, place(true)), (1, place(true))],
        );
        b.add(CAM, 1000, &[d(Class::Person, 0.2)], &[(0, place(true))]);
        let v = b.verdict(Class::Person, &s);
        assert!(v.all_stationary());
        assert_eq!(v.stationary_for_ms, Some(600_000));
        assert_eq!(v.set.len(), 2, "the picture with the most hits");
        // One moving hit among stationary ones: not all stationary.
        b.add(CAM, 2000, &[d(Class::Person, 0.4)], &[(0, place(false))]);
        let v = b.verdict(Class::Person, &s);
        assert!(!v.all_stationary());
        assert_eq!(v.stationary_for_ms, None);
        // Out-of-range hit indexes are dropped.
        let mut b = FilterBook::new();
        b.add(CAM, 0, &[d(Class::Person, 0.2)], &[(5, place(true))]);
        assert_eq!(b.verdict(Class::Person, &s).hits, 0);
        assert!(!b.verdict(Class::Person, &s).all_stationary());
    }
}
