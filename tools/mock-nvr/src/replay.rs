//! Replay of a recorded stream (`NVRREC01`, see
//! [`bc_protocol::recording`]): every media record in its original order, at
//! its original arrival time (optionally accelerated), so a viewer sees the
//! interleaving of video and audio, the device's timestamps, and the
//! network's jitter exactly as recorded.
//!
//! [`Replay`] holds a loaded recording. [`Replay::playback`] iterates over it
//! as a schedule (stream time and what to send) without any I/O or clock, so
//! tests and simulations can consume it in simulated time; the server sends
//! it over TCP on the wall clock.
//!
//! Rules:
//!
//! - **Speed.** Stream time is divided by the speed: at 60x a 10-minute
//!   recording takes 10 seconds. Timestamps inside the records are never
//!   changed by the speed.
//! - **Discontinuities.** A discontinuity marker keeps its place in the
//!   timeline. The wire has no marker for it (a device that restarts a
//!   stream just sends new records), so nothing is sent for it; the caller
//!   gets [`Step::Discontinuity`] from the iterator.
//! - **Looping.** After the last entry the recording starts over. Arrival
//!   times and the video timestamps in the records are both shifted by the
//!   length of one pass (the last arrival time plus the average spacing of
//!   entries), so a looped stream looks like one continuous capture: time
//!   and picture timestamps keep rising. The first pass is byte-exact; later
//!   passes differ only in the 8-byte timestamp of each video record.

use bc_protocol::media::Codec;
use bc_protocol::recording::{Entry, Reader};
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

impl Replay {
    /// Loads a recording file.
    pub fn load(path: &Path) -> io::Result<Replay> {
        Replay::from_reader(std::fs::File::open(path)?)
    }

    /// Reads a recording.
    ///
    /// # Errors
    /// Not a recording, or no entries.
    pub fn from_reader<R: Read>(input: R) -> io::Result<Replay> {
        let mut reader = Reader::new(io::BufReader::new(input))?;
        let mut entries = Vec::new();
        while let Some(e) = reader.next_entry()? {
            entries.push(e);
        }
        if entries.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recording has no entries",
            ));
        }
        let last = entries.last().map_or(0, arrival);
        let spacing = (last / entries.len() as u64).max(1_000);
        Ok(Replay {
            entries,
            pass_us: last + spacing,
        })
    }

    /// The entries of one pass.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Length of one pass in microseconds, including the gap before a loop
    /// repeats.
    pub fn pass_us(&self) -> u64 {
        self.pass_us
    }

    /// Codec of the first video record, if any.
    pub fn codec(&self) -> Option<Codec> {
        self.entries.iter().find_map(|e| match e {
            Entry::Record { bytes, .. } if is_video(bytes) => match &bytes[4..8] {
                b"H264" => Some(Codec::H264),
                b"H265" => Some(Codec::H265),
                _ => None,
            },
            _ => None,
        })
    }

    /// Width, height, and frame rate of the first info record, if any.
    pub fn info(&self) -> Option<(u32, u32, u8)> {
        self.entries.iter().find_map(|e| match e {
            Entry::Record { bytes, .. } if bytes.len() >= 32 && &bytes[..3] == b"100" => Some((
                u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes")),
                u32::from_le_bytes(bytes[12..16].try_into().expect("4 bytes")),
                bytes[17],
            )),
            _ => None,
        })
    }

    /// The schedule: `(stream time in microseconds, step)`, one pass, or
    /// passes forever when `looped`. Stream time is the recorded arrival
    /// time, not yet divided by any speed.
    pub fn playback(&self, looped: bool) -> Playback<'_> {
        Playback {
            replay: self,
            looped,
            pass: 0,
            index: 0,
        }
    }
}

fn arrival(e: &Entry) -> u64 {
    match e {
        Entry::Record { arrival_us, .. } | Entry::Discontinuity { arrival_us } => *arrival_us,
    }
}

/// Iterator over a replay's schedule; see [`Replay::playback`].
#[derive(Debug)]
pub struct Playback<'a> {
    replay: &'a Replay,
    looped: bool,
    pass: u64,
    index: usize,
}

impl<'a> Iterator for Playback<'a> {
    type Item = (u64, Step<'a>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.replay.entries.len() {
            if !self.looped {
                return None;
            }
            self.pass += 1;
            self.index = 0;
        }
        let shift = self.pass * self.replay.pass_us;
        let e = &self.replay.entries[self.index];
        self.index += 1;
        Some(match e {
            Entry::Discontinuity { arrival_us } => (arrival_us + shift, Step::Discontinuity),
            Entry::Record { arrival_us, bytes } => {
                let step = if shift > 0 && is_video(bytes) {
                    let mut v = bytes.clone();
                    let pts = u64::from_le_bytes(
                        v[VIDEO_PTS_AT..VIDEO_PTS_AT + 8]
                            .try_into()
                            .expect("8 bytes"),
                    );
                    v[VIDEO_PTS_AT..VIDEO_PTS_AT + 8].copy_from_slice(&(pts + shift).to_le_bytes());
                    Step::Record(Cow::Owned(v))
                } else {
                    Step::Record(Cow::Borrowed(bytes.as_slice()))
                };
                (arrival_us + shift, step)
            }
        })
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
    use bc_protocol::media::{Codec, Record};
    use bc_protocol::recording::Writer;

    fn sample() -> Replay {
        let mut w = Writer::new(Vec::new()).unwrap();
        let info = Record::Info {
            width: 64,
            height: 48,
            fps: 10,
        };
        w.record(0, &info).unwrap();
        for i in 0..4u64 {
            w.record(
                10_000 * i,
                &Record::Video {
                    codec: Codec::H265,
                    keyframe: i == 0,
                    pts_us: 5_000 + 100_000 * i,
                    data: vec![0, 0, 0, 1, 2, 3],
                },
            )
            .unwrap();
        }
        w.discontinuity(45_000).unwrap();
        Replay::from_reader(&w.finish().unwrap()[..]).unwrap()
    }

    #[test]
    fn describes_the_recording() {
        let r = sample();
        assert_eq!(r.codec(), Some(Codec::H265));
        assert_eq!(r.info(), Some((64, 48, 10)));
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
}
