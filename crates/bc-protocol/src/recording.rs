//! A simple file format for live streams as received: every media record in
//! arrival order with its arrival time, so a recording can be replayed with
//! the original interleaving of video and audio, the device's timestamps,
//! and the network's timing.
//!
//! Two versions exist. Both start with an 8-byte header; all integers are
//! little-endian.
//!
//! **`NVRREC01`: one stream.** After the header, entries: `u64` arrival time
//! in microseconds since the recording started, `u32` length, and that many
//! bytes of one media record exactly as [`crate::media`] encodes it. A length
//! of 0 marks a discontinuity (the stream restarted, for example after a
//! reconnect). Which stream tier was recorded is not stored.
//!
//! **`NVRREC02`: several tiers of one channel on one clock.** After the
//! header, entries of a 16-byte head and a body:
//!
//! | offset | size | field |
//! | --- | --- | --- |
//! | 0 | 8 | `u64` arrival time, µs since the recording started; one clock for every tier |
//! | 8 | 1 | kind: 1 record, 2 discontinuity, 3 start requested, 4 stop requested |
//! | 9 | 1 | stream tier number, as in a preview handle (0 main, 1 sub, 2 mobile, 4 extern) |
//! | 10 | 2 | reserved, 0 |
//! | 12 | 4 | `u32` body length |
//! | 16 | length | body: for a record, one media record as [`crate::media`] encodes it; empty for the other kinds |
//!
//! A "start requested" entry is written when the recorder asked for the
//! tier's preview to start (just before the request is sent), and "stop
//! requested" when it asked for it to stop, so the time from a request to
//! the first record can be measured. Entries are written in arrival order.
//! A reader skips entries of unknown kinds (their length is still bounded by
//! [`MAX_ENTRY`]) and rejects unknown tier numbers.
//!
//! In both versions a video record keeps its extra header as received, so
//! the device's wall-clock seconds in it ([`crate::media::wall_clock_secs`])
//! survive a recording. (Recordings made before the demuxer kept extra
//! headers have empty ones.) The video stream index in the record magic is
//! not kept by the demuxer and is written as 0.
//!
//! Recordings of real devices hold real video and audio: keep them in the
//! git-ignored `captures/` directory (docs/SANITIZATION.md).

use crate::media::{Record, encode_audio, encode_info, encode_video};
use crate::stream::StreamTier;
use std::io::{self, Read, Write};

/// Header of a one-stream recording.
pub const MAGIC: &[u8; 8] = b"NVRREC01";

/// Header of a several-tier recording.
pub const MAGIC_V2: &[u8; 8] = b"NVRREC02";

/// Upper bound on one entry, to reject corrupt files instead of allocating.
pub const MAX_ENTRY: u32 = 16 * 1024 * 1024;

const KIND_RECORD: u8 = 1;
const KIND_DISCONTINUITY: u8 = 2;
const KIND_START: u8 = 3;
const KIND_STOP: u8 = 4;

/// One entry of a recording. `stream` is the tier the entry belongs to:
/// always set in an `NVRREC02` recording, `None` in an `NVRREC01` one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// One media record (encoded bytes), received at `arrival_us`.
    Record {
        /// Microseconds since the recording started.
        arrival_us: u64,
        /// Tier of the record (`None` in a one-stream recording).
        stream: Option<StreamTier>,
        /// The record, as the media encoders produce it.
        bytes: Vec<u8>,
    },
    /// The stream restarted at `arrival_us`.
    Discontinuity {
        /// Microseconds since the recording started.
        arrival_us: u64,
        /// Tier that restarted (`None` in a one-stream recording).
        stream: Option<StreamTier>,
    },
    /// The recorder asked for the tier's preview to start.
    StartRequested {
        /// Microseconds since the recording started.
        arrival_us: u64,
        /// The tier.
        stream: StreamTier,
    },
    /// The recorder asked for the tier's preview to stop.
    StopRequested {
        /// Microseconds since the recording started.
        arrival_us: u64,
        /// The tier.
        stream: StreamTier,
    },
}

impl Entry {
    /// When the entry happened, in microseconds since the recording started.
    pub fn arrival_us(&self) -> u64 {
        match self {
            Entry::Record { arrival_us, .. }
            | Entry::Discontinuity { arrival_us, .. }
            | Entry::StartRequested { arrival_us, .. }
            | Entry::StopRequested { arrival_us, .. } => *arrival_us,
        }
    }

    /// The tier the entry belongs to (`None` in a one-stream recording).
    pub fn stream(&self) -> Option<StreamTier> {
        match self {
            Entry::Record { stream, .. } | Entry::Discontinuity { stream, .. } => *stream,
            Entry::StartRequested { stream, .. } | Entry::StopRequested { stream, .. } => {
                Some(*stream)
            }
        }
    }
}

/// Encodes a demuxed record back into its wire form, extra header included.
/// The video stream index is not kept by the demuxer, so video is written
/// with index 0.
pub fn encode_record(record: &Record) -> Vec<u8> {
    match record {
        Record::Info { width, height, fps } => encode_info(*width, *height, *fps),
        Record::Video {
            codec,
            keyframe,
            pts_us,
            data,
            extra_header,
        } => encode_video(0, *keyframe, *codec, *pts_us, extra_header, data),
        Record::Audio { stream_index, data } => encode_audio(*stream_index, data),
    }
}

/// Writes a one-stream (`NVRREC01`) recording.
pub struct Writer<W: Write> {
    out: W,
}

impl<W: Write> Writer<W> {
    /// Writes the header.
    pub fn new(mut out: W) -> io::Result<Writer<W>> {
        out.write_all(MAGIC)?;
        Ok(Writer { out })
    }

    /// Appends one record.
    pub fn record(&mut self, arrival_us: u64, record: &Record) -> io::Result<()> {
        let bytes = encode_record(record);
        self.out.write_all(&arrival_us.to_le_bytes())?;
        self.out.write_all(&(bytes.len() as u32).to_le_bytes())?;
        self.out.write_all(&bytes)
    }

    /// Appends a discontinuity marker.
    pub fn discontinuity(&mut self, arrival_us: u64) -> io::Result<()> {
        self.out.write_all(&arrival_us.to_le_bytes())?;
        self.out.write_all(&0u32.to_le_bytes())
    }

    /// Flushes and returns the inner writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Writes a several-tier (`NVRREC02`) recording.
pub struct TierWriter<W: Write> {
    out: W,
}

impl<W: Write> TierWriter<W> {
    /// Writes the header.
    pub fn new(mut out: W) -> io::Result<TierWriter<W>> {
        out.write_all(MAGIC_V2)?;
        Ok(TierWriter { out })
    }

    fn entry(
        &mut self,
        arrival_us: u64,
        kind: u8,
        tier: StreamTier,
        body: &[u8],
    ) -> io::Result<()> {
        let mut head = [0u8; 16];
        head[..8].copy_from_slice(&arrival_us.to_le_bytes());
        head[8] = kind;
        head[9] = tier.number() as u8;
        head[12..].copy_from_slice(&(body.len() as u32).to_le_bytes());
        self.out.write_all(&head)?;
        self.out.write_all(body)
    }

    /// Appends one record of `tier`.
    pub fn record(&mut self, tier: StreamTier, arrival_us: u64, record: &Record) -> io::Result<()> {
        self.entry(arrival_us, KIND_RECORD, tier, &encode_record(record))
    }

    /// Appends one already encoded record of `tier`.
    pub fn record_bytes(
        &mut self,
        tier: StreamTier,
        arrival_us: u64,
        bytes: &[u8],
    ) -> io::Result<()> {
        self.entry(arrival_us, KIND_RECORD, tier, bytes)
    }

    /// Appends a discontinuity of `tier`.
    pub fn discontinuity(&mut self, tier: StreamTier, arrival_us: u64) -> io::Result<()> {
        self.entry(arrival_us, KIND_DISCONTINUITY, tier, &[])
    }

    /// Marks that the preview of `tier` was requested to start.
    pub fn start_requested(&mut self, tier: StreamTier, arrival_us: u64) -> io::Result<()> {
        self.entry(arrival_us, KIND_START, tier, &[])
    }

    /// Marks that the preview of `tier` was requested to stop.
    pub fn stop_requested(&mut self, tier: StreamTier, arrival_us: u64) -> io::Result<()> {
        self.entry(arrival_us, KIND_STOP, tier, &[])
    }

    /// Flushes the output (entries written so far survive a crash).
    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    /// Flushes and returns the inner writer.
    pub fn finish(mut self) -> io::Result<W> {
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Reads a recording of either version entry by entry. A truncated final
/// entry (a recording cut off mid-write) ends the iteration without an
/// error.
pub struct Reader<R: Read> {
    input: R,
    version: u8,
}

fn invalid(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

impl<R: Read> Reader<R> {
    /// Checks the header.
    pub fn new(mut input: R) -> io::Result<Reader<R>> {
        let mut magic = [0u8; 8];
        input.read_exact(&mut magic)?;
        let version = match &magic {
            m if m == MAGIC => 1,
            m if m == MAGIC_V2 => 2,
            _ => return Err(invalid("not an NVRREC01 or NVRREC02 recording")),
        };
        Ok(Reader { input, version })
    }

    /// The format version: 1 (`NVRREC01`) or 2 (`NVRREC02`).
    pub fn version(&self) -> u8 {
        self.version
    }

    /// The next entry, `Ok(None)` at the end.
    pub fn next_entry(&mut self) -> io::Result<Option<Entry>> {
        if self.version == 1 {
            return self.next_v1();
        }
        loop {
            let mut head = [0u8; 16];
            if read_full(&mut self.input, &mut head)? < head.len() {
                return Ok(None);
            }
            let arrival_us = u64::from_le_bytes(head[..8].try_into().expect("8 bytes"));
            let kind = head[8];
            let len = u32::from_le_bytes(head[12..].try_into().expect("4 bytes"));
            if len > MAX_ENTRY {
                return Err(invalid("entry too large"));
            }
            let mut body = vec![0u8; len as usize];
            if read_full(&mut self.input, &mut body)? < body.len() {
                return Ok(None);
            }
            if !(KIND_RECORD..=KIND_STOP).contains(&kind) {
                continue;
            }
            let stream = StreamTier::from_number(u32::from(head[9]))
                .ok_or_else(|| invalid("unknown stream tier"))?;
            return Ok(Some(match kind {
                KIND_RECORD if !body.is_empty() => Entry::Record {
                    arrival_us,
                    stream: Some(stream),
                    bytes: body,
                },
                KIND_RECORD => return Err(invalid("empty record entry")),
                KIND_DISCONTINUITY => Entry::Discontinuity {
                    arrival_us,
                    stream: Some(stream),
                },
                KIND_START => Entry::StartRequested { arrival_us, stream },
                _ => Entry::StopRequested { arrival_us, stream },
            }));
        }
    }

    fn next_v1(&mut self) -> io::Result<Option<Entry>> {
        let mut head = [0u8; 12];
        if read_full(&mut self.input, &mut head)? < head.len() {
            return Ok(None);
        }
        let arrival_us = u64::from_le_bytes(head[..8].try_into().expect("8 bytes"));
        let len = u32::from_le_bytes(head[8..].try_into().expect("4 bytes"));
        if len == 0 {
            return Ok(Some(Entry::Discontinuity {
                arrival_us,
                stream: None,
            }));
        }
        if len > MAX_ENTRY {
            return Err(invalid("entry too large"));
        }
        let mut bytes = vec![0u8; len as usize];
        if read_full(&mut self.input, &mut bytes)? < bytes.len() {
            return Ok(None);
        }
        Ok(Some(Entry::Record {
            arrival_us,
            stream: None,
            bytes,
        }))
    }
}

/// Reads until `buf` is full or the input ends; returns the bytes read.
fn read_full<R: Read>(input: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match input.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{Codec, Demuxer};

    fn demux(bytes: &[u8]) -> Record {
        let mut d = Demuxer::new();
        d.push(bytes);
        d.next_record().unwrap().unwrap()
    }

    fn sample_records() -> Vec<Record> {
        let mut wall = 1_700_000_000u32.to_le_bytes().to_vec();
        wall.extend([0u8; 12]);
        vec![
            Record::Info {
                width: 640,
                height: 480,
                fps: 15,
            },
            Record::Video {
                codec: Codec::H264,
                keyframe: true,
                pts_us: 1_000,
                data: vec![0, 0, 0, 1, 0x67, 1, 2, 3],
                extra_header: wall,
            },
            Record::Audio {
                stream_index: 5,
                data: vec![0xff, 0xf1, 0x60, 0x40, 0x01, 0x1f, 0xfc, 9],
            },
            Record::Video {
                codec: Codec::H264,
                keyframe: false,
                pts_us: 67_000,
                data: vec![0, 0, 0, 1, 0x41, 7],
                extra_header: Vec::new(),
            },
        ]
    }

    #[test]
    fn round_trip_through_the_demuxer() {
        let records = sample_records();
        let mut w = Writer::new(Vec::new()).unwrap();
        for (i, r) in records.iter().enumerate() {
            w.record(i as u64 * 10, r).unwrap();
            if i == 1 {
                w.discontinuity(15).unwrap();
            }
        }
        let bytes = w.finish().unwrap();

        let mut r = Reader::new(&bytes[..]).unwrap();
        assert_eq!(r.version(), 1);
        let mut out = Vec::new();
        let mut discontinuities = Vec::new();
        while let Some(e) = r.next_entry().unwrap() {
            assert_eq!(e.stream(), None);
            match e {
                Entry::Record { bytes, .. } => out.push(demux(&bytes)),
                Entry::Discontinuity { arrival_us, .. } => discontinuities.push(arrival_us),
                other => panic!("{other:?}"),
            }
        }
        // The extra header (with its wall-clock seconds) survives.
        assert_eq!(out, records);
        assert_eq!(discontinuities, vec![15]);
    }

    #[test]
    fn tiers_and_markers_round_trip() {
        let records = sample_records();
        let mut w = TierWriter::new(Vec::new()).unwrap();
        w.start_requested(StreamTier::Sub, 0).unwrap();
        w.record(StreamTier::Sub, 5, &records[0]).unwrap();
        w.start_requested(StreamTier::Main, 7).unwrap();
        w.record(StreamTier::Sub, 9, &records[1]).unwrap();
        w.record(StreamTier::Main, 11, &records[3]).unwrap();
        w.discontinuity(StreamTier::Main, 12).unwrap();
        w.stop_requested(StreamTier::Main, 13).unwrap();
        w.record_bytes(StreamTier::Extern, 14, &encode_record(&records[2]))
            .unwrap();
        let mut bytes = w.finish().unwrap();
        assert_eq!(&bytes[..8], MAGIC_V2);
        // An entry of a kind this reader does not know is skipped.
        let mut unknown = 15u64.to_le_bytes().to_vec();
        unknown.extend([99, 1, 0, 0]);
        unknown.extend(3u32.to_le_bytes());
        unknown.extend([1, 2, 3]);
        bytes.extend(unknown);

        let mut r = Reader::new(&bytes[..]).unwrap();
        assert_eq!(r.version(), 2);
        let mut got = Vec::new();
        while let Some(e) = r.next_entry().unwrap() {
            got.push(e);
        }
        let rec = |at, t, r: &Record| Entry::Record {
            arrival_us: at,
            stream: Some(t),
            bytes: encode_record(r),
        };
        assert_eq!(
            got,
            vec![
                Entry::StartRequested {
                    arrival_us: 0,
                    stream: StreamTier::Sub
                },
                rec(5, StreamTier::Sub, &records[0]),
                Entry::StartRequested {
                    arrival_us: 7,
                    stream: StreamTier::Main
                },
                rec(9, StreamTier::Sub, &records[1]),
                rec(11, StreamTier::Main, &records[3]),
                Entry::Discontinuity {
                    arrival_us: 12,
                    stream: Some(StreamTier::Main)
                },
                Entry::StopRequested {
                    arrival_us: 13,
                    stream: StreamTier::Main
                },
                rec(14, StreamTier::Extern, &records[2]),
            ]
        );
        if let Entry::Record { bytes, .. } = &got[3] {
            assert_eq!(demux(bytes), records[1]);
        }
        assert_eq!(got[4].arrival_us(), 11);
        assert_eq!(got[4].stream(), Some(StreamTier::Main));
    }

    #[test]
    fn truncated_and_bad_input() {
        assert!(Reader::new(&b"NOTAREC0"[..]).is_err());
        let mut w = Writer::new(Vec::new()).unwrap();
        w.record(
            1,
            &Record::Info {
                width: 1,
                height: 1,
                fps: 1,
            },
        )
        .unwrap();
        let mut bytes = w.finish().unwrap();
        bytes.truncate(bytes.len() - 3);
        let mut r = Reader::new(&bytes[..]).unwrap();
        assert_eq!(r.next_entry().unwrap(), None);
        let mut huge = MAGIC.to_vec();
        huge.extend(0u64.to_le_bytes());
        huge.extend(u32::MAX.to_le_bytes());
        assert!(Reader::new(&huge[..]).unwrap().next_entry().is_err());

        // Version 2: truncated, oversized, unknown tier.
        let mut w = TierWriter::new(Vec::new()).unwrap();
        w.record(
            StreamTier::Main,
            1,
            &Record::Info {
                width: 1,
                height: 1,
                fps: 1,
            },
        )
        .unwrap();
        let mut bytes = w.finish().unwrap();
        bytes.truncate(bytes.len() - 1);
        assert_eq!(Reader::new(&bytes[..]).unwrap().next_entry().unwrap(), None);
        let mut huge = MAGIC_V2.to_vec();
        huge.extend(0u64.to_le_bytes());
        huge.extend([1, 1, 0, 0]);
        huge.extend(u32::MAX.to_le_bytes());
        assert!(Reader::new(&huge[..]).unwrap().next_entry().is_err());
        let mut bad_tier = MAGIC_V2.to_vec();
        bad_tier.extend(0u64.to_le_bytes());
        bad_tier.extend([2, 3, 0, 0]);
        bad_tier.extend(0u32.to_le_bytes());
        assert!(Reader::new(&bad_tier[..]).unwrap().next_entry().is_err());
    }
}
