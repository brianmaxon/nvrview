//! Static-scene memory: per camera and class, a coarse grid of warmth.
//!
//! When a learned picture has a box of a class, every cell the box covers
//! moves toward 1 by [`FilterSettings::scene_rise`]; with nothing covering
//! it a cell halves every [`FilterSettings::scene_half_life_min`]. A camera
//! and class learns at most once per spacing, so warmth grows with the time
//! something has stayed in place, not with how often pictures were taken.
//! A box with most of its cells hot sits where boxes of its class have been
//! for a while: it is stationary. Grids are kept per class so a parked car
//! never hides a person walking past it.
//!
//! The memory is a running per-cell average with exponential forgetting,
//! the idea behind adaptive background models (Stauffer and Grimson, 1999),
//! applied to detector boxes instead of pixels.

use std::collections::BTreeMap;

use crate::filters::FilterSettings;
use crate::model::{CameraId, Class};
use crate::time::{MINUTE_MS, Millis};
use crate::verify::Detection;
use crate::zone::BBox;

pub const COLS: usize = 64;
pub const ROWS: usize = 36;
pub const CELLS: usize = COLS * ROWS;
/// Below this a cell counts as cold (its "since" time is forgotten).
const COLD: f32 = 0.05;
/// The most grids kept (cameras times classes); the least recently updated
/// goes first.
pub const MAX_GRIDS: usize = 384;
/// The saved form's version.
pub const FILE_VERSION: u32 = 1;
const MAGIC: &[u8; 4] = b"NVSM";

#[derive(Clone, Debug, PartialEq)]
struct Grid {
    values: Vec<f32>,
    /// Per cell, the minute (since the epoch) it last warmed up from cold;
    /// 0 when cold.
    since_min: Vec<u32>,
    /// When `values` were last decayed to.
    updated_ms: Millis,
    /// When it last learned.
    learned_ms: Option<Millis>,
}

impl Grid {
    fn new(now_ms: Millis) -> Self {
        Grid {
            values: vec![0.0; CELLS],
            since_min: vec![0; CELLS],
            updated_ms: now_ms,
            learned_ms: None,
        }
    }

    fn factor(&self, now_ms: Millis, half_life_ms: Millis) -> f32 {
        let dt = now_ms - self.updated_ms;
        if dt <= 0 || half_life_ms <= 0 {
            return 1.0;
        }
        0.5f64.powf(dt as f64 / half_life_ms as f64) as f32
    }

    fn decay(&mut self, now_ms: Millis, half_life_ms: Millis) {
        let f = self.factor(now_ms, half_life_ms);
        if f < 1.0 {
            for (v, s) in self.values.iter_mut().zip(self.since_min.iter_mut()) {
                *v *= f;
                if *v < COLD {
                    *v = 0.0;
                    *s = 0;
                }
            }
        }
        self.updated_ms = self.updated_ms.max(now_ms);
    }
}

/// The cells a box covers: those whose centers are inside it, or the cell
/// under its center when it is smaller than a cell.
pub fn cells(b: &BBox) -> Vec<usize> {
    let ok = [b.x, b.y, b.w, b.h].iter().all(|v| v.is_finite());
    if !ok {
        return Vec::new();
    }
    let range = |lo: f32, len: f32, n: usize| {
        let a = (lo * n as f32 - 0.5).ceil().max(0.0) as usize;
        let z = ((lo + len) * n as f32 - 0.5).floor();
        let z = if z < 0.0 {
            None
        } else {
            Some((z as usize).min(n - 1))
        };
        (a, z)
    };
    let (c0, c1) = range(b.x, b.w, COLS);
    let (r0, r1) = range(b.y, b.h, ROWS);
    let mut out = Vec::new();
    if let (Some(c1), Some(r1)) = (c1, r1) {
        for r in r0..=r1 {
            for c in c0..=c1 {
                out.push(r * COLS + c);
            }
        }
    }
    if out.is_empty() {
        let cx = ((b.x + b.w / 2.0) * COLS as f32).clamp(0.0, COLS as f32 - 1.0) as usize;
        let cy = ((b.y + b.h / 2.0) * ROWS as f32).clamp(0.0, ROWS as f32 - 1.0) as usize;
        out.push(cy * COLS + cx);
    }
    out
}

/// How one box sits in the memory.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Most of its cells are hot.
    pub stationary: bool,
    /// The share of its cells that are hot.
    pub hot_share: f32,
    /// For a stationary box: how long its hot cells have been warm (the
    /// most recent of them, so "at least this long").
    pub for_ms: Option<Millis>,
}

/// One grid as the insight view shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct GridView {
    pub class: Class,
    pub values: Vec<f32>,
    pub learned_ms: Option<Millis>,
}

/// The static-scene memory of every camera and class.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneMemory {
    grids: BTreeMap<(CameraId, Class), Grid>,
    /// Changed since the host last saved it.
    dirty: bool,
}

impl SceneMemory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    pub fn len(&self) -> usize {
        self.grids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.grids.is_empty()
    }

    /// Where `bbox` of `class` sits in the camera's memory at `now_ms`.
    pub fn place(
        &self,
        camera: CameraId,
        class: Class,
        bbox: &BBox,
        now_ms: Millis,
        s: &FilterSettings,
    ) -> Placement {
        let cold = Placement {
            stationary: false,
            hot_share: 0.0,
            for_ms: None,
        };
        let Some(g) = self.grids.get(&(camera, class)) else {
            return cold;
        };
        let cells = cells(bbox);
        if cells.is_empty() {
            return cold;
        }
        let f = g.factor(now_ms, s.half_life_ms());
        let mut hot = 0usize;
        let mut since: Option<u32> = None;
        for &i in &cells {
            if g.values[i] * f >= s.scene_hot {
                hot += 1;
                let m = g.since_min[i];
                if m > 0 {
                    since = Some(since.map_or(m, |x| x.max(m)));
                }
            }
        }
        let hot_share = hot as f32 / cells.len() as f32;
        let stationary = hot > 0 && hot_share + 1e-6 >= s.scene_cover;
        Placement {
            stationary,
            hot_share,
            for_ms: if stationary {
                since.map(|m| (now_ms - Millis::from(m) * MINUTE_MS).max(0))
            } else {
                None
            },
        }
    }

    /// Learns one picture's boxes (detector classes at or above the learn
    /// score). Returns whether any grid learned (spacing may hold it off).
    pub fn learn(
        &mut self,
        camera: CameraId,
        detections: &[Detection],
        now_ms: Millis,
        s: &FilterSettings,
    ) -> bool {
        let mut by_class: BTreeMap<Class, Vec<&BBox>> = BTreeMap::new();
        for d in detections {
            if d.class.is_detector_class()
                && d.score.is_finite()
                && d.score >= s.scene_learn_min_score
            {
                by_class.entry(d.class).or_default().push(&d.bbox);
            }
        }
        let mut any = false;
        let minute = u32::try_from((now_ms / MINUTE_MS).max(1)).unwrap_or(u32::MAX);
        for (class, boxes) in by_class {
            let key = (camera, class);
            if !self.grids.contains_key(&key) {
                self.make_room();
            }
            let g = self.grids.entry(key).or_insert_with(|| Grid::new(now_ms));
            if g.learned_ms
                .is_some_and(|t| now_ms >= t && now_ms - t < s.learn_spacing_ms())
            {
                continue;
            }
            g.decay(now_ms, s.half_life_ms());
            let mut covered = vec![false; CELLS];
            for b in boxes {
                for i in cells(b) {
                    covered[i] = true;
                }
            }
            for (i, c) in covered.into_iter().enumerate() {
                if c {
                    if g.values[i] < COLD || g.since_min[i] == 0 {
                        g.since_min[i] = minute;
                    }
                    g.values[i] += (1.0 - g.values[i]) * s.scene_rise;
                }
            }
            g.learned_ms = Some(now_ms);
            any = true;
        }
        self.dirty |= any;
        any
    }

    fn make_room(&mut self) {
        while self.grids.len() >= MAX_GRIDS {
            let oldest = self
                .grids
                .iter()
                .min_by_key(|(_, g)| g.learned_ms.unwrap_or(g.updated_ms))
                .map(|(k, _)| *k);
            match oldest {
                Some(k) => {
                    self.grids.remove(&k);
                }
                None => break,
            }
        }
    }

    /// Forgets one camera (every class).
    pub fn reset(&mut self, camera: CameraId) -> bool {
        let before = self.grids.len();
        self.grids.retain(|(c, _), _| *c != camera);
        let changed = self.grids.len() != before;
        self.dirty |= changed;
        changed
    }

    /// The cameras with any memory, each with its classes.
    pub fn cameras(&self) -> Vec<(CameraId, Vec<Class>)> {
        let mut out: Vec<(CameraId, Vec<Class>)> = Vec::new();
        for (cam, class) in self.grids.keys() {
            match out.last_mut() {
                Some((c, classes)) if c == cam => classes.push(*class),
                _ => out.push((*cam, vec![*class])),
            }
        }
        out
    }

    /// A camera's grids, decayed to `now_ms`, in class order.
    pub fn view(&self, camera: CameraId, now_ms: Millis, s: &FilterSettings) -> Vec<GridView> {
        self.grids
            .iter()
            .filter(|((c, _), _)| *c == camera)
            .map(|((_, class), g)| {
                let f = g.factor(now_ms, s.half_life_ms());
                GridView {
                    class: *class,
                    values: g
                        .values
                        .iter()
                        .map(|v| if v * f < COLD { 0.0 } else { v * f })
                        .collect(),
                    learned_ms: g.learned_ms,
                }
            })
            .collect()
    }

    /// The saved form: a small versioned binary file (warmth quantized to a
    /// byte per cell). At most [`MAX_GRIDS`] grids of about 11.5 KB.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(20 + self.grids.len() * (CELLS * 5 + 32));
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&FILE_VERSION.to_le_bytes());
        out.extend_from_slice(&(COLS as u32).to_le_bytes());
        out.extend_from_slice(&(ROWS as u32).to_le_bytes());
        out.extend_from_slice(&(self.grids.len() as u32).to_le_bytes());
        for ((cam, class), g) in &self.grids {
            out.extend_from_slice(&cam.device.to_le_bytes());
            out.extend_from_slice(&cam.channel.to_le_bytes());
            out.push(cam.lens);
            out.push(class_code(*class));
            out.extend_from_slice(&g.updated_ms.to_le_bytes());
            out.extend_from_slice(&g.learned_ms.unwrap_or(i64::MIN).to_le_bytes());
            out.extend(
                g.values
                    .iter()
                    .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8),
            );
            for s in &g.since_min {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        out
    }

    /// Reads the saved form; anything malformed or of another version or
    /// size gives `None` (the host then starts empty).
    pub fn from_bytes(bytes: &[u8]) -> Option<SceneMemory> {
        let mut r = Reader { b: bytes, at: 0 };
        if r.take(4)? != MAGIC
            || r.u32()? != FILE_VERSION
            || r.u32()? as usize != COLS
            || r.u32()? as usize != ROWS
        {
            return None;
        }
        let n = r.u32()? as usize;
        if n > MAX_GRIDS {
            return None;
        }
        let mut grids = BTreeMap::new();
        for _ in 0..n {
            let device = r.u32()?;
            let channel = u16::from_le_bytes(r.take(2)?.try_into().ok()?);
            let lens = r.take(1)?[0];
            let class = class_of_code(r.take(1)?[0])?;
            let updated_ms = i64::from_le_bytes(r.take(8)?.try_into().ok()?);
            let learned = i64::from_le_bytes(r.take(8)?.try_into().ok()?);
            let values = r
                .take(CELLS)?
                .iter()
                .map(|&b| f32::from(b) / 255.0)
                .collect();
            let mut since_min = Vec::with_capacity(CELLS);
            for _ in 0..CELLS {
                since_min.push(r.u32()?);
            }
            grids.insert(
                (CameraId::new(device, channel, lens), class),
                Grid {
                    values,
                    since_min,
                    updated_ms,
                    learned_ms: (learned != i64::MIN).then_some(learned),
                },
            );
        }
        (r.at == bytes.len()).then_some(SceneMemory {
            grids,
            dirty: false,
        })
    }
}

fn class_code(c: Class) -> u8 {
    match c {
        Class::Person => 0,
        Class::Vehicle => 1,
        Class::Animal => 2,
        _ => 255,
    }
}

fn class_of_code(b: u8) -> Option<Class> {
    Some(match b {
        0 => Class::Person,
        1 => Class::Vehicle,
        2 => Class::Animal,
        _ => return None,
    })
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.b.get(self.at..end)?;
        self.at = end;
        Some(s)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAM: CameraId = CameraId::new(1, 2, 0);
    const T0: Millis = 1_800_000_000_000;

    fn person(x: f32, y: f32, score: f32) -> Detection {
        Detection {
            class: Class::Person,
            score,
            bbox: BBox::new(x, y, 0.1, 0.3),
        }
    }

    fn s() -> FilterSettings {
        FilterSettings::default()
    }

    #[test]
    fn covered_cells() {
        let c = cells(&BBox::new(0.0, 0.0, 1.0, 1.0));
        assert_eq!(c.len(), CELLS);
        // A box smaller than a cell still has one.
        assert_eq!(cells(&BBox::new(0.5, 0.5, 0.001, 0.001)).len(), 1);
        // A box at the right edge stays in the picture.
        assert!(
            cells(&BBox::new(0.99, 0.99, 0.5, 0.5))
                .iter()
                .all(|&i| i < CELLS)
        );
        assert!(cells(&BBox::new(f32::NAN, 0.0, 0.1, 0.1)).is_empty());
        let b = cells(&BBox::new(0.25, 0.5, 0.25, 0.25));
        assert_eq!(b.len(), 16 * 9);
    }

    #[test]
    fn warms_with_time_in_place_and_cools() {
        let mut m = SceneMemory::new();
        let p = person(0.4, 0.4, 0.6);
        // Many pictures within the spacing count once.
        for i in 0..10 {
            m.learn(CAM, std::slice::from_ref(&p), T0 + i * 1000, &s());
        }
        assert!(
            !m.place(CAM, Class::Person, &p.bbox, T0 + 10_000, &s())
                .stationary
        );
        // Three spaced learns: 1 - 0.65^3 = 0.73 is hot.
        m.learn(CAM, std::slice::from_ref(&p), T0 + 2 * MINUTE_MS, &s());
        assert!(
            !m.place(CAM, Class::Person, &p.bbox, T0 + 2 * MINUTE_MS, &s())
                .stationary
        );
        m.learn(CAM, std::slice::from_ref(&p), T0 + 4 * MINUTE_MS, &s());
        let at = m.place(CAM, Class::Person, &p.bbox, T0 + 4 * MINUTE_MS, &s());
        assert!(at.stationary, "{at:?}");
        assert_eq!(at.for_ms, Some(4 * MINUTE_MS));
        // Per class: a vehicle in the same place is not stationary.
        assert!(
            !m.place(CAM, Class::Vehicle, &p.bbox, T0 + 4 * MINUTE_MS, &s())
                .stationary
        );
        // Another camera has its own memory.
        assert!(
            !m.place(CameraId::new(1, 3, 0), Class::Person, &p.bbox, T0, &s())
                .stationary
        );
        // Elsewhere in the picture is not.
        let away = person(0.0, 0.0, 0.6);
        assert!(
            !m.place(CAM, Class::Person, &away.bbox, T0 + 4 * MINUTE_MS, &s())
                .stationary
        );
        // A box half over the hot place is not "mostly over" it.
        let half = BBox::new(0.45, 0.4, 0.1, 0.3);
        assert!(
            !m.place(CAM, Class::Person, &half, T0 + 4 * MINUTE_MS, &s())
                .stationary
        );
        // One half-life later it has cooled below hot.
        assert!(
            !m.place(CAM, Class::Person, &p.bbox, T0 + 34 * MINUTE_MS, &s())
                .stationary
        );
        // Low scores below the learn floor are not learned.
        let mut m2 = SceneMemory::new();
        assert!(!m2.learn(CAM, &[person(0.4, 0.4, 0.1)], T0, &s()));
        assert!(m2.is_empty() && !m2.is_dirty());
    }

    #[test]
    fn saves_and_loads() {
        let mut m = SceneMemory::new();
        for i in 0..3 {
            m.learn(CAM, &[person(0.4, 0.4, 0.6)], T0 + i * 3 * MINUTE_MS, &s());
        }
        m.learn(
            CameraId::new(9, 0, 1),
            &[Detection {
                class: Class::Vehicle,
                score: 0.9,
                bbox: BBox::new(0.1, 0.1, 0.3, 0.2),
            }],
            T0,
            &s(),
        );
        assert!(m.is_dirty());
        let bytes = m.to_bytes();
        let back = SceneMemory::from_bytes(&bytes).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back.cameras().len(), 2);
        let p = person(0.4, 0.4, 0.6).bbox;
        let now = T0 + 6 * MINUTE_MS;
        assert_eq!(
            back.place(CAM, Class::Person, &p, now, &s()).stationary,
            m.place(CAM, Class::Person, &p, now, &s()).stationary
        );
        // Corrupt, truncated, or another version: nothing.
        assert!(SceneMemory::from_bytes(&bytes[..bytes.len() - 1]).is_none());
        let mut other = bytes.clone();
        other[4] = 9;
        assert!(SceneMemory::from_bytes(&other).is_none());
        assert!(SceneMemory::from_bytes(b"junk").is_none());
        assert!(SceneMemory::from_bytes(&[]).is_none());
        // Reset forgets one camera.
        let mut back = back;
        assert!(back.reset(CAM));
        assert!(!back.reset(CAM));
        assert_eq!(back.len(), 1);
        assert!(back.view(CAM, now, &s()).is_empty());
        assert_eq!(back.view(CameraId::new(9, 0, 1), now, &s()).len(), 1);
    }

    #[test]
    fn bounded() {
        let mut m = SceneMemory::new();
        for ch in 0..(MAX_GRIDS as u16 + 10) {
            m.learn(
                CameraId::new(1, ch, 0),
                &[person(0.1, 0.1, 0.9)],
                T0 + Millis::from(ch),
                &s(),
            );
        }
        assert_eq!(m.len(), MAX_GRIDS);
        // The oldest went first.
        assert!(m.view(CameraId::new(1, 0, 0), T0, &s()).is_empty());
        assert!(m.to_bytes().len() < MAX_GRIDS * (CELLS * 5 + 32) + 32);
    }
}
