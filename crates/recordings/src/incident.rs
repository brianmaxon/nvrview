//! Incidents: one real-world event with every camera that saw it
//! (docs/PLAYBACK.md, "Incidents").
//!
//! [`group`] is deterministic: the same events and settings always give the
//! same incidents with the same IDs, in the same order. Events join an
//! incident when they start within [`GroupingConfig::gap`] seconds of the
//! end of the incident so far, on cameras of the same group (any cameras
//! when no groups are set; a camera outside every group is a group of its
//! own). An incident keeps, per camera, the union of its events' times;
//! downloads add [`Incident::margin`] before and after.
//!
//! The user's edits are plain functions on incidents: [`merge`], [`split`],
//! [`Incident::add_camera`], [`Incident::remove_camera`], and renaming
//! through [`Incident::name`].

use crate::Secs;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A camera: a channel of a device.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CameraId {
    /// The app's identifier of the device.
    pub device: String,
    /// Channel on that device.
    pub channel: u8,
}

impl CameraId {
    /// A camera.
    pub fn new(device: impl Into<String>, channel: u8) -> CameraId {
        CameraId {
            device: device.into(),
            channel,
        }
    }
}

/// One event of one camera.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CameraEvent {
    /// Start (naive seconds).
    pub start: Secs,
    /// End (naive seconds).
    pub end: Secs,
    /// The camera.
    pub camera: CameraId,
    /// Type as the device names it.
    pub kind: String,
}

/// Cameras the user treats as covering one area.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraGroup {
    /// The group's name (also part of incident IDs).
    pub name: String,
    /// Its cameras.
    pub cameras: Vec<CameraId>,
}

/// Grouping settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupingConfig {
    /// Largest pause between events of one incident, in seconds (default 10).
    pub gap: Secs,
    /// Seconds added before and after each camera's range (default 5).
    pub margin: Secs,
    /// Camera groups; when empty, all cameras form one group.
    pub groups: Vec<CameraGroup>,
}

impl Default for GroupingConfig {
    fn default() -> Self {
        GroupingConfig {
            gap: 10,
            margin: 5,
            groups: Vec::new(),
        }
    }
}

/// An incident.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incident {
    /// Stable identifier, derived from the group, the cameras, and the
    /// first start and last end (an incident still growing gets a new one).
    pub id: String,
    /// The user's name for it, if renamed.
    pub name: Option<String>,
    /// The camera group it belongs to, if any.
    pub group: Option<String>,
    /// Seconds to add before and after each camera's range for downloads.
    pub margin: Secs,
    /// Its events, sorted.
    pub events: Vec<CameraEvent>,
    /// Cameras the user added without events of their own.
    pub extra_cameras: BTreeSet<CameraId>,
}

/// Merges overlapping or touching intervals.
fn union(mut v: Vec<(Secs, Secs)>) -> Vec<(Secs, Secs)> {
    v.sort_unstable();
    let mut out: Vec<(Secs, Secs)> = Vec::new();
    for (s, e) in v {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// FNV-1a, for short stable IDs (not for security).
fn fnv(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain([0xff]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

impl Incident {
    fn build(group: Option<String>, margin: Secs, mut events: Vec<CameraEvent>) -> Incident {
        events.sort();
        events.dedup();
        let mut inc = Incident {
            id: String::new(),
            name: None,
            group,
            margin,
            events,
            extra_cameras: BTreeSet::new(),
        };
        inc.id = inc.compute_id();
        inc
    }

    fn compute_id(&self) -> String {
        let cams: Vec<String> = self
            .cameras()
            .iter()
            .map(|c| format!("{}:{}", c.device, c.channel))
            .collect();
        let mut parts: Vec<&str> = vec![self.group.as_deref().unwrap_or("")];
        parts.extend(cams.iter().map(String::as_str));
        let (start, end) = (self.start().to_string(), self.end().to_string());
        parts.push(&start);
        parts.push(&end);
        format!("{}-{:016x}", self.start(), fnv(&parts))
    }

    /// Start of the first event (naive seconds; 0 without events).
    pub fn start(&self) -> Secs {
        self.events.iter().map(|e| e.start).min().unwrap_or(0)
    }

    /// End of the last event.
    pub fn end(&self) -> Secs {
        self.events.iter().map(|e| e.end).max().unwrap_or(0)
    }

    /// Every camera involved (with events, or added by the user).
    pub fn cameras(&self) -> BTreeSet<CameraId> {
        let mut s: BTreeSet<CameraId> = self.events.iter().map(|e| e.camera.clone()).collect();
        s.extend(self.extra_cameras.iter().cloned());
        s
    }

    /// Event types involved.
    pub fn kinds(&self) -> BTreeSet<String> {
        self.events.iter().map(|e| e.kind.clone()).collect()
    }

    /// Per camera, the union of its events' times (no margin). A camera
    /// added without events gets the incident's whole span.
    pub fn intervals(&self) -> BTreeMap<CameraId, Vec<(Secs, Secs)>> {
        let mut m: BTreeMap<CameraId, Vec<(Secs, Secs)>> = BTreeMap::new();
        for e in &self.events {
            m.entry(e.camera.clone())
                .or_default()
                .push((e.start, e.end));
        }
        for c in &self.extra_cameras {
            m.entry(c.clone())
                .or_default()
                .push((self.start(), self.end()));
        }
        m.into_iter().map(|(c, v)| (c, union(v))).collect()
    }

    /// Per camera, the one range to download: from its first event's start
    /// minus the margin to its last event's end plus the margin.
    pub fn download_ranges(&self) -> BTreeMap<CameraId, (Secs, Secs)> {
        self.intervals()
            .into_iter()
            .filter_map(|(c, v)| {
                let s = v.first()?.0;
                let e = v.last()?.1;
                Some((c, (s - self.margin, e + self.margin)))
            })
            .collect()
    }

    /// Adds a camera that has no events of its own in this incident (the
    /// user knows it saw something). Its range is the incident's span.
    pub fn add_camera(&mut self, camera: CameraId) {
        if !self.events.iter().any(|e| e.camera == camera) {
            self.extra_cameras.insert(camera);
        }
    }

    /// Removes a camera and its events. `false` if that would leave the
    /// incident with no camera (it is not removed then).
    pub fn remove_camera(&mut self, camera: &CameraId) -> bool {
        let remaining = self.cameras().into_iter().filter(|c| c != camera).count();
        if remaining == 0 {
            return false;
        }
        self.events.retain(|e| &e.camera != camera);
        self.extra_cameras.remove(camera);
        true
    }
}

/// Which group a camera is in: the first configured group that lists it,
/// a group of its own otherwise; one shared group when none are set.
fn group_of(cfg: &GroupingConfig, camera: &CameraId) -> (String, Option<String>) {
    if cfg.groups.is_empty() {
        return (String::new(), None);
    }
    match cfg.groups.iter().find(|g| g.cameras.contains(camera)) {
        Some(g) => (format!("g:{}", g.name), Some(g.name.clone())),
        None => (format!("c:{}:{}", camera.device, camera.channel), None),
    }
}

/// Groups events into incidents, newest first.
pub fn group(events: &[CameraEvent], cfg: &GroupingConfig) -> Vec<Incident> {
    let mut by_group: BTreeMap<String, (Option<String>, Vec<CameraEvent>)> = BTreeMap::new();
    for e in events {
        let mut e = e.clone();
        e.end = e.end.max(e.start);
        let (key, name) = group_of(cfg, &e.camera);
        by_group.entry(key).or_insert((name, Vec::new())).1.push(e);
    }
    let mut out = Vec::new();
    for (_, (name, mut evs)) in by_group {
        evs.sort();
        let mut current: Vec<CameraEvent> = Vec::new();
        let mut end = Secs::MIN;
        for e in evs {
            if !current.is_empty() && e.start > end.saturating_add(cfg.gap) {
                out.push(Incident::build(
                    name.clone(),
                    cfg.margin,
                    std::mem::take(&mut current),
                ));
                end = Secs::MIN;
            }
            end = end.max(e.end);
            current.push(e);
        }
        if !current.is_empty() {
            out.push(Incident::build(name.clone(), cfg.margin, current));
        }
    }
    out.sort_by(|a, b| b.start().cmp(&a.start()).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Merges two incidents into one (the user says they are one event). The
/// name of the first survives (else the second's).
pub fn merge(a: &Incident, b: &Incident) -> Incident {
    let mut events = a.events.clone();
    events.extend(b.events.iter().cloned());
    let group = if a.group == b.group {
        a.group.clone()
    } else {
        None
    };
    let mut m = Incident::build(group, a.margin.max(b.margin), events);
    m.extra_cameras = a
        .extra_cameras
        .union(&b.extra_cameras)
        .filter(|c| !m.events.iter().any(|e| &e.camera == *c))
        .cloned()
        .collect();
    m.name = a.name.clone().or_else(|| b.name.clone());
    m.id = m.compute_id();
    m
}

/// Splits an incident at `at`: events that start before it go to the
/// first part, the rest to the second. `None` if either part would be
/// empty. Added cameras stay with both parts; the name stays with the first.
pub fn split(inc: &Incident, at: Secs) -> Option<(Incident, Incident)> {
    let (before, after): (Vec<CameraEvent>, Vec<CameraEvent>) =
        inc.events.iter().cloned().partition(|e| e.start < at);
    if before.is_empty() || after.is_empty() {
        return None;
    }
    let mut a = Incident::build(inc.group.clone(), inc.margin, before);
    let mut b = Incident::build(inc.group.clone(), inc.margin, after);
    for c in &inc.extra_cameras {
        a.add_camera(c.clone());
        b.add_camera(c.clone());
    }
    a.name = inc.name.clone();
    a.id = a.compute_id();
    b.id = b.compute_id();
    Some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(dev: &str, ch: u8, kind: &str, start: Secs, end: Secs) -> CameraEvent {
        CameraEvent {
            start,
            end,
            camera: CameraId::new(dev, ch),
            kind: kind.into(),
        }
    }

    fn sample() -> Vec<CameraEvent> {
        vec![
            // One vehicle passing three cameras of two devices.
            ev("nvr-a", 0, "vehicle", 1000, 1015),
            ev("nvr-a", 1, "vehicle", 1012, 1030),
            ev("nvr-b", 0, "vehicle", 1038, 1050),
            ev("nvr-a", 0, "md", 1003, 1008),
            // Much later: a person on one camera.
            ev("nvr-a", 1, "people", 5000, 5020),
            // Another camera, far away, within the gap of the first.
            ev("nvr-b", 3, "md", 1020, 1025),
        ]
    }

    #[test]
    fn groups_overlapping_and_nearby_events_newest_first() {
        let incidents = group(&sample(), &GroupingConfig::default());
        assert_eq!(incidents.len(), 2);
        assert_eq!(incidents[0].start(), 5000);
        let first = &incidents[1];
        assert_eq!(first.cameras().len(), 4);
        assert_eq!((first.start(), first.end()), (1000, 1050));
        assert_eq!(
            first.kinds().into_iter().collect::<Vec<_>>(),
            vec!["md", "vehicle"]
        );
        let ranges = first.download_ranges();
        assert_eq!(ranges[&CameraId::new("nvr-a", 0)], (995, 1020));
        assert_eq!(ranges[&CameraId::new("nvr-b", 0)], (1033, 1055));
        assert_eq!(
            first.intervals()[&CameraId::new("nvr-a", 0)],
            vec![(1000, 1015)]
        );
    }

    #[test]
    fn grouping_is_deterministic_regardless_of_input_order() {
        let cfg = GroupingConfig::default();
        let a = group(&sample(), &cfg);
        let mut rev = sample();
        rev.reverse();
        assert_eq!(a, group(&rev, &cfg));
        // The IDs stay when unrelated events are added.
        let mut more = sample();
        more.push(ev("nvr-a", 0, "md", 9000, 9010));
        let b = group(&more, &cfg);
        assert_eq!(b[1].id, a[0].id);
        assert_eq!(b[2].id, a[1].id);
    }

    #[test]
    fn camera_groups_keep_areas_apart() {
        let cfg = GroupingConfig {
            groups: vec![CameraGroup {
                name: "front".into(),
                cameras: vec![
                    CameraId::new("nvr-a", 0),
                    CameraId::new("nvr-a", 1),
                    CameraId::new("nvr-b", 0),
                ],
            }],
            ..GroupingConfig::default()
        };
        let incidents = group(&sample(), &cfg);
        // front 1000-1050, the outside camera alone, and the later person.
        assert_eq!(incidents.len(), 3);
        let front = incidents
            .iter()
            .find(|i| i.group.as_deref() == Some("front") && i.start() == 1000)
            .unwrap();
        assert_eq!(front.cameras().len(), 3);
        assert!(incidents.iter().any(|i| i.group.is_none()
            && i.cameras().into_iter().collect::<Vec<_>>() == vec![CameraId::new("nvr-b", 3)]));
    }

    #[test]
    fn gap_is_respected() {
        let evs = vec![ev("d", 0, "md", 0, 10), ev("d", 1, "md", 21, 30)];
        let cfg = GroupingConfig::default();
        assert_eq!(group(&evs, &cfg).len(), 2);
        let cfg = GroupingConfig {
            gap: 11,
            ..GroupingConfig::default()
        };
        assert_eq!(group(&evs, &cfg).len(), 1);
    }

    #[test]
    fn merge_split_add_remove_rename() {
        let incidents = group(&sample(), &GroupingConfig::default());
        let (late, early) = (&incidents[0], &incidents[1]);
        let mut m = merge(early, late);
        assert_eq!((m.start(), m.end()), (1000, 5020));
        assert_ne!(m.id, early.id);
        m.name = Some("Delivery".into());

        let (a, b) = split(&m, 3000).unwrap();
        assert_eq!(a.events, early.events);
        assert_eq!(b.events, late.events);
        assert_eq!(a.name.as_deref(), Some("Delivery"));
        assert_eq!(b.id, late.id);
        assert!(split(&m, 0).is_none());
        assert!(split(&m, 9999).is_none());

        let mut l = late.clone();
        let cam = CameraId::new("nvr-b", 0);
        l.add_camera(cam.clone());
        assert_eq!(l.download_ranges()[&cam], (4995, 5025));
        assert!(l.remove_camera(&cam));
        assert!(!l.remove_camera(&CameraId::new("nvr-a", 1)));
        assert_eq!(l.cameras().len(), 1);
    }
}
