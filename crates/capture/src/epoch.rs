//! The recording's epoch timeline: every source's timestamps mapped onto
//! one continuous clock, the **file clock** (microseconds, 0 at the first
//! video frame the timeline sees).
//!
//! Three sources feed it:
//!
//! - **video**, on the camera's clock (`push_video`), which drives the
//!   timeline;
//! - **camera audio**, on the same camera clock (`audio_file_us`), placed
//!   against the epoch the video is in;
//! - **sent audio** (our talk-back), on the host clock (`sent_file_us`),
//!   mapped through the median offset between host arrival time and file
//!   time of the recent video, the "presentation map" of
//!   docs/CAPTURE.md ("The user's side").
//!
//! # Rules
//!
//! - **Epochs.** An epoch maps one run of the camera clock onto the file
//!   clock with a fixed offset. A camera timestamp that jumps forward by
//!   more than `max_forward_us`, or back by more than `max_backward_us`
//!   (a small step back is B-frame or arrival reordering and stays in the
//!   epoch), starts a new epoch. The new epoch starts at the previous
//!   frame's file time plus the **gap measured on the host clock** (at
//!   least `min_step_us`), so the file clock never goes backwards and a
//!   long outage appears as a gap of the right size.
//! - **Wrap.** Camera timestamps wrap at `wrap_modulus` (2^32 microseconds
//!   by default, about 71.6 minutes; 0 disables wrap handling). A step is
//!   read as the shortest signed distance modulo the wrap, so a wrap is not
//!   a discontinuity and the unwrapped clock keeps counting.
//! - **Late joins.** Audio that maps to a file time before the start of
//!   the segment is dropped by the segment policy (file time 0 of a
//!   segment is its first keyframe); audio that arrives before the first
//!   video frame cannot be placed and is dropped and counted.
//! - **Audio placement.** Camera audio is read relative to the latest video
//!   timestamp (nearest wrap representation). If it is more than
//!   `audio_skew_us` away it belongs to a clock the video has not reached
//!   (or already left) and is dropped and counted (`audio_unplaced`). It
//!   is mapped with the newest epoch if it is at most one second before
//!   that epoch's first frame, else with an older epoch that contains it.
//! - **Sample accuracy.** [`AudioPlacer`] turns a chunk's file time into a
//!   sample position at the track's rate, snaps chunks that are within
//!   `tolerance` of contiguous, reports the silence before a later chunk,
//!   and trims overlap, so an audio track's sample positions are exact and
//!   never drift by rounding.

use std::collections::VecDeque;

/// Microseconds per second.
pub const MICROS: i64 = 1_000_000;

/// Epochs kept for placing late audio.
const KEPT_EPOCHS: usize = 4;

/// Timeline settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineConfig {
    /// Camera timestamps wrap at this value (0: never).
    pub wrap_modulus: u64,
    /// A forward step beyond this starts a new epoch.
    pub max_forward_us: i64,
    /// A backward step beyond this starts a new epoch.
    pub max_backward_us: i64,
    /// Camera audio further than this from the latest video is unplaced.
    pub audio_skew_us: i64,
    /// Least file-clock advance across an epoch change.
    pub min_step_us: i64,
    /// Video frames the host-to-file median looks at.
    pub offset_window: usize,
}

impl Default for TimelineConfig {
    fn default() -> Self {
        TimelineConfig {
            wrap_modulus: 1 << 32,
            max_forward_us: 5 * MICROS,
            max_backward_us: MICROS,
            audio_skew_us: 10 * MICROS,
            min_step_us: 1_000,
            offset_window: 64,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Epoch {
    camera_start: i64,
    camera_last: i64,
    file_start: i64,
}

/// Where a video frame landed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoPos {
    /// The frame's time on the file clock.
    pub file_us: i64,
    /// True if this frame started a new epoch.
    pub new_epoch: bool,
}

/// Counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimelineStats {
    /// Epochs started after the first.
    pub epoch_changes: u32,
    /// Camera audio chunks that could not be placed.
    pub audio_unplaced: u32,
}

/// The timeline. See the module documentation.
#[derive(Clone, Debug)]
pub struct Timeline {
    cfg: TimelineConfig,
    epochs: VecDeque<Epoch>,
    last_raw: u64,
    last_u: i64,
    last_host: i64,
    last_file: i64,
    started: bool,
    offsets: VecDeque<i64>,
    stats: TimelineStats,
}

impl Timeline {
    /// An empty timeline.
    pub fn new(cfg: TimelineConfig) -> Timeline {
        Timeline {
            cfg,
            epochs: VecDeque::new(),
            last_raw: 0,
            last_u: 0,
            last_host: 0,
            last_file: 0,
            started: false,
            offsets: VecDeque::new(),
            stats: TimelineStats::default(),
        }
    }

    /// Counters.
    pub fn stats(&self) -> TimelineStats {
        self.stats
    }

    /// File time of the latest video frame (0 before any).
    pub fn last_file_us(&self) -> i64 {
        self.last_file
    }

    /// The shortest signed distance from `from` to `to` on the camera clock.
    fn delta(&self, to: u64, from: u64) -> i64 {
        let m = self.cfg.wrap_modulus;
        if m == 0 {
            return to as i64 - from as i64;
        }
        let (to, from) = (to % m, from % m);
        let d = (to + m - from) % m;
        if d > m / 2 {
            d as i64 - m as i64
        } else {
            d as i64
        }
    }

    /// Adds a video frame with camera timestamp `raw_ts` that arrived at
    /// `host_us` (any steady host clock, microseconds).
    pub fn push_video(&mut self, raw_ts: u64, host_us: i64) -> VideoPos {
        let mut new_epoch = false;
        let u;
        if !self.started {
            self.started = true;
            u = raw_ts as i64;
            self.epochs.push_back(Epoch {
                camera_start: u,
                camera_last: u,
                file_start: 0,
            });
        } else {
            let d = self.delta(raw_ts, self.last_raw);
            if (-self.cfg.max_backward_us..=self.cfg.max_forward_us).contains(&d) {
                u = self.last_u + d;
            } else {
                u = raw_ts as i64;
                new_epoch = true;
                self.stats.epoch_changes += 1;
                let host_gap = (host_us - self.last_host).max(self.cfg.min_step_us);
                self.epochs.push_back(Epoch {
                    camera_start: u,
                    camera_last: u,
                    file_start: self.last_file + host_gap,
                });
                while self.epochs.len() > KEPT_EPOCHS {
                    self.epochs.pop_front();
                }
            }
        }
        let e = self.epochs.back_mut().expect("an epoch exists");
        e.camera_last = e.camera_last.max(u);
        let file_us = e.file_start + (u - e.camera_start);
        self.last_raw = raw_ts;
        self.last_u = u;
        self.last_host = host_us;
        self.last_file = file_us;
        self.offsets.push_back(host_us - file_us);
        while self.offsets.len() > self.cfg.offset_window.max(1) {
            self.offsets.pop_front();
        }
        VideoPos { file_us, new_epoch }
    }

    /// Maps camera audio with camera timestamp `raw_ts` (its first sample)
    /// to the file clock, or `None` if it cannot be placed.
    pub fn audio_file_us(&mut self, raw_ts: u64) -> Option<i64> {
        if !self.started {
            self.stats.audio_unplaced += 1;
            return None;
        }
        let d = self.delta(raw_ts, self.last_raw);
        if d.abs() > self.cfg.audio_skew_us {
            self.stats.audio_unplaced += 1;
            return None;
        }
        let u = self.last_u + d;
        let newest = self.epochs.len() - 1;
        for (i, e) in self.epochs.iter().enumerate().rev() {
            let ok = if i == newest {
                u >= e.camera_start - MICROS
            } else {
                u >= e.camera_start && u <= e.camera_last + MICROS
            };
            if ok {
                return Some(e.file_start + (u - e.camera_start));
            }
        }
        self.stats.audio_unplaced += 1;
        None
    }

    /// Maps a host instant (when we sent audio) to the file clock with the
    /// median host-minus-file offset of the recent video; `None` before any
    /// video.
    pub fn sent_file_us(&self, host_us: i64) -> Option<i64> {
        if self.offsets.is_empty() {
            return None;
        }
        let mut v: Vec<i64> = self.offsets.iter().copied().collect();
        v.sort_unstable();
        Some(host_us - v[v.len() / 2])
    }
}

/// Where an audio chunk goes on its track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Sample position (at the track's rate, 0 at the segment start) of the
    /// first sample kept.
    pub pos: i64,
    /// Samples to drop from the front of the chunk (overlap with what was
    /// already placed, or time before the segment start).
    pub skip: usize,
    /// Samples of silence between the end of the previous chunk and `pos`.
    pub gap_before: u64,
}

/// Turns chunk times into exact sample positions on one track.
#[derive(Clone, Debug)]
pub struct AudioPlacer {
    rate: u32,
    tolerance: i64,
    next: i64,
    placed: bool,
}

impl AudioPlacer {
    /// A placer for a track at `rate` Hz; chunks within `tolerance_us` of
    /// contiguous are snapped to contiguous (20 ms is a good value).
    pub fn new(rate: u32, tolerance_us: i64) -> AudioPlacer {
        AudioPlacer {
            rate,
            tolerance: (tolerance_us * i64::from(rate) / MICROS).max(0),
            next: 0,
            placed: false,
        }
    }

    /// The sample position the next contiguous chunk would have.
    pub fn next_pos(&self) -> i64 {
        self.next
    }

    /// Places a chunk of `frames` samples whose first sample is at
    /// `rel_us` microseconds after the segment start (negative: before
    /// it). Returns `None` when nothing of it is kept.
    pub fn place(&mut self, rel_us: i64, frames: usize) -> Option<Placement> {
        let rate = i128::from(self.rate);
        let micros = i128::from(MICROS);
        // Round to the nearest sample, ties away from zero.
        let n = i128::from(rel_us) * rate;
        let pos0 = if n >= 0 {
            (n + micros / 2) / micros
        } else {
            -((-n + micros / 2) / micros)
        } as i64;
        let (pos, skip, gap_before);
        if self.placed && (pos0 - self.next).abs() <= self.tolerance {
            pos = self.next;
            skip = 0;
            gap_before = 0;
        } else if pos0 >= self.next {
            pos = pos0;
            skip = 0;
            gap_before = (pos0 - self.next) as u64;
        } else {
            pos = self.next;
            skip = (self.next - pos0) as usize;
            gap_before = 0;
        }
        if skip >= frames {
            return None;
        }
        self.placed = true;
        self.next = pos + (frames - skip) as i64;
        Some(Placement {
            pos,
            skip,
            gap_before,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tl() -> Timeline {
        Timeline::new(TimelineConfig::default())
    }

    #[test]
    fn video_is_continuous_within_an_epoch() {
        let mut t = tl();
        let p0 = t.push_video(5_000_000, 100);
        assert_eq!(
            p0,
            VideoPos {
                file_us: 0,
                new_epoch: false
            }
        );
        assert_eq!(t.push_video(5_040_000, 40_100).file_us, 40_000);
        // A small step back (reordering) stays in the epoch.
        let p = t.push_video(5_030_000, 50_000);
        assert_eq!(p.file_us, 30_000);
        assert!(!p.new_epoch);
        assert_eq!(t.stats().epoch_changes, 0);
    }

    #[test]
    fn a_timestamp_wrap_is_not_a_discontinuity() {
        let mut t = tl();
        let m = 1u64 << 32;
        t.push_video(m - 40_000, 0);
        let p = t.push_video(m - 1, 40_000);
        assert_eq!(p.file_us, 39_999);
        let p = t.push_video(40_000, 80_000);
        assert!(!p.new_epoch);
        assert_eq!(p.file_us, 80_000);
        // And again an hour later: still counting.
        let p = t.push_video(80_000, 120_000);
        assert_eq!(p.file_us, 120_000);
        assert_eq!(t.stats().epoch_changes, 0);
    }

    #[test]
    fn wrap_handling_can_be_off() {
        let mut t = Timeline::new(TimelineConfig {
            wrap_modulus: 0,
            ..TimelineConfig::default()
        });
        t.push_video(5_000_000, 0);
        // Backwards by more than a second: an epoch change.
        assert!(t.push_video(10, 40_000).new_epoch);
    }

    #[test]
    fn a_jump_starts_an_epoch_offset_by_the_host_gap() {
        let mut t = tl();
        t.push_video(1_000_000, 1_000);
        t.push_video(1_040_000, 41_000); // file 40_000
        // The camera restarted its clock; 8 s passed on the host.
        let p = t.push_video(500, 8_041_000);
        assert!(p.new_epoch);
        assert_eq!(p.file_us, 40_000 + 8_000_000);
        assert_eq!(t.push_video(40_500, 8_081_000).file_us, 8_080_000);
        assert_eq!(t.stats().epoch_changes, 1);
    }

    #[test]
    fn a_forward_jump_without_a_host_gap_still_moves_forward() {
        let mut t = tl();
        t.push_video(0, 0);
        t.push_video(40_000, 40_000);
        // Host spacing 40 ms but the camera jumped an hour.
        let p = t.push_video(3_600_000_000, 80_000);
        assert!(p.new_epoch);
        assert_eq!(p.file_us, 80_000);
        // Host going backwards cannot move the file clock back.
        let p = t.push_video(7_200_000_000, 0);
        assert!(p.new_epoch);
        assert_eq!(p.file_us, 81_000);
    }

    #[test]
    fn camera_audio_follows_the_video_epoch() {
        let mut t = tl();
        // Audio before any video cannot be placed (a late join).
        assert_eq!(t.audio_file_us(10), None);
        t.push_video(1_000_000, 0);
        t.push_video(1_040_000, 40_000);
        // Slightly before the first frame: negative file time.
        assert_eq!(t.audio_file_us(990_000), Some(-10_000));
        assert_eq!(t.audio_file_us(1_050_000), Some(50_000));
        // Far from the video: another clock.
        assert_eq!(t.audio_file_us(90_000_000), None);
        assert_eq!(t.stats().audio_unplaced, 2);
    }

    #[test]
    fn late_audio_of_the_previous_epoch_keeps_its_epoch() {
        let mut t = tl();
        t.push_video(1_000_000, 0);
        t.push_video(1_040_000, 40_000);
        t.push_video(3_000_000_000, 5_040_000); // new epoch, file 5.04 s
        // Old-epoch audio nearest to the new video is skewed: unplaced.
        assert_eq!(t.audio_file_us(1_030_000), None);
        // A jump of 3 s forward in the same neighborhood keeps both maps.
        let mut t = tl();
        t.push_video(1_000_000, 0);
        t.push_video(1_040_000, 40_000);
        t.push_video(7_000_000, 6_040_000); // forward 5.96 s: new epoch
        assert_eq!(t.audio_file_us(7_020_000), Some(6_060_000));
        // Old-epoch audio within a second of old camera_last is the old map.
        assert_eq!(t.audio_file_us(1_020_000), Some(20_000));
    }

    #[test]
    fn camera_audio_across_a_wrap() {
        let mut t = tl();
        let m = 1u64 << 32;
        t.push_video(m - 20_000, 0);
        t.push_video(20_000, 40_000);
        assert_eq!(t.audio_file_us(m - 10_000), Some(10_000));
        assert_eq!(t.audio_file_us(30_000), Some(50_000));
    }

    #[test]
    fn sent_audio_maps_through_the_median_offset() {
        let mut t = tl();
        assert_eq!(t.sent_file_us(5), None);
        // Host is file + 700 ms, with arrival jitter on a few frames.
        for i in 0..40i64 {
            let jitter = if i % 7 == 0 { 90_000 } else { 0 };
            t.push_video((i * 40_000) as u64, i * 40_000 + 700_000 + jitter);
        }
        let f = t.sent_file_us(2_000_000).unwrap();
        assert_eq!(f, 1_300_000);
    }

    #[test]
    fn the_median_window_is_bounded() {
        let mut t = Timeline::new(TimelineConfig {
            offset_window: 4,
            ..TimelineConfig::default()
        });
        for i in 0..100i64 {
            t.push_video((i * 40_000) as u64, i * 40_000 + 10);
        }
        assert!(t.offsets.len() <= 4);
    }

    #[test]
    fn placer_snaps_contiguous_chunks_and_reports_gaps_and_overlap() {
        // 16 kHz, 20 ms tolerance = 320 samples.
        let mut p = AudioPlacer::new(16_000, 20_000);
        // First chunk at 0.5 s: 8000 samples of silence precede it.
        let a = p.place(500_000, 1024).unwrap();
        assert_eq!(
            a,
            Placement {
                pos: 8000,
                skip: 0,
                gap_before: 8000
            }
        );
        // Next chunk 5 ms "late" by jitter: snapped, no gap.
        let b = p.place(500_000 + 64_000 + 5_000, 1024).unwrap();
        assert_eq!(b.pos, 9024);
        assert_eq!(b.gap_before, 0);
        // A chunk 100 ms later than contiguous: 1600 samples of silence.
        let c = p.place(500_000 + 128_000 + 100_000, 1024).unwrap();
        assert_eq!(c.pos, 8000 + 2048 + 1600);
        assert_eq!(c.gap_before, 1600);
        // A chunk that overlaps by 50 ms: front trimmed.
        let end = p.next_pos();
        let d = p.place(end * 1_000_000 / 16_000 - 50_000, 1024).unwrap();
        assert_eq!(d.pos, end);
        assert_eq!(d.skip, 800);
        // A chunk entirely in the past is dropped.
        assert_eq!(p.place(0, 100), None);
    }

    #[test]
    fn placer_trims_audio_before_the_segment_start() {
        let mut p = AudioPlacer::new(48_000, 20_000);
        // Starts 10 ms (480 samples) before the start.
        let a = p.place(-10_000, 1024).unwrap();
        assert_eq!(
            a,
            Placement {
                pos: 0,
                skip: 480,
                gap_before: 0
            }
        );
        assert_eq!(p.next_pos(), 544);
        // Entirely before the start: nothing.
        let mut p = AudioPlacer::new(48_000, 20_000);
        assert_eq!(p.place(-100_000, 1024), None);
    }

    #[test]
    fn placer_positions_are_exact_over_a_long_run() {
        // 1024-sample chunks at 48 kHz stamped with their exact times, with
        // microsecond rounding: the position never drifts.
        let mut p = AudioPlacer::new(48_000, 20_000);
        for n in 0..10_000i64 {
            let t = n * 1024 * 1_000_000 / 48_000;
            let pl = p.place(t, 1024).unwrap();
            assert_eq!(pl.pos, n * 1024);
            assert_eq!(pl.gap_before, 0);
        }
    }
}
