//! The local index of one device: days with recordings, recording files,
//! and events, per camera (channel and lens).
//!
//! **Store.** One small JSON file per day (`days/YYYY-MM-DD.json`, every
//! camera's files and events of that day) plus `calendar.json` (the days
//! with recordings per camera, from command 142), all under the device's
//! directory, each replaced atomically (write, flush, rename). Why not an
//! embedded database: the data arrives and is refreshed a whole day of one
//! camera at a time (a search covers exactly one day), so a day file is the
//! natural unit of update and of expiry; the whole index of a busy device is
//! a few megabytes and is held in memory for queries; and plain files need no
//! C library (SQLite would add a C build to every target, including the
//! Windows cross-check) and survive crashes by construction. The only
//! dependency is `serde_json` (MIT OR Apache-2.0).
//!
//! **Cameras.** Entries are keyed by [`DeviceCamera`], so both lenses of a
//! multi-lens camera are kept side by side. Files of format 1 (keyed by
//! channel only) are dropped when the index opens: the index is a cache of
//! the device's own, and the days are searched again when a view needs them.
//!
//! **Bounds.** Days older than [`IndexConfig::max_age_days`] are dropped,
//! and the oldest days go first whenever the files exceed
//! [`IndexConfig::max_bytes`]. Days a locked incident covers
//! ([`DeviceIndex::set_kept_days`]) are never dropped; they count toward
//! the byte limit, so every other day goes first.
//!
//! **Revision.** Every stored change raises [`DeviceIndex::revision`], so a
//! view can tell whether what it shows is current.

use crate::store::{read_json, write_json};
use crate::{DeviceCamera, Secs, date_time, day_start};
use bc_protocol::archive::{EventRecord, RecordFile};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

const FORMAT_VERSION: u32 = 2;

/// Limits of one device's index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexConfig {
    /// Days older than this many days before "today" are dropped.
    pub max_age_days: u32,
    /// Most bytes of day files kept; the oldest days are dropped first.
    pub max_bytes: u64,
}

impl Default for IndexConfig {
    fn default() -> Self {
        IndexConfig {
            max_age_days: 90,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

/// A recording file in the index.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedFile {
    /// Channel.
    pub channel: u8,
    /// Lens (0 first, 1 second).
    #[serde(default)]
    pub lens: u8,
    /// The device's `name` for the file (opaque).
    pub name: String,
    /// The device's `Id` for the file (opaque; events refer to it).
    pub id: String,
    /// Start (naive seconds).
    pub start: Secs,
    /// End (naive seconds).
    pub end: Secs,
    /// Size in bytes.
    pub size: u64,
    /// Stream name, when the device said.
    pub stream: Option<String>,
    /// Recording causes.
    pub record_types: Vec<String>,
}

impl IndexedFile {
    /// A file of `camera` from a search result.
    pub fn of(camera: DeviceCamera, f: &RecordFile) -> IndexedFile {
        IndexedFile {
            channel: camera.channel,
            lens: camera.lens,
            name: f.name.clone(),
            id: f.id.clone(),
            start: crate::secs(f.start),
            end: crate::secs(f.end),
            size: f.size,
            stream: f.stream.map(|s| s.name().to_owned()),
            record_types: f.record_types.clone(),
        }
    }

    /// The camera it belongs to.
    pub fn camera(&self) -> DeviceCamera {
        DeviceCamera::new(self.channel, self.lens)
    }
}

/// Where an event came from (docs/FFI-RECORDINGS.md, section 2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventSource {
    /// The camera or NVR reported it.
    #[default]
    Device,
    /// The computer's sound analysis found it (section 2a). Stored in
    /// [`CameraDay::local`], so a search of the device never replaces it.
    LocalAudio,
}

impl EventSource {
    fn is_device(&self) -> bool {
        *self == EventSource::Device
    }

    /// The name the C interface uses.
    pub fn as_str(self) -> &'static str {
        match self {
            EventSource::Device => "device",
            EventSource::LocalAudio => "local_audio",
        }
    }
}

/// An event in the index.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IndexedEvent {
    /// Channel.
    pub channel: u8,
    /// Lens (0 first, 1 second).
    #[serde(default)]
    pub lens: u8,
    /// Type as the device names it (`md`, `people`, `vehicle`, ...).
    pub kind: String,
    /// Start (naive seconds).
    pub start: Secs,
    /// End (naive seconds).
    pub end: Secs,
    /// The `Id` of the recording file that contains it (empty for events of
    /// the computer's own analysis).
    pub file_id: String,
    /// Where it came from.
    #[serde(default, skip_serializing_if = "EventSource::is_device")]
    pub source: EventSource,
    /// The analyzer that found it (local events only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analyzer: Option<String>,
    /// The classifier's own name for it (local events only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Confidence in thousandths (local events only; none: it matched or
    /// it did not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_pm: Option<u16>,
}

impl IndexedEvent {
    /// An event of `camera` from a search result.
    pub fn of(camera: DeviceCamera, e: &EventRecord) -> IndexedEvent {
        let (start, end) = (crate::secs(e.start), crate::secs(e.end));
        IndexedEvent {
            channel: camera.channel,
            lens: camera.lens,
            kind: e.kind.clone(),
            start,
            end: end.max(start),
            file_id: e.file_id.clone(),
            ..IndexedEvent::default()
        }
    }

    /// The camera it belongs to.
    pub fn camera(&self) -> DeviceCamera {
        DeviceCamera::new(self.channel, self.lens)
    }
}

/// One camera's files and events of one day.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CameraDay {
    /// When this was fetched (device-local naive seconds).
    pub fetched_at: Secs,
    /// Files, in device order.
    pub files: Vec<IndexedFile>,
    /// Events, sorted by start.
    pub events: Vec<IndexedEvent>,
    /// Events the computer found itself (sound labels), kept apart so a
    /// search of the device replaces `events` and leaves these. Sorted by
    /// start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub local: Vec<IndexedEvent>,
}

/// A map stored as a list of pairs (JSON object keys must be strings).
pub(crate) mod pairs {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::BTreeMap;

    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(
        m: &BTreeMap<K, V>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        s.collect_seq(m.iter())
    }

    pub fn deserialize<'de, K, V, D>(d: D) -> Result<BTreeMap<K, V>, D::Error>
    where
        K: Deserialize<'de> + Ord,
        V: Deserialize<'de>,
        D: Deserializer<'de>,
    {
        Ok(Vec::<(K, V)>::deserialize(d)?.into_iter().collect())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct DayFile {
    version: u32,
    day: Secs,
    #[serde(with = "pairs")]
    cameras: BTreeMap<DeviceCamera, CameraDay>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Calendar {
    version: u32,
    /// Camera to the start of every day with recordings.
    #[serde(with = "pairs")]
    days: BTreeMap<DeviceCamera, BTreeSet<Secs>>,
    /// Months (start of their first day) already asked with 142, per camera,
    /// and when.
    #[serde(with = "pairs")]
    months: BTreeMap<DeviceCamera, BTreeMap<Secs, Secs>>,
}

/// Only the version of a stored file, to tell old formats apart.
#[derive(Deserialize)]
struct VersionOnly {
    version: u32,
}

/// A query for events.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventQuery {
    /// Only these cameras (all when empty).
    pub cameras: Vec<DeviceCamera>,
    /// Only these types (all when empty).
    pub kinds: Vec<String>,
    /// Events that end at or after this time.
    pub start: Secs,
    /// Events that start at or before this time.
    pub end: Secs,
}

/// One device's index, held in memory and mirrored to files.
#[derive(Debug)]
pub struct DeviceIndex {
    dir: PathBuf,
    cfg: IndexConfig,
    days: BTreeMap<Secs, DayFile>,
    sizes: BTreeMap<Secs, u64>,
    calendar: Calendar,
    /// Days a lock keeps (never dropped by age or size).
    kept: BTreeSet<Secs>,
    revision: u64,
}

fn day_name(day: Secs) -> String {
    let t = date_time(day);
    format!("{:04}-{:02}-{:02}.json", t.year, t.month, t.day)
}

impl DeviceIndex {
    /// Opens (or creates) the index in `dir`. Unreadable day files, and
    /// files of an older format, are removed and refetched later; they are
    /// a cache.
    pub fn open(dir: &Path, cfg: IndexConfig) -> io::Result<DeviceIndex> {
        let days_dir = dir.join("days");
        fs::create_dir_all(&days_dir)?;
        let mut idx = DeviceIndex {
            dir: dir.to_owned(),
            cfg,
            days: BTreeMap::new(),
            sizes: BTreeMap::new(),
            calendar: Calendar::default(),
            kept: BTreeSet::new(),
            revision: 0,
        };
        let calendar_path = dir.join("calendar.json");
        match read_json::<VersionOnly>(&calendar_path) {
            Ok(Some(v)) if v.version == FORMAT_VERSION => {
                match read_json::<Calendar>(&calendar_path) {
                    Ok(Some(c)) => idx.calendar = c,
                    Ok(None) => {}
                    Err(e) => warn!(error = %e, "index calendar unreadable; starting empty"),
                }
            }
            Ok(Some(_)) => info!("index calendar of an older format; starting empty"),
            Ok(None) => {}
            Err(e) => warn!(error = %e, "index calendar unreadable; starting empty"),
        }
        let (mut old, mut bad) = (0usize, 0usize);
        for entry in fs::read_dir(&days_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let keep = match read_json::<VersionOnly>(&path) {
                Ok(Some(v)) if v.version == FORMAT_VERSION => match read_json::<DayFile>(&path) {
                    Ok(Some(d)) if path.ends_with(day_name(d.day)) => {
                        let size = fs::metadata(&path).map_or(0, |m| m.len());
                        idx.sizes.insert(d.day, size);
                        idx.days.insert(d.day, d);
                        true
                    }
                    _ => {
                        bad += 1;
                        false
                    }
                },
                Ok(Some(_)) => {
                    old += 1;
                    false
                }
                _ => {
                    bad += 1;
                    false
                }
            };
            if !keep {
                let _ = fs::remove_file(&path);
            }
        }
        if old > 0 {
            info!(days = old, "index days of an older format dropped");
        }
        if bad > 0 {
            warn!(days = bad, "unreadable index day files removed");
        }
        Ok(idx)
    }

    /// The limits in force.
    pub fn config(&self) -> IndexConfig {
        self.cfg
    }

    /// Raised by every stored change (starts at 0 when opened).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Records the days with recordings of `camera` within the month that
    /// starts at `month_start`, as fetched at `fetched_at`.
    pub fn set_month(
        &mut self,
        camera: DeviceCamera,
        month_start: Secs,
        month_end: Secs,
        days: impl IntoIterator<Item = Secs>,
        fetched_at: Secs,
    ) -> io::Result<()> {
        let set = self.calendar.days.entry(camera).or_default();
        set.retain(|d| *d < month_start || *d > month_end);
        set.extend(days.into_iter().map(day_start));
        self.calendar
            .months
            .entry(camera)
            .or_default()
            .insert(month_start, fetched_at);
        self.calendar.version = FORMAT_VERSION;
        self.revision += 1;
        write_json(&self.dir.join("calendar.json"), &self.calendar)
    }

    /// When the month starting at `month_start` was last asked for `camera`.
    pub fn month_fetched_at(&self, camera: DeviceCamera, month_start: Secs) -> Option<Secs> {
        self.calendar
            .months
            .get(&camera)?
            .get(&month_start)
            .copied()
    }

    /// Days with recordings of `camera` (starts of day), oldest first.
    pub fn recorded_days(&self, camera: DeviceCamera) -> Vec<Secs> {
        self.calendar
            .days
            .get(&camera)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Every camera with calendar entries.
    pub fn cameras(&self) -> Vec<DeviceCamera> {
        self.calendar.days.keys().copied().collect()
    }

    /// Every camera with calendar entries or an indexed day.
    pub fn indexed_cameras(&self) -> BTreeSet<DeviceCamera> {
        let mut s: BTreeSet<DeviceCamera> = self.calendar.days.keys().copied().collect();
        for d in self.days.values() {
            s.extend(d.cameras.keys().copied());
        }
        s
    }

    /// Stores one camera's files and events of the day that starts at
    /// `day`, replacing what was there, and writes the day file.
    pub fn put_day(
        &mut self,
        day: Secs,
        camera: DeviceCamera,
        files: Vec<IndexedFile>,
        mut events: Vec<IndexedEvent>,
        fetched_at: Secs,
    ) -> io::Result<()> {
        let day = day_start(day);
        events.sort_by(|a, b| (a.start, a.end, &a.kind).cmp(&(b.start, b.end, &b.kind)));
        events.dedup();
        let d = self.days.entry(day).or_insert_with(|| DayFile {
            version: FORMAT_VERSION,
            day,
            cameras: BTreeMap::new(),
        });
        let local = d
            .cameras
            .get(&camera)
            .map(|c| c.local.clone())
            .unwrap_or_default();
        d.cameras.insert(
            camera,
            CameraDay {
                fetched_at,
                files,
                events,
                local,
            },
        );
        let path = self.dir.join("days").join(day_name(day));
        self.revision += 1;
        write_json(&path, d)?;
        self.sizes
            .insert(day, fs::metadata(&path).map_or(0, |m| m.len()));
        Ok(())
    }

    /// One camera's entry for a day, if indexed.
    pub fn camera_day(&self, day: Secs, camera: DeviceCamera) -> Option<&CameraDay> {
        self.days.get(&day_start(day))?.cameras.get(&camera)
    }

    /// Indexed days (starts of day), oldest first.
    pub fn days(&self) -> Vec<Secs> {
        self.days.keys().copied().collect()
    }

    /// Events matching `q`, sorted by start (then camera).
    pub fn events(&self, q: &EventQuery) -> Vec<IndexedEvent> {
        let mut out: Vec<IndexedEvent> = Vec::new();
        // Events can run past midnight: look one day back.
        for (_, d) in self
            .days
            .range(day_start(q.start) - 86_400..=day_start(q.end))
        {
            for (cam, cd) in &d.cameras {
                if !q.cameras.is_empty() && !q.cameras.contains(cam) {
                    continue;
                }
                out.extend(
                    cd.events
                        .iter()
                        .chain(&cd.local)
                        .filter(|e| e.end >= q.start && e.start <= q.end)
                        .filter(|e| q.kinds.is_empty() || q.kinds.contains(&e.kind))
                        .cloned(),
                );
            }
        }
        out.sort_by_key(|e| (e.start, e.channel, e.lens, e.end));
        out.dedup();
        out
    }

    /// Stores sound labels of `camera` from `analyzer`: the analyzer's
    /// earlier labels that start within `start..=end` are replaced by
    /// `labels` (so analyzing a range again does not duplicate), and each
    /// day keeps at most `cap` labels of the camera in all (the lowest
    /// confidence goes first; a label without confidence counts as sure).
    /// Labels outside the range are ignored. Returns the starts of the days
    /// whose file changed.
    ///
    /// # Errors
    /// A day file could not be written.
    pub fn put_labels(
        &mut self,
        camera: DeviceCamera,
        analyzer: &str,
        start: Secs,
        end: Secs,
        labels: Vec<IndexedEvent>,
        cap: usize,
    ) -> io::Result<Vec<Secs>> {
        let mut by_day: BTreeMap<Secs, Vec<IndexedEvent>> = BTreeMap::new();
        for mut l in labels {
            if l.start < start || l.start > end {
                continue;
            }
            l.channel = camera.channel;
            l.lens = camera.lens;
            l.end = l.end.max(l.start);
            l.source = EventSource::LocalAudio;
            l.analyzer = Some(analyzer.to_owned());
            l.file_id = String::new();
            by_day.entry(day_start(l.start)).or_default().push(l);
        }
        let existing: Vec<Secs> = self
            .days
            .range(day_start(start)..=day_start(end))
            .filter(|(_, d)| {
                d.cameras.get(&camera).is_some_and(|c| {
                    c.local
                        .iter()
                        .any(|e| e.analyzer.as_deref() == Some(analyzer))
                })
            })
            .map(|(d, _)| *d)
            .collect();
        let touched: BTreeSet<Secs> = by_day.keys().copied().chain(existing).collect();
        let mut changed = Vec::new();
        for day in touched {
            let new = by_day.remove(&day).unwrap_or_default();
            let before = self
                .days
                .get(&day)
                .and_then(|d| d.cameras.get(&camera))
                .map(|c| c.local.clone())
                .unwrap_or_default();
            let mut local: Vec<IndexedEvent> = before
                .iter()
                .filter(|e| {
                    e.analyzer.as_deref() != Some(analyzer) || e.start < start || e.start > end
                })
                .cloned()
                .collect();
            local.extend(new);
            local.sort_by(|a, b| (a.start, a.end, &a.kind).cmp(&(b.start, b.end, &b.kind)));
            local.dedup();
            if local.len() > cap {
                // Lowest confidence first; the later ones among equals.
                let mut order: Vec<usize> = (0..local.len()).collect();
                order.sort_by_key(|&i| {
                    (
                        std::cmp::Reverse(local[i].confidence_pm.unwrap_or(1000)),
                        std::cmp::Reverse(local[i].start),
                    )
                });
                order.truncate(cap);
                order.sort_unstable();
                local = order.into_iter().map(|i| local[i].clone()).collect();
            }
            if local == before {
                continue;
            }
            let d = self.days.entry(day).or_insert_with(|| DayFile {
                version: FORMAT_VERSION,
                day,
                cameras: BTreeMap::new(),
            });
            d.cameras.entry(camera).or_default().local = local;
            let path = self.dir.join("days").join(day_name(day));
            self.revision += 1;
            write_json(&path, d)?;
            self.sizes
                .insert(day, fs::metadata(&path).map_or(0, |m| m.len()));
            changed.push(day);
        }
        Ok(changed)
    }

    /// Recording files of `camera` overlapping `start..=end`.
    pub fn files(&self, camera: DeviceCamera, start: Secs, end: Secs) -> Vec<IndexedFile> {
        let mut out: Vec<IndexedFile> = Vec::new();
        for (_, d) in self.days.range(day_start(start) - 86_400..=day_start(end)) {
            if let Some(cd) = d.cameras.get(&camera) {
                out.extend(
                    cd.files
                        .iter()
                        .filter(|f| f.end >= start && f.start <= end)
                        .cloned(),
                );
            }
        }
        out.sort_by(|a, b| (a.start, &a.id).cmp(&(b.start, &b.id)));
        out.dedup();
        out
    }

    /// Total bytes of the day files.
    pub fn bytes(&self) -> u64 {
        self.sizes.values().sum()
    }

    /// Sets the days locked incidents cover (starts of day): they are kept
    /// past the age limit and never dropped to make room, but still count
    /// toward the byte limit, so other days go first (docs/FFI-RECORDINGS.md,
    /// "Locked incidents").
    pub fn set_kept_days(&mut self, days: BTreeSet<Secs>) {
        self.kept = days.into_iter().map(day_start).collect();
    }

    /// The days kept for locks.
    pub fn kept_days(&self) -> &BTreeSet<Secs> {
        &self.kept
    }

    /// Applies the age and size limits relative to `now`, except to the
    /// kept days; returns how many days were dropped.
    pub fn prune(&mut self, now: Secs) -> io::Result<usize> {
        let oldest = day_start(now) - i64::from(self.cfg.max_age_days) * 86_400;
        let kept = &self.kept;
        let mut drop: Vec<Secs> = self
            .days
            .range(..oldest)
            .map(|(d, _)| *d)
            .filter(|d| !kept.contains(d))
            .collect();
        let mut total: u64 = self
            .sizes
            .iter()
            .filter(|(d, _)| **d >= oldest || kept.contains(d))
            .map(|(_, s)| s)
            .sum();
        for (d, s) in self.sizes.range(oldest..) {
            if total <= self.cfg.max_bytes {
                break;
            }
            // Never drop today's file to make room.
            if *d >= day_start(now) {
                break;
            }
            if kept.contains(d) {
                continue;
            }
            drop.push(*d);
            total -= s;
        }
        for d in &drop {
            self.days.remove(d);
            self.sizes.remove(d);
            match fs::remove_file(self.dir.join("days").join(day_name(*d))) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let mut changed = false;
        for set in self.calendar.days.values_mut() {
            let before = set.len();
            set.retain(|d| *d >= oldest || self.kept.contains(d));
            changed |= set.len() != before;
        }
        for months in self.calendar.months.values_mut() {
            let before = months.len();
            months.retain(|m, _| *m + 31 * 86_400 >= oldest);
            changed |= months.len() != before;
        }
        if changed || !drop.is_empty() {
            self.revision += 1;
        }
        if changed {
            write_json(&self.dir.join("calendar.json"), &self.calendar)?;
        }
        Ok(drop.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secs;
    use bc_protocol::archive::DateTime;

    fn day(d: u8) -> Secs {
        secs(DateTime::date(2026, 10, d).unwrap())
    }

    fn cam(ch: u8) -> DeviceCamera {
        DeviceCamera::first(ch)
    }

    fn ev(c: DeviceCamera, kind: &str, start: Secs, len: Secs) -> IndexedEvent {
        IndexedEvent {
            channel: c.channel,
            lens: c.lens,
            kind: kind.into(),
            start,
            end: start + len,
            file_id: format!("f{}", c.channel),
            ..IndexedEvent::default()
        }
    }

    #[test]
    fn stores_queries_reloads_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap();
        idx.set_month(cam(0), day(1), day(31), [day(8), day(9)], day(10))
            .unwrap();
        idx.put_day(
            day(9),
            cam(0),
            vec![],
            vec![
                ev(cam(0), "md", day(9) + 100, 10),
                ev(cam(0), "vehicle", day(9) + 50, 10),
            ],
            day(10),
        )
        .unwrap();
        idx.put_day(
            day(9),
            cam(1),
            vec![],
            vec![ev(cam(1), "people", day(9) + 70, 10)],
            day(10),
        )
        .unwrap();
        // Runs past midnight into the 10th.
        idx.put_day(
            day(8),
            cam(1),
            vec![],
            vec![ev(cam(1), "md", day(9) - 5, 20)],
            day(10),
        )
        .unwrap();
        assert_eq!(idx.revision(), 4);
        let all = EventQuery {
            start: day(9),
            end: day(9) + 86_399,
            ..EventQuery::default()
        };
        let got = idx.events(&all);
        assert_eq!(
            got.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            vec!["md", "vehicle", "people", "md"]
        );
        let q = EventQuery {
            kinds: vec!["vehicle".into(), "people".into()],
            cameras: vec![cam(1)],
            ..all.clone()
        };
        assert_eq!(idx.events(&q).len(), 1);

        drop(idx);
        let mut idx = DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap();
        assert_eq!(idx.events(&all).len(), 4);
        assert_eq!(idx.recorded_days(cam(0)), vec![day(8), day(9)]);
        assert_eq!(idx.month_fetched_at(cam(0), day(1)), Some(day(10)));
        assert!(idx.bytes() > 0);

        // Age: with a 1-day limit at the 10th, the 8th goes, the 9th stays.
        idx.cfg.max_age_days = 1;
        assert_eq!(idx.prune(day(10) + 5).unwrap(), 1);
        assert_eq!(idx.days(), vec![day(9)]);
        assert_eq!(idx.recorded_days(cam(0)), vec![day(9)]);
        // Size: everything older than today goes when over the limit.
        idx.cfg = IndexConfig {
            max_age_days: 90,
            max_bytes: 10,
        };
        assert_eq!(idx.prune(day(10) + 5).unwrap(), 1);
        assert!(idx.days().is_empty());
        assert!(!dir.path().join("days").join(day_name(day(9))).exists());
    }

    fn label(kind: &str, start: Secs, len: Secs, conf: Option<u16>) -> IndexedEvent {
        IndexedEvent {
            kind: kind.into(),
            start,
            end: start + len,
            label: Some(format!("{kind}-raw")),
            confidence_pm: conf,
            ..IndexedEvent::default()
        }
    }

    #[test]
    fn sound_labels_replace_by_analyzer_survive_searches_and_are_capped() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap();
        let c = cam(0);
        idx.put_day(
            day(9),
            c,
            vec![],
            vec![ev(c, "md", day(9) + 100, 10)],
            day(10),
        )
        .unwrap();
        let q = EventQuery {
            start: day(9),
            end: day(9) + 86_399,
            ..EventQuery::default()
        };
        let put = |idx: &mut DeviceIndex, a: &str, s: Secs, e: Secs, l: Vec<IndexedEvent>, cap| {
            idx.put_labels(c, a, s, e, l, cap).unwrap()
        };
        let range = (day(9), day(9) + 86_399);
        let r = put(
            &mut idx,
            "app-v1",
            range.0,
            range.1,
            vec![
                label("dog_bark", day(9) + 500, 6, Some(910)),
                label("siren", day(9) + 900, 20, Some(500)),
                // Outside the range: ignored.
                label("siren", day(10) + 5, 2, Some(500)),
            ],
            2000,
        );
        assert_eq!(r, vec![day(9)]);
        let got = idx.events(&q);
        assert_eq!(
            got.iter()
                .map(|e| (e.kind.as_str(), e.source))
                .collect::<Vec<_>>(),
            [
                ("md", EventSource::Device),
                ("dog_bark", EventSource::LocalAudio),
                ("siren", EventSource::LocalAudio)
            ]
        );
        assert_eq!(got[1].analyzer.as_deref(), Some("app-v1"));
        assert_eq!(got[1].label.as_deref(), Some("dog_bark-raw"));
        assert_eq!(got[1].camera(), c);

        // Another analyzer's labels are kept; the same analyzer's are
        // replaced within the range only; the same labels change nothing.
        let rev = idx.revision();
        put(
            &mut idx,
            "pattern-v1",
            range.0,
            range.1,
            vec![label("smoke_alarm", day(9) + 700, 30, None)],
            2000,
        );
        assert_eq!(idx.events(&q).len(), 4);
        let again = put(
            &mut idx,
            "pattern-v1",
            range.0,
            range.1,
            vec![label("smoke_alarm", day(9) + 700, 30, None)],
            2000,
        );
        assert!(again.is_empty());
        assert_eq!(idx.revision(), rev + 1);
        put(
            &mut idx,
            "app-v1",
            day(9) + 800,
            day(9) + 1000,
            vec![label("glass_break", day(9) + 950, 1, Some(800))],
            2000,
        );
        let kinds: Vec<_> = idx.events(&q).iter().map(|e| e.kind.clone()).collect();
        assert_eq!(kinds, ["md", "dog_bark", "smoke_alarm", "glass_break"]);

        // A device search of the day leaves the labels alone.
        idx.put_day(
            day(9),
            c,
            vec![],
            vec![ev(c, "people", day(9) + 50, 5)],
            day(11),
        )
        .unwrap();
        let kinds: Vec<_> = idx.events(&q).iter().map(|e| e.kind.clone()).collect();
        assert_eq!(kinds, ["people", "dog_bark", "smoke_alarm", "glass_break"]);
        // Kinds filter across sources.
        let only = EventQuery {
            kinds: vec!["smoke_alarm".into()],
            ..q.clone()
        };
        assert_eq!(idx.events(&only).len(), 1);

        // They survive a reload, as local events, without a device source
        // in the file of device events.
        drop(idx);
        let mut idx = DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap();
        let got = idx.events(&q);
        assert_eq!(got.len(), 4);
        assert_eq!(got[2].confidence_pm, None);
        assert_eq!(got[1].confidence_pm, Some(910));
        assert_eq!(got[0].source, EventSource::Device);

        // The cap keeps the most confident (labels without a confidence
        // count as sure), per camera and day.
        let r = put(
            &mut idx,
            "app-v1",
            range.0,
            range.1,
            vec![
                label("a", day(9) + 10, 1, Some(100)),
                label("b", day(9) + 20, 1, Some(900)),
                label("c", day(9) + 30, 1, Some(500)),
                label("d", day(9) + 40, 1, Some(900)),
            ],
            3,
        );
        assert_eq!(r, vec![day(9)]);
        let kinds: Vec<_> = idx
            .events(&q)
            .iter()
            .filter(|e| e.source == EventSource::LocalAudio)
            .map(|e| e.kind.clone())
            .collect();
        // Cap 3 across the day: the sure smoke alarm, then the two 0.9s.
        assert_eq!(kinds, ["b", "d", "smoke_alarm"]);

        // Labels of days that no search covered are days of the index too
        // (the lane still treats them as not fetched), and age out with it.
        let other = DeviceCamera::first(5);
        idx.put_labels(
            other,
            "app-v1",
            day(3),
            day(3) + 86_399,
            vec![label("knock", day(3) + 5, 1, None)],
            10,
        )
        .unwrap();
        assert_eq!(idx.camera_day(day(3), other).unwrap().fetched_at, 0);
        idx.cfg.max_age_days = 2;
        idx.prune(day(10)).unwrap();
        assert!(idx.camera_day(day(3), other).is_none());
    }

    #[test]
    fn kept_days_outlive_the_age_limit_and_go_last_by_size() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap();
        idx.set_month(cam(0), day(1), day(31), [3, 4, 5, 6, 7].map(day), day(10))
            .unwrap();
        for d in [3, 4, 5, 6, 7] {
            idx.put_day(
                day(d),
                cam(0),
                vec![],
                vec![ev(cam(0), "md", day(d) + 10, 5)],
                day(10),
            )
            .unwrap();
        }
        idx.set_kept_days(BTreeSet::from([day(3) + 7, day(6)]));
        assert_eq!(idx.kept_days(), &BTreeSet::from([day(3), day(6)]));
        // Age: the 3rd is older than the limit but kept; the 4th goes.
        idx.cfg.max_age_days = 5;
        assert_eq!(idx.prune(day(10)).unwrap(), 1);
        assert_eq!(idx.days(), vec![day(3), day(5), day(6), day(7)]);
        assert!(idx.recorded_days(cam(0)).contains(&day(3)));
        // Size: kept days count, so every other day goes first, oldest
        // first; kept days stay even when the limit cannot be met.
        let one = idx.sizes[&day(5)];
        idx.cfg.max_bytes = idx.bytes() - one;
        assert_eq!(idx.prune(day(10)).unwrap(), 1);
        assert_eq!(idx.days(), vec![day(3), day(6), day(7)]);
        idx.cfg.max_bytes = 1;
        assert_eq!(idx.prune(day(10)).unwrap(), 1);
        assert_eq!(idx.days(), vec![day(3), day(6)]);
        // Unkept, they go like any other.
        idx.set_kept_days(BTreeSet::new());
        assert_eq!(idx.prune(day(10)).unwrap(), 2);
        assert!(idx.days().is_empty());
    }

    #[test]
    fn both_lenses_of_a_channel_are_kept_apart() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap();
        let (wide, tele) = (DeviceCamera::new(2, 0), DeviceCamera::new(2, 1));
        idx.set_month(wide, day(1), day(31), [day(9)], day(10))
            .unwrap();
        idx.set_month(tele, day(1), day(31), [day(9)], day(10))
            .unwrap();
        idx.put_day(
            day(9),
            wide,
            vec![],
            vec![ev(wide, "md", day(9) + 10, 5)],
            day(10),
        )
        .unwrap();
        idx.put_day(
            day(9),
            tele,
            vec![],
            vec![ev(tele, "people", day(9) + 12, 5)],
            day(10),
        )
        .unwrap();
        drop(idx);
        let idx = DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap();
        assert_eq!(idx.cameras(), vec![wide, tele]);
        let q = |c: DeviceCamera| EventQuery {
            cameras: vec![c],
            start: day(9),
            end: day(10),
            ..EventQuery::default()
        };
        assert_eq!(idx.events(&q(wide))[0].kind, "md");
        assert_eq!(idx.events(&q(tele))[0].kind, "people");
        assert_eq!(idx.events(&q(tele))[0].camera(), tele);
    }

    #[test]
    fn corrupt_and_old_format_files_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let days = dir.path().join("days");
        fs::create_dir_all(&days).unwrap();
        fs::write(days.join("2026-10-09.json"), b"{oops").unwrap();
        // Format 1 was keyed by channel only.
        fs::write(
            days.join("2026-10-08.json"),
            br#"{"version":1,"day":1791417600,"channels":{"0":{"fetched_at":0,"files":[],"events":[]}}}"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("calendar.json"),
            br#"{"version":1,"days":{"0":[1791417600]},"months":{}}"#,
        )
        .unwrap();
        let idx = DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap();
        assert!(idx.days().is_empty());
        assert!(idx.cameras().is_empty());
        assert!(!days.join("2026-10-09.json").exists());
        assert!(!days.join("2026-10-08.json").exists());
    }
}
