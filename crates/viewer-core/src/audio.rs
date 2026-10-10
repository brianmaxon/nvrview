//! The seam where per-tile audio will be decoded (after M2).
//!
//! Live streams interleave audio records with video; each audio record has
//! no timestamp of its own, so its position among the video records and its
//! arrival time are the only timing information. The tile thread hands every
//! audio record to its [`AudioSink`] in stream order, with that context, and
//! tells the sink about discontinuities. Today the only sink counts and drops
//! the frames; decoding, drift correction, and any audio interface replace
//! [`CountingAudioSink`] without restructuring the tile.

/// One audio record with the timing context the stream gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioFrame {
    /// The two-digit stream index from the record magic.
    pub stream_index: u8,
    /// The record's payload, as received.
    pub data: Vec<u8>,
    /// Position of this record among all media items of the tile's stream
    /// (video and audio share the counter), preserving interleave order.
    pub sequence: u64,
    /// Timestamp of the last video picture before this record, if any since
    /// the last discontinuity.
    pub after_video_pts_us: Option<u64>,
    /// Arrival time on the core's monotonic clock (microseconds).
    pub arrived_us: u64,
}

/// Receives a tile's audio, in order, on the tile's thread. Must not block.
pub trait AudioSink: Send {
    /// One audio record.
    fn push(&mut self, frame: AudioFrame);
    /// The stream restarted (reconnect or overflow): timing starts over.
    fn discontinuity(&mut self);
}

/// The current sink: counts what it would have played and drops it.
#[derive(Debug, Default)]
pub struct CountingAudioSink {
    /// Audio records received.
    pub frames: u64,
    /// Payload bytes received.
    pub bytes: u64,
    /// Discontinuities seen.
    pub discontinuities: u64,
}

impl AudioSink for CountingAudioSink {
    fn push(&mut self, frame: AudioFrame) {
        self.frames += 1;
        self.bytes += frame.data.len() as u64;
    }

    fn discontinuity(&mut self) {
        self.discontinuities += 1;
    }
}
