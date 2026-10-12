//! Recorded video, the shared core of docs/PLAYBACK.md: everything between
//! the device session and the platform's player, encoder, and UI.
//!
//! - [`index`]: a local, bounded index per device of the days with
//!   recordings, the recording files, and the motion and detection events.
//! - [`lane`]: one search lane per device: fills that index with the days
//!   views ask for, keeps a shown today fresh, and fetches cover thumbnails
//!   first, one search at a time.
//! - [`indexer`]: whole-range index passes (recent days first, today
//!   periodically), for an optional background fill.
//! - [`incident`]: groups events from all cameras into incidents, with the
//!   user's merge, split, add, remove, and rename operations.
//! - [`edits`]: the user's edits to incidents, stored and replayed over
//!   every fresh grouping, keyed by the incidents' stable keys.
//! - [`groups`]: the user's camera groups and camera names, stored.
//! - [`locks`]: locked incidents (kept past every age and size limit),
//!   stored and matched to every fresh grouping.
//! - [`scheduler`]: downloads incidents in the background: one job per
//!   camera, fetched in chunks with command 143, retried on stalls and
//!   disconnects, resumed from the first missing chunk after any failure or
//!   restart, in parallel across devices and bounded per device.
//! - [`chunk`]: the raw chunk files a download writes (Annex-B video, ADTS
//!   audio, and a small index of timestamps), the input of the MP4 step.
//! - [`finalize`]: joins a camera job's chunks into one standard MP4
//!   (crash-safe, through a fragmented file): the scheduler's final step.
//! - [`sound`]: the loudness envelope store (peak and RMS per second, per
//!   camera and day), bounded like the index.
//! - [`thumbs`]: cover thumbnails (one keyframe per time range) in a
//!   bounded on-disk cache, capped by count and bytes.
//! - [`naming`]: the output folder and file names and `incident.json`.
//! - [`edl`]: edit decision lists (follow cut, 2-up, 4-up): pure planners
//!   over an incident and its finished files.
//! - [`export`]: export planners (trim, range download, stills, and the
//!   destination and share-safe rules they share).
//! - [`sync_samples`]: the keyframe times of a finished MP4, for the planners.
//! - [`recorded_time`]: when a recorded picture was taken, from the
//!   wall-clock seconds keyframes carry.
//!
//! Times are the device's local wall-clock time as "naive seconds" (see
//! [`bc_protocol::archive::DateTime::to_naive_secs`]): the device reports no
//! zone, so they are ordered and subtracted but never converted to UTC.
//!
//! Everything here is read-only toward the devices and free of platform
//! APIs; it builds and tests on Linux, macOS, and Windows.

pub mod audio_track;
pub mod chunk;
pub mod edits;
pub mod edl;
pub mod export;
pub mod finalize;
pub mod groups;
pub mod incident;
pub mod index;
pub mod indexer;
pub mod lane;
pub mod locks;
pub mod naming;
pub mod recorded_time;
pub mod scheduler;
pub mod sound;
/// Small file helpers (crash-safe writes, bounded JSON reads), shared with
/// the capture crate.
pub mod store;
pub mod sync_samples;
pub mod thumbs;

pub use bc_protocol::archive::DateTime;
use bc_protocol::stream::StreamTier;
use serde::{Deserialize, Serialize};

/// Device-local wall-clock seconds (see the crate documentation).
pub type Secs = i64;

/// A camera as its device knows it: a channel and, for a multi-lens
/// camera, which lens (0 first, 1 second). The index, searches, and
/// downloads of one device are keyed by this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeviceCamera {
    /// Channel.
    pub channel: u8,
    /// Lens (0 or 1).
    #[serde(default)]
    pub lens: u8,
}

impl DeviceCamera {
    /// A channel's lens.
    pub const fn new(channel: u8, lens: u8) -> DeviceCamera {
        DeviceCamera { channel, lens }
    }

    /// A channel's first lens.
    pub const fn first(channel: u8) -> DeviceCamera {
        DeviceCamera { channel, lens: 0 }
    }

    /// The stream that carries this lens at `tier` (protocol.md, "Download
    /// by time range": `streamType` alone selects lens and tier).
    pub const fn stream(self, tier: Tier) -> StreamTier {
        match (self.lens, tier) {
            (0, Tier::Main) => StreamTier::Main,
            (0, Tier::Sub) => StreamTier::Sub,
            (_, Tier::Main) => StreamTier::Extern,
            (_, Tier::Sub) => StreamTier::Mobile,
        }
    }
}

/// Quality tier of a lens, independent of the protocol's stream names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// The high-quality stream.
    Main,
    /// The low-quality stream.
    Sub,
}

/// Lens and tier of a stream: the inverse of [`DeviceCamera::stream`].
pub const fn lens_and_tier(stream: StreamTier) -> (u8, Tier) {
    match stream {
        StreamTier::Main => (0, Tier::Main),
        StreamTier::Sub => (0, Tier::Sub),
        StreamTier::Extern => (1, Tier::Main),
        StreamTier::Mobile => (1, Tier::Sub),
    }
}

/// Naive seconds of a [`DateTime`].
pub fn secs(t: DateTime) -> Secs {
    t.to_naive_secs()
}

/// The [`DateTime`] of naive seconds (clamped to the year range the
/// protocol can express).
pub fn date_time(s: Secs) -> DateTime {
    DateTime::from_naive_secs(s.clamp(0, 253_402_300_799)).unwrap_or(DateTime {
        year: 1970,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    })
}

/// Midnight at the start of the day that contains `s`.
pub fn day_start(s: Secs) -> Secs {
    s.div_euclid(86_400) * 86_400
}

/// The first and last second of the month that contains `s`.
pub fn month_span(s: Secs) -> (Secs, Secs) {
    let (first, last) = indexer::month_bounds(s);
    (secs(first), secs(last))
}

/// Supplies the device's current local time in naive seconds. Apps pass a
/// clock that applies the device's offset from UTC; tests pass a fixed one.
pub type Clock = std::sync::Arc<dyn Fn() -> Secs + Send + Sync>;

/// A clock from this computer's UTC time plus a fixed offset (seconds east
/// of UTC). The core has no time-zone database; the app supplies the offset.
pub fn system_clock(utc_offset_secs: i64) -> Clock {
    std::sync::Arc::new(move || {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        now + utc_offset_secs
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_helpers() {
        let t = DateTime::new(2026, 10, 10, 7, 42, 5).unwrap();
        assert_eq!(date_time(secs(t)), t);
        assert_eq!(date_time(day_start(secs(t))), t.start_of_day());
        assert_eq!(day_start(-1), -86_400);
        let c = system_clock(3600);
        assert!(c() > 1_700_000_000);
    }

    #[test]
    fn lens_and_tier_map_to_streams_and_back() {
        for lens in [0, 1] {
            for tier in [Tier::Main, Tier::Sub] {
                let s = DeviceCamera::new(3, lens).stream(tier);
                assert_eq!(lens_and_tier(s), (lens, tier));
            }
        }
        assert_eq!(
            DeviceCamera::new(0, 1).stream(Tier::Main),
            StreamTier::Extern
        );
        assert_eq!(DeviceCamera::first(0).stream(Tier::Sub), StreamTier::Sub);
    }
}
