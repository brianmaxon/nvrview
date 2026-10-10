//! A simple file format for one live stream as received: every media record
//! in arrival order with its arrival time, so a recording can be replayed
//! with the original interleaving of video and audio, the device's
//! timestamps, and the network's timing.
//!
//! Layout (all integers little-endian):
//!
//! - header: the 8 bytes `NVRREC01`;
//! - then entries: `u64` arrival time in microseconds since the recording
//!   started, `u32` length, and that many bytes of one media record exactly
//!   as [`crate::media`] encodes it. A length of 0 marks a discontinuity (the
//!   stream restarted, for example after a reconnect).
//!
//! Recordings of real devices hold real video and audio: keep them in the
//! git-ignored `captures/` directory (docs/SANITIZATION.md).

use crate::media::{Record, encode_audio, encode_info, encode_video};
use std::io::{self, Read, Write};

/// File header.
pub const MAGIC: &[u8; 8] = b"NVRREC01";

/// Upper bound on one entry, to reject corrupt files instead of allocating.
pub const MAX_ENTRY: u32 = 16 * 1024 * 1024;

/// One entry of a recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// One media record (encoded bytes), received at `arrival_us`.
    Record {
        /// Microseconds since the recording started.
        arrival_us: u64,
        /// The record, as the media encoders produce it.
        bytes: Vec<u8>,
    },
    /// The stream restarted at `arrival_us`.
    Discontinuity {
        /// Microseconds since the recording started.
        arrival_us: u64,
    },
}

/// Encodes a demuxed record back into its wire form. The video stream index
/// is not kept by the demuxer, so video is written with index 0.
pub fn encode_record(record: &Record) -> Vec<u8> {
    match record {
        Record::Info { width, height, fps } => encode_info(*width, *height, *fps),
        Record::Video {
            codec,
            keyframe,
            pts_us,
            data,
        } => encode_video(0, *keyframe, *codec, *pts_us, &[], data),
        Record::Audio { stream_index, data } => encode_audio(*stream_index, data),
    }
}

/// Writes a recording.
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

/// Reads a recording entry by entry. A truncated final entry (a recording cut
/// off mid-write) ends the iteration without an error.
pub struct Reader<R: Read> {
    input: R,
}

impl<R: Read> Reader<R> {
    /// Checks the header.
    pub fn new(mut input: R) -> io::Result<Reader<R>> {
        let mut magic = [0u8; 8];
        input.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not an NVRREC01 recording",
            ));
        }
        Ok(Reader { input })
    }

    /// The next entry, `Ok(None)` at the end.
    pub fn next_entry(&mut self) -> io::Result<Option<Entry>> {
        let mut head = [0u8; 12];
        match read_full(&mut self.input, &mut head)? {
            0 => return Ok(None),
            12 => {}
            _ => return Ok(None),
        }
        let arrival_us = u64::from_le_bytes(head[..8].try_into().expect("8 bytes"));
        let len = u32::from_le_bytes(head[8..].try_into().expect("4 bytes"));
        if len == 0 {
            return Ok(Some(Entry::Discontinuity { arrival_us }));
        }
        if len > MAX_ENTRY {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "entry too large",
            ));
        }
        let mut bytes = vec![0u8; len as usize];
        if read_full(&mut self.input, &mut bytes)? < bytes.len() {
            return Ok(None);
        }
        Ok(Some(Entry::Record { arrival_us, bytes }))
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

    #[test]
    fn round_trip_through_the_demuxer() {
        let records = vec![
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
            },
        ];
        let mut w = Writer::new(Vec::new()).unwrap();
        for (i, r) in records.iter().enumerate() {
            w.record(i as u64 * 10, r).unwrap();
            if i == 1 {
                w.discontinuity(15).unwrap();
            }
        }
        let bytes = w.finish().unwrap();

        let mut r = Reader::new(&bytes[..]).unwrap();
        let mut out = Vec::new();
        let mut discontinuities = Vec::new();
        while let Some(e) = r.next_entry().unwrap() {
            match e {
                Entry::Record { bytes, .. } => {
                    let mut d = Demuxer::new();
                    d.push(&bytes);
                    out.push(d.next_record().unwrap().unwrap());
                }
                Entry::Discontinuity { arrival_us } => discontinuities.push(arrival_us),
            }
        }
        assert_eq!(out, records);
        assert_eq!(discontinuities, vec![15]);
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
    }
}
