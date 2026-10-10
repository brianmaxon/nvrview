//! The local index of one device: days with recordings, recording files,
//! and events, per channel.
//!
//! **Store.** One small JSON file per day (`days/YYYY-MM-DD.json`, every
//! channel's files and events of that day) plus `calendar.json` (the days
//! with recordings per channel, from command 142), all under the device's
//! directory, each replaced atomically (write, flush, rename). Why not an
//! embedded database: the data arrives and is refreshed a whole day of one
//! channel at a time (a search covers exactly one day), so a day file is the
//! natural unit of update and of expiry; the whole index of a busy device is
//! a few megabytes and is held in memory for queries; and plain files need no
//! C library (SQLite would add a C build to every target, including the
//! Windows cross-check) and survive crashes by construction. The only
//! dependency is `serde_json` (MIT OR Apache-2.0).
//!
//! **Bounds.** Days older than [`IndexConfig::max_age_days`] are dropped,
//! and the oldest days go first whenever the files exceed
//! [`IndexConfig::max_bytes`].

use crate::store::{read_json, write_json};
use crate::{Secs, date_time, day_start};
use bc_protocol::archive::{EventRecord, RecordFile};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tracing::warn;

const FORMAT_VERSION: u32 = 1;

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

impl From<&RecordFile> for IndexedFile {
    fn from(f: &RecordFile) -> Self {
        IndexedFile {
            channel: f.channel,
            name: f.name.clone(),
            id: f.id.clone(),
            start: crate::secs(f.start),
            end: crate::secs(f.end),
            size: f.size,
            stream: f.stream.map(|s| s.name().to_owned()),
            record_types: f.record_types.clone(),
        }
    }
}

/// An event in the index.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IndexedEvent {
    /// Channel.
    pub channel: u8,
    /// Type as the device names it (`md`, `people`, `vehicle`, ...).
    pub kind: String,
    /// Start (naive seconds).
    pub start: Secs,
    /// End (naive seconds).
    pub end: Secs,
    /// The `Id` of the recording file that contains it.
    pub file_id: String,
}

impl From<&EventRecord> for IndexedEvent {
    fn from(e: &EventRecord) -> Self {
        let (start, end) = (crate::secs(e.start), crate::secs(e.end));
        IndexedEvent {
            channel: e.channel,
            kind: e.kind.clone(),
            start,
            end: end.max(start),
            file_id: e.file_id.clone(),
        }
    }
}

/// One channel's files and events of one day.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelDay {
    /// When this was fetched (device-local naive seconds).
    pub fetched_at: Secs,
    /// Files, in device order.
    pub files: Vec<IndexedFile>,
    /// Events, sorted by start.
    pub events: Vec<IndexedEvent>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct DayFile {
    version: u32,
    day: Secs,
    channels: BTreeMap<u8, ChannelDay>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Calendar {
    version: u32,
    /// Channel to the start of every day with recordings.
    days: BTreeMap<u8, BTreeSet<Secs>>,
    /// Months (start of their first day) already asked with 142, per channel,
    /// and when.
    months: BTreeMap<u8, BTreeMap<Secs, Secs>>,
}

/// A query for events.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventQuery {
    /// Only these channels (all when empty).
    pub channels: Vec<u8>,
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
}

fn day_name(day: Secs) -> String {
    let t = date_time(day);
    format!("{:04}-{:02}-{:02}.json", t.year, t.month, t.day)
}

impl DeviceIndex {
    /// Opens (or creates) the index in `dir`. Unreadable day files are
    /// removed and refetched later; they are a cache.
    pub fn open(dir: &Path, cfg: IndexConfig) -> io::Result<DeviceIndex> {
        let days_dir = dir.join("days");
        fs::create_dir_all(&days_dir)?;
        let mut idx = DeviceIndex {
            dir: dir.to_owned(),
            cfg,
            days: BTreeMap::new(),
            sizes: BTreeMap::new(),
            calendar: Calendar::default(),
        };
        match read_json::<Calendar>(&dir.join("calendar.json")) {
            Ok(Some(c)) if c.version == FORMAT_VERSION => idx.calendar = c,
            Ok(_) => {}
            Err(e) => warn!(error = %e, "index calendar unreadable; starting empty"),
        }
        for entry in fs::read_dir(&days_dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match read_json::<DayFile>(&path) {
                Ok(Some(d)) if d.version == FORMAT_VERSION && path.ends_with(day_name(d.day)) => {
                    let size = fs::metadata(&path).map_or(0, |m| m.len());
                    idx.sizes.insert(d.day, size);
                    idx.days.insert(d.day, d);
                }
                _ => {
                    warn!("unreadable index day file removed");
                    let _ = fs::remove_file(&path);
                }
            }
        }
        Ok(idx)
    }

    /// The limits in force.
    pub fn config(&self) -> IndexConfig {
        self.cfg
    }

    /// Records the days with recordings of `channel` within the month that
    /// starts at `month_start`, as fetched at `fetched_at`.
    pub fn set_month(
        &mut self,
        channel: u8,
        month_start: Secs,
        month_end: Secs,
        days: impl IntoIterator<Item = Secs>,
        fetched_at: Secs,
    ) -> io::Result<()> {
        let set = self.calendar.days.entry(channel).or_default();
        set.retain(|d| *d < month_start || *d > month_end);
        set.extend(days.into_iter().map(day_start));
        self.calendar
            .months
            .entry(channel)
            .or_default()
            .insert(month_start, fetched_at);
        self.calendar.version = FORMAT_VERSION;
        write_json(&self.dir.join("calendar.json"), &self.calendar)
    }

    /// When the month starting at `month_start` was last asked for `channel`.
    pub fn month_fetched_at(&self, channel: u8, month_start: Secs) -> Option<Secs> {
        self.calendar
            .months
            .get(&channel)?
            .get(&month_start)
            .copied()
    }

    /// Days with recordings of `channel` (starts of day), oldest first.
    pub fn recorded_days(&self, channel: u8) -> Vec<Secs> {
        self.calendar
            .days
            .get(&channel)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Stores one channel's files and events of the day that starts at
    /// `day`, replacing what was there, and writes the day file.
    pub fn put_day(
        &mut self,
        day: Secs,
        channel: u8,
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
            channels: BTreeMap::new(),
        });
        d.channels.insert(
            channel,
            ChannelDay {
                fetched_at,
                files,
                events,
            },
        );
        let path = self.dir.join("days").join(day_name(day));
        write_json(&path, d)?;
        self.sizes
            .insert(day, fs::metadata(&path).map_or(0, |m| m.len()));
        Ok(())
    }

    /// One channel's entry for a day, if indexed.
    pub fn channel_day(&self, day: Secs, channel: u8) -> Option<&ChannelDay> {
        self.days.get(&day_start(day))?.channels.get(&channel)
    }

    /// Indexed days (starts of day), oldest first.
    pub fn days(&self) -> Vec<Secs> {
        self.days.keys().copied().collect()
    }

    /// Events matching `q`, sorted by start (then channel).
    pub fn events(&self, q: &EventQuery) -> Vec<IndexedEvent> {
        let mut out: Vec<IndexedEvent> = Vec::new();
        // Events can run past midnight: look one day back.
        for (_, d) in self
            .days
            .range(day_start(q.start) - 86_400..=day_start(q.end))
        {
            for (ch, cd) in &d.channels {
                if !q.channels.is_empty() && !q.channels.contains(ch) {
                    continue;
                }
                out.extend(
                    cd.events
                        .iter()
                        .filter(|e| e.end >= q.start && e.start <= q.end)
                        .filter(|e| q.kinds.is_empty() || q.kinds.contains(&e.kind))
                        .cloned(),
                );
            }
        }
        out.sort_by_key(|e| (e.start, e.channel, e.end));
        out.dedup();
        out
    }

    /// Recording files of `channel` overlapping `start..=end`.
    pub fn files(&self, channel: u8, start: Secs, end: Secs) -> Vec<IndexedFile> {
        let mut out: Vec<IndexedFile> = Vec::new();
        for (_, d) in self.days.range(day_start(start) - 86_400..=day_start(end)) {
            if let Some(cd) = d.channels.get(&channel) {
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

    /// Applies the age and size limits relative to `now`; returns how many
    /// days were dropped.
    pub fn prune(&mut self, now: Secs) -> io::Result<usize> {
        let oldest = day_start(now) - i64::from(self.cfg.max_age_days) * 86_400;
        let mut drop: Vec<Secs> = self.days.range(..oldest).map(|(d, _)| *d).collect();
        let mut total: u64 = self
            .sizes
            .iter()
            .filter(|(d, _)| **d >= oldest)
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
            set.retain(|d| *d >= oldest);
            changed |= set.len() != before;
        }
        for months in self.calendar.months.values_mut() {
            let before = months.len();
            months.retain(|m, _| *m + 31 * 86_400 >= oldest);
            changed |= months.len() != before;
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

    fn ev(ch: u8, kind: &str, start: Secs, len: Secs) -> IndexedEvent {
        IndexedEvent {
            channel: ch,
            kind: kind.into(),
            start,
            end: start + len,
            file_id: format!("f{ch}"),
        }
    }

    #[test]
    fn stores_queries_reloads_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let mut idx = DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap();
        idx.set_month(0, day(1), day(31), [day(8), day(9)], day(10))
            .unwrap();
        idx.put_day(
            day(9),
            0,
            vec![],
            vec![
                ev(0, "md", day(9) + 100, 10),
                ev(0, "vehicle", day(9) + 50, 10),
            ],
            day(10),
        )
        .unwrap();
        idx.put_day(
            day(9),
            1,
            vec![],
            vec![ev(1, "people", day(9) + 70, 10)],
            day(10),
        )
        .unwrap();
        // Runs past midnight into the 10th.
        idx.put_day(
            day(8),
            1,
            vec![],
            vec![ev(1, "md", day(9) - 5, 20)],
            day(10),
        )
        .unwrap();
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
            channels: vec![1],
            ..all.clone()
        };
        assert_eq!(idx.events(&q).len(), 1);

        drop(idx);
        let mut idx = DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap();
        assert_eq!(idx.events(&all).len(), 4);
        assert_eq!(idx.recorded_days(0), vec![day(8), day(9)]);
        assert_eq!(idx.month_fetched_at(0, day(1)), Some(day(10)));
        assert!(idx.bytes() > 0);

        // Age: with a 1-day limit at the 10th, the 8th goes, the 9th stays.
        idx.cfg.max_age_days = 1;
        assert_eq!(idx.prune(day(10) + 5).unwrap(), 1);
        assert_eq!(idx.days(), vec![day(9)]);
        assert_eq!(idx.recorded_days(0), vec![day(9)]);
        // Size: everything older than today goes when over the limit.
        idx.cfg = IndexConfig {
            max_age_days: 90,
            max_bytes: 10,
        };
        assert_eq!(idx.prune(day(10) + 5).unwrap(), 1);
        assert!(idx.days().is_empty());
        assert!(!dir.path().join("days").join(day_name(day(9))).exists());
    }

    #[test]
    fn corrupt_day_files_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("days")).unwrap();
        fs::write(dir.path().join("days").join("2026-10-09.json"), b"{oops").unwrap();
        let idx = DeviceIndex::open(dir.path(), IndexConfig::default()).unwrap();
        assert!(idx.days().is_empty());
        assert!(!dir.path().join("days").join("2026-10-09.json").exists());
    }
}
