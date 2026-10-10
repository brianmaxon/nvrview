//! Cover thumbnails for the timeline and the incident list, in a bounded
//! on-disk cache.
//!
//! A cover image (command 298) is one keyframe of about 260 KB from a main
//! stream. The core does not decode pictures (platform decoders do), so a
//! [`Thumbnail`] is opaque bytes plus what the app needs to use them: the
//! fetched keyframe ([`ThumbKind::H264Keyframe`], [`ThumbKind::H265Keyframe`])
//! or, once the app has decoded and shrunk it, a small image in whatever
//! format the app chose ([`ThumbKind::Image`]). Storing the small image
//! instead of the keyframe ([`ThumbnailCache::put`] replaces the entry) is
//! how thousands of thumbnails fit in a few megabytes.
//!
//! **Bounds.** The cache never holds more than
//! [`ThumbCacheConfig::max_entries`] entries or
//! [`ThumbCacheConfig::max_bytes`] bytes of files; when a put exceeds either,
//! the least recently used entries are removed (a [`get`](ThumbnailCache::get)
//! counts as use). An entry larger than the byte limit by itself is refused.
//!
//! **Store.** One file per entry under the cache directory, named by a hash
//! of the key and holding the key itself, so a hash collision is a miss and
//! never a wrong picture. Files are replaced atomically. Opening the cache
//! scans the directory: damaged files and leftovers of an interrupted write
//! are deleted, and the order of last use comes from the files' modification
//! times, which a get refreshes.
//!
//! A cache is not internally synchronized; share one behind a `Mutex`, and
//! do not hold the lock across a device request ([`cover_thumbnail`] does
//! this correctly).

use crate::store::write_atomic;
use crate::{Secs, date_time};
use bc_protocol::archive::CoverRequest;
use bc_protocol::stream::StreamTier;
use bc_session::{DeviceSession, SearchError};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;
use tracing::{debug, warn};

const MAGIC: &[u8; 4] = b"NVTH";
const VERSION: u8 = 1;
/// Fixed part of an entry file: magic, version, kind, key length, width,
/// height, timestamp, data length.
const HEADER_LEN: usize = 4 + 1 + 1 + 2 + 4 + 4 + 8 + 4;
const EXT: &str = "thumb";
/// Longest key stored (a device name is the only unbounded part).
const MAX_KEY_LEN: usize = 1024;

/// What a thumbnail's bytes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbKind {
    /// An H.264 keyframe as Annex-B NAL units, parameter sets included.
    H264Keyframe,
    /// An H.265 keyframe as Annex-B NAL units, parameter sets included.
    H265Keyframe,
    /// A small decoded image in a format the app chose (for example JPEG).
    Image,
}

impl ThumbKind {
    fn code(self) -> u8 {
        match self {
            ThumbKind::H264Keyframe => 1,
            ThumbKind::H265Keyframe => 2,
            ThumbKind::Image => 3,
        }
    }

    fn from_code(c: u8) -> Option<ThumbKind> {
        match c {
            1 => Some(ThumbKind::H264Keyframe),
            2 => Some(ThumbKind::H265Keyframe),
            3 => Some(ThumbKind::Image),
            _ => None,
        }
    }
}

/// One cached thumbnail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnail {
    /// What `data` is.
    pub kind: ThumbKind,
    /// Width in pixels (0 when unknown: take it from the keyframe's SPS).
    pub width: u32,
    /// Height in pixels (0 when unknown).
    pub height: u32,
    /// The picture's timestamp in microseconds, as the device sent it (0
    /// for a decoded image that did not keep it).
    pub pts_us: u64,
    /// The bytes.
    pub data: Vec<u8>,
}

/// What a thumbnail is of: one camera's stream over a time range.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ThumbKey {
    /// The device (any stable name the app uses for it).
    pub device: String,
    /// Channel number.
    pub channel: u8,
    /// Stream (lens and tier).
    pub stream: StreamTier,
    /// Start of the range the cover image was asked for (naive seconds).
    pub start: Secs,
    /// End of the range.
    pub end: Secs,
}

impl ThumbKey {
    fn encode(&self) -> String {
        format!(
            "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
            self.device,
            self.channel,
            self.stream.name(),
            self.start,
            self.end
        )
    }
}

/// FNV-1a, 64 bit: a stable file name for a key (the key inside the file
/// settles collisions).
fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn file_name(key: &str) -> String {
    format!("{:016x}.{EXT}", fnv64(key.as_bytes()))
}

/// Limits of a [`ThumbnailCache`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThumbCacheConfig {
    /// Most entries kept.
    pub max_entries: usize,
    /// Most bytes of entry files kept.
    pub max_bytes: u64,
}

impl Default for ThumbCacheConfig {
    fn default() -> Self {
        ThumbCacheConfig {
            max_entries: 4096,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

struct Entry {
    key: String,
    bytes: u64,
    tick: u64,
}

/// A bounded, least-recently-used cache of thumbnails on disk.
pub struct ThumbnailCache {
    dir: PathBuf,
    cfg: ThumbCacheConfig,
    /// By file name.
    entries: HashMap<String, Entry>,
    /// Recency: tick to file name; the smallest tick is evicted first.
    order: BTreeMap<u64, String>,
    bytes: u64,
    clock: u64,
}

fn encode(key: &str, t: &Thumbnail) -> Vec<u8> {
    let mut b = Vec::with_capacity(HEADER_LEN + key.len() + t.data.len());
    b.extend_from_slice(MAGIC);
    b.push(VERSION);
    b.push(t.kind.code());
    b.extend_from_slice(&(key.len() as u16).to_le_bytes());
    b.extend_from_slice(&t.width.to_le_bytes());
    b.extend_from_slice(&t.height.to_le_bytes());
    b.extend_from_slice(&t.pts_us.to_le_bytes());
    b.extend_from_slice(&(t.data.len() as u32).to_le_bytes());
    b.extend_from_slice(key.as_bytes());
    b.extend_from_slice(&t.data);
    b
}

/// Parses an entry file whole; the key and the thumbnail.
fn decode(b: &[u8]) -> Option<(String, Thumbnail)> {
    if b.len() < HEADER_LEN || &b[..4] != MAGIC || b[4] != VERSION {
        return None;
    }
    let kind = ThumbKind::from_code(b[5])?;
    let key_len = usize::from(u16::from_le_bytes([b[6], b[7]]));
    let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let (width, height) = (u32_at(8), u32_at(12));
    let pts_us = u64::from_le_bytes(b[16..24].try_into().ok()?);
    let data_len = u32_at(24) as usize;
    if key_len > MAX_KEY_LEN || b.len() != HEADER_LEN + key_len + data_len {
        return None;
    }
    let key = std::str::from_utf8(&b[HEADER_LEN..HEADER_LEN + key_len]).ok()?;
    Some((
        key.to_owned(),
        Thumbnail {
            kind,
            width,
            height,
            pts_us,
            data: b[HEADER_LEN + key_len..].to_vec(),
        },
    ))
}

fn touch(path: &Path) {
    if let Ok(f) = OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(SystemTime::now());
    }
}

impl ThumbnailCache {
    /// Opens (creating it if needed) the cache in `dir`, deleting damaged
    /// files and enforcing `cfg` on what is there (a smaller limit than an
    /// earlier run's evicts at once).
    pub fn open(dir: impl Into<PathBuf>, cfg: ThumbCacheConfig) -> io::Result<ThumbnailCache> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let mut found = Vec::new();
        for e in fs::read_dir(&dir)? {
            let e = e?;
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if !e.file_type()?.is_file() {
                continue;
            }
            let valid = if name.ends_with(&format!(".{EXT}")) {
                fs::read(&path)
                    .ok()
                    .and_then(|b| decode(&b).map(|(k, _)| (k, b.len() as u64)))
                    .filter(|(k, _)| file_name(k) == name)
            } else {
                None
            };
            match valid {
                Some((key, bytes)) => {
                    let used = e
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    found.push((used, name, key, bytes));
                }
                None => {
                    debug!(file = %name, "removing a damaged or foreign cache file");
                    let _ = fs::remove_file(&path);
                }
            }
        }
        found.sort();
        let mut c = ThumbnailCache {
            dir,
            cfg,
            entries: HashMap::new(),
            order: BTreeMap::new(),
            bytes: 0,
            clock: 0,
        };
        for (_, name, key, bytes) in found {
            c.clock += 1;
            c.bytes += bytes;
            c.order.insert(c.clock, name.clone());
            c.entries.insert(
                name,
                Entry {
                    key,
                    bytes,
                    tick: c.clock,
                },
            );
        }
        c.evict()?;
        Ok(c)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes of entry files held.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Whether `key` is cached (does not count as use).
    pub fn contains(&self, key: &ThumbKey) -> bool {
        let k = key.encode();
        self.entries.get(&file_name(&k)).is_some_and(|e| e.key == k)
    }

    /// The thumbnail of `key`, marking it recently used. A damaged file is
    /// deleted and reported as a miss.
    pub fn get(&mut self, key: &ThumbKey) -> Option<Thumbnail> {
        let k = key.encode();
        let name = file_name(&k);
        if !self.entries.get(&name).is_some_and(|e| e.key == k) {
            return None;
        }
        let path = self.dir.join(&name);
        match fs::read(&path).ok().and_then(|b| decode(&b)) {
            Some((stored, t)) if stored == k => {
                self.bump(&name);
                touch(&path);
                Some(t)
            }
            _ => {
                warn!(file = %name, "dropping a damaged cached thumbnail");
                self.remove_entry(&name);
                None
            }
        }
    }

    /// Stores `thumb` for `key`, replacing an earlier entry, then removes the
    /// least recently used entries until the limits hold. Returns `false`
    /// (and stores nothing, keeping any earlier entry) when the entry alone
    /// would exceed the byte limit or the key is too long.
    pub fn put(&mut self, key: &ThumbKey, thumb: &Thumbnail) -> io::Result<bool> {
        let k = key.encode();
        let body = encode(&k, thumb);
        if k.len() > MAX_KEY_LEN || thumb.data.len() > u32::MAX as usize {
            return Ok(false);
        }
        if body.len() as u64 > self.cfg.max_bytes || self.cfg.max_entries == 0 {
            return Ok(false);
        }
        let name = file_name(&k);
        write_atomic(&self.dir.join(&name), &body)?;
        if let Some(old) = self.entries.remove(&name) {
            self.bytes -= old.bytes;
            self.order.remove(&old.tick);
        }
        self.clock += 1;
        self.bytes += body.len() as u64;
        self.order.insert(self.clock, name.clone());
        self.entries.insert(
            name,
            Entry {
                key: k,
                bytes: body.len() as u64,
                tick: self.clock,
            },
        );
        self.evict()?;
        Ok(true)
    }

    /// Removes `key`; whether it was cached.
    pub fn remove(&mut self, key: &ThumbKey) -> bool {
        let k = key.encode();
        let name = file_name(&k);
        if self.entries.get(&name).is_some_and(|e| e.key == k) {
            self.remove_entry(&name);
            true
        } else {
            false
        }
    }

    /// Removes everything.
    pub fn clear(&mut self) {
        let names: Vec<String> = self.entries.keys().cloned().collect();
        for n in names {
            self.remove_entry(&n);
        }
    }

    fn bump(&mut self, name: &str) {
        if let Some(e) = self.entries.get_mut(name) {
            self.order.remove(&e.tick);
            self.clock += 1;
            e.tick = self.clock;
            self.order.insert(self.clock, name.to_owned());
        }
    }

    fn remove_entry(&mut self, name: &str) {
        if let Some(e) = self.entries.remove(name) {
            self.bytes -= e.bytes;
            self.order.remove(&e.tick);
        }
        let _ = fs::remove_file(self.dir.join(name));
    }

    fn evict(&mut self) -> io::Result<()> {
        while self.entries.len() > self.cfg.max_entries || self.bytes > self.cfg.max_bytes {
            let Some((_, name)) = self.order.pop_first() else {
                break;
            };
            if let Some(e) = self.entries.remove(&name) {
                self.bytes -= e.bytes;
            }
            match fs::remove_file(self.dir.join(&name)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// The cover thumbnail of `key`: from the cache, or fetched from the device
/// with a cover request (command 298) and cached as the keyframe the device
/// sent. `None` when the device has no recording in the range (nothing is
/// cached for that, so a later call asks again). The cache lock is not held
/// while the device is asked.
///
/// `uid` is the camera's device ID, needed for a second lens.
pub async fn cover_thumbnail(
    session: &DeviceSession,
    cache: &Mutex<ThumbnailCache>,
    key: &ThumbKey,
    uid: Option<String>,
) -> Result<Option<Thumbnail>, SearchError> {
    let hit = cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(key);
    if hit.is_some() {
        return Ok(hit);
    }
    let image = session
        .cover_image(&CoverRequest {
            channel: key.channel,
            uid,
            stream: key.stream,
            start: date_time(key.start),
            end: date_time(key.end),
        })
        .await?;
    let Some(image) = image else {
        return Ok(None);
    };
    let thumb = Thumbnail {
        kind: match image.codec {
            bc_protocol::media::Codec::H264 => ThumbKind::H264Keyframe,
            bc_protocol::media::Codec::H265 => ThumbKind::H265Keyframe,
        },
        width: image.width,
        height: image.height,
        pts_us: image.pts_us,
        data: image.keyframe,
    };
    if let Err(e) = cache
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .put(key, &thumb)
    {
        warn!(error = %e, "could not cache a cover thumbnail");
    }
    Ok(Some(thumb))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: i64) -> ThumbKey {
        ThumbKey {
            device: "nvr-a".into(),
            channel: 0,
            stream: StreamTier::Main,
            start: n * 60,
            end: n * 60 + 60,
        }
    }

    fn thumb(len: usize, fill: u8) -> Thumbnail {
        Thumbnail {
            kind: ThumbKind::Image,
            width: 160,
            height: 90,
            pts_us: 7,
            data: vec![fill; len],
        }
    }

    fn cache(dir: &Path, entries: usize, bytes: u64) -> ThumbnailCache {
        ThumbnailCache::open(
            dir,
            ThumbCacheConfig {
                max_entries: entries,
                max_bytes: bytes,
            },
        )
        .unwrap()
    }

    fn files(dir: &Path) -> usize {
        fs::read_dir(dir).unwrap().count()
    }

    #[test]
    fn stores_and_returns_what_was_put() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cache(dir.path(), 10, 1 << 20);
        assert!(c.get(&key(1)).is_none() && c.is_empty());
        let t = Thumbnail {
            kind: ThumbKind::H265Keyframe,
            width: 640,
            height: 360,
            pts_us: 123_456,
            data: vec![1, 2, 3, 4],
        };
        assert!(c.put(&key(1), &t).unwrap());
        assert_eq!(c.get(&key(1)), Some(t));
        assert!(c.contains(&key(1)) && !c.contains(&key(2)));
        // Another range, channel, or stream is another entry.
        let mut other = key(1);
        other.channel = 1;
        assert!(c.get(&other).is_none());
        other = key(1);
        other.stream = StreamTier::Sub;
        assert!(c.get(&other).is_none());
        // Replacing keeps one entry and re-counts its bytes.
        let before = c.bytes();
        assert!(c.put(&key(1), &thumb(1000, 9)).unwrap());
        assert_eq!(c.len(), 1);
        assert_eq!(c.bytes(), before - 4 + 1000);
        assert_eq!(c.get(&key(1)).unwrap().data.len(), 1000);
        assert!(c.remove(&key(1)) && !c.remove(&key(1)));
        assert_eq!((c.len(), c.bytes(), files(dir.path())), (0, 0, 0));
    }

    #[test]
    fn evicts_the_least_recently_used_by_count() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cache(dir.path(), 3, u64::MAX);
        for n in 1..=3 {
            c.put(&key(n), &thumb(10, n as u8)).unwrap();
        }
        // Using 1 makes 2 the oldest.
        assert!(c.get(&key(1)).is_some());
        c.put(&key(4), &thumb(10, 4)).unwrap();
        assert_eq!(c.len(), 3);
        assert!(!c.contains(&key(2)));
        assert!([1, 3, 4].iter().all(|n| c.contains(&key(*n))));
        assert_eq!(files(dir.path()), 3);
        // Replacing an entry is use too: 3 is now the oldest.
        c.put(&key(1), &thumb(11, 1)).unwrap();
        c.put(&key(5), &thumb(10, 5)).unwrap();
        assert!(!c.contains(&key(3)));
        assert!([1, 4, 5].iter().all(|n| c.contains(&key(*n))));
        assert_eq!(c.len(), 3);
    }

    #[test]
    fn evicts_until_the_byte_limit_holds() {
        let dir = tempfile::tempdir().unwrap();
        let one = encode(&key(11).encode(), &thumb(1000, 0)).len() as u64;
        let mut c = cache(dir.path(), 100, one * 3 + one / 2);
        for n in 1..=3 {
            c.put(&key(n + 10), &thumb(1000, n as u8)).unwrap();
        }
        assert_eq!((c.len(), c.bytes()), (3, one * 3));
        // One bigger entry pushes out as many old ones as it needs.
        c.put(&key(14), &thumb(2000, 4)).unwrap();
        assert!(c.bytes() <= one * 3 + one / 2);
        assert_eq!(c.len(), 2);
        assert!(c.contains(&key(14)) && c.contains(&key(13)));
        let on_disk: u64 = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert_eq!(on_disk, c.bytes());
        // An entry that cannot fit alone is refused and changes nothing.
        assert!(!c.put(&key(15), &thumb(10_000, 5)).unwrap());
        assert!(!c.contains(&key(15)) && c.len() == 2);
        // And so does a refused replacement keep the old entry.
        assert!(!c.put(&key(14), &thumb(10_000, 6)).unwrap());
        assert_eq!(c.get(&key(14)).unwrap().data.len(), 2000);
    }

    #[test]
    fn a_reopened_cache_keeps_entries_order_and_limits() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut c = cache(dir.path(), 10, u64::MAX);
            for n in 1..=4 {
                c.put(&key(n), &thumb(10, n as u8)).unwrap();
                // File times have coarse resolution on some systems.
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(c.get(&key(1)).is_some()); // 1 is now the newest
        }
        let mut c = cache(dir.path(), 10, u64::MAX);
        assert_eq!(c.len(), 4);
        assert_eq!(c.get(&key(3)).unwrap().data, vec![3; 10]);
        drop(c);
        // A smaller limit at the next start evicts the oldest first: 2 and 4
        // go (3 was just used, 1 before it).
        let c = cache(dir.path(), 2, u64::MAX);
        assert_eq!(c.len(), 2);
        assert!(c.contains(&key(1)) && c.contains(&key(3)));
        assert_eq!(files(dir.path()), 2);
    }

    #[test]
    fn damaged_and_foreign_files_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cache(dir.path(), 10, u64::MAX);
        c.put(&key(1), &thumb(50, 1)).unwrap();
        c.put(&key(2), &thumb(50, 2)).unwrap();
        let bad = dir.path().join(file_name(&key(2).encode()));
        drop(c);
        fs::write(dir.path().join("junk.thumb"), b"not a thumbnail").unwrap();
        fs::write(dir.path().join("0123456789abcdef.thumb.tmp"), b"half").unwrap();
        let mut c = cache(dir.path(), 10, u64::MAX);
        assert_eq!(c.len(), 2);
        assert_eq!(files(dir.path()), 2);
        // Damage after opening: the read notices, drops it, and misses.
        let mut b = fs::read(&bad).unwrap();
        b.truncate(b.len() - 1);
        fs::write(&bad, b).unwrap();
        assert!(c.get(&key(2)).is_none());
        assert!(!c.contains(&key(2)) && !bad.exists());
        assert_eq!(c.len(), 1);
        assert!(c.get(&key(1)).is_some());
    }

    #[test]
    fn a_colliding_name_never_returns_another_key() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cache(dir.path(), 10, u64::MAX);
        c.put(&key(1), &thumb(10, 1)).unwrap();
        // Pose key 2's entry under key 1's file name, as a collision would.
        let name = file_name(&key(1).encode());
        c.entries.get_mut(&name).unwrap().key = key(2).encode();
        assert!(c.get(&key(1)).is_none());
        assert!(!c.contains(&key(2)));
    }

    #[test]
    fn clear_empties_the_cache_and_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cache(dir.path(), 10, u64::MAX);
        for n in 1..=3 {
            c.put(&key(n), &thumb(10, 1)).unwrap();
        }
        c.clear();
        assert_eq!((c.len(), c.bytes(), files(dir.path())), (0, 0, 0));
    }
}
