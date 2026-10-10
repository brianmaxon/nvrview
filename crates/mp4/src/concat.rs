//! Building one MP4 from a sequence of chunks.

use std::io::{Read, Seek, Write};

use crate::error::Result;
use crate::nal::VideoCodec;
use crate::writer::{Mp4Writer, Options, Summary, VideoFrame};

/// An audio frame with its presentation time (µs, video clock).
#[derive(Clone, Copy, Debug)]
pub struct TimedAudio<'a> {
    pub adts: &'a [u8],
    pub ts_us: i64,
}

/// One downloaded chunk: access units and audio frames in time order.
#[derive(Clone, Copy, Debug)]
pub struct Chunk<'a> {
    pub video: &'a [VideoFrame<'a>],
    pub audio: &'a [TimedAudio<'a>],
}

/// What `push_chunk` did with a chunk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChunkStats {
    pub video_written: u32,
    /// Frames at or before the last written timestamp (overlap with the
    /// previous chunk).
    pub video_duplicates: u32,
    /// Frames skipped at the start of the file because no keyframe had been
    /// seen yet.
    pub video_leading_skipped: u32,
    pub audio_written: u32,
    pub audio_duplicates: u32,
}

/// Joins chunks into one file, deduplicating overlaps by timestamp.
///
/// Video: a frame whose decode time is not after the last written frame is
/// dropped. A true overlap (the device repeated frames it already sent)
/// therefore resumes seamlessly in the same group of pictures; a gap just
/// extends the previous frame's duration. Audio: see
/// [`Mp4Writer::write_audio_at`]; gaps become silent edit-list segments.
pub struct Concatenator<W: Read + Write + Seek> {
    writer: Mp4Writer<W>,
    seen_key: bool,
}

impl<W: Read + Write + Seek> Concatenator<W> {
    pub fn new(out: W, codec: VideoCodec, opts: Options) -> Result<Self> {
        Ok(Self {
            writer: Mp4Writer::new(out, codec, opts)?,
            seen_key: false,
        })
    }

    pub fn push_chunk(&mut self, chunk: &Chunk<'_>) -> Result<ChunkStats> {
        let mut s = ChunkStats::default();
        for f in chunk.video {
            if self
                .writer
                .last_video_dts_us()
                .is_some_and(|last| f.dts_us <= last)
            {
                s.video_duplicates += 1;
                continue;
            }
            if !self.seen_key {
                if !f.keyframe {
                    s.video_leading_skipped += 1;
                    continue;
                }
                self.seen_key = true;
            }
            self.writer.write_video(*f)?;
            s.video_written += 1;
        }
        // Audio before the first video frame cannot be placed on the video
        // clock, so it is only written once video exists.
        if self.seen_key {
            for a in chunk.audio {
                if self.writer.write_audio_at(a.adts, a.ts_us)? {
                    s.audio_written += 1;
                } else {
                    s.audio_duplicates += 1;
                }
            }
        }
        Ok(s)
    }

    pub fn finish(self) -> Result<(W, Summary)> {
        self.writer.finish()
    }
}
