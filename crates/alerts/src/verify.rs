//! Verification inputs and k-of-n counting.
//!
//! The host samples pictures (keyframes from the main stream of each
//! camera angle), runs its detector, and hands the boxes in as
//! [`Sample`]s. This module only counts: which samples have a box of an
//! allowed class above the score floor whose foot point is in the region
//! (and the camera's zones), weighted per angle, against the k needed.
//! The engine adds what the cheap filters found (crate::filters): which hits
//! sit in the static-scene memory, and [`Check::moving`].

use serde::{Deserialize, Serialize};

use crate::model::{CameraId, Class};
use crate::time::Millis;
use crate::zone::{BBox, Polygon, Zone, box_counts};

/// One detector box.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    pub class: Class,
    pub score: f32,
    #[serde(rename = "box")]
    pub bbox: BBox,
}

/// The detector's output for one picture of one camera angle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub camera: CameraId,
    /// The picture's time.
    pub at_ms: Millis,
    pub detections: Vec<Detection>,
}

/// One angle taking part in a check: where to look and how much a hit
/// counts.
#[derive(Clone, Debug, PartialEq)]
pub struct Angle {
    pub camera: CameraId,
    /// The region of interest; an unusable (empty) polygon is the whole
    /// picture.
    pub roi: Polygon,
    /// The camera's own zones and masks.
    pub zones: Vec<Zone>,
    pub weight: f32,
}

/// What a check looks for.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckSpec {
    pub angles: Vec<Angle>,
    pub classes: Vec<Class>,
    pub min_score: f32,
    /// Weighted hits needed.
    pub k: f32,
    /// Samples outside [from, until] are ignored.
    pub from_ms: Millis,
    pub until_ms: Millis,
}

/// How one detection in a sample was judged.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Judged {
    pub class: Class,
    pub score: f32,
    /// At or above the score floor.
    pub above: bool,
    /// Foot point in the region and the camera's zones.
    pub in_region: bool,
    /// Counted as a hit (an allowed class, above, in the region).
    pub hit: bool,
    /// A hit over hot cells of the static-scene memory.
    #[serde(default, skip_serializing_if = "is_false")]
    pub stationary: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// The running count for one class.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClassCount {
    /// Weighted hits so far.
    pub weighted: f32,
    /// Samples with a hit.
    pub hits: u32,
    /// Bit n set when angle n had a hit.
    pub angle_bits: u32,
    pub best_score: f32,
    /// Samples with a box of this class above the floor but outside the
    /// region.
    pub outside_region: u32,
}

impl ClassCount {
    pub fn angles(&self) -> u32 {
        self.angle_bits.count_ones()
    }
}

/// The result of a check over the samples so far.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Check {
    /// k reached for `class`.
    Verified {
        class: Class,
        hits: u32,
        n: u32,
        angles: u32,
        score: f32,
        /// Moving (`true`) or parked (`false`) between the pictures, when
        /// a hit could be compared with the other picture.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        moving: Option<bool>,
    },
    /// Some hits, fewer than k.
    Weak {
        class: Class,
        hits: u32,
        n: u32,
        score: f32,
    },
    /// Pictures were checked and nothing allowed was in the region.
    NotSeen {
        n: u32,
        /// Something allowed was seen, but only outside the region.
        outside_region: bool,
    },
    /// No picture could be checked.
    Unavailable,
}

impl Check {
    pub fn is_verified(&self) -> bool {
        matches!(self, Check::Verified { .. })
    }

    /// The class a check with hits settled on.
    pub fn class(&self) -> Option<Class> {
        match self {
            Check::Verified { class, .. } | Check::Weak { class, .. } => Some(*class),
            _ => None,
        }
    }

    /// The same check with the geometry's moving or parked result.
    pub fn with_moving(self, m: Option<bool>) -> Check {
        match self {
            Check::Verified {
                class,
                hits,
                n,
                angles,
                score,
                ..
            } => Check::Verified {
                class,
                hits,
                n,
                angles,
                score,
                moving: m,
            },
            other => other,
        }
    }
}

/// Accumulates samples for one check.
#[derive(Clone, Debug)]
pub struct Counter {
    spec: CheckSpec,
    n: u32,
    counts: Vec<(Class, ClassCount)>,
    /// Per angle, samples counted.
    per_angle: Vec<u32>,
}

impl Counter {
    pub fn new(spec: CheckSpec) -> Self {
        let counts = spec
            .classes
            .iter()
            .map(|&c| (c, ClassCount::default()))
            .collect();
        let per_angle = vec![0; spec.angles.len()];
        Counter {
            spec,
            n: 0,
            counts,
            per_angle,
        }
    }

    pub fn spec(&self) -> &CheckSpec {
        &self.spec
    }

    /// Extends the time window (an external retrigger).
    pub fn extend_until(&mut self, until_ms: Millis) {
        self.spec.until_ms = self.spec.until_ms.max(until_ms);
    }

    /// Adds one sample. Returns `None` when it does not belong to the check
    /// (unmapped camera or outside the window), else how each detection
    /// was judged.
    pub fn add(&mut self, sample: &Sample) -> Option<Vec<Judged>> {
        if sample.at_ms < self.spec.from_ms || sample.at_ms > self.spec.until_ms {
            return None;
        }
        let ai = self
            .spec
            .angles
            .iter()
            .position(|a| a.camera == sample.camera)?;
        let angle = &self.spec.angles[ai];
        self.n += 1;
        self.per_angle[ai] += 1;
        let mut judged = Vec::with_capacity(sample.detections.len());
        let mut hit_classes: Vec<(Class, f32)> = Vec::new();
        let mut outside: Vec<Class> = Vec::new();
        for d in &sample.detections {
            let allowed = self.spec.classes.contains(&d.class);
            let above = d.score.is_finite() && d.score >= self.spec.min_score;
            let in_roi = !angle.roi.is_usable() || angle.roi.contains(d.bbox.foot());
            let in_region = in_roi && box_counts(&angle.zones, &d.bbox);
            let hit = allowed && above && in_region;
            judged.push(Judged {
                class: d.class,
                score: d.score,
                above,
                in_region,
                hit,
                stationary: false,
            });
            if hit {
                match hit_classes.iter_mut().find(|(c, _)| *c == d.class) {
                    Some((_, s)) => *s = s.max(d.score),
                    None => hit_classes.push((d.class, d.score)),
                }
            } else if allowed && above && !outside.contains(&d.class) {
                outside.push(d.class);
            }
        }
        let weight = if angle.weight.is_finite() {
            angle.weight.max(0.0)
        } else {
            0.0
        };
        for (class, count) in &mut self.counts {
            if let Some((_, s)) = hit_classes.iter().find(|(c, _)| c == class) {
                count.hits += 1;
                count.weighted += weight;
                count.angle_bits |= 1 << ai.min(31);
                count.best_score = count.best_score.max(*s);
            } else if outside.contains(class) {
                count.outside_region += 1;
            }
        }
        Some(judged)
    }

    /// Samples counted so far.
    pub fn n(&self) -> u32 {
        self.n
    }

    pub fn counts(&self) -> &[(Class, ClassCount)] {
        &self.counts
    }

    /// The per-class count with the most weighted hits (ties: the order the
    /// classes were listed in).
    fn best(&self) -> Option<(Class, ClassCount)> {
        let mut best: Option<(Class, ClassCount)> = None;
        for &(c, count) in &self.counts {
            if count.hits > 0 && best.is_none_or(|(_, b)| count.weighted > b.weighted) {
                best = Some((c, count));
            }
        }
        best
    }

    /// The camera angle with the most hits for `class` (the first angle
    /// when none).
    pub fn best_camera(&self) -> Option<CameraId> {
        let class = self.best().map(|(c, _)| c);
        let first = self.spec.angles.first().map(|a| a.camera);
        let Some(class) = class else { return first };
        let bits = self
            .counts
            .iter()
            .find(|(c, _)| *c == class)
            .map_or(0, |(_, cc)| cc.angle_bits);
        self.spec
            .angles
            .iter()
            .enumerate()
            .find(|(i, _)| bits & (1 << (*i).min(31)) != 0)
            .map(|(_, a)| a.camera)
            .or(first)
    }

    /// Whether k has been reached (the host may stop sampling).
    pub fn reached(&self) -> bool {
        self.best()
            .is_some_and(|(_, c)| c.weighted + 1e-6 >= self.spec.k)
    }

    pub fn result(&self) -> Check {
        if self.n == 0 {
            return Check::Unavailable;
        }
        match self.best() {
            Some((class, c)) if c.weighted + 1e-6 >= self.spec.k => Check::Verified {
                class,
                hits: c.hits,
                n: self.n,
                angles: c.angles(),
                score: c.best_score,
                moving: None,
            },
            Some((class, c)) => Check::Weak {
                class,
                hits: c.hits,
                n: self.n,
                score: c.best_score,
            },
            None => Check::NotSeen {
                n: self.n,
                outside_region: self.counts.iter().any(|(_, c)| c.outside_region > 0),
            },
        }
    }
}

/// How unusual an event is here at this time, from learned baselines
/// (a later slice). "About 1 in `one_in`" of comparable time slots on this
/// camera had an event of this class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rarity {
    pub one_in: u32,
}

/// Where learned baselines plug in.
pub trait RarityModel {
    fn rarity(&self, camera: CameraId, class: Class, at_ms: Millis) -> Option<Rarity>;
}

/// The stand-in until baselines exist: nothing is known, rules decide alone.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoBaseline;

impl RarityModel for NoBaseline {
    fn rarity(&self, _: CameraId, _: Class, _: Millis) -> Option<Rarity> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: CameraId = CameraId::new(1, 0, 0);
    const B: CameraId = CameraId::new(1, 1, 0);

    fn person(score: f32, x: f32) -> Detection {
        Detection {
            class: Class::Person,
            score,
            bbox: BBox::new(x, 0.4, 0.1, 0.3),
        }
    }

    fn spec(k: f32, roi: Polygon) -> CheckSpec {
        CheckSpec {
            angles: vec![
                Angle {
                    camera: A,
                    roi: roi.clone(),
                    zones: vec![],
                    weight: 1.0,
                },
                Angle {
                    camera: B,
                    roi,
                    zones: vec![],
                    weight: 1.0,
                },
            ],
            classes: vec![Class::Person, Class::Vehicle],
            min_score: 0.5,
            k,
            from_ms: 0,
            until_ms: 10_000,
        }
    }

    fn sample(camera: CameraId, at_ms: Millis, d: Vec<Detection>) -> Sample {
        Sample {
            camera,
            at_ms,
            detections: d,
        }
    }

    #[test]
    fn two_of_two_in_one_angle() {
        let mut c = Counter::new(spec(2.0, Polygon::default()));
        c.add(&sample(A, 100, vec![person(0.8, 0.4)])).unwrap();
        assert!(!c.reached());
        assert!(matches!(c.result(), Check::Weak { hits: 1, n: 1, .. }));
        c.add(&sample(A, 1100, vec![person(0.7, 0.42)])).unwrap();
        assert!(c.reached());
        assert_eq!(
            c.result(),
            Check::Verified {
                class: Class::Person,
                hits: 2,
                n: 2,
                angles: 1,
                score: 0.8,
                moving: None
            }
        );
    }

    #[test]
    fn across_angles_and_window() {
        let mut c = Counter::new(spec(2.0, Polygon::default()));
        // Outside the window and an unmapped camera are ignored.
        assert!(c.add(&sample(A, 20_000, vec![person(0.9, 0.4)])).is_none());
        assert!(
            c.add(&sample(CameraId::new(2, 0, 0), 100, vec![person(0.9, 0.4)]))
                .is_none()
        );
        c.add(&sample(A, 100, vec![person(0.6, 0.4)])).unwrap();
        c.add(&sample(B, 150, vec![person(0.6, 0.1)])).unwrap();
        match c.result() {
            Check::Verified { angles, hits, .. } => assert_eq!((angles, hits), (2, 2)),
            other => panic!("{other:?}"),
        }
        assert_eq!(c.best_camera(), Some(A));
    }

    #[test]
    fn region_and_floor() {
        let roi = Polygon(vec![[0.0, 0.0], [0.3, 0.0], [0.3, 1.0], [0.0, 1.0]]);
        let mut c = Counter::new(spec(1.0, roi));
        let j = c
            .add(&sample(A, 100, vec![person(0.9, 0.6), person(0.3, 0.1)]))
            .unwrap();
        assert_eq!(j.len(), 2);
        assert!(j[0].above && !j[0].in_region && !j[0].hit);
        assert!(!j[1].above && j[1].in_region && !j[1].hit);
        assert_eq!(
            c.result(),
            Check::NotSeen {
                n: 1,
                outside_region: true
            }
        );
        // A class not asked for never counts.
        let mut c = Counter::new(spec(1.0, Polygon::default()));
        c.add(&sample(
            A,
            100,
            vec![Detection {
                class: Class::Animal,
                score: 0.99,
                bbox: BBox::new(0.1, 0.1, 0.1, 0.1),
            }],
        ));
        assert_eq!(
            c.result(),
            Check::NotSeen {
                n: 1,
                outside_region: false
            }
        );
        assert_eq!(
            Counter::new(spec(1.0, Polygon::default())).result(),
            Check::Unavailable
        );
    }

    #[test]
    fn weights() {
        let mut s = spec(2.0, Polygon::default());
        s.angles[1].weight = 2.0;
        let mut c = Counter::new(s);
        c.add(&sample(B, 100, vec![person(0.6, 0.4)]));
        assert!(c.reached());
        assert_eq!(c.best_camera(), Some(B));
        let mut s = spec(1.0, Polygon::default());
        s.angles[0].weight = 0.0;
        let mut c = Counter::new(s);
        c.add(&sample(A, 100, vec![person(0.9, 0.4)]));
        assert!(matches!(c.result(), Check::Weak { .. }));
    }

    #[test]
    fn stand_in_baseline() {
        assert_eq!(NoBaseline.rarity(A, Class::Person, 0), None);
    }
}
