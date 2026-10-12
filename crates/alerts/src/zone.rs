//! Zones and regions of interest: polygons in normalized picture
//! coordinates (0..1 across and down), tested by a box's foot point.

use serde::{Deserialize, Serialize};

/// A detection box in normalized coordinates: left, top, width, height.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl BBox {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        BBox { x, y, w, h }
    }

    /// The bottom center: where a person or vehicle meets the ground, which
    /// is what decides whether it is "in" a zone drawn on the ground.
    pub fn foot(&self) -> [f32; 2] {
        [self.x + self.w / 2.0, self.y + self.h]
    }
}

/// The most points a polygon keeps.
pub const MAX_POLYGON_POINTS: usize = 32;

/// A simple polygon in normalized coordinates.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Polygon(pub Vec<[f32; 2]>);

impl Polygon {
    /// Whether the polygon has enough points to enclose anything.
    pub fn is_usable(&self) -> bool {
        self.0.len() >= 3
    }

    /// Even-odd test. Points on the boundary may land either side.
    pub fn contains(&self, p: [f32; 2]) -> bool {
        if !self.is_usable() || !p[0].is_finite() || !p[1].is_finite() {
            return false;
        }
        let pts = &self.0;
        let mut inside = false;
        let mut j = pts.len() - 1;
        for i in 0..pts.len() {
            let (xi, yi) = (pts[i][0], pts[i][1]);
            let (xj, yj) = (pts[j][0], pts[j][1]);
            if (yi > p[1]) != (yj > p[1]) && p[0] < (xj - xi) * (p[1] - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            j = i;
        }
        inside
    }

    /// Drops non-finite points, clamps the rest to the picture, and keeps at
    /// most [`MAX_POLYGON_POINTS`]. Returns whether anything changed.
    pub fn sanitize(&mut self) -> bool {
        let before = self.0.clone();
        self.0.retain(|p| p[0].is_finite() && p[1].is_finite());
        for p in &mut self.0 {
            p[0] = p[0].clamp(0.0, 1.0);
            p[1] = p[1].clamp(0.0, 1.0);
        }
        self.0.truncate(MAX_POLYGON_POINTS);
        self.0 != before
    }
}

/// A zone on one camera. Include zones limit alerts to themselves; exclude
/// zones (masks) remove areas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Zone {
    pub polygon: Polygon,
    #[serde(default = "yes")]
    pub include: bool,
}

fn yes() -> bool {
    true
}

/// Whether a box counts on a camera with these zones: its foot point is in
/// some include zone (or there are none) and in no exclude zone.
pub fn box_counts(zones: &[Zone], b: &BBox) -> bool {
    let foot = b.foot();
    let mut any_include = false;
    let mut in_include = false;
    for z in zones.iter().filter(|z| z.polygon.is_usable()) {
        if z.include {
            any_include = true;
            in_include |= z.polygon.contains(foot);
        } else if z.polygon.contains(foot) {
            return false;
        }
    }
    !any_include || in_include
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f32, y0: f32, x1: f32, y1: f32) -> Polygon {
        Polygon(vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]])
    }

    #[test]
    fn point_in_polygon() {
        let sq = square(0.2, 0.2, 0.6, 0.6);
        assert!(sq.contains([0.4, 0.4]));
        assert!(!sq.contains([0.7, 0.4]));
        assert!(!sq.contains([f32::NAN, 0.4]));
        // A concave L shape.
        let l = Polygon(vec![
            [0.0, 0.0],
            [0.5, 0.0],
            [0.5, 0.5],
            [1.0, 0.5],
            [1.0, 1.0],
            [0.0, 1.0],
        ]);
        assert!(l.contains([0.25, 0.25]));
        assert!(!l.contains([0.75, 0.25]));
        assert!(l.contains([0.75, 0.75]));
        assert!(!Polygon(vec![[0.0, 0.0], [1.0, 1.0]]).contains([0.5, 0.5]));
    }

    #[test]
    fn foot_point_decides() {
        let zones = vec![Zone {
            polygon: square(0.0, 0.5, 1.0, 1.0),
            include: true,
        }];
        // Head above the zone, feet in it: counts.
        assert!(box_counts(&zones, &BBox::new(0.4, 0.2, 0.1, 0.4)));
        // Entirely above the zone: does not.
        assert!(!box_counts(&zones, &BBox::new(0.4, 0.0, 0.1, 0.3)));
        // No zones: everything counts.
        assert!(box_counts(&[], &BBox::new(0.0, 0.0, 0.1, 0.1)));
        // An exclude zone wins over an include zone.
        let mut z2 = zones.clone();
        z2.push(Zone {
            polygon: square(0.3, 0.5, 0.6, 1.0),
            include: false,
        });
        assert!(!box_counts(&z2, &BBox::new(0.4, 0.2, 0.1, 0.4)));
        assert!(box_counts(&z2, &BBox::new(0.7, 0.2, 0.1, 0.4)));
    }

    #[test]
    fn sanitize_clamps_and_caps() {
        let mut p = Polygon(vec![[-0.5, 0.5], [f32::NAN, 0.0], [1.5, 2.0], [0.5, 0.5]]);
        assert!(p.sanitize());
        assert_eq!(p.0, vec![[0.0, 0.5], [1.0, 1.0], [0.5, 0.5]]);
        assert!(!p.sanitize());
        let mut big = Polygon(vec![[0.5, 0.5]; 100]);
        big.sanitize();
        assert_eq!(big.0.len(), MAX_POLYGON_POINTS);
    }
}
