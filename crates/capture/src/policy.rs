//! The segment and gap policy: for each video frame, whether to drop it,
//! start a file, write it, cut to a new segment, or stop the recording.
//!
//! Pure: the caller passes the frame's file time (from the
//! [`Timeline`](crate::epoch::Timeline)), the host time, and the bytes
//! written so far, and acts on the answer.
//!
//! # Rules
//!
//! - **Keyframe-gated starts.** Nothing is written until a keyframe. File
//!   time 0 of a segment is its first keyframe; the answer carries that
//!   frame's file time as the segment's `origin_us`, and every later
//!   position is `file_us - origin_us`. Non-key frames before the start are
//!   dropped (state `waiting_keyframe`). With no keyframe for
//!   `keyframe_timeout_us` of host time the recording ends with `no_video`.
//! - **Gaps.** A gap is a video file-time step larger than `gap_floor_us`
//!   (2 s; frames are never that far apart in a live stream). Up to
//!   `max_gap_us` (60 s) the file continues, the last picture holds
//!   through the gap, and the gap is counted. Over that, the segment ends
//!   (`long_gap`) and the next keyframe starts the next part.
//! - **Format changes.** A frame whose [`VideoFormat`] (codec, size,
//!   parameter sets) differs from the segment's ends the segment
//!   (`format_changed`); the frame starts the next part when it is a
//!   keyframe, else the policy waits for one.
//! - **Silence from the stream.** With no frame for `max_gap_us` of host
//!   time ([`SegmentPolicy::tick`]) the segment is ended (`long_gap`) so it
//!   is finalized and playable while the stream is down; the next keyframe
//!   starts the next part.
//! - **Caps (no roll-over).** The duration cap counts file time from the
//!   first segment's origin (gaps included, so a recording across
//!   reconnects stops at the same wall length); the size cap counts the
//!   bytes written by the whole recording across its segments. Either one
//!   ends the recording (`max_duration`, `max_size`) before the frame that
//!   would pass it is written.
//! - **Audio** is placed relative to the segment origin
//!   ([`SegmentPolicy::audio_rel_us`]) and only while a segment is open;
//!   audio before the keyframe is dropped.

use crate::caps::Limits;
use crate::reason::Reason;

/// Video codecs the recorder muxes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    /// H.264.
    H264,
    /// H.265.
    H265,
}

/// What must stay equal within one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoFormat {
    /// The codec.
    pub codec: Codec,
    /// Picture width.
    pub width: u32,
    /// Picture height.
    pub height: u32,
    /// A hash of the parameter sets (VPS/SPS/PPS), computed by the caller.
    pub param_hash: u64,
}

/// Policy settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PolicyConfig {
    /// Duration and size caps.
    pub limits: Limits,
    /// Longest gap the file continues across.
    pub max_gap_us: i64,
    /// Smallest video step that counts as a gap.
    pub gap_floor_us: i64,
    /// Host time to wait for the first keyframe of a segment.
    pub keyframe_timeout_us: i64,
}

impl PolicyConfig {
    /// The contract's values around the given caps.
    pub fn new(limits: Limits) -> PolicyConfig {
        PolicyConfig {
            limits,
            max_gap_us: 60_000_000,
            gap_floor_us: 2_000_000,
            keyframe_timeout_us: 30_000_000,
        }
    }
}

/// One video frame as the policy sees it.
#[derive(Clone, Copy, Debug)]
pub struct VideoSample {
    /// File time (from the timeline).
    pub file_us: i64,
    /// Host arrival time.
    pub host_us: i64,
    /// A keyframe.
    pub keyframe: bool,
    /// The stream's current format.
    pub format: VideoFormat,
    /// Bytes the recording has written so far, all segments.
    pub total_bytes: u64,
    /// Size of this frame.
    pub frame_bytes: u64,
}

/// What to do with a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoDecision {
    /// Not a keyframe and no segment is open: discard.
    Drop,
    /// Open segment `segment` (1 for the first) with this frame as its
    /// first; positions are relative to `origin_us`.
    Start {
        /// Segment number, 1-based.
        segment: u32,
        /// File time of the keyframe.
        origin_us: i64,
    },
    /// Write the frame at `rel_us` into the open segment. `gap_us` is set
    /// when this frame follows a counted gap of that length.
    Write {
        /// Position in the segment.
        rel_us: i64,
        /// The gap just crossed, if any.
        gap_us: Option<i64>,
    },
    /// Finalize the open segment for `reason` (`format_changed` or
    /// `long_gap`). If `next` is set the frame is a keyframe and opens the
    /// next segment at once; otherwise the policy waits for a keyframe.
    Cut {
        /// Why.
        reason: Reason,
        /// The next segment, when this frame starts it.
        next: Option<(u32, i64)>,
    },
    /// Finalize and end the recording; the frame is not written.
    Stop(Reason),
}

/// The policy's state, as the event reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyState {
    /// Waiting for a keyframe (to start, or to start the next part).
    WaitingKeyframe,
    /// A segment is open.
    Recording,
    /// The recording ended.
    Finished(Reason),
}

/// Counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PolicyStats {
    /// Gaps the file continued across.
    pub gaps: u32,
    /// Segments opened.
    pub segments: u32,
}

/// The policy. See the module documentation.
#[derive(Clone, Debug)]
pub struct SegmentPolicy {
    cfg: PolicyConfig,
    state: PolicyState,
    wait_since_host: i64,
    segment: u32,
    first_origin: Option<i64>,
    origin: i64,
    format: Option<VideoFormat>,
    last_file: i64,
    last_host: i64,
    stats: PolicyStats,
}

impl SegmentPolicy {
    /// A policy for a recording started at host time `now_us`.
    pub fn new(cfg: PolicyConfig, now_us: i64) -> SegmentPolicy {
        SegmentPolicy {
            cfg,
            state: PolicyState::WaitingKeyframe,
            wait_since_host: now_us,
            segment: 0,
            first_origin: None,
            origin: 0,
            format: None,
            last_file: 0,
            last_host: now_us,
            stats: PolicyStats::default(),
        }
    }

    /// The state.
    pub fn state(&self) -> PolicyState {
        self.state
    }

    /// Counters.
    pub fn stats(&self) -> PolicyStats {
        self.stats
    }

    /// The open segment's number (0 before the first).
    pub fn segment(&self) -> u32 {
        self.segment
    }

    /// Media duration so far: file time from the first origin to the last
    /// frame.
    pub fn duration_us(&self) -> i64 {
        self.first_origin.map_or(0, |o| (self.last_file - o).max(0))
    }

    fn open(&mut self, f: &VideoSample) -> (u32, i64) {
        self.segment += 1;
        self.stats.segments += 1;
        self.origin = f.file_us;
        self.first_origin.get_or_insert(f.file_us);
        self.format = Some(f.format);
        self.last_file = f.file_us;
        self.last_host = f.host_us;
        self.state = PolicyState::Recording;
        (self.segment, f.file_us)
    }

    fn wait(&mut self, host_us: i64) {
        self.state = PolicyState::WaitingKeyframe;
        self.wait_since_host = host_us;
    }

    fn finish(&mut self, r: Reason) -> VideoDecision {
        self.state = PolicyState::Finished(r);
        VideoDecision::Stop(r)
    }

    /// Decides what to do with a video frame.
    pub fn on_video(&mut self, f: VideoSample) -> VideoDecision {
        match self.state {
            PolicyState::Finished(r) => VideoDecision::Stop(r),
            PolicyState::WaitingKeyframe => {
                if !f.keyframe {
                    if f.host_us - self.wait_since_host > self.cfg.keyframe_timeout_us {
                        return self.finish(Reason::NoVideo);
                    }
                    return VideoDecision::Drop;
                }
                if let Some(r) = self.cap_hit(&f, f.file_us) {
                    return self.finish(r);
                }
                let (segment, origin_us) = self.open(&f);
                VideoDecision::Start { segment, origin_us }
            }
            PolicyState::Recording => {
                if self.format != Some(f.format) {
                    return self.cut(Reason::FormatChanged, f);
                }
                let step = f.file_us - self.last_file;
                if step > self.cfg.max_gap_us {
                    return self.cut(Reason::LongGap, f);
                }
                if let Some(r) = self.cap_hit(&f, f.file_us) {
                    return self.finish(r);
                }
                let gap_us = (step > self.cfg.gap_floor_us).then_some(step);
                if gap_us.is_some() {
                    self.stats.gaps += 1;
                }
                self.last_file = f.file_us;
                self.last_host = f.host_us;
                VideoDecision::Write {
                    rel_us: f.file_us - self.origin,
                    gap_us,
                }
            }
        }
    }

    fn cut(&mut self, reason: Reason, f: VideoSample) -> VideoDecision {
        if f.keyframe && self.cap_hit(&f, f.file_us).is_none() {
            let next = self.open(&f);
            VideoDecision::Cut {
                reason,
                next: Some(next),
            }
        } else {
            self.wait(f.host_us);
            VideoDecision::Cut { reason, next: None }
        }
    }

    fn cap_hit(&self, f: &VideoSample, file_us: i64) -> Option<Reason> {
        let l = &self.cfg.limits;
        let start = self.first_origin.unwrap_or(file_us);
        if file_us - start >= l.max_duration_us {
            return Some(Reason::MaxDuration);
        }
        if f.total_bytes.saturating_add(f.frame_bytes) > l.max_bytes {
            return Some(Reason::MaxSize);
        }
        None
    }

    /// Time passing with no frame. Ends the open segment after
    /// `max_gap_us` of silence, or the recording after
    /// `keyframe_timeout_us` with nothing to start from.
    pub fn tick(&mut self, now_us: i64) -> Option<VideoDecision> {
        match self.state {
            PolicyState::Recording if now_us - self.last_host > self.cfg.max_gap_us => {
                self.wait(now_us);
                Some(VideoDecision::Cut {
                    reason: Reason::LongGap,
                    next: None,
                })
            }
            PolicyState::WaitingKeyframe
                if now_us - self.wait_since_host > self.cfg.keyframe_timeout_us
                    && self.first_origin.is_none() =>
            {
                Some(self.finish(Reason::NoVideo))
            }
            _ => None,
        }
    }

    /// Ends the recording for `reason` (a stop, an error, a shutdown).
    pub fn stop(&mut self, reason: Reason) {
        if !matches!(self.state, PolicyState::Finished(_)) {
            self.state = PolicyState::Finished(reason);
        }
    }

    /// Position of an audio chunk (file time `file_us`) in the open
    /// segment, or `None` when no segment is open. Negative positions are
    /// before the keyframe (the placer trims them).
    pub fn audio_rel_us(&self, file_us: i64) -> Option<i64> {
        matches!(self.state, PolicyState::Recording).then(|| file_us - self.origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000_000;

    fn fmt(w: u32) -> VideoFormat {
        VideoFormat {
            codec: Codec::H264,
            width: w,
            height: 1080,
            param_hash: 7,
        }
    }

    fn cfg() -> PolicyConfig {
        PolicyConfig::new(Limits::live(3600 * S, 4_000_000_000))
    }

    fn frame(file_us: i64, key: bool) -> VideoSample {
        VideoSample {
            file_us,
            host_us: file_us,
            keyframe: key,
            format: fmt(1920),
            total_bytes: 0,
            frame_bytes: 1000,
        }
    }

    #[test]
    fn nothing_is_written_before_a_keyframe() {
        let mut p = SegmentPolicy::new(cfg(), 0);
        assert_eq!(p.on_video(frame(0, false)), VideoDecision::Drop);
        assert_eq!(p.state(), PolicyState::WaitingKeyframe);
        assert_eq!(
            p.on_video(frame(S, true)),
            VideoDecision::Start {
                segment: 1,
                origin_us: S
            }
        );
        assert_eq!(
            p.on_video(frame(S + 40_000, false)),
            VideoDecision::Write {
                rel_us: 40_000,
                gap_us: None
            }
        );
        // Audio is relative to the keyframe; earlier audio is negative.
        assert_eq!(p.audio_rel_us(S - 5_000), Some(-5_000));
        assert_eq!(p.audio_rel_us(S + 1_000), Some(1_000));
    }

    #[test]
    fn audio_is_dropped_while_no_segment_is_open() {
        let p = SegmentPolicy::new(cfg(), 0);
        assert_eq!(p.audio_rel_us(0), None);
    }

    #[test]
    fn no_keyframe_in_time_is_no_video() {
        let mut p = SegmentPolicy::new(cfg(), 0);
        assert_eq!(p.tick(29 * S), None);
        assert_eq!(p.tick(31 * S), Some(VideoDecision::Stop(Reason::NoVideo)));
        assert_eq!(p.state(), PolicyState::Finished(Reason::NoVideo));
        // Also through frames that never include a keyframe.
        let mut p = SegmentPolicy::new(cfg(), 0);
        assert_eq!(
            p.on_video(frame(31 * S, false)),
            VideoDecision::Stop(Reason::NoVideo)
        );
    }

    #[test]
    fn small_gaps_continue_and_are_counted() {
        let mut p = SegmentPolicy::new(cfg(), 0);
        p.on_video(frame(0, true));
        p.on_video(frame(40_000, false));
        // 1.5 s step is under the floor: not a gap.
        assert_eq!(
            p.on_video(frame(1_540_000, false)),
            VideoDecision::Write {
                rel_us: 1_540_000,
                gap_us: None
            }
        );
        // 10 s: a counted gap, the file continues.
        let d = p.on_video(frame(11_540_000, true));
        assert_eq!(
            d,
            VideoDecision::Write {
                rel_us: 11_540_000,
                gap_us: Some(10 * S)
            }
        );
        // Exactly the limit continues.
        let d = p.on_video(frame(11_540_000 + 60 * S, false));
        assert!(matches!(
            d,
            VideoDecision::Write {
                gap_us: Some(_),
                ..
            }
        ));
        assert_eq!(p.stats().gaps, 2);
        assert_eq!(p.segment(), 1);
    }

    #[test]
    fn a_long_gap_cuts_and_the_keyframe_opens_the_next_part() {
        let mut p = SegmentPolicy::new(cfg(), 0);
        p.on_video(frame(0, true));
        let d = p.on_video(frame(61 * S, true));
        assert_eq!(
            d,
            VideoDecision::Cut {
                reason: Reason::LongGap,
                next: Some((2, 61 * S))
            }
        );
        // The new part's positions restart at its keyframe.
        assert_eq!(
            p.on_video(frame(61 * S + 40_000, false)),
            VideoDecision::Write {
                rel_us: 40_000,
                gap_us: None
            }
        );
        // A non-key frame after a long gap waits for a keyframe.
        let mut p = SegmentPolicy::new(cfg(), 0);
        p.on_video(frame(0, true));
        assert_eq!(
            p.on_video(frame(100 * S, false)),
            VideoDecision::Cut {
                reason: Reason::LongGap,
                next: None
            }
        );
        assert_eq!(p.state(), PolicyState::WaitingKeyframe);
        assert_eq!(p.audio_rel_us(100 * S), None);
        assert_eq!(
            p.on_video(frame(100 * S + 40_000, false)),
            VideoDecision::Drop
        );
        assert_eq!(
            p.on_video(frame(101 * S, true)),
            VideoDecision::Start {
                segment: 2,
                origin_us: 101 * S
            }
        );
    }

    #[test]
    fn a_format_change_cuts_even_without_a_gap() {
        let mut p = SegmentPolicy::new(cfg(), 0);
        p.on_video(frame(0, true));
        let mut f = frame(40_000, true);
        f.format = fmt(2560);
        assert_eq!(
            p.on_video(f),
            VideoDecision::Cut {
                reason: Reason::FormatChanged,
                next: Some((2, 40_000))
            }
        );
        // Changed parameter sets alone are a change too.
        let mut f = frame(80_000, true);
        f.format = VideoFormat {
            param_hash: 8,
            ..fmt(2560)
        };
        assert!(matches!(
            p.on_video(f),
            VideoDecision::Cut {
                reason: Reason::FormatChanged,
                next: Some((3, _))
            }
        ));
        // A codec change on a non-key frame waits for the keyframe.
        let mut f = frame(120_000, false);
        f.format = VideoFormat {
            codec: Codec::H265,
            ..f.format
        };
        assert!(matches!(
            p.on_video(f),
            VideoDecision::Cut {
                reason: Reason::FormatChanged,
                next: None
            }
        ));
    }

    #[test]
    fn silence_from_the_stream_ends_the_segment() {
        let mut p = SegmentPolicy::new(cfg(), 0);
        p.on_video(frame(0, true));
        assert_eq!(p.tick(60 * S), None);
        assert_eq!(
            p.tick(61 * S),
            Some(VideoDecision::Cut {
                reason: Reason::LongGap,
                next: None
            })
        );
        assert_eq!(p.state(), PolicyState::WaitingKeyframe);
        // After the first segment, waiting longer does not end the recording.
        assert_eq!(p.tick(500 * S), None);
    }

    #[test]
    fn the_duration_cap_stops_before_writing_the_frame() {
        let mut p = SegmentPolicy::new(PolicyConfig::new(Limits::exact(60 * S, 1 << 40)), 0);
        p.on_video(frame(0, true));
        assert!(matches!(
            p.on_video(frame(59 * S, false)),
            VideoDecision::Write { .. }
        ));
        assert_eq!(
            p.on_video(frame(60 * S, false)),
            VideoDecision::Stop(Reason::MaxDuration)
        );
        assert_eq!(p.state(), PolicyState::Finished(Reason::MaxDuration));
        // Stays stopped.
        assert_eq!(
            p.on_video(frame(61 * S, true)),
            VideoDecision::Stop(Reason::MaxDuration)
        );
    }

    #[test]
    fn the_duration_cap_counts_across_segments() {
        let mut p = SegmentPolicy::new(PolicyConfig::new(Limits::exact(100 * S, 1 << 40)), 0);
        p.on_video(frame(0, true));
        // Long gap to 80 s, new part; 100 s of recording counted from 0.
        assert!(matches!(
            p.on_video(frame(80 * S, true)),
            VideoDecision::Cut { next: Some(_), .. }
        ));
        assert!(matches!(
            p.on_video(frame(99 * S, false)),
            VideoDecision::Write { .. }
        ));
        assert_eq!(
            p.on_video(frame(100 * S, false)),
            VideoDecision::Stop(Reason::MaxDuration)
        );
        assert_eq!(p.duration_us(), 99 * S);
    }

    #[test]
    fn the_size_cap_counts_all_segments_and_stops_before_the_frame() {
        let mut p = SegmentPolicy::new(PolicyConfig::new(Limits::exact(3600 * S, 10_000)), 0);
        let mut f = frame(0, true);
        f.total_bytes = 5_000;
        assert!(matches!(p.on_video(f), VideoDecision::Start { .. }));
        let mut f = frame(40_000, false);
        f.total_bytes = 9_000;
        assert!(matches!(p.on_video(f), VideoDecision::Write { .. })); // 9000 + 1000 == cap
        let mut f = frame(80_000, false);
        f.total_bytes = 9_001;
        assert_eq!(p.on_video(f), VideoDecision::Stop(Reason::MaxSize));
    }

    #[test]
    fn a_cap_already_reached_prevents_the_next_part() {
        let mut p = SegmentPolicy::new(PolicyConfig::new(Limits::exact(3600 * S, 10_000)), 0);
        p.on_video(frame(0, true));
        let mut f = frame(40_000, true);
        f.format = fmt(2560);
        f.total_bytes = 9_500;
        // The cut happens, but the next part is not opened over the cap...
        assert_eq!(
            p.on_video(f),
            VideoDecision::Cut {
                reason: Reason::FormatChanged,
                next: None
            }
        );
        // ...and the following keyframe stops the recording.
        let mut f = frame(80_000, true);
        f.total_bytes = 9_500;
        assert_eq!(p.on_video(f), VideoDecision::Stop(Reason::MaxSize));
    }

    #[test]
    fn stop_sets_the_reason_once() {
        let mut p = SegmentPolicy::new(cfg(), 0);
        p.stop(Reason::Stopped);
        p.stop(Reason::DiskLow);
        assert_eq!(p.state(), PolicyState::Finished(Reason::Stopped));
    }
}
