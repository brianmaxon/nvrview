//! One camera's buffer of received play media, ahead of the shared clock,
//! and the recorded time of each picture.
//!
//! **Recorded time.** A picture's timestamp is on the camera's own clock;
//! keyframes carry the device's wall-clock seconds in UTC (observed,
//! docs/protocol.md, "Rates, keyframes, and how a play ends"). Every
//! keyframe bounds the offset between the two clocks to a one-second
//! window, and the windows of recent keyframes are intersected
//! ([`recordings::recorded_time::refine`], slice C7), which narrows the
//! anchor as the play goes on. Adding the device's UTC offset gives device
//! local time, the clock of the index, the seek, and the playback position.
//! A stream without those seconds is anchored at its seek time instead.
//!
//! **The device's offset.** The app passes the computer's UTC offset, which
//! need not be the device's (a device set to another zone, or one that does
//! not follow daylight saving time). A play's first keyframe lies within
//! seconds of its seek time (observed, docs/protocol.md), so a first keyframe
//! of a fresh run that is half a quarter hour or more away from the seek in
//! local time shows the offset is wrong: the offset is then taken from the
//! keyframe, rounded to a quarter hour, and kept. Without this every picture
//! would be hours before (or after) its target, released as fast as the
//! decoder answers as each group of pictures arrives (or never), and
//! playback stutters.
//!
//! **Bounds.** The reader that fills the buffer stops its play once the
//! buffer holds more than its limit of media time or bytes
//! ([`Push::Full`]); the engine starts the play again from the newest
//! picture held once the buffer has drained to half
//! ([`CamBuf::low_water`]). A restarted play's pictures up to the newest
//! one held are dropped, so nothing is shown twice.

use bc_protocol::media::{Record, wall_clock_secs};
use recordings::recorded_time::{Anchor, refine};
use std::collections::VecDeque;

/// UTC offsets come in quarter hours.
const OFFSET_STEP_S: i64 = 900;

/// Keyframes kept for refining the anchor.
const ANCHOR_KEYFRAMES: usize = 64;

/// One received record.
#[derive(Debug)]
pub(crate) struct Entry {
    pub(crate) record: Record,
    bytes: usize,
}

impl Entry {
    /// The picture's timestamp (video only).
    pub(crate) fn pts(&self) -> Option<u64> {
        match &self.record {
            Record::Video { pts_us, .. } => Some(*pts_us),
            _ => None,
        }
    }

    pub(crate) fn keyframe(&self) -> bool {
        matches!(self.record, Record::Video { keyframe: true, .. })
    }
}

/// What a push did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Push {
    /// Kept (or dropped by the run's own rules).
    Kept,
    /// Kept, and the buffer is over its limit: stop the play.
    Full,
    /// From a play that was replaced: discard it, stop the play.
    Stale,
}

/// A camera's buffer (shared by its play's reader and the engine).
#[derive(Debug)]
pub(crate) struct CamBuf {
    /// The play run whose media is accepted.
    run: u64,
    entries: VecDeque<Entry>,
    bytes: usize,
    keyframes: VecDeque<(u64, u32)>,
    anchor: Option<Anchor>,
    /// Device-local µs = pts + this, for a stream without wall-clock
    /// seconds.
    fallback_us: Option<i64>,
    utc_offset_s: i64,
    /// The offset was checked against this run's first keyframe.
    offset_checked: bool,
    /// The run's seek time (device-local ms), for the fallback.
    seek_ms: i64,
    /// Newest picture received (pts).
    newest_pts: Option<u64>,
    /// Pictures of the run up to this timestamp were held already.
    skip_through: Option<u64>,
    /// Waiting for the run's first keyframe.
    gated: bool,
    /// Something arrived in this run.
    pub(crate) got_media: bool,
    /// Limits.
    max_ms: i64,
    max_bytes: usize,
}

impl CamBuf {
    pub(crate) fn new(utc_offset_s: i64, max_ms: i64, max_bytes: usize) -> CamBuf {
        CamBuf {
            run: 0,
            entries: VecDeque::new(),
            bytes: 0,
            keyframes: VecDeque::new(),
            anchor: None,
            fallback_us: None,
            utc_offset_s,
            offset_checked: false,
            seek_ms: 0,
            newest_pts: None,
            skip_through: None,
            gated: true,
            got_media: false,
            max_ms,
            max_bytes,
        }
    }

    /// Starts a new run that replaces what is held (a seek, a restart of
    /// the group): everything is dropped.
    pub(crate) fn restart(&mut self, run: u64, seek_ms: i64) {
        self.entries.clear();
        self.bytes = 0;
        self.newest_pts = None;
        self.skip_through = None;
        self.offset_checked = false;
        self.begin(run, seek_ms);
    }

    /// Starts a new run that continues what is held (a refill after the
    /// buffer was full, or a camera whose recording resumes): pictures up
    /// to the newest one held are skipped.
    pub(crate) fn resume(&mut self, run: u64, seek_ms: i64) {
        self.skip_through = self.newest_pts;
        self.begin(run, seek_ms);
    }

    fn begin(&mut self, run: u64, seek_ms: i64) {
        self.run = run;
        self.seek_ms = seek_ms;
        self.gated = true;
        self.got_media = false;
    }

    /// Adds a record of play run `run`.
    pub(crate) fn push(&mut self, run: u64, record: Record) -> Push {
        if run != self.run {
            return Push::Stale;
        }
        self.got_media = true;
        match &record {
            Record::Video {
                keyframe,
                pts_us,
                extra_header,
                ..
            } => {
                if self.gated && !keyframe {
                    return Push::Kept;
                }
                if self.skip_through.is_some_and(|s| *pts_us <= s) {
                    // Held already; what follows continues it.
                    self.gated = false;
                    return Push::Kept;
                }
                if *keyframe {
                    self.gated = false;
                    if let Some(s) = wall_clock_secs(extra_header) {
                        self.check_offset(s);
                        if self.keyframes.len() == ANCHOR_KEYFRAMES {
                            self.keyframes.pop_front();
                        }
                        self.keyframes.push_back((*pts_us, s));
                        self.anchor = refine(self.keyframes.iter().copied());
                    }
                }
                if self.anchor.is_none() && self.fallback_us.is_none() {
                    self.fallback_us = Some(self.seek_ms * 1000 - *pts_us as i64);
                }
                self.newest_pts = Some(*pts_us);
            }
            Record::Audio { .. } if self.gated => return Push::Kept,
            Record::Audio { .. } | Record::Info { .. } => {}
        }
        let bytes = match &record {
            Record::Video { data, .. } => data.len(),
            Record::Audio { data, .. } => data.len(),
            Record::Info { .. } => 32,
        };
        self.bytes += bytes;
        self.entries.push_back(Entry { record, bytes });
        if self.over(self.max_ms, self.max_bytes) {
            Push::Full
        } else {
            Push::Kept
        }
    }

    /// Checks the offset against a fresh run's first keyframe (UTC seconds
    /// `s`; see the module comment).
    fn check_offset(&mut self, s: u32) {
        if std::mem::replace(&mut self.offset_checked, true) {
            return;
        }
        let seen = self.seek_ms.div_euclid(1000) - i64::from(s);
        if (seen - self.utc_offset_s).abs() >= OFFSET_STEP_S / 2 {
            let offset = (seen as f64 / OFFSET_STEP_S as f64).round() as i64 * OFFSET_STEP_S;
            tracing::warn!(
                given = self.utc_offset_s,
                seen = offset,
                "the device's UTC offset differs from the one given; using the device's"
            );
            self.utc_offset_s = offset;
        }
    }

    /// The device's UTC offset in use (seconds east).
    #[cfg(test)]
    fn utc_offset_s(&self) -> i64 {
        self.utc_offset_s
    }

    fn over(&self, ms: i64, bytes: usize) -> bool {
        self.bytes > bytes || self.span_ms().is_some_and(|s| s > ms)
    }

    /// Media time held: newest picture less the oldest one held.
    fn span_ms(&self) -> Option<i64> {
        let oldest = self.entries.iter().find_map(Entry::pts)?;
        Some(self.recorded_ms(self.newest_pts?)? - self.recorded_ms(oldest)?)
    }

    /// Whether a stopped play may start again: at most half the limits
    /// held.
    pub(crate) fn low_water(&self) -> bool {
        !self.over(self.max_ms / 2, self.max_bytes / 2)
    }

    /// Device-local ms of a picture's timestamp, once anchored.
    pub(crate) fn recorded_ms(&self, pts: u64) -> Option<i64> {
        if let Some(a) = &self.anchor {
            return Some(a.wall_us(pts).div_euclid(1000) + self.utc_offset_s * 1000);
        }
        self.fallback_us.map(|f| (pts as i64 + f).div_euclid(1000))
    }

    /// Device-local ms of the newest picture received.
    pub(crate) fn horizon_ms(&self) -> Option<i64> {
        self.recorded_ms(self.newest_pts?)
    }

    /// Device-local ms of the oldest picture held.
    pub(crate) fn next_video_ms(&self) -> Option<i64> {
        self.recorded_ms(self.entries.iter().find_map(Entry::pts)?)
    }

    /// The oldest record held.
    pub(crate) fn front(&self) -> Option<&Entry> {
        self.entries.front()
    }

    /// Takes the oldest record.
    pub(crate) fn pop(&mut self) -> Option<Entry> {
        let e = self.entries.pop_front()?;
        self.bytes -= e.bytes;
        Some(e)
    }

    /// Changes the media time held before the play is stopped (it scales
    /// with the rate: at 16x a play bursts tens of media seconds).
    pub(crate) fn set_max_ms(&mut self, max_ms: i64) {
        self.max_ms = max_ms;
    }

    /// Drops everything held (the play goes on).
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// Whether nothing is held.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bc_protocol::media::Codec;

    /// A picture at device-local second `local_s` plus `ms`, on a device one
    /// hour west of UTC, with timestamps equal to local µs.
    fn video(local_s: i64, ms: i64, keyframe: bool, len: usize) -> Record {
        let pts = (local_s * 1000 + ms) as u64 * 1000;
        let mut extra = vec![0u8; 8];
        extra[..4].copy_from_slice(&((local_s + 3600) as u32).to_le_bytes());
        Record::Video {
            codec: Codec::H264,
            keyframe,
            pts_us: pts,
            data: vec![0; len],
            extra_header: if keyframe { extra } else { Vec::new() },
        }
    }

    #[test]
    fn recorded_time_is_local_and_bounds_stop_the_play() {
        let mut b = CamBuf::new(-3600, 4_000, 1 << 20);
        b.restart(1, 1_000_000);
        // Before the first keyframe nothing is kept.
        assert_eq!(b.push(1, video(1000, 900, false, 10)), Push::Kept);
        assert!(b.is_empty());
        assert_eq!(b.push(1, video(1001, 0, true, 10)), Push::Kept);
        // The anchor puts the keyframe within its second (the window's
        // middle) in local time.
        let t = b.recorded_ms((1_001_000) * 1000).unwrap();
        assert!((1_001_000..1_002_000).contains(&t), "{t}");
        for i in 1..40 {
            let r = b.push(1, video(1001, i * 100, false, 10));
            assert_eq!(r, Push::Kept);
        }
        // More than 4 s held: full.
        assert_eq!(b.push(1, video(1005, 100, true, 10)), Push::Full);
        assert!(!b.low_water());
        // An old run's media is stale.
        assert_eq!(b.push(0, video(1006, 0, true, 10)), Push::Stale);
        // Releasing drains it.
        while b.front().is_some_and(|e| e.pts().unwrap() < 1_004_000_000) {
            b.pop();
        }
        assert!(b.low_water());
        // A resumed run skips what was held already.
        b.resume(2, 1_005_000);
        let n = b.entries.len();
        assert_eq!(b.push(2, video(1005, 0, true, 10)), Push::Kept);
        assert_eq!(b.entries.len(), n);
        assert_eq!(b.push(2, video(1005, 200, false, 10)), Push::Kept);
        assert_eq!(b.entries.len(), n + 1);
    }

    #[test]
    fn a_wrong_offset_is_corrected_from_the_first_keyframe() {
        // Given the computer's offset (four hours west) for a device one
        // hour west; the first keyframe is a second before the seek.
        let mut b = CamBuf::new(-4 * 3600, 10_000, 1 << 20);
        b.restart(1, 1_001 * 1000);
        b.push(1, video(1000, 0, true, 10));
        assert_eq!(b.utc_offset_s(), -3600);
        let t = b.recorded_ms(1_000_000 * 1000).unwrap();
        assert!((1_000_000..1_001_000).contains(&t), "{t}");
        // Kept for later runs; a right offset is left alone.
        b.restart(2, 2_000 * 1000);
        b.push(2, video(1998, 0, true, 10));
        assert_eq!(b.utc_offset_s(), -3600);
        let mut ok = CamBuf::new(-3600, 10_000, 1 << 20);
        ok.restart(1, 1_005 * 1000);
        ok.push(1, video(1000, 0, true, 10));
        assert_eq!(ok.utc_offset_s(), -3600);
    }

    #[test]
    fn a_stream_without_seconds_is_anchored_at_its_seek() {
        let mut b = CamBuf::new(0, 10_000, 1 << 20);
        b.restart(1, 500_000);
        let r = Record::Video {
            codec: Codec::H264,
            keyframe: true,
            pts_us: 7_000_000,
            data: vec![0; 4],
            extra_header: Vec::new(),
        };
        b.push(1, r);
        assert_eq!(b.recorded_ms(7_000_000), Some(500_000));
        assert_eq!(b.recorded_ms(7_500_000), Some(500_500));
        assert_eq!(b.horizon_ms(), Some(500_000));
    }
}
