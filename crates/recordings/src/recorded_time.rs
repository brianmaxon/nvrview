//! When a recorded picture was taken (docs/MILESTONE-P.md, slice C7).
//!
//! A video record's timestamp is in microseconds on a clock of the camera's
//! own, not wall-clock time. Keyframes also carry the device's wall-clock
//! seconds in their extra header (protocol.md, "Sub and main of one
//! camera": observed on live view, every keyframe, whole seconds). So each
//! keyframe says: at this timestamp the wall clock read `s`, that is, it
//! was somewhere in `[s, s + 1)` seconds. One constant offset links the two
//! clocks, and every keyframe bounds it to a one-second window;
//! intersecting the windows of many keyframes narrows it (about 15 ms from
//! two minutes of a main stream, protocol.md). [`refine`] does that,
//! starting over when a keyframe does not fit (the clock jumped, or the
//! recording was cut).
//!
//! **Open (seam).** Whether those seconds count the device's local wall
//! time (the time the index and incidents use) or UTC has not been
//! established: the first keyframe of a replay matched the play's
//! `seekTime`, a local time, but the capture does not say in which frame of
//! reference that comparison was made. Times here therefore stay on the
//! keyframe clock (`wall_*`); converting them to device-local time is left
//! to the playback and edit-list slices once a decryptable capture settles
//! it (docs/MILESTONE-P.md, "The re-capture").

use crate::chunk::{self, VideoEntry};
use std::io;
use std::path::Path;

/// The offset from a stream's timestamps to the keyframe wall clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    /// Lowest offset consistent with every keyframe (µs): wall time =
    /// pts + offset.
    pub lo_us: i64,
    /// Highest offset (exclusive).
    pub hi_us: i64,
    /// Keyframes that agreed.
    pub keyframes: u32,
}

impl Anchor {
    /// The middle of the window: the best single estimate.
    pub fn offset_us(&self) -> i64 {
        self.lo_us + (self.hi_us - self.lo_us) / 2
    }

    /// Half the window: how far the estimate can be off.
    pub fn uncertainty_us(&self) -> i64 {
        (self.hi_us - self.lo_us + 1) / 2
    }

    /// The wall time (µs on the keyframe clock) of a picture's timestamp.
    pub fn wall_us(&self, pts_us: u64) -> i64 {
        pts_us as i64 + self.offset_us()
    }
}

/// Narrows the offset over keyframes `(pts_us, wall_secs)`, in stream
/// order. A keyframe that does not fit the window so far starts a new
/// window (the newest one wins). `None` without any keyframe.
pub fn refine(keyframes: impl IntoIterator<Item = (u64, u32)>) -> Option<Anchor> {
    let mut a: Option<Anchor> = None;
    for (pts, s) in keyframes {
        let lo = i64::from(s) * 1_000_000 - pts as i64;
        let hi = lo + 1_000_000;
        a = Some(match a {
            Some(w) if lo.max(w.lo_us) < hi.min(w.hi_us) => Anchor {
                lo_us: lo.max(w.lo_us),
                hi_us: hi.min(w.hi_us),
                keyframes: w.keyframes + 1,
            },
            _ => Anchor {
                lo_us: lo,
                hi_us: hi,
                keyframes: 1,
            },
        });
    }
    a
}

fn keyframes(v: &[VideoEntry]) -> impl Iterator<Item = (u64, u32)> + '_ {
    v.iter()
        .filter(|e| e.key)
        .filter_map(|e| Some((e.pts_us, e.wall_secs?)))
}

/// The wall time (ms on the keyframe clock) of the first picture of a
/// camera job's chunks `0..chunks` in `dir`, with the anchor refined over
/// all of them. `Ok(None)` when no chunk has a keyframe with wall-clock
/// seconds. Blocking (reads the chunk indexes).
pub fn first_picture_wall_ms(dir: &Path, chunks: u32) -> io::Result<Option<i64>> {
    let mut first: Option<u64> = None;
    let mut all: Vec<(u64, u32)> = Vec::new();
    for n in 0..chunks {
        let Some(idx) = chunk::read_index(dir, n)? else {
            continue;
        };
        if first.is_none() {
            first = idx.video.first().map(|v| v.pts_us);
        }
        all.extend(keyframes(&idx.video));
    }
    Ok(match (first, refine(all)) {
        (Some(pts), Some(a)) => Some(a.wall_us(pts).div_euclid(1000)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyframes_narrow_the_offset() {
        // True offset: wall = pts + 5_000_250_000 µs. Keyframes every
        // 1.92 s; the seconds are the truncated wall time.
        let truth: i64 = 5_000_250_000;
        let kf: Vec<(u64, u32)> = (0..60u64)
            .map(|i| {
                let pts = 1_000_000 + i * 1_920_000;
                (pts, ((pts as i64 + truth) / 1_000_000) as u32)
            })
            .collect();
        let one = refine(kf[..1].iter().copied()).unwrap();
        assert_eq!(one.hi_us - one.lo_us, 1_000_000);
        let a = refine(kf.iter().copied()).unwrap();
        assert_eq!(a.keyframes, 60);
        assert!(a.lo_us <= truth && truth < a.hi_us);
        assert!(a.uncertainty_us() <= 40_000, "{a:?}");
        assert!((a.offset_us() - truth).abs() <= a.uncertainty_us());
        assert_eq!(a.wall_us(0), a.offset_us());
    }

    #[test]
    fn a_jump_starts_a_new_window() {
        let a = refine([
            (0, 100),
            (1_000_000, 101),
            (2_000_000, 500),
            (3_000_000, 501),
        ])
        .unwrap();
        assert_eq!(a.keyframes, 2);
        assert!(a.lo_us >= 498_000_000);
        assert!(refine([]).is_none());
    }

    #[test]
    fn first_picture_of_a_job() {
        let dir = tempfile::tempdir().unwrap();
        // No chunks: nothing.
        assert_eq!(first_picture_wall_ms(dir.path(), 2).unwrap(), None);
        for n in 0..2u32 {
            let mut w = chunk::ChunkWriter::create(dir.path(), n, 0, 10).unwrap();
            for i in 0..4u64 {
                let pts = u64::from(n) * 4_000_000 + i * 1_000_000;
                let key = i % 2 == 0;
                let extra = (((pts + 7_000_300_000) / 1_000_000) as u32).to_le_bytes();
                let mut extra = extra.to_vec();
                extra.extend([0; 4]);
                let rec = bc_protocol::media::Record::Video {
                    codec: bc_protocol::media::Codec::H264,
                    keyframe: key,
                    pts_us: pts,
                    data: vec![0, 0, 0, 1, if key { 0x65 } else { 0x41 }],
                    extra_header: if key { extra } else { vec![] },
                };
                w.write(&rec).unwrap();
            }
            w.finish().unwrap();
        }
        let ms = first_picture_wall_ms(dir.path(), 2).unwrap().unwrap();
        // True: 7_000_300 ms at pts 0, within the refined window.
        assert!((ms - 7_000_300).abs() <= 300, "{ms}");
    }
}
