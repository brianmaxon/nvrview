//! The loudness envelope store of one device (docs/FFI-RECORDINGS.md,
//! section 2a): peak and RMS per second, per camera and day, in the one-byte
//! form of [`sound_events::Level`] (0: no audio analyzed; 1 to 128: -127 to
//! 0 dBFS).
//!
//! **Time.** Seconds are device-local naive seconds like the rest of the
//! index; the caller converts the Unix seconds the live meter produces with
//! the device's offset from UTC.
//!
//! **Storage.** One file per camera and day under the device's directory,
//! `c<channel>l<lens>-YYYY-MM-DD.env`, replaced atomically (write, flush,
//! rename). A file holds only the stretches that have audio, so a camera
//! that was listened to for an hour costs about 7 KB, and one that listened
//! all day at most 173 KB (two bytes per second):
//!
//! ```text
//! "NVSE"  version:u8 (1)  3 zero bytes  day:i64  segments:u32   (little endian)
//! per segment: start:u32  len:u32  peak[len]  rms[len]          (seconds of the day)
//! ```
//!
//! Segments are sorted and apart; a gap of up to [`MERGE_GAP_S`] seconds
//! between stretches is stored as zero bytes instead of starting a segment.
//! A file that does not parse is deleted when it is first read (it is a
//! cache of what was heard, and nothing can repair it).
//!
//! **Memory.** A day being written lives in memory (at most 172.8 KB) and is
//! written by [`EnvelopeStore::flush`]; days read for a query stay while
//! fewer than [`MAX_LOADED`] days are held, then the least recently used
//! clean ones are dropped. Without a directory ([`EnvelopeStore::in_memory`])
//! nothing is written or dropped but by the limits below.
//!
//! **Bounds.** As for the index ([`crate::index::IndexConfig`]): days older
//! than `max_age_days` before "today" are dropped, and the oldest days go
//! first whenever the store exceeds `max_bytes`; today is never dropped to
//! make room.

use crate::index::IndexConfig;
use crate::store::write_atomic;
use crate::{DeviceCamera, Secs, date_time, day_start};
use bc_protocol::archive::DateTime;
use sound_events::{Level, aggregate_peak};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Seconds in a day (the values per camera and day).
pub const DAY_S: usize = 86_400;

/// Gaps of up to this many seconds inside a stretch of audio are stored
/// instead of starting a new segment.
pub const MERGE_GAP_S: usize = 16;

/// Most days held in memory before clean ones are dropped.
pub const MAX_LOADED: usize = 48;

/// Most buckets one loudness lane may have.
pub const MAX_BUCKETS: usize = 100_000;

const MAGIC: &[u8; 4] = b"NVSE";
const VERSION: u8 = 1;
const HEADER: usize = 20;

/// One camera's values for a day, and what the store knows about them.
#[derive(Debug)]
struct DayEnv {
    peak: Vec<u8>,
    rms: Vec<u8>,
    dirty: bool,
    used: u64,
}

impl DayEnv {
    fn put(&mut self, i: usize, peak: Level, rms: Level) {
        if self.peak.len() <= i {
            self.peak.resize(i + 1, 0);
            self.rms.resize(i + 1, 0);
        }
        self.peak[i] = self.peak[i].max(peak.to_byte());
        self.rms[i] = self.rms[i].max(rms.to_byte());
        self.dirty = true;
    }

    fn segments(&self) -> Vec<(usize, usize)> {
        let mut segs: Vec<(usize, usize)> = Vec::new();
        for i in 0..self.peak.len() {
            if self.peak[i] == 0 && self.rms[i] == 0 {
                continue;
            }
            match segs.last_mut() {
                Some((s, l)) if i - (*s + *l) <= MERGE_GAP_S => *l = i + 1 - *s,
                _ => segs.push((i, 1)),
            }
        }
        segs
    }

    fn encoded_len(&self) -> u64 {
        let segs = self.segments();
        (HEADER + segs.iter().map(|(_, l)| 8 + 2 * l).sum::<usize>()) as u64
    }

    fn encode(&self, day: Secs) -> Vec<u8> {
        let segs = self.segments();
        let mut out =
            Vec::with_capacity(HEADER + segs.iter().map(|(_, l)| 8 + 2 * l).sum::<usize>());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&[0; 3]);
        out.extend_from_slice(&day.to_le_bytes());
        out.extend_from_slice(&(segs.len() as u32).to_le_bytes());
        for (s, l) in segs {
            out.extend_from_slice(&(s as u32).to_le_bytes());
            out.extend_from_slice(&(l as u32).to_le_bytes());
            out.extend_from_slice(&self.peak[s..s + l]);
            out.extend_from_slice(&self.rms[s..s + l]);
        }
        out
    }

    /// Parses a file of `day`; `None` if it is not a valid one.
    fn decode(bytes: &[u8], day: Secs) -> Option<DayEnv> {
        if bytes.len() < HEADER || &bytes[..4] != MAGIC || bytes[4] != VERSION {
            return None;
        }
        if i64::from_le_bytes(bytes[8..16].try_into().ok()?) != day {
            return None;
        }
        let n = u32::from_le_bytes(bytes[16..20].try_into().ok()?) as usize;
        if n > DAY_S {
            return None;
        }
        let mut env = DayEnv {
            peak: Vec::new(),
            rms: Vec::new(),
            dirty: false,
            used: 0,
        };
        let (mut at, mut next) = (HEADER, 0usize);
        for _ in 0..n {
            let head = bytes.get(at..at + 8)?;
            let s = u32::from_le_bytes(head[..4].try_into().ok()?) as usize;
            let l = u32::from_le_bytes(head[4..].try_into().ok()?) as usize;
            at += 8;
            if l == 0 || s < next || s + l > DAY_S {
                return None;
            }
            let peak = bytes.get(at..at + l)?;
            let rms = bytes.get(at + l..at + 2 * l)?;
            at += 2 * l;
            env.peak.resize(s, 0);
            env.rms.resize(s, 0);
            env.peak.extend(peak.iter().map(|b| (*b).min(128)));
            env.rms.extend(rms.iter().map(|b| (*b).min(128)));
            next = s + l;
        }
        (at == bytes.len()).then_some(env)
    }
}

/// One camera's lane for a range: a value per bucket, whole dBFS, `None`
/// where no audio was analyzed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lane {
    /// The range's first second (the first bucket's).
    pub start: Secs,
    /// Seconds per bucket.
    pub bucket_s: u32,
    /// The loudest second of each bucket.
    pub peak_db: Vec<Option<i8>>,
    /// The power mean of the seconds of each bucket that have audio.
    pub rms_db: Vec<Option<i8>>,
}

/// Limits of one device's envelope store (those of its index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EnvelopeConfig {
    /// Days older than this many days before "today" are dropped.
    pub max_age_days: u32,
    /// Most bytes kept; the oldest days are dropped first.
    pub max_bytes: u64,
}

impl From<IndexConfig> for EnvelopeConfig {
    fn from(c: IndexConfig) -> EnvelopeConfig {
        EnvelopeConfig {
            max_age_days: c.max_age_days,
            max_bytes: c.max_bytes,
        }
    }
}

type Key = (DeviceCamera, Secs);

fn file_name(camera: DeviceCamera, day: Secs) -> String {
    let t = date_time(day);
    format!(
        "c{}l{}-{:04}-{:02}-{:02}.env",
        camera.channel, camera.lens, t.year, t.month, t.day
    )
}

/// The camera and day a file name stands for.
fn parse_name(name: &str) -> Option<Key> {
    let rest = name.strip_prefix('c')?.strip_suffix(".env")?;
    let (cam, date) = rest.split_once('-')?;
    let (ch, lens) = cam.split_once('l')?;
    let mut d = date.splitn(3, '-');
    let (y, m, dd) = (d.next()?, d.next()?, d.next()?);
    let day = DateTime::date(y.parse().ok()?, m.parse().ok()?, dd.parse().ok()?)?;
    let camera = DeviceCamera::new(ch.parse().ok()?, lens.parse().ok().filter(|l| *l <= 1)?);
    let key = (camera, day.to_naive_secs());
    (file_name(key.0, key.1) == name).then_some(key)
}

/// One device's loudness envelopes (see the module documentation).
#[derive(Debug)]
pub struct EnvelopeStore {
    dir: Option<PathBuf>,
    cfg: EnvelopeConfig,
    loaded: BTreeMap<Key, DayEnv>,
    /// Every day that has data, loaded or only on disk, with its size on
    /// disk (0 when never written).
    known: BTreeMap<Key, u64>,
    tick: u64,
    revision: u64,
}

impl EnvelopeStore {
    /// A store that lives in memory only.
    pub fn in_memory(cfg: EnvelopeConfig) -> EnvelopeStore {
        EnvelopeStore {
            dir: None,
            cfg,
            loaded: BTreeMap::new(),
            known: BTreeMap::new(),
            tick: 0,
            revision: 0,
        }
    }

    /// Opens (or creates) the store in `dir`. Nothing is read but the
    /// directory listing; days load when first used.
    ///
    /// # Errors
    /// The directory cannot be created or listed.
    pub fn open(dir: &Path, cfg: EnvelopeConfig) -> io::Result<EnvelopeStore> {
        fs::create_dir_all(dir)?;
        let mut s = EnvelopeStore::in_memory(cfg);
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(key) = name.to_str().and_then(parse_name) else {
                continue;
            };
            s.known.insert(key, entry.metadata().map_or(0, |m| m.len()));
        }
        s.dir = Some(dir.to_owned());
        Ok(s)
    }

    /// Raised by every stored change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The limits in force.
    pub fn config(&self) -> EnvelopeConfig {
        self.cfg
    }

    /// Replaces the limits (they apply at the next [`EnvelopeStore::flush`]).
    pub fn set_config(&mut self, cfg: EnvelopeConfig) {
        self.cfg = cfg;
    }

    /// Every camera that has data.
    pub fn cameras(&self) -> Vec<DeviceCamera> {
        let mut v: Vec<DeviceCamera> = self.known.keys().map(|(c, _)| *c).collect();
        v.dedup();
        v.sort();
        v.dedup();
        v
    }

    /// Days of `camera` that have data, oldest first.
    pub fn days(&self, camera: DeviceCamera) -> Vec<Secs> {
        self.known
            .keys()
            .filter(|(c, _)| *c == camera)
            .map(|(_, d)| *d)
            .collect()
    }

    /// Bytes the store holds on disk, plus what is only in memory.
    pub fn bytes(&self) -> u64 {
        self.known
            .iter()
            .map(|(k, on_disk)| self.loaded.get(k).map_or(*on_disk, DayEnv::encoded_len))
            .sum()
    }

    /// Days held in memory.
    pub fn loaded_days(&self) -> usize {
        self.loaded.len()
    }

    fn path(&self, key: Key) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(file_name(key.0, key.1)))
    }

    /// Makes sure `key`'s day is in memory if it has data; `create` also
    /// makes an empty one.
    fn load(&mut self, key: Key, create: bool) -> Option<&mut DayEnv> {
        self.tick += 1;
        let tick = self.tick;
        if !self.loaded.contains_key(&key) {
            let mut env = None;
            if let Some(path) = self
                .known
                .contains_key(&key)
                .then(|| self.path(key))
                .flatten()
            {
                match fs::read(&path).map(|b| DayEnv::decode(&b, key.1)) {
                    Ok(Some(e)) => env = Some(e),
                    Ok(None) => {
                        warn!(file = %path.display(), "unreadable loudness file removed");
                        let _ = fs::remove_file(&path);
                        self.known.remove(&key);
                    }
                    Err(e) => {
                        warn!(error = %e, "cannot read a loudness file");
                        self.known.remove(&key);
                    }
                }
            }
            match env {
                Some(e) => {
                    self.loaded.insert(key, e);
                }
                None if create => {
                    self.known.entry(key).or_insert(0);
                    self.loaded.insert(
                        key,
                        DayEnv {
                            peak: Vec::new(),
                            rms: Vec::new(),
                            dirty: false,
                            used: 0,
                        },
                    );
                }
                None => return None,
            }
        }
        let e = self.loaded.get_mut(&key)?;
        e.used = tick;
        Some(e)
    }

    /// Records one second of `camera` (a naive second of the device). A
    /// second heard twice (two streams of one camera) keeps the louder
    /// values.
    pub fn put(&mut self, camera: DeviceCamera, second: Secs, peak: Level, rms: Level) {
        if second < 0 || (!peak.is_some() && !rms.is_some()) {
            return;
        }
        let day = day_start(second);
        let i = (second - day) as usize;
        if let Some(e) = self.load((camera, day), true) {
            e.put(i, peak, rms);
            self.revision += 1;
        }
    }

    /// The lane of `camera` for `start..=end`, aggregated into buckets of
    /// `bucket_s` seconds. `None` if there would be more than
    /// [`MAX_BUCKETS`] buckets or the range is empty.
    pub fn lane(
        &mut self,
        camera: DeviceCamera,
        start: Secs,
        end: Secs,
        bucket_s: u32,
    ) -> Option<Lane> {
        if end < start || bucket_s == 0 {
            return None;
        }
        let b = i64::from(bucket_s);
        let n = ((end - start) / b + 1) as usize;
        if n > MAX_BUCKETS {
            return None;
        }
        let mut peak = vec![Level::NONE; n];
        let mut power = vec![0.0f64; n];
        let mut count = vec![0u32; n];
        let mut day = day_start(start);
        while day <= end {
            if let Some(e) = self.load((camera, day), false) {
                let len = e.peak.len();
                let from = (start - day).max(0) as usize;
                let to = ((end - day + 1) as usize).min(len);
                for i in from..to {
                    let bucket = ((day + i as i64 - start) / b) as usize;
                    let p = Level::from_byte(e.peak[i]);
                    peak[bucket] = aggregate_peak(&[peak[bucket], p]);
                    if let Some(db) = Level::from_byte(e.rms[i]).db() {
                        power[bucket] += 10f64.powf(f64::from(db) / 10.0);
                        count[bucket] += 1;
                    }
                }
            }
            day += 86_400;
        }
        Some(Lane {
            start,
            bucket_s,
            peak_db: peak.iter().map(|l| l.db()).collect(),
            rms_db: (0..n)
                .map(|i| {
                    (count[i] > 0)
                        .then(|| Level::from_power(power[i] / f64::from(count[i])).db())
                        .flatten()
                })
                .collect(),
        })
    }

    /// Writes the days changed since the last flush, applies the limits
    /// relative to `now` (naive seconds), and drops clean days from memory
    /// beyond [`MAX_LOADED`]. Returns how many days the limits removed.
    ///
    /// # Errors
    /// A file could not be written or removed; everything else was still
    /// done, and the day stays dirty to be written again.
    pub fn flush(&mut self, now: Secs) -> io::Result<usize> {
        let mut first_error = None;
        let dirty: Vec<Key> = self
            .loaded
            .iter()
            .filter(|(_, e)| e.dirty)
            .map(|(k, _)| *k)
            .collect();
        for key in dirty {
            let Some(path) = self.path(key) else {
                continue;
            };
            let bytes = self.loaded[&key].encode(key.1);
            match write_atomic(&path, &bytes) {
                Ok(()) => {
                    self.known.insert(key, bytes.len() as u64);
                    if let Some(e) = self.loaded.get_mut(&key) {
                        e.dirty = false;
                    }
                }
                Err(e) => {
                    warn!(error = %e, "cannot store a loudness day");
                    first_error.get_or_insert(e);
                }
            }
        }
        let dropped = match self.prune(now) {
            Ok(n) => n,
            Err(e) => {
                first_error.get_or_insert(e);
                0
            }
        };
        if self.dir.is_some() && self.loaded.len() > MAX_LOADED {
            let mut clean: Vec<(u64, Key)> = self
                .loaded
                .iter()
                .filter(|(_, e)| !e.dirty)
                .map(|(k, e)| (e.used, *k))
                .collect();
            clean.sort();
            let excess = self.loaded.len() - MAX_LOADED;
            for (_, k) in clean.into_iter().take(excess) {
                self.loaded.remove(&k);
            }
        }
        first_error.map_or(Ok(dropped), Err)
    }

    /// Applies the age and size limits relative to `now`; returns how many
    /// days were dropped.
    ///
    /// # Errors
    /// A file could not be removed.
    pub fn prune(&mut self, now: Secs) -> io::Result<usize> {
        let today = day_start(now);
        let oldest = today - i64::from(self.cfg.max_age_days) * 86_400;
        let mut drop: Vec<Key> = self
            .known
            .keys()
            .filter(|(_, d)| *d < oldest)
            .copied()
            .collect();
        let size = |s: &Self, k: &Key| {
            s.loaded
                .get(k)
                .map_or_else(|| s.known.get(k).copied().unwrap_or(0), DayEnv::encoded_len)
        };
        let mut total: u64 = self
            .known
            .keys()
            .filter(|(_, d)| *d >= oldest)
            .map(|k| size(self, k))
            .sum();
        if total > self.cfg.max_bytes {
            // Oldest day first, across cameras; never today.
            let mut by_age: Vec<Key> = self
                .known
                .keys()
                .filter(|(_, d)| *d >= oldest && *d < today)
                .copied()
                .collect();
            by_age.sort_by_key(|(c, d)| (*d, *c));
            for k in by_age {
                if total <= self.cfg.max_bytes {
                    break;
                }
                total -= size(self, &k);
                drop.push(k);
            }
        }
        let mut result = Ok(());
        for k in &drop {
            self.loaded.remove(k);
            self.known.remove(k);
            if let Some(p) = self.path(*k) {
                match fs::remove_file(&p) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => result = Err(e),
                }
            }
        }
        if !drop.is_empty() {
            self.revision += 1;
            info!(days = drop.len(), "loudness days dropped by the limits");
        }
        result.map(|()| drop.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(n: u8) -> Secs {
        DateTime::date(2026, 10, n).unwrap().to_naive_secs()
    }

    fn cam(ch: u8) -> DeviceCamera {
        DeviceCamera::first(ch)
    }

    fn lv(db: f32) -> Level {
        Level::from_dbfs(db)
    }

    fn cfg() -> EnvelopeConfig {
        EnvelopeConfig {
            max_age_days: 90,
            max_bytes: 64 * 1024 * 1024,
        }
    }

    #[test]
    fn names_round_trip_and_reject_others() {
        let k = (DeviceCamera::new(3, 1), day(9));
        assert_eq!(file_name(k.0, k.1), "c3l1-2026-10-09.env");
        assert_eq!(parse_name("c3l1-2026-10-09.env"), Some(k));
        for bad in [
            "c3l2-2026-10-09.env",
            "c3l1-2026-13-09.env",
            "c3l1-2026-10-09.env.tmp",
            "x3l1-2026-10-09.env",
            "c3l1-2026-1-9.env",
            "c300l1-2026-10-09.env",
        ] {
            assert_eq!(parse_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn lanes_aggregate_and_mark_gaps() {
        let mut s = EnvelopeStore::in_memory(cfg());
        let t = day(9) + 3600;
        // Seconds 0, 1, 2 and 4 of the bucket of 5 have audio.
        for (i, (p, r)) in [(-10.0, -20.0), (-30.0, -30.0), (-12.0, -22.0), (0.0, 0.0)]
            .iter()
            .enumerate()
        {
            let at = if i == 3 { 4 } else { i as i64 };
            s.put(cam(0), t + at, lv(*p), lv(*r));
        }
        // The next bucket: nothing. The third: silence is a level.
        s.put(cam(0), t + 10, Level::SILENCE, Level::SILENCE);
        let lane = s.lane(cam(0), t, t + 14, 5).unwrap();
        assert_eq!(lane.peak_db, [Some(0), None, Some(-127)]);
        let rms = lane.rms_db[0].unwrap();
        // Power mean of -20, -30, -22, 0 dB is about -6 dB.
        assert!((-7..=-5).contains(&rms), "{rms}");
        assert_eq!(lane.rms_db[1], None);
        // One-second buckets; a camera without data is all gaps.
        let l1 = s.lane(cam(0), t, t + 2, 1).unwrap();
        assert_eq!(l1.peak_db, [Some(-10), Some(-30), Some(-12)]);
        assert_eq!(l1.rms_db, [Some(-20), Some(-30), Some(-22)]);
        assert_eq!(s.lane(cam(1), t, t + 2, 1).unwrap().peak_db, [None; 3]);
        // The same second heard twice keeps the louder values.
        s.put(cam(0), t + 1, lv(-5.0), lv(-40.0));
        let l1 = s.lane(cam(0), t + 1, t + 1, 1).unwrap();
        assert_eq!((l1.peak_db[0], l1.rms_db[0]), (Some(-5), Some(-30)));
        // Limits on the range.
        assert!(s.lane(cam(0), t, t - 1, 1).is_none());
        assert!(s.lane(cam(0), t, t + 5, 0).is_none());
        assert!(s.lane(cam(0), t, t + MAX_BUCKETS as i64, 1).is_none());
        assert!(s.lane(cam(0), t, t + MAX_BUCKETS as i64 - 1, 1).is_some());
    }

    #[test]
    fn buckets_cross_midnight() {
        let mut s = EnvelopeStore::in_memory(cfg());
        let midnight = day(10);
        s.put(cam(0), midnight - 1, lv(-20.0), lv(-20.0));
        s.put(cam(0), midnight, lv(-10.0), lv(-10.0));
        // A bucket of 60 s that starts before midnight has both.
        let lane = s.lane(cam(0), midnight - 30, midnight + 29, 60).unwrap();
        assert_eq!(lane.peak_db, [Some(-10)]);
        let lane = s.lane(cam(0), midnight - 2, midnight + 1, 2).unwrap();
        assert_eq!(lane.peak_db, [Some(-20), Some(-10)]);
        assert_eq!(s.days(cam(0)), [day(9), day(10)]);
        // Last second of the day, and the day that starts at 0.
        s.put(cam(0), midnight + 86_399, lv(-1.0), lv(-1.0));
        assert_eq!(
            s.lane(cam(0), midnight + 86_399, midnight + 86_399, 1)
                .unwrap()
                .peak_db,
            [Some(-1)]
        );
    }

    #[test]
    fn persists_sparse_files_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let sound = dir.path().join("sound");
        let mut s = EnvelopeStore::open(&sound, cfg()).unwrap();
        let t = day(9) + 7200;
        for i in 0..100 {
            s.put(cam(2), t + i, lv(-20.0 - i as f32 / 10.0), lv(-30.0));
        }
        // A second stretch beyond the merge gap.
        s.put(cam(2), t + 100 + MERGE_GAP_S as i64 + 5, lv(-9.0), lv(-9.0));
        assert_eq!(s.flush(day(10)).unwrap(), 0);
        let file = sound.join("c2l0-2026-10-09.env");
        let len = fs::metadata(&file).unwrap().len();
        // Two segments: 100 and 1 values, two bytes each, plus headers.
        assert_eq!(len, (HEADER + 8 + 200 + 8 + 2) as u64);
        assert!(!sound.join("c2l0-2026-10-09.env.tmp").exists());
        assert_eq!(s.bytes(), len);
        drop(s);

        let mut s = EnvelopeStore::open(&sound, cfg()).unwrap();
        assert_eq!(s.loaded_days(), 0, "nothing is read until asked");
        assert_eq!(s.days(cam(2)), [day(9)]);
        let lane = s.lane(cam(2), t, t + 130, 1).unwrap();
        assert_eq!(lane.peak_db[0], Some(-20));
        assert_eq!(lane.peak_db[99], Some(-30));
        assert_eq!(lane.peak_db[100], None);
        assert_eq!(lane.peak_db[105], None);
        assert_eq!(lane.peak_db[100 + MERGE_GAP_S + 5], Some(-9));
        assert_eq!(lane.rms_db[50], Some(-30));
        assert_eq!(s.loaded_days(), 1);
        // A full day is two bytes per second.
        let mut full = EnvelopeStore::in_memory(cfg());
        for i in 0..DAY_S as i64 {
            full.put(cam(0), day(9) + i, lv(-20.0), lv(-30.0));
        }
        assert_eq!(full.bytes(), (HEADER + 8 + 2 * DAY_S) as u64);
    }

    #[test]
    fn damaged_files_are_removed_when_read() {
        let dir = tempfile::tempdir().unwrap();
        let sound = dir.path().join("sound");
        {
            let mut s = EnvelopeStore::open(&sound, cfg()).unwrap();
            s.put(cam(0), day(9) + 5, lv(-20.0), lv(-20.0));
            s.put(cam(1), day(9) + 5, lv(-20.0), lv(-20.0));
            s.flush(day(9)).unwrap();
        }
        let good = sound.join("c0l0-2026-10-09.env");
        let mut bytes = fs::read(&good).unwrap();
        bytes.truncate(bytes.len() - 3);
        fs::write(&good, &bytes).unwrap();
        // A file for another day under this name is no good either.
        fs::copy(
            sound.join("c1l0-2026-10-09.env"),
            sound.join("c1l0-2026-10-08.env"),
        )
        .unwrap();
        fs::write(sound.join("notes.txt"), b"x").unwrap();
        let mut s = EnvelopeStore::open(&sound, cfg()).unwrap();
        assert_eq!(
            s.lane(cam(0), day(9), day(9) + 9, 1).unwrap().peak_db,
            [None; 10]
        );
        assert!(!good.exists());
        assert_eq!(
            s.lane(cam(1), day(8), day(8) + 9, 1).unwrap().peak_db,
            [None; 10]
        );
        assert!(!sound.join("c1l0-2026-10-08.env").exists());
        assert!(sound.join("notes.txt").exists());
        // The intact one reads.
        assert_eq!(
            s.lane(cam(1), day(9) + 5, day(9) + 5, 1).unwrap().peak_db,
            [Some(-20)]
        );
    }

    #[test]
    fn age_and_size_limits_drop_the_oldest_days() {
        let dir = tempfile::tempdir().unwrap();
        let sound = dir.path().join("sound");
        let mut s = EnvelopeStore::open(&sound, cfg()).unwrap();
        for d in 1..=10u8 {
            for i in 0..50 {
                s.put(cam(0), day(d) + i, lv(-20.0), lv(-20.0));
            }
        }
        // Today is the 10th. Age: keep 3 days back.
        s.set_config(EnvelopeConfig {
            max_age_days: 3,
            max_bytes: u64::MAX,
        });
        assert_eq!(s.flush(day(10) + 100).unwrap(), 6);
        assert_eq!(s.days(cam(0)), [day(7), day(8), day(9), day(10)]);
        assert!(!sound.join("c0l0-2026-10-06.env").exists());
        assert!(sound.join("c0l0-2026-10-07.env").exists());
        // Size: room for two days of 128 bytes (20 + 8 + 100) each.
        s.set_config(EnvelopeConfig {
            max_age_days: 90,
            max_bytes: 260,
        });
        assert_eq!(s.flush(day(10) + 100).unwrap(), 2);
        assert_eq!(s.days(cam(0)), [day(9), day(10)]);
        // Today is never dropped to make room.
        s.set_config(EnvelopeConfig {
            max_age_days: 90,
            max_bytes: 1,
        });
        assert_eq!(s.flush(day(10) + 100).unwrap(), 1);
        assert_eq!(s.days(cam(0)), [day(10)]);
        // The same bounds apply in memory.
        let mut m = EnvelopeStore::in_memory(EnvelopeConfig {
            max_age_days: 1,
            max_bytes: 1 << 20,
        });
        m.put(cam(0), day(7), lv(-1.0), lv(-1.0));
        m.put(cam(0), day(10), lv(-1.0), lv(-1.0));
        assert_eq!(m.flush(day(10)).unwrap(), 1);
        assert_eq!(m.days(cam(0)), [day(10)]);
    }

    #[test]
    fn clean_days_leave_memory_beyond_the_cap_but_dirty_ones_stay() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = EnvelopeStore::open(&dir.path().join("s"), cfg()).unwrap();
        let base = day(1);
        for d in 0..(MAX_LOADED as i64 + 10) {
            s.put(cam(0), base + d * 86_400 + 1, lv(-20.0), lv(-20.0));
        }
        assert_eq!(s.loaded_days(), MAX_LOADED + 10);
        s.flush(base + 70 * 86_400).unwrap();
        assert_eq!(s.loaded_days(), MAX_LOADED);
        // Everything is still there.
        assert_eq!(s.days(cam(0)).len(), MAX_LOADED + 10);
        let lane = s.lane(cam(0), base + 1, base + 1, 1).unwrap();
        assert_eq!(lane.peak_db, [Some(-20)]);
        // In memory only: nothing is ever dropped from memory but by limits.
        let mut m = EnvelopeStore::in_memory(cfg());
        for d in 0..(MAX_LOADED as i64 + 10) {
            m.put(cam(0), base + d * 86_400, lv(-20.0), lv(-20.0));
        }
        m.flush(base + 70 * 86_400).unwrap();
        assert_eq!(m.loaded_days(), MAX_LOADED + 10);
    }
}
