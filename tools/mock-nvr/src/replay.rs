//! Replay of a recorded stream ([`bc_protocol::recording`]): every media
//! record in its original order, at its original arrival time (optionally
//! accelerated), so a viewer sees the interleaving of video and audio, the
//! device's timestamps, and the network's jitter exactly as recorded.
//!
//! [`Replay`] holds a loaded recording. [`Replay::playback`] iterates over
//! one stream of it as a schedule (stream time and what to send) without any
//! I/O or clock, so tests and simulations can consume it in simulated time;
//! the server sends it over TCP on the wall clock.
//!
//! Rules for both versions:
//!
//! - **Speed.** Stream time is divided by the speed: at 60x a 10-minute
//!   recording takes 10 seconds. Timestamps inside the records are never
//!   changed by the speed.
//! - **Discontinuities.** A discontinuity marker keeps its place in the
//!   timeline. The wire has no marker for it (a device that restarts a
//!   stream just sends new records), so nothing is sent for it; the caller
//!   gets [`Step::Discontinuity`] from the iterator. Start and stop markers
//!   of an `NVRREC02` recording send nothing either and are not steps.
//! - **Looping.** After the last entry the recording starts over. Arrival
//!   times and the video timestamps in the records are both shifted by the
//!   length of one pass (the last arrival time plus the average spacing of
//!   entries), so a looped stream looks like one continuous capture: time
//!   and picture timestamps keep rising. The first pass is byte-exact; later
//!   passes differ only in the 8-byte timestamp of each video record. In an
//!   `NVRREC02` recording one pass length covers the whole file and every
//!   tier is shifted by it, so the timestamp relation between tiers is the
//!   recorded one in every pass.
//!
//! `NVRREC01` (one stream): every preview, whatever its tier, plays the
//! recording from its start, on a clock that starts with that preview.
//!
//! `NVRREC02` (several tiers on one clock), see [`Replay::join`]:
//!
//! - **One replay clock.** The server keeps one clock for all previews (it
//!   starts with the first preview any client starts). Replay time is the
//!   time on that clock times the speed, and every tier plays its entries at
//!   their recorded arrival times on it. A client that starts the main tier
//!   while the sub-stream plays therefore gets main from the same capture
//!   moment, with the recorded timing between the tiers.
//! - **Joining.** A tier started at replay time *T* begins with that tier's
//!   latest stream-info record, then the tier's most recent keyframe that
//!   arrived at or before *T*, provided the tier's recorded stream ran
//!   without a break (no discontinuity or stop marker) from that keyframe
//!   to *T*. The records from that keyframe up to *T* are due already, so
//!   they go out at once, as a burst; later ones keep their recorded times.
//!   Without such a keyframe (the tier was stopped at *T* in the recording,
//!   or had not started yet), the tier starts at its next keyframe, at that
//!   keyframe's recorded time. Whether real devices start a preview this
//!   way (a burst of the current GOP, or the next keyframe) is what a
//!   capture with `bc-cli capture` measures; the rule here is a model.
//! - **Tiers not recorded.** A preview of a tier the recording does not
//!   hold plays the default tier ([`Replay::default_tier`]: the sub-stream
//!   when recorded, otherwise the first tier in the file).

use bc_protocol::media::Codec;
use bc_protocol::recording::{Entry, Reader};
use bc_protocol::stream::StreamTier;
use std::borrow::Cow;
use std::io::{self, Read};
use std::path::Path;

/// Offset of the 64-bit microsecond timestamp in a video record header.
const VIDEO_PTS_AT: usize = 16;
/// Length of a video record header.
const VIDEO_HEADER: usize = 24;

/// A loaded recording.
#[derive(Clone, Debug)]
pub struct Replay {
    entries: Vec<Entry>,
    pass_us: u64,
    version: u8,
    /// Each tier with records (one `None` stream for `NVRREC01`) and the
    /// indices of its entries, in file order.
    streams: Vec<(Option<StreamTier>, Vec<usize>)>,
}

/// What one schedule step carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step<'a> {
    /// One encoded media record, as the media encoders produce it.
    Record(Cow<'a, [u8]>),
    /// The recorded stream restarted here; nothing goes on the wire.
    Discontinuity,
}

fn is_video(record: &[u8]) -> bool {
    record.len() >= VIDEO_HEADER && &record[2..4] == b"dc"
}

fn is_keyframe(record: &[u8]) -> bool {
    is_video(record) && record[1] == b'0'
}

fn is_info(record: &[u8]) -> bool {
    record.len() >= 32 && &record[..3] == b"100"
}

/// Where a tier joins the replay; see [`Replay::join`].
#[derive(Clone, Debug)]
pub struct Join<'a> {
    /// The stream-info record to send first, if the tier has one.
    pub info: Option<&'a [u8]>,
    /// The schedule from the starting keyframe on.
    pub playback: Playback<'a>,
}

impl Replay {
    /// Loads a recording file.
    pub fn load(path: &Path) -> io::Result<Replay> {
        Replay::from_reader(std::fs::File::open(path)?)
    }

    /// Reads a recording of either version.
    ///
    /// # Errors
    /// Not a recording, or no media records.
    pub fn from_reader<R: Read>(input: R) -> io::Result<Replay> {
        let mut reader = Reader::new(io::BufReader::new(input))?;
        let version = reader.version();
        let mut entries = Vec::new();
        while let Some(e) = reader.next_entry()? {
            entries.push(e);
        }
        let mut streams: Vec<(Option<StreamTier>, Vec<usize>)> = Vec::new();
        for e in &entries {
            if matches!(e, Entry::Record { .. }) && !streams.iter().any(|(t, _)| *t == e.stream()) {
                streams.push((e.stream(), Vec::new()));
            }
        }
        if streams.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recording has no records",
            ));
        }
        for (i, e) in entries.iter().enumerate() {
            if let Some((_, idx)) = streams.iter_mut().find(|(t, _)| *t == e.stream()) {
                idx.push(i);
            }
        }
        let last = entries.last().map_or(0, Entry::arrival_us);
        let spacing = (last / entries.len() as u64).max(1_000);
        Ok(Replay {
            entries,
            pass_us: last + spacing,
            version,
            streams,
        })
    }

    /// The format version of the file: 1 (`NVRREC01`) or 2 (`NVRREC02`).
    pub fn version(&self) -> u8 {
        self.version
    }

    /// Every entry of one pass, all tiers, in file order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The tiers that have records (empty for an `NVRREC01` recording).
    pub fn tiers(&self) -> Vec<StreamTier> {
        self.streams.iter().filter_map(|(t, _)| *t).collect()
    }

    /// The tier [`Replay::playback`] plays: the sub-stream if recorded, else
    /// the first tier in the file; `None` for an `NVRREC01` recording.
    pub fn default_tier(&self) -> Option<StreamTier> {
        let tiers = self.tiers();
        tiers
            .iter()
            .copied()
            .find(|t| *t == StreamTier::Sub)
            .or(tiers.first().copied())
    }

    /// The stream a preview of `tier` plays (the default one when `tier` is
    /// not recorded, and always the one stream of an `NVRREC01` recording).
    fn stream(&self, tier: Option<StreamTier>) -> &[usize] {
        let want = tier
            .filter(|t| self.tiers().contains(t))
            .or(self.default_tier());
        self.streams
            .iter()
            .find(|(t, _)| *t == want)
            .map_or(&[][..], |(_, idx)| idx.as_slice())
    }

    fn records(&self, tier: Option<StreamTier>) -> impl Iterator<Item = &[u8]> {
        self.stream(tier)
            .iter()
            .filter_map(|&i| match &self.entries[i] {
                Entry::Record { bytes, .. } => Some(bytes.as_slice()),
                _ => None,
            })
    }

    /// Length of one pass in microseconds, including the gap before a loop
    /// repeats.
    pub fn pass_us(&self) -> u64 {
        self.pass_us
    }

    /// Codec of the default stream's first video record, if any.
    pub fn codec(&self) -> Option<Codec> {
        self.codec_of(None)
    }

    /// Codec of the first video record a preview of `tier` plays.
    pub fn codec_of(&self, tier: Option<StreamTier>) -> Option<Codec> {
        self.records(tier)
            .filter(|b| is_video(b))
            .find_map(|b| match &b[4..8] {
                b"H264" => Some(Codec::H264),
                b"H265" => Some(Codec::H265),
                _ => None,
            })
    }

    /// Width, height, and frame rate of the default stream's first info
    /// record, if any.
    pub fn info(&self) -> Option<(u32, u32, u8)> {
        self.info_of(None)
    }

    /// Width, height, and frame rate of the first info record a preview of
    /// `tier` plays.
    pub fn info_of(&self, tier: Option<StreamTier>) -> Option<(u32, u32, u8)> {
        self.records(tier).find(|b| is_info(b)).map(|b| {
            (
                u32::from_le_bytes(b[8..12].try_into().expect("4 bytes")),
                u32::from_le_bytes(b[12..16].try_into().expect("4 bytes")),
                b[17],
            )
        })
    }

    /// The default stream's schedule: `(stream time in microseconds, step)`,
    /// one pass, or passes forever when `looped`. Stream time is the
    /// recorded arrival time, not yet divided by any speed.
    pub fn playback(&self, looped: bool) -> Playback<'_> {
        self.playback_of(None, looped)
    }

    /// The schedule of the stream a preview of `tier` plays, from its start.
    pub fn playback_of(&self, tier: Option<StreamTier>, looped: bool) -> Playback<'_> {
        Playback {
            replay: self,
            stream: self.stream(tier),
            looped,
            pass: 0,
            index: 0,
        }
    }

    /// Where a preview of `tier` started at replay time `at_us` begins (see
    /// the module docs): the info record to send first, and the schedule
    /// from the starting keyframe on (whose first steps may be due already).
    /// `None` when the recording holds no further keyframe for the tier, or
    /// has ended (`at_us` past one pass) and does not loop.
    pub fn join(&self, tier: Option<StreamTier>, at_us: u64, looped: bool) -> Option<Join<'_>> {
        let stream = self.stream(tier);
        if !looped && at_us >= self.pass_us {
            return None;
        }
        let (mut pass, local) = if looped {
            (at_us / self.pass_us, at_us % self.pass_us)
        } else {
            (0, at_us)
        };
        // The latest keyframe at or before `local` in an unbroken run.
        let mut candidate = None;
        let mut next = stream.len();
        for (pos, &i) in stream.iter().enumerate() {
            let e = &self.entries[i];
            if e.arrival_us() > local {
                next = pos;
                break;
            }
            match e {
                Entry::Record { bytes, .. } if is_keyframe(bytes) => candidate = Some(pos),
                Entry::Record { .. } => {}
                _ => candidate = None,
            }
        }
        let keyframe_at = |pos: usize| match &self.entries[stream[pos]] {
            Entry::Record { bytes, .. } => is_keyframe(bytes),
            _ => false,
        };
        let start = match candidate {
            Some(pos) => pos,
            None => match (next..stream.len()).find(|&p| keyframe_at(p)) {
                Some(pos) => pos,
                None if looped => {
                    pass += 1;
                    (0..stream.len()).find(|&p| keyframe_at(p))?
                }
                None => return None,
            },
        };
        let info_at = |p: usize| match &self.entries[stream[p]] {
            Entry::Record { bytes, .. } if is_info(bytes) => Some(bytes.as_slice()),
            _ => None,
        };
        let info = (0..start)
            .rev()
            .find_map(info_at)
            .or_else(|| (start..stream.len()).find_map(info_at));
        Some(Join {
            info,
            playback: Playback {
                replay: self,
                stream,
                looped,
                pass,
                index: start,
            },
        })
    }
}

/// Iterator over a replay's schedule; see [`Replay::playback`].
#[derive(Clone, Debug)]
pub struct Playback<'a> {
    replay: &'a Replay,
    stream: &'a [usize],
    looped: bool,
    pass: u64,
    index: usize,
}

impl<'a> Iterator for Playback<'a> {
    type Item = (u64, Step<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.index >= self.stream.len() {
                if !self.looped || self.stream.is_empty() {
                    return None;
                }
                self.pass += 1;
                self.index = 0;
            }
            let shift = self.pass * self.replay.pass_us;
            let e = &self.replay.entries[self.stream[self.index]];
            self.index += 1;
            match e {
                Entry::Discontinuity { arrival_us, .. } => {
                    return Some((arrival_us + shift, Step::Discontinuity));
                }
                Entry::Record {
                    arrival_us, bytes, ..
                } => {
                    let step = if shift > 0 && is_video(bytes) {
                        let mut v = bytes.clone();
                        let pts = u64::from_le_bytes(
                            v[VIDEO_PTS_AT..VIDEO_PTS_AT + 8]
                                .try_into()
                                .expect("8 bytes"),
                        );
                        v[VIDEO_PTS_AT..VIDEO_PTS_AT + 8]
                            .copy_from_slice(&(pts + shift).to_le_bytes());
                        Step::Record(Cow::Owned(v))
                    } else {
                        Step::Record(Cow::Borrowed(bytes.as_slice()))
                    };
                    return Some((arrival_us + shift, step));
                }
                Entry::StartRequested { .. } | Entry::StopRequested { .. } => {}
            }
        }
    }
}

/// Replay settings of a server.
#[derive(Clone, Debug)]
pub struct ReplayConfig {
    /// The recording every preview serves.
    pub replay: std::sync::Arc<Replay>,
    /// Speed factor (1.0 = as recorded).
    pub speed: f64,
    /// Start over after the last entry.
    pub looped: bool,
}

impl ReplayConfig {
    /// One pass at 1x.
    pub fn new(replay: Replay) -> ReplayConfig {
        ReplayConfig {
            replay: std::sync::Arc::new(replay),
            speed: 1.0,
            looped: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bc_protocol::media::{Codec, Record, encode_info};
    use bc_protocol::recording::{TierWriter, Writer};

    fn video(keyframe: bool, pts_us: u64) -> Record {
        Record::Video {
            codec: Codec::H265,
            keyframe,
            pts_us,
            data: vec![0, 0, 0, 1, 2, 3],
            extra_header: Vec::new(),
        }
    }

    fn sample() -> Replay {
        let mut w = Writer::new(Vec::new()).unwrap();
        let info = Record::Info {
            width: 64,
            height: 48,
            fps: 10,
        };
        w.record(0, &info).unwrap();
        for i in 0..4u64 {
            w.record(10_000 * i, &video(i == 0, 5_000 + 100_000 * i))
                .unwrap();
        }
        w.discontinuity(45_000).unwrap();
        Replay::from_reader(&w.finish().unwrap()[..]).unwrap()
    }

    #[test]
    fn describes_the_recording() {
        let r = sample();
        assert_eq!(r.version(), 1);
        assert_eq!(r.tiers(), vec![]);
        assert_eq!(r.default_tier(), None);
        assert_eq!(r.codec(), Some(Codec::H265));
        assert_eq!(r.info(), Some((64, 48, 10)));
        assert_eq!(r.info_of(Some(StreamTier::Main)), Some((64, 48, 10)));
        // 45 ms last arrival over 6 entries: 7 ms spacing.
        assert_eq!(r.pass_us(), 45_000 + 7_500);
    }

    #[test]
    fn loop_shifts_time_and_video_timestamps_only() {
        let r = sample();
        let two: Vec<_> = r.playback(true).take(12).collect();
        assert_eq!(r.playback(false).count(), 6);
        for (a, b) in two[..6].iter().zip(&two[6..]) {
            assert_eq!(b.0, a.0 + r.pass_us());
            match (&a.1, &b.1) {
                (Step::Record(x), Step::Record(y)) if is_video(x) => {
                    assert_eq!(x[..VIDEO_PTS_AT], y[..VIDEO_PTS_AT]);
                    assert_eq!(x[VIDEO_PTS_AT + 8..], y[VIDEO_PTS_AT + 8..]);
                    let px = u64::from_le_bytes(x[16..24].try_into().unwrap());
                    let py = u64::from_le_bytes(y[16..24].try_into().unwrap());
                    assert_eq!(py, px + r.pass_us());
                }
                (x, y) => assert_eq!(x, y),
            }
        }
    }

    fn pts(step: &Step<'_>) -> Option<(bool, u64)> {
        match step {
            Step::Record(b) if is_video(b) => Some((
                is_keyframe(b),
                u64::from_le_bytes(b[16..24].try_into().unwrap()),
            )),
            _ => None,
        }
    }

    /// Two tiers on one clock: sub (pictures every 100 ms, keyframe every
    /// 10) from 0 s; main (every 50 ms, keyframe every 20) started at 2 s
    /// and stopped from 5.5 s to 6.5 s. Both share the picture clock; main
    /// arrives 120 ms after its capture time, sub 20 ms.
    fn tiered() -> Replay {
        enum What {
            Start,
            Stop,
            Rec(Record),
        }
        let mut items: Vec<(u64, StreamTier, What)> = vec![
            (0, StreamTier::Sub, What::Start),
            (
                1_000,
                StreamTier::Sub,
                What::Rec(Record::Info {
                    width: 64,
                    height: 48,
                    fps: 10,
                }),
            ),
            (2_000_000, StreamTier::Main, What::Start),
            (
                2_100_000,
                StreamTier::Main,
                What::Rec(Record::Info {
                    width: 640,
                    height: 480,
                    fps: 20,
                }),
            ),
            (5_500_000, StreamTier::Main, What::Stop),
            (6_400_000, StreamTier::Main, What::Start),
        ];
        for i in 0..90u64 {
            let t = i * 100_000;
            items.push((
                t + 20_000,
                StreamTier::Sub,
                What::Rec(video(i % 10 == 0, t)),
            ));
        }
        let mut k = 0;
        for i in 0..180u64 {
            let (t, at) = (i * 50_000, i * 50_000 + 120_000);
            if t >= 2_000_000 && at < 5_500_000 || t >= 6_500_000 {
                // A start begins with a keyframe (observed with
                // `newestIFrame`); then every 20th picture is one.
                items.push((at, StreamTier::Main, What::Rec(video(k % 20 == 0, t))));
                k += 1;
            } else {
                k = 0;
            }
        }
        items.sort_by_key(|(at, t, _)| (*at, t.number()));
        let mut w = TierWriter::new(Vec::new()).unwrap();
        for (at, tier, what) in items {
            match what {
                What::Start => w.start_requested(tier, at).unwrap(),
                What::Stop => w.stop_requested(tier, at).unwrap(),
                What::Rec(r) => w.record(tier, at, &r).unwrap(),
            }
        }
        Replay::from_reader(&w.finish().unwrap()[..]).unwrap()
    }

    #[test]
    fn tiers_and_default() {
        let r = tiered();
        assert_eq!(r.version(), 2);
        assert_eq!(r.tiers(), vec![StreamTier::Sub, StreamTier::Main]);
        assert_eq!(r.default_tier(), Some(StreamTier::Sub));
        assert_eq!(r.info(), Some((64, 48, 10)));
        assert_eq!(r.info_of(Some(StreamTier::Main)), Some((640, 480, 20)));
        // Not recorded: the default tier.
        assert_eq!(r.info_of(Some(StreamTier::Extern)), Some((64, 48, 10)));
        // The default playback is the sub-stream alone.
        assert_eq!(
            r.playback(false).filter(|(_, s)| pts(s).is_some()).count(),
            90
        );
    }

    #[test]
    fn join_starts_at_the_latest_keyframe_of_an_unbroken_run() {
        let r = tiered();
        let main = Some(StreamTier::Main);
        let first = |j: Join<'_>| {
            let (at, step) = j.playback.clone().find(|(_, s)| pts(s).is_some()).unwrap();
            (at, pts(&step).unwrap())
        };
        // At 3.5 s: main's keyframes were captured at 2.0 and 3.0 s
        // (arrivals 2.12 and 3.12 s); start at 3.0 s, its info first.
        let j = r.join(main, 3_500_000, false).unwrap();
        assert_eq!(j.info, Some(&encode_info(640, 480, 20)[..]));
        assert_eq!(first(j), (3_120_000, (true, 3_000_000)));
        // Exactly at a keyframe's arrival, that keyframe.
        let j = r.join(main, 4_120_000, false).unwrap();
        assert_eq!(first(j).1, (true, 4_000_000));
        // Before main started: its first keyframe, at its own time.
        let j = r.join(main, 500_000, false).unwrap();
        assert_eq!(first(j), (2_120_000, (true, 2_000_000)));
        // While main was stopped: the keyframe that began the restart, not
        // the one before the stop.
        let j = r.join(main, 6_000_000, false).unwrap();
        assert_eq!(first(j), (6_620_000, (true, 6_500_000)));
        // The sub-stream at 3.5 s: its keyframe captured at 3.0 s.
        let j = r.join(Some(StreamTier::Sub), 3_500_000, false).unwrap();
        assert_eq!(first(j).1, (true, 3_000_000));
        // After the end: nothing without a loop; the next pass with one.
        assert!(r.join(main, r.pass_us(), false).is_none());
        assert!(r.join(main, r.pass_us() - 1, false).is_some());
        let j = r.join(main, r.pass_us() + 3_500_000, true).unwrap();
        assert_eq!(
            first(j),
            (r.pass_us() + 3_120_000, (true, r.pass_us() + 3_000_000))
        );
        let j = r.join(main, 2 * r.pass_us() - 1, true).unwrap();
        // The last main keyframe of the pass, or the next pass's first.
        let (at, (key, _)) = first(j);
        assert!(key && at <= 2 * r.pass_us() + 2_120_000);
    }

    #[test]
    fn joined_tiers_keep_the_recorded_relation() {
        let r = tiered();
        let at = 4_400_000;
        let main: Vec<_> = r
            .join(Some(StreamTier::Main), at, true)
            .unwrap()
            .playback
            .take(60)
            .filter_map(|(t, s)| pts(&s).map(|p| (t, p.1)))
            .collect();
        // Main arrives 120 ms after capture, whatever the pass.
        for (t, p) in &main {
            assert_eq!(t - p, 120_000);
        }
        let sub: Vec<_> = r
            .join(Some(StreamTier::Sub), at, true)
            .unwrap()
            .playback
            .take(30)
            .filter_map(|(t, s)| pts(&s).map(|p| (t, p.1)))
            .collect();
        for (t, p) in &sub {
            assert_eq!(t - p, 20_000);
        }
    }
}
