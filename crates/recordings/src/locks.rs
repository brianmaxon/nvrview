//! Locked incidents (docs/FFI-RECORDINGS.md, section 3, "Locked
//! incidents"): the incidents the user wants kept, stored like the edits.
//!
//! A [`Lock`] holds the keys of the incidents it covers and its extent: per
//! camera, the time ranges (with the download margins) those incidents
//! cover. Incidents are derived, so after every regroup and edit replay
//! [`LockStore::follow`] decides which incidents are locked:
//!
//! - an incident is locked when its key is one of a lock's keys, or when
//!   one of its event intervals overlaps the lock's extent on the same
//!   camera (unless the user unlocked that key from that lock);
//! - an incident that more than one lock claims joins them into one lock
//!   (the oldest survives), which frees slots;
//! - each lock's keys become the incidents it covers now, plus earlier keys
//!   whose earliest event no incident holds at the moment (their days may
//!   not be indexed in this run); its extent grows to cover them all. Extents only grow, except by
//!   unlocking (below).
//!
//! So a merged incident is locked if any part was; the parts of a split, of
//! a removed camera, or of a regroup that cut an incident stay under the
//! same lock; and an incident whose key changed with its camera group still
//! overlaps its extent. [`LockStore::unlock`] removes one incident's ranges
//! and key from its lock (other parts stay locked) and drops a lock with no
//! key left.
//!
//! **Bounds.** At most [`MAX_LOCKS`] locks; only [`LockStore::lock`] adds one
//! (refused at the cap; the store never drops a lock to make room). A
//! lock's extent spans at most [`MAX_LOCK_SPAN`] seconds: locking beyond it
//! is refused, and an incident that would stretch a lock beyond it by
//! overlap alone is not joined. Each lock remembers at most
//! [`MAX_RELEASED`] unlocked keys.
//!
//! **Store.** `incident-locks.json`, written atomically on every change;
//! an unreadable file is set aside as `<name>.bad` (the locks are the
//! user's).

use crate::Secs;
use crate::incident::{CameraId, Incident, key_anchor};
use crate::store::{read_json, write_json};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Most locks stored.
pub const MAX_LOCKS: usize = 1000;
/// Longest extent of one lock, first start to last end over every camera.
pub const MAX_LOCK_SPAN: Secs = 31 * 86_400;
/// Most unlocked keys one lock remembers (the oldest are forgotten).
pub const MAX_RELEASED: usize = 256;
const VERSION: u32 = 1;

/// Time ranges, sorted, none overlapping or touching.
pub type Ranges = Vec<(Secs, Secs)>;

/// One lock.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    /// Grows with every new lock: a lower number is an older lock.
    pub id: u64,
    /// Keys of the incidents it covers.
    pub keys: BTreeSet<String>,
    /// Per camera, the ranges (with margins) its incidents cover.
    #[serde(with = "crate::index::pairs")]
    pub extent: BTreeMap<CameraId, Ranges>,
    /// Keys the user unlocked from it: never joined again by overlap.
    #[serde(default)]
    pub released: Vec<String>,
    /// Keys whose download waits until the incident stops growing.
    #[serde(default)]
    pub settle: BTreeSet<String>,
}

fn add_range(v: &mut Ranges, s: Secs, e: Secs) -> bool {
    if v.iter().any(|r| r.0 <= s && e <= r.1) {
        return false;
    }
    v.push((s, e));
    v.sort_unstable();
    let mut out: Ranges = Vec::with_capacity(v.len());
    for (s, e) in v.drain(..) {
        match out.last_mut() {
            Some(last) if s <= last.1.saturating_add(1) => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    *v = out;
    true
}

fn remove_range(v: &mut Ranges, s: Secs, e: Secs) {
    let mut out: Ranges = Vec::with_capacity(v.len() + 1);
    for &(a, b) in v.iter() {
        if b < s || a > e {
            out.push((a, b));
            continue;
        }
        if a < s {
            out.push((a, s - 1));
        }
        if b > e {
            out.push((e + 1, b));
        }
    }
    *v = out;
}

fn overlaps(v: &[(Secs, Secs)], s: Secs, e: Secs) -> bool {
    v.iter().any(|r| r.0 <= e && s <= r.1)
}

impl Lock {
    /// First start and last end over every camera.
    pub fn span(&self) -> Option<(Secs, Secs)> {
        let mut it = self.extent.values().flatten();
        let first = *it.next()?;
        Some(it.fold(first, |(s, e), r| (s.min(r.0), e.max(r.1))))
    }

    /// Whether the extent covers `t` on `camera`.
    pub fn covers(&self, camera: &CameraId, t: Secs) -> bool {
        self.extent.get(camera).is_some_and(|v| overlaps(v, t, t))
    }

    /// Whether an event interval of `inc` overlaps the extent.
    fn overlaps(&self, inc: &Incident) -> bool {
        inc.intervals().iter().any(|(c, iv)| {
            self.extent
                .get(c)
                .is_some_and(|v| iv.iter().any(|(s, e)| overlaps(v, *s, *e)))
        })
    }

    /// The span with `inc`'s ranges added.
    fn span_with(&self, inc: &Incident) -> Secs {
        let (mut s, mut e) = self.span().unwrap_or((Secs::MAX, Secs::MIN));
        for (a, b) in inc.download_ranges().into_values() {
            s = s.min(a);
            e = e.max(b);
        }
        e.saturating_sub(s)
    }

    /// Adds `inc`'s ranges; whether the extent grew.
    fn grow(&mut self, inc: &Incident) -> bool {
        let mut grew = false;
        for (c, (s, e)) in inc.download_ranges() {
            grew |= add_range(self.extent.entry(c).or_default(), s, e);
        }
        grew
    }

    /// Takes in another lock (joined by an incident both claim).
    fn absorb(&mut self, other: Lock) {
        self.keys.extend(other.keys);
        for (c, v) in other.extent {
            let mine = self.extent.entry(c).or_default();
            for (s, e) in v {
                add_range(mine, s, e);
            }
        }
        for k in other.released {
            if !self.released.contains(&k) {
                self.released.push(k);
            }
        }
        let n = self.released.len().saturating_sub(MAX_RELEASED);
        self.released.drain(..n);
        self.settle.extend(other.settle);
    }

    /// Whether a key's earliest event lies in the extent.
    fn anchors(&self, key: &str) -> bool {
        key_anchor(key).is_some_and(|(t, c)| self.covers(&c, t))
    }
}

/// The span of `inc`'s download ranges.
fn incident_span(inc: &Incident) -> Secs {
    Lock::default().span_with(inc)
}

/// Why a lock was refused.
#[derive(Debug)]
pub enum LockError {
    /// [`MAX_LOCKS`] locks are stored already.
    Full,
    /// The lock's extent would span more than [`MAX_LOCK_SPAN`].
    TooLong,
    /// The locks could not be written.
    Io(io::Error),
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockError::Full => write!(f, "too many locked incidents"),
            LockError::TooLong => write!(f, "a lock would span more than 31 days"),
            LockError::Io(e) => write!(f, "cannot store the locks: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct LockFile {
    version: u32,
    next_id: u64,
    locks: Vec<Lock>,
}

/// The stored locks.
#[derive(Debug)]
pub struct LockStore {
    path: Option<PathBuf>,
    next_id: u64,
    /// By id, oldest first.
    locks: Vec<Lock>,
    revision: u64,
}

impl LockStore {
    /// No locks, kept in memory only (no data directory).
    pub fn in_memory() -> LockStore {
        LockStore {
            path: None,
            next_id: 1,
            locks: Vec::new(),
            revision: 0,
        }
    }

    /// Opens the locks stored at `path` (created on the first lock).
    pub fn open(path: &Path) -> LockStore {
        let mut store = LockStore {
            path: Some(path.to_owned()),
            ..LockStore::in_memory()
        };
        match read_json::<LockFile>(path) {
            Ok(Some(f)) if f.version == VERSION => {
                let mut locks = f.locks;
                locks.sort_by_key(|l| l.id);
                locks.truncate(MAX_LOCKS);
                store.next_id = f.next_id.max(locks.last().map_or(0, |l| l.id) + 1).max(1);
                store.locks = locks;
            }
            Ok(None) => {}
            Ok(Some(_)) | Err(_) => {
                warn!("incident locks unreadable; set aside");
                let mut bad = path.as_os_str().to_owned();
                bad.push(".bad");
                let _ = std::fs::rename(path, PathBuf::from(bad));
            }
        }
        store
    }

    /// The locks, oldest first.
    pub fn locks(&self) -> &[Lock] {
        &self.locks
    }

    /// Number of locks.
    pub fn len(&self) -> usize {
        self.locks.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
    }

    /// Raised by every change.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn save(&self) -> io::Result<()> {
        match &self.path {
            Some(p) => write_json(
                p,
                &LockFile {
                    version: VERSION,
                    next_id: self.next_id,
                    locks: self.locks.clone(),
                },
            ),
            None => Ok(()),
        }
    }

    /// Replaces the locks and stores them; on failure nothing changes.
    fn commit(&mut self, locks: Vec<Lock>, next_id: u64) -> io::Result<()> {
        let old = std::mem::replace(&mut self.locks, locks);
        let old_next = std::mem::replace(&mut self.next_id, next_id);
        if let Err(e) = self.save() {
            self.locks = old;
            self.next_id = old_next;
            return Err(e);
        }
        self.revision += 1;
        Ok(())
    }

    /// The id of the lock that has this key.
    pub fn lock_of(&self, key: &str) -> Option<u64> {
        self.locks
            .iter()
            .find(|l| l.keys.contains(key))
            .map(|l| l.id)
    }

    /// Whether an edit with this key is a locked incident's: the key is a
    /// lock's, or its earliest event lies in a lock's extent.
    pub fn covers_key(&self, key: &str) -> bool {
        self.locks
            .iter()
            .any(|l| l.keys.contains(key) || l.anchors(key))
    }

    /// Locks `inc` and returns its lock's id. An incident that overlaps a
    /// lock it was unlocked from joins that lock again; otherwise a new
    /// lock is made. Already locked: nothing changes.
    ///
    /// # Errors
    /// [`LockError::Full`] at [`MAX_LOCKS`], [`LockError::TooLong`] beyond
    /// [`MAX_LOCK_SPAN`], [`LockError::Io`]; nothing changes then.
    pub fn lock(&mut self, inc: &Incident) -> Result<u64, LockError> {
        let key = inc.key();
        if let Some(id) = self.lock_of(&key) {
            return Ok(id);
        }
        let mut locks = self.locks.clone();
        let mut next_id = self.next_id;
        for l in &mut locks {
            l.released.retain(|k| *k != key);
        }
        let joins = |l: &&mut Lock| l.overlaps(inc) && l.span_with(inc) <= MAX_LOCK_SPAN;
        let id = match locks.iter_mut().find(joins) {
            Some(l) => {
                l.keys.insert(key);
                l.grow(inc);
                l.id
            }
            None => {
                if locks.len() >= MAX_LOCKS {
                    return Err(LockError::Full);
                }
                if incident_span(inc) > MAX_LOCK_SPAN {
                    return Err(LockError::TooLong);
                }
                let mut l = Lock {
                    id: next_id,
                    ..Lock::default()
                };
                next_id += 1;
                l.keys.insert(key);
                l.grow(inc);
                locks.push(l);
                next_id - 1
            }
        };
        self.commit(locks, next_id).map_err(LockError::Io)?;
        Ok(id)
    }

    /// Unlocks `inc`: removes its ranges and key from its lock, remembers
    /// the key there as unlocked, and drops the lock once no key is left
    /// (keys whose earliest event is no longer in the extent go too).
    /// Returns whether it was locked.
    ///
    /// # Errors
    /// [`LockError::Io`]; nothing changes then.
    pub fn unlock(&mut self, inc: &Incident) -> Result<bool, LockError> {
        let key = inc.key();
        let Some(i) = self.locks.iter().position(|l| l.keys.contains(&key)) else {
            return Ok(false);
        };
        let mut locks = self.locks.clone();
        let l = &mut locks[i];
        for (c, (s, e)) in inc.download_ranges() {
            if let Some(v) = l.extent.get_mut(&c) {
                remove_range(v, s, e);
            }
        }
        l.extent.retain(|_, v| !v.is_empty());
        l.keys.remove(&key);
        l.settle.remove(&key);
        let anchored: BTreeSet<String> = l.keys.iter().filter(|k| l.anchors(k)).cloned().collect();
        l.keys = anchored;
        l.released.retain(|k| *k != key);
        l.released.push(key);
        let n = l.released.len().saturating_sub(MAX_RELEASED);
        l.released.drain(..n);
        if l.keys.is_empty() || l.extent.is_empty() {
            locks.remove(i);
        }
        self.commit(locks, self.next_id).map_err(LockError::Io)?;
        Ok(true)
    }

    /// Whether `incidents` merged into one would fit the span limit
    /// together with every lock any of them belongs to (a merge edit is
    /// refused otherwise).
    pub fn merge_fits(&self, incidents: &[&Incident]) -> bool {
        let mut joined = Lock::default();
        let mut any = false;
        for inc in incidents {
            let key = inc.key();
            for l in &self.locks {
                if l.keys.contains(&key) || (l.overlaps(inc) && !l.released.contains(&key)) {
                    joined.absorb(l.clone());
                    any = true;
                }
            }
        }
        if !any {
            return true;
        }
        for inc in incidents {
            joined.grow(inc);
        }
        joined
            .span()
            .is_none_or(|(s, e)| e.saturating_sub(s) <= MAX_LOCK_SPAN)
    }

    /// Marks a locked key's download as waiting for the incident to stop
    /// growing.
    ///
    /// # Errors
    /// [`LockError::Io`]; nothing changes then.
    pub fn settle(&mut self, key: &str, waiting: bool) -> Result<(), LockError> {
        let Some(i) = self.locks.iter().position(|l| l.keys.contains(key)) else {
            return Ok(());
        };
        if self.locks[i].settle.contains(key) == waiting {
            return Ok(());
        }
        let mut locks = self.locks.clone();
        if waiting {
            locks[i].settle.insert(key.to_owned());
        } else {
            locks[i].settle.remove(key);
        }
        self.commit(locks, self.next_id).map_err(LockError::Io)
    }

    /// Replaces every key `f` maps to a new one (keys, unlocked keys, and
    /// waiting downloads). Returns how many changed.
    ///
    /// # Errors
    /// [`LockError::Io`]; nothing changes then.
    pub fn rewrite_keys(&mut self, f: impl Fn(&str) -> Option<String>) -> Result<usize, LockError> {
        let mut locks = self.locks.clone();
        let mut n = 0;
        let mut map = |k: String| match f(&k) {
            Some(new) => {
                n += 1;
                new
            }
            None => k,
        };
        for l in &mut locks {
            l.keys = std::mem::take(&mut l.keys)
                .into_iter()
                .map(&mut map)
                .collect();
            l.settle = std::mem::take(&mut l.settle)
                .into_iter()
                .map(&mut map)
                .collect();
            l.released = std::mem::take(&mut l.released)
                .into_iter()
                .map(&mut map)
                .collect();
        }
        if n > 0 {
            self.commit(locks, self.next_id).map_err(LockError::Io)?;
        }
        Ok(n)
    }

    /// Matches the locks to freshly grouped and edited incidents (see the
    /// module documentation) and stores any change. Returns, for every
    /// locked incident's key, its lock's id. A failure to store is logged;
    /// the locks in memory follow anyway and are stored with the next
    /// change.
    pub fn follow(&mut self, incidents: &[Incident]) -> BTreeMap<String, u64> {
        let n = self.locks.len();
        if n == 0 {
            return BTreeMap::new();
        }
        let keys: Vec<String> = incidents.iter().map(Incident::key).collect();
        let present: BTreeSet<&str> = keys.iter().map(String::as_str).collect();
        // Earliest events an absent key may name: if an incident holds it,
        // that key was merged away or regrouped, not left unindexed.
        let starts: BTreeSet<(Secs, &CameraId)> = incidents
            .iter()
            .flat_map(|i| i.events.iter().map(|e| (e.start, &e.camera)))
            .collect();
        let mut by_key: HashMap<&str, usize> = HashMap::new();
        let mut by_camera: HashMap<&CameraId, Vec<usize>> = HashMap::new();
        for (i, l) in self.locks.iter().enumerate() {
            for k in &l.keys {
                by_key.entry(k.as_str()).or_insert(i);
            }
            for c in l.extent.keys() {
                by_camera.entry(c).or_default().push(i);
            }
        }
        // Which locks claim each incident; locks claiming one incident are
        // joined (union-find over lock positions, the oldest as root).
        let mut root: Vec<usize> = (0..n).collect();
        fn find(root: &mut [usize], mut i: usize) -> usize {
            while root[i] != i {
                root[i] = root[root[i]];
                i = root[i];
            }
            i
        }
        let mut claimed: Vec<(usize, usize)> = Vec::new();
        for (ii, inc) in incidents.iter().enumerate() {
            let key = keys[ii].as_str();
            let mut ls: BTreeSet<usize> = BTreeSet::new();
            if let Some(&i) = by_key.get(key) {
                ls.insert(i);
            }
            let intervals = inc.intervals();
            for (c, iv) in &intervals {
                for &i in by_camera.get(c).into_iter().flatten() {
                    if ls.contains(&i) {
                        continue;
                    }
                    let l = &self.locks[i];
                    let hit = l
                        .extent
                        .get(c)
                        .is_some_and(|v| iv.iter().any(|(s, e)| overlaps(v, *s, *e)));
                    if hit
                        && !l.released.iter().any(|k| k == key)
                        && l.span_with(inc) <= MAX_LOCK_SPAN
                    {
                        ls.insert(i);
                    }
                }
            }
            let Some(&first) = ls.first() else {
                continue;
            };
            for &i in &ls {
                let (a, b) = (find(&mut root, first), find(&mut root, i));
                if a != b {
                    root[a.max(b)] = a.min(b);
                }
            }
            claimed.push((ii, first));
        }
        // Join, then set keys and grow extents.
        let mut next: Vec<Option<Lock>> = self.locks.iter().cloned().map(Some).collect();
        for i in 0..n {
            let r = find(&mut root, i);
            if r != i {
                let l = next[i].take().expect("each lock moves once");
                next[r].as_mut().expect("roots stay").absorb(l);
            }
        }
        let mut covered: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (ii, i) in claimed {
            covered.entry(find(&mut root, i)).or_default().push(ii);
        }
        let mut out = BTreeMap::new();
        for (r, list) in covered {
            let l = next[r].as_mut().expect("a root");
            let mut new_keys: BTreeSet<String> = l
                .keys
                .iter()
                .filter(|k| {
                    !present.contains(k.as_str())
                        && key_anchor(k).is_none_or(|(t, c)| !starts.contains(&(t, &c)))
                })
                .cloned()
                .collect();
            for ii in list {
                new_keys.insert(keys[ii].clone());
                l.grow(&incidents[ii]);
                out.insert(keys[ii].clone(), l.id);
            }
            l.keys = new_keys;
        }
        let next: Vec<Lock> = next.into_iter().flatten().collect();
        if next != self.locks {
            self.locks = next;
            self.revision += 1;
            if let Err(e) = self.save() {
                warn!(error = %e, "cannot store the incident locks");
            }
        }
        out
    }

    /// Per device, the days (starts of day) the locks' extents touch.
    pub fn kept_days(&self) -> HashMap<String, BTreeSet<Secs>> {
        let mut out: HashMap<String, BTreeSet<Secs>> = HashMap::new();
        for l in &self.locks {
            for (c, v) in &l.extent {
                let days = out.entry(c.device.clone()).or_default();
                for &(s, e) in v {
                    let mut d = crate::day_start(s);
                    while d <= e {
                        days.insert(d);
                        d += 86_400;
                    }
                }
            }
        }
        out
    }

    /// The locks' claims on cached thumbnails.
    pub fn pins(&self) -> Vec<crate::thumbs::Pin> {
        self.locks
            .iter()
            .map(|l| crate::thumbs::Pin {
                rank: l.id,
                ranges: l
                    .extent
                    .iter()
                    .flat_map(|(c, v)| {
                        v.iter().map(|&(start, end)| crate::thumbs::PinRange {
                            device: c.device.clone(),
                            channel: c.channel,
                            lens: c.lens,
                            start,
                            end,
                        })
                    })
                    .collect(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edits::{Edit, EditLog};
    use crate::incident::{CameraEvent, GroupingConfig, group};

    fn ev(ch: u8, kind: &str, start: Secs, end: Secs) -> CameraEvent {
        CameraEvent {
            start,
            end,
            camera: CameraId::new("nvr-a", ch),
            kind: kind.into(),
        }
    }

    /// Incidents at 1000–1030 (cameras 0 and 1), 2000–2010 (camera 0), and
    /// 5000–5020 (camera 2).
    fn events() -> Vec<CameraEvent> {
        vec![
            ev(0, "vehicle", 1000, 1015),
            ev(1, "vehicle", 1012, 1030),
            ev(0, "md", 2000, 2010),
            ev(2, "people", 5000, 5020),
        ]
    }

    fn at(v: &[Incident], start: Secs) -> &Incident {
        v.iter().find(|i| i.start() == start).unwrap()
    }

    /// The incidents after the edits, as the owner derives them.
    fn derive(log: &EditLog, evs: &[CameraEvent]) -> Vec<Incident> {
        log.apply(&group(evs, &GroupingConfig::default())).incidents
    }

    #[test]
    fn ranges_add_and_remove() {
        let mut v = Vec::new();
        assert!(add_range(&mut v, 10, 20));
        assert!(add_range(&mut v, 30, 40));
        assert!(!add_range(&mut v, 12, 18));
        assert!(add_range(&mut v, 21, 25));
        assert_eq!(v, vec![(10, 25), (30, 40)]);
        remove_range(&mut v, 15, 32);
        assert_eq!(v, vec![(10, 14), (33, 40)]);
        remove_range(&mut v, 0, 100);
        assert!(v.is_empty());
    }

    #[test]
    fn lock_unlock_and_a_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("incident-locks.json");
        let base = group(&events(), &GroupingConfig::default());
        let a = at(&base, 1000);
        let mut s = LockStore::open(&path);
        let id = s.lock(a).unwrap();
        assert_eq!(s.lock(a).unwrap(), id, "already locked: unchanged");
        assert_eq!(s.len(), 1);
        let l = &s.locks()[0];
        assert_eq!(l.extent[&CameraId::new("nvr-a", 0)], vec![(995, 1020)]);
        assert_eq!(l.extent[&CameraId::new("nvr-a", 1)], vec![(1007, 1035)]);
        assert_eq!(l.span(), Some((995, 1035)));
        s.settle(&a.key(), true).unwrap();

        let back = LockStore::open(&path);
        assert_eq!(back.locks(), s.locks());
        assert_eq!(back.lock_of(&a.key()), Some(id));
        assert!(back.locks()[0].settle.contains(&a.key()));
        let mut back = back;
        let next = back.lock(at(&base, 5000)).unwrap();
        assert!(next > id);
        assert_eq!(back.kept_days()["nvr-a"], BTreeSet::from([0]));
        assert_eq!(back.pins().len(), 2);

        assert!(back.unlock(a).unwrap());
        assert!(!back.unlock(a).unwrap());
        assert_eq!(back.len(), 1);
        assert_eq!(LockStore::open(&path).len(), 1);
        // A damaged file is set aside.
        std::fs::write(&path, b"{").unwrap();
        assert!(LockStore::open(&path).is_empty());
        assert!(dir.path().join("incident-locks.json.bad").exists());
    }

    #[test]
    fn split_parts_share_a_lock_and_unlocking_one_keeps_the_other() {
        let evs = events();
        let mut log = EditLog::in_memory();
        let mut s = LockStore::in_memory();
        let inc = derive(&log, &evs);
        let a = at(&inc, 1000).clone();
        s.lock(&a).unwrap();
        // Split: the second part has a new key and stays under the lock.
        let base = group(&evs, &GroupingConfig::default());
        let keys = log
            .push(
                &base,
                Edit::Split {
                    key: a.key(),
                    at: 1010,
                },
            )
            .unwrap();
        let inc = derive(&log, &evs);
        let locked = s.follow(&inc);
        assert_eq!(locked.len(), 2);
        assert!(keys.iter().all(|k| locked.contains_key(k)));
        assert_eq!(s.len(), 1, "edits never raise the count");
        // Unlock the second part: the first stays locked, and the second
        // is not joined again by overlap.
        let second = inc.iter().find(|i| i.key() == keys[0]).unwrap();
        assert!(s.unlock(second).unwrap());
        let locked = s.follow(&inc);
        assert_eq!(locked.keys().collect::<Vec<_>>(), vec![&a.key()]);
        // Locking it again: its range left the first lock (another
        // camera), so it gets a lock of its own.
        s.lock(second).unwrap();
        assert_eq!(s.follow(&inc).len(), 2);
        assert_eq!(s.len(), 2);
        // Unlocking both drops both.
        s.unlock(second).unwrap();
        s.unlock(inc.iter().find(|i| i.key() == a.key()).unwrap())
            .unwrap();
        assert!(s.is_empty());
        assert!(s.follow(&inc).is_empty());
    }

    #[test]
    fn merging_joins_locks_and_frees_a_slot() {
        let evs = events();
        let mut log = EditLog::in_memory();
        let mut s = LockStore::in_memory();
        let base = group(&evs, &GroupingConfig::default());
        let (a, b) = (at(&base, 1000), at(&base, 2000));
        s.lock(a).unwrap();
        s.lock(b).unwrap();
        assert_eq!(s.len(), 2);
        assert!(s.merge_fits(&[a, b]));
        log.push(
            &base,
            Edit::Merge {
                keys: vec![a.key(), b.key()],
            },
        )
        .unwrap();
        let inc = derive(&log, &evs);
        let locked = s.follow(&inc);
        assert_eq!(locked.len(), 1);
        assert_eq!(s.len(), 1);
        let l = &s.locks()[0];
        assert_eq!(l.keys, BTreeSet::from([a.key()]));
        assert_eq!(l.span(), Some((995, 2015)));
        // An unlocked incident merged into a locked one is locked.
        let c = at(&base, 5000);
        let merged = at(&inc, 1000);
        assert!(s.merge_fits(&[merged, c]));
        log.push(
            &base,
            Edit::Merge {
                keys: vec![a.key(), c.key()],
            },
        )
        .unwrap();
        let inc = derive(&log, &evs);
        assert_eq!(s.follow(&inc).len(), 1);
        assert!(s.locks()[0].extent.contains_key(&CameraId::new("nvr-a", 2)));
    }

    #[test]
    fn a_changed_key_still_overlaps_its_lock() {
        let evs = events();
        let mut s = LockStore::in_memory();
        let none = GroupingConfig::default();
        let a = at(&group(&evs, &none), 1000).clone();
        s.lock(&a).unwrap();
        let named = GroupingConfig {
            groups: vec![crate::incident::CameraGroup {
                name: "Group 1".into(),
                cameras: vec![CameraId::new("nvr-a", 0), CameraId::new("nvr-a", 1)],
            }],
            ..GroupingConfig::default()
        };
        let regrouped = group(&evs, &named);
        let new = at(&regrouped, 1000);
        assert_ne!(new.key(), a.key());
        let mut renamed = LockStore::in_memory();
        renamed.lock(&a).unwrap();
        let locked = s.follow(&regrouped);
        assert!(locked.contains_key(&new.key()));
        // The old key's earliest event is the new incident's: replaced.
        assert_eq!(s.locks()[0].keys, BTreeSet::from([new.key()]));
        assert!(s.covers_key(&new.key()) && s.covers_key(&a.key()));
        // The owner rewrites keys as the groups change, before matching.
        let n = renamed
            .rewrite_keys(|k| crate::incident::rekey(k, &none, &named))
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(renamed.lock_of(&new.key()), Some(1));
        assert!(!s.covers_key(&at(&regrouped, 5000).key()));
    }

    #[test]
    fn a_lock_whose_incidents_are_not_indexed_stays() {
        let evs = events();
        let mut s = LockStore::in_memory();
        let a = at(&group(&evs, &GroupingConfig::default()), 1000).clone();
        s.lock(&a).unwrap();
        let before = s.locks().to_vec();
        assert!(s.follow(&[]).is_empty());
        let only_later = group(&evs[2..], &GroupingConfig::default());
        assert!(s.follow(&only_later).is_empty());
        assert_eq!(s.locks(), &before[..]);
    }

    #[test]
    fn caps_and_the_span_limit() {
        let mut s = LockStore::in_memory();
        for n in 0..MAX_LOCKS as Secs {
            let inc = &group(
                &[ev(0, "md", n * 100, n * 100 + 1)],
                &GroupingConfig::default(),
            )[0];
            s.lock(inc).unwrap();
        }
        let extra = &group(&[ev(1, "md", 0, 1)], &GroupingConfig::default())[0];
        assert!(matches!(s.lock(extra), Err(LockError::Full)));
        assert_eq!(s.len(), MAX_LOCKS);
        // Joining an existing lock does not need a slot.
        let near = &group(&[ev(0, "md", 2, 3)], &GroupingConfig::default())[0];
        assert!(s.lock(near).is_ok());
        assert_eq!(s.len(), MAX_LOCKS);

        let mut s = LockStore::in_memory();
        let long = &group(
            &[ev(0, "md", 0, 10), ev(0, "md", 5, MAX_LOCK_SPAN)],
            &GroupingConfig::default(),
        )[0];
        assert!(matches!(s.lock(long), Err(LockError::TooLong)));
        let a = &group(&[ev(0, "md", 0, 10)], &GroupingConfig::default())[0];
        let far = &group(
            &[ev(0, "md", 40 * 86_400, 40 * 86_400 + 9)],
            &GroupingConfig::default(),
        )[0];
        s.lock(a).unwrap();
        assert!(s.merge_fits(&[a]));
        assert!(!s.merge_fits(&[a, far]));
        // An incident overlapping a lock that it would stretch beyond the
        // limit is not joined by overlap, and locking it makes a new lock.
        let other = &group(
            &[ev(0, "md", 5, 6), ev(0, "md", 6, MAX_LOCK_SPAN - 7)],
            &GroupingConfig::default(),
        )[0];
        assert!(s.follow(std::slice::from_ref(other)).is_empty());
        assert_eq!(s.lock(other).unwrap(), 2);
        assert_eq!(s.len(), 2);
    }
}
