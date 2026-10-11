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

use crate::{DeviceCamera, Secs};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// A camera: one lens of a channel of a device.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CameraId {
    /// The app's identifier of the device.
    pub device: String,
    /// Channel on that device.
    pub channel: u8,
    /// Lens of a multi-lens camera: 0 for the first (and usually only)
    /// lens, 1 for the second. Missing in files written before lenses were
    /// tracked, which were all first lenses.
    #[serde(default)]
    pub lens: u8,
}

impl CameraId {
    /// The first lens of a channel.
    pub fn new(device: impl Into<String>, channel: u8) -> CameraId {
        CameraId::with_lens(device, channel, 0)
    }

    /// One lens of a channel.
    pub fn with_lens(device: impl Into<String>, channel: u8, lens: u8) -> CameraId {
        CameraId {
            device: device.into(),
            channel,
            lens,
        }
    }

    /// The camera as its device knows it.
    pub fn local(&self) -> DeviceCamera {
        DeviceCamera::new(self.channel, self.lens)
    }

    /// `device:channel`, plus `:lens` for a second lens only, so that
    /// identifiers made before lenses were tracked stay the same.
    pub(crate) fn tag(&self) -> String {
        if self.lens == 0 {
            format!("{}:{}", self.device, self.channel)
        } else {
            format!("{}:{}:{}", self.device, self.channel, self.lens)
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
    /// Content identifier, derived from the group, the cameras, and the
    /// first start and last end (an incident still growing gets a new one);
    /// it names the download folder. [`Incident::key`] stays the same while
    /// the incident grows.
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

    pub(crate) fn compute_id(&self) -> String {
        let cams: Vec<String> = self.cameras().iter().map(CameraId::tag).collect();
        let mut parts: Vec<&str> = vec![self.group.as_deref().unwrap_or("")];
        parts.extend(cams.iter().map(String::as_str));
        let (start, end) = (self.start().to_string(), self.end().to_string());
        parts.push(&start);
        parts.push(&end);
        format!("{}-{:016x}", self.start(), fnv(&parts))
    }

    /// An identifier that stays the same while the incident grows: from its
    /// group and its earliest event (start, then the lowest camera), so
    /// later events joining it do not change it, unlike [`Incident::id`].
    /// The user's edits refer to incidents by this key. Format:
    /// `i-<start>-<device>-<channel>-<lens>`, plus `-<hash>` of the group's
    /// name for an incident of a named group.
    pub fn key(&self) -> String {
        let first = self
            .events
            .iter()
            .map(|e| (e.start, &e.camera))
            .min()
            .map(|(s, c)| (s, c.clone()))
            .or_else(|| self.extra_cameras.first().map(|c| (0, c.clone())));
        match first {
            Some((start, c)) => make_key(start, &c, self.group.as_deref()),
            None => match &self.group {
                Some(g) => format!("i-0{}", group_suffix(g)),
                None => "i-0".to_owned(),
            },
        }
    }

    /// Whether more events may still join: its end is within `gap` seconds
    /// of the device's `now` (the grouping gap), or after it.
    pub fn is_growing(&self, now: Secs, gap: Secs) -> bool {
        self.end().saturating_add(gap) >= now
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

fn group_suffix(group: &str) -> String {
    format!("-{:08x}", fnv(&[group]) as u32)
}

fn make_key(start: Secs, c: &CameraId, group: Option<&str>) -> String {
    let mut key = format!("i-{start}-{}-{}-{}", c.device, c.channel, c.lens);
    if let Some(g) = group {
        key.push_str(&group_suffix(g));
    }
    key
}

/// What an incident key says about the incident's earliest event: its
/// start and camera (the key's anchor, the same whatever the group). `None`
/// for a key [`Incident::key`] could not have made.
pub fn key_anchor(key: &str) -> Option<(Secs, CameraId)> {
    let rest = key.strip_prefix("i-")?;
    let digits = rest
        .char_indices()
        .find(|(i, c)| !(c.is_ascii_digit() || (*i == 0 && *c == '-')))
        .map(|(i, _)| i)?;
    let start: Secs = rest[..digits].parse().ok()?;
    let rest = rest[digits..].strip_prefix('-')?;
    let camera = |s: &str| -> Option<CameraId> {
        let mut it = s.rsplitn(3, '-');
        let lens: u8 = it.next()?.parse().ok()?;
        let channel: u8 = it.next()?.parse().ok()?;
        let device = it.next().filter(|d| !d.is_empty())?;
        Some(CameraId::with_lens(device, channel, lens))
    };
    // A group's hash is 8 hex digits, which a lens (at most 3) never is.
    if let Some((head, tail)) = rest.rsplit_once('-')
        && tail.len() == 8
        && tail.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return camera(head).map(|c| (start, c));
    }
    camera(rest).map(|c| (start, c))
}

/// The key the same incident has under the groups of `new`, for a key made
/// under the groups of `old`: renaming a group, deleting one, or moving a
/// camera between groups changes the keys (docs/FFI-RECORDINGS.md,
/// "Locked incidents"). `None` when the key stays the same, or when it was
/// not made under `old` (a merge across groups, for example).
pub fn rekey(key: &str, old: &GroupingConfig, new: &GroupingConfig) -> Option<String> {
    let (start, cam) = key_anchor(key)?;
    if make_key(start, &cam, group_of(old, &cam).1.as_deref()) != key {
        return None;
    }
    let k = make_key(start, &cam, group_of(new, &cam).1.as_deref());
    (k != key).then_some(k)
}

/// Which group a camera is in: the first configured group that lists it,
/// a group of its own otherwise; one shared group when none are set.
fn group_of(cfg: &GroupingConfig, camera: &CameraId) -> (String, Option<String>) {
    if cfg.groups.is_empty() {
        return (String::new(), None);
    }
    match cfg.groups.iter().find(|g| g.cameras.contains(camera)) {
        Some(g) => (format!("g:{}", g.name), Some(g.name.clone())),
        None => (format!("c:{}", camera.tag()), None),
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
    fn lenses_are_cameras_of_their_own_and_old_files_still_load() {
        // Written before lenses were tracked.
        let old: CameraId = serde_json::from_str(r#"{"device":"nvr-a","channel":3}"#).unwrap();
        assert_eq!(old, CameraId::new("nvr-a", 3));
        assert_eq!(old.local(), DeviceCamera::first(3));
        // A first lens keeps the identifier format of before (job folders
        // and edits made then still match), a second lens differs.
        assert_eq!(old.tag(), "nvr-a:3");
        let tele = CameraId::with_lens("nvr-a", 3, 1);
        assert_eq!(tele.tag(), "nvr-a:3:1");
        let evs = vec![
            ev("nvr-a", 3, "md", 100, 110),
            CameraEvent {
                camera: tele.clone(),
                ..ev("nvr-a", 3, "people", 105, 115)
            },
        ];
        let incs = group(&evs, &GroupingConfig::default());
        assert_eq!(incs.len(), 1);
        assert_eq!(incs[0].cameras().len(), 2);
        let wide_only = group(&evs[..1], &GroupingConfig::default());
        assert_ne!(incs[0].id, wide_only[0].id);
    }

    #[test]
    fn key_stays_while_the_incident_grows() {
        let cfg = GroupingConfig::default();
        let mut evs = vec![ev("nvr-a", 1, "vehicle", 1000, 1015)];
        let early = group(&evs, &cfg).remove(0);
        assert_eq!(early.key(), "i-1000-nvr-a-1-0");
        assert!(early.is_growing(1020, cfg.gap));
        assert!(!early.is_growing(1026, cfg.gap));

        // More events join, on later and on lower cameras: the ID changes,
        // the key does not.
        evs.push(ev("nvr-a", 0, "vehicle", 1010, 1030));
        evs.push(ev("nvr-b", 2, "md", 1035, 1040));
        let grown = group(&evs, &cfg).remove(0);
        assert_ne!(grown.id, early.id);
        assert_eq!(grown.key(), early.key());

        // A tie on the start goes to the lowest camera, whatever the ends.
        evs.push(ev("nvr-a", 0, "md", 1000, 1001));
        assert_eq!(group(&evs, &cfg)[0].key(), "i-1000-nvr-a-0-0");

        // A named group adds a hash of its name.
        let cfg = GroupingConfig {
            groups: vec![CameraGroup {
                name: "front".into(),
                cameras: vec![CameraId::new("nvr-a", 1)],
            }],
            ..GroupingConfig::default()
        };
        let k = group(&evs[..1], &cfg)[0].key();
        assert!(k.starts_with("i-1000-nvr-a-1-0-") && k.len() == "i-1000-nvr-a-1-0-".len() + 8);
    }

    #[test]
    fn keys_are_followed_across_group_changes() {
        let evs = vec![ev("d-1-x", 1, "vehicle", 1000, 1015)];
        let none = GroupingConfig::default();
        let named = |name: &str| GroupingConfig {
            groups: vec![CameraGroup {
                name: name.into(),
                cameras: vec![CameraId::new("d-1-x", 1)],
            }],
            ..GroupingConfig::default()
        };
        let plain = group(&evs, &none)[0].key();
        let front = group(&evs, &named("front"))[0].key();
        let back = group(&evs, &named("back"))[0].key();
        // The anchor is the same whatever the group, and devices may hold
        // dashes.
        for k in [&plain, &front, &back] {
            assert_eq!(key_anchor(k), Some((1000, CameraId::new("d-1-x", 1))));
        }
        assert_eq!(
            key_anchor("i--5-d-0-1"),
            Some((-5, CameraId::with_lens("d", 0, 1)))
        );
        assert_eq!(key_anchor("i-0"), None);
        assert_eq!(key_anchor("x-1-d-0-0"), None);
        // Renaming, adding, and deleting the group.
        assert_eq!(
            rekey(&front, &named("front"), &named("back")),
            Some(back.clone())
        );
        assert_eq!(rekey(&plain, &none, &named("front")), Some(front.clone()));
        assert_eq!(rekey(&back, &named("back"), &none), Some(plain.clone()));
        assert_eq!(rekey(&front, &named("front"), &named("front")), None);
        // A key not made under the old groups is left alone.
        assert_eq!(rekey(&plain, &named("front"), &named("back")), None);
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
