//! Live pacing (docs/RENDERING.md, "Live pacing"): a live stream's pictures
//! are handed to the decoder at the pace of their own timestamps, not in
//! the groups the network delivers them in.
//!
//! Devices were seen to deliver live pictures unevenly: gaps of two or
//! three frame intervals, then the pictures of the gap together. Decoded on
//! arrival, a group lands within one display refresh, and a renderer that
//! shows the newest picture per refresh shows only its last one; a
//! 15 fps stream then looks like 7. The pacer holds each picture until its
//! timestamp, on the arrival clock, plus a small delay:
//!
//! - **Base.** `arrival - pts` of the picture that came earliest relative
//!   to its timestamp over the last few seconds: the device's clock on the
//!   host's, as it would be without network delay. A drifting device clock
//!   moves it with the window.
//! - **Delay.** How much later than the base the pictures of that window
//!   came, at the 90th percentile, capped at [`MAX_DELAY_US`]: enough that
//!   most pictures are on hand when their time comes, never much latency.
//!   An evenly delivered stream gets (almost) none.
//! - **Order.** Everything queued after a picture (stream info, the next
//!   pictures) waits for it, so the decoder sees the stream in order; a
//!   picture whose time has passed goes at once.
//! - **Breaks.** A timestamp that goes backwards or jumps by more than
//!   [`JUMP_US`] (a restarted stream, a gap) starts the estimate over and
//!   releases what is held; so does a discontinuity, which the caller
//!   passes through after [`LivePacer::flush`].
//! - **Bounds.** At most [`MAX_HELD`] items are held; beyond that the
//!   oldest is released at once.
//!
//! Pure: the caller supplies the clock (µs) and asks for what is due.

use bc_protocol::media::Record;
use std::collections::VecDeque;

/// The most latency the pacer adds.
pub(crate) const MAX_DELAY_US: u64 = 200_000;
/// Timestamp jumps larger than this start the estimate over.
pub(crate) const JUMP_US: u64 = 2_000_000;
/// Items held at most.
pub(crate) const MAX_HELD: usize = 64;
/// The window the base and the delay are estimated over.
const WINDOW_US: u64 = 4_000_000;
/// Samples kept for the estimate (a few seconds at 30 fps or more).
const MAX_SAMPLES: usize = 160;

/// See the module comment.
#[derive(Debug, Default)]
pub(crate) struct LivePacer {
    /// Held records and when each is due (host µs).
    held: VecDeque<(u64, Record)>,
    /// Recent pictures: (arrival µs, arrival - pts µs).
    samples: VecDeque<(u64, i64)>,
    last_pts: Option<u64>,
    /// Due time of the newest held item (keeps the order).
    last_due: u64,
    /// The delay last applied (for reports and tests), µs.
    delay_us: u64,
}

impl LivePacer {
    /// Takes a record that arrived at `now`. Audio is not held (the caller
    /// handles it at once).
    pub(crate) fn push(&mut self, now: u64, record: Record) {
        let due = match &record {
            Record::Video { pts_us, .. } => self.picture_due(now, *pts_us),
            _ => now,
        };
        let due = due.max(self.last_due);
        self.last_due = due;
        if self.held.len() >= MAX_HELD {
            // Released at once (with everything before it).
            if let Some((_, r)) = self.held.pop_front() {
                self.held.push_front((0, r));
            }
        }
        self.held.push_back((due, record));
    }

    fn picture_due(&mut self, now: u64, pts: u64) -> u64 {
        if self
            .last_pts
            .is_some_and(|last| pts < last || pts - last > JUMP_US)
        {
            self.restart_estimate();
            for item in &mut self.held {
                item.0 = 0;
            }
            self.last_due = 0;
        }
        self.last_pts = Some(pts);
        let offset = now as i64 - pts as i64;
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.saturating_sub(*at) > WINDOW_US)
            || self.samples.len() >= MAX_SAMPLES
        {
            self.samples.pop_front();
        }
        self.samples.push_back((now, offset));
        let base = self.samples.iter().map(|(_, o)| *o).min().unwrap_or(offset);
        let mut late: Vec<u64> = self
            .samples
            .iter()
            .map(|(_, o)| (*o - base) as u64)
            .collect();
        late.sort_unstable();
        let rank = (late.len() * 9).div_ceil(10).max(1) - 1;
        self.delay_us = late[rank].min(MAX_DELAY_US);
        let due = pts as i64 + base + self.delay_us as i64;
        due.max(0) as u64
    }

    fn restart_estimate(&mut self) {
        self.samples.clear();
        self.delay_us = 0;
    }

    /// The oldest held record if it is due at `now`.
    pub(crate) fn pop_due(&mut self, now: u64) -> Option<Record> {
        if self.held.front().is_some_and(|(due, _)| *due <= now) {
            self.held.pop_front().map(|(_, r)| r)
        } else {
            None
        }
    }

    /// When the next held record is due, if any is held.
    pub(crate) fn next_due(&self) -> Option<u64> {
        self.held.front().map(|(due, _)| *due)
    }

    /// Everything held, in order (a discontinuity, a closing tile), and the
    /// estimate starts over.
    pub(crate) fn flush(&mut self) -> impl Iterator<Item = Record> + '_ {
        self.restart_estimate();
        self.last_pts = None;
        self.last_due = 0;
        self.held.drain(..).map(|(_, r)| r)
    }

    /// The delay applied to the newest picture, µs.
    #[cfg(test)]
    pub(crate) fn delay_us(&self) -> u64 {
        self.delay_us
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bc_protocol::media::Codec;

    fn video(pts_us: u64) -> Record {
        Record::Video {
            codec: Codec::H264,
            keyframe: false,
            pts_us,
            data: vec![0],
            extra_header: Vec::new(),
        }
    }

    fn pts_of(r: &Record) -> u64 {
        match r {
            Record::Video { pts_us, .. } => *pts_us,
            _ => u64::MAX,
        }
    }

    /// Feeds pictures every `frame` µs, delivered in groups of `group`
    /// (each group when its last picture exists), and returns the release
    /// times, polling every millisecond.
    fn run(frame: u64, group: u64, count: u64) -> (Vec<(u64, u64)>, LivePacer) {
        let mut p = LivePacer::default();
        let mut out = Vec::new();
        let mut next = 0;
        let end = frame * (count + group + 10);
        let mut t = 0;
        while t < end {
            while next < count && t >= (next / group * group + group - 1) * frame + 5_000 {
                p.push(t, video(1_000_000 + next * frame));
                next += 1;
            }
            while let Some(r) = p.pop_due(t) {
                out.push((t, pts_of(&r)));
            }
            t += 1_000;
        }
        (out, p)
    }

    #[test]
    fn an_even_stream_passes_with_no_delay() {
        let (out, p) = run(66_667, 1, 60);
        assert_eq!(out.len(), 60);
        // At most the test's 1 ms polling grid.
        assert!(p.delay_us() <= 1_000, "{}", p.delay_us());
        for w in out.windows(2) {
            let wall = w[1].0 as i64 - w[0].0 as i64;
            let media = w[1].1 as i64 - w[0].1 as i64;
            assert!((wall - media).abs() <= 1_000, "{wall} {media}");
        }
    }

    #[test]
    fn pictures_delivered_in_pairs_are_released_at_their_own_times() {
        let (out, p) = run(66_667, 2, 120);
        assert_eq!(out.len(), 120);
        // One frame interval of delay covers the pairs.
        assert!(
            (60_000..=70_000).contains(&p.delay_us()),
            "{}",
            p.delay_us()
        );
        // After the first few pictures (the estimate settles), every
        // picture is released at its own spacing.
        for w in out[6..].windows(2) {
            let wall = w[1].0 as i64 - w[0].0 as i64;
            let media = w[1].1 as i64 - w[0].1 as i64;
            assert!((wall - media).abs() <= 2_000, "{wall} {media}");
        }
    }

    #[test]
    fn the_delay_is_capped_and_order_is_kept() {
        let mut p = LivePacer::default();
        // A picture very late relative to the first: capped delay.
        p.push(0, video(0));
        p.push(1_000_000, video(100_000));
        assert_eq!(p.delay_us(), MAX_DELAY_US);
        p.push(
            1_000_000,
            Record::Info {
                width: 1,
                height: 1,
                fps: 1,
            },
        );
        let mut got = Vec::new();
        let mut t = 0;
        while got.len() < 3 && t < 5_000_000 {
            while let Some(r) = p.pop_due(t) {
                got.push(pts_of(&r));
            }
            t += 1_000;
        }
        assert_eq!(got, vec![0, 100_000, u64::MAX]);
    }

    #[test]
    fn a_jump_releases_what_is_held_and_starts_over() {
        let mut p = LivePacer::default();
        p.push(0, video(10_000_000));
        p.push(10_000, video(10_066_667));
        assert_eq!(p.pop_due(10_000).map(|r| pts_of(&r)), Some(10_000_000));
        // The second is held until later...
        assert!(p.next_due().unwrap() > 10_000);
        // ...but a restarted stream releases it and starts over.
        p.push(20_000, video(5_000));
        assert_eq!(p.pop_due(20_000).map(|r| pts_of(&r)), Some(10_066_667));
        assert_eq!(p.pop_due(20_000).map(|r| pts_of(&r)), Some(5_000));
        assert_eq!(p.delay_us(), 0);
        p.push(30_000, video(15_000));
        assert_eq!(p.flush().count(), 1);
        assert_eq!(p.next_due(), None);
    }
}
