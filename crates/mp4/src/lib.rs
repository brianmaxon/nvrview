//! Platform-free MP4 (ISO BMFF) writer that remuxes H.264/H.265 video and AAC
//! audio without re-encoding.
//!
//! [`Mp4Writer`] makes one file from a stream of access units and ADTS
//! frames; [`Concatenator`] makes one file from a sequence of downloaded
//! chunks, deduplicating overlaps by timestamp.
//!
//! # What goes in the file
//!
//! * Video samples are the input access units with 4-byte NAL lengths; access
//!   unit delimiters, end-of-sequence NALs, and parameter sets are removed.
//!   Parameter sets (SPS/PPS, and VPS for H.265) found in band become `avcC`
//!   or `hvcC` (sample entry `avc1` / `hvc1`, which QuickTime accepts).
//! * Audio samples are the raw AAC payloads of the ADTS frames, 1024 samples
//!   each, with an `esds` built from the first ADTS header.
//! * Timing: video uses a 90 kHz media timescale; each frame's duration is
//!   the difference of its (rounded) timestamp and the next one's, so
//!   irregular camera frame rates are kept exactly. The last frame repeats
//!   the previous duration. Audio uses its sample rate as timescale. Decode
//!   and presentation times may differ (`ctts`) but never go backward from
//!   decode to presentation; cameras send no B-frames, so they are equal.
//! * Alignment: both tracks share one clock (the video timestamps' clock). The
//!   audio start is `Options::audio_start_us` (or the first
//!   `write_audio_at` timestamp, or the first video frame). The difference
//!   becomes an empty edit at the start of whichever track starts later, so
//!   players show them aligned.
//! * Offsets: `mdat` uses a 64-bit size; chunk offsets are `stco` unless the
//!   file needs `co64` (any offset above 4 GiB, chosen automatically).
//!   `Options::faststart` puts `moov` first by shifting the media data once
//!   at the end, in 1 MiB steps, so it needs `Read + Write + Seek`.
//!
//! # Resolution and parameter-set changes
//!
//! When an access unit carries parameter sets different from those in effect
//! (for example after the camera switches stream resolution), the writer adds
//! a second sample description to the same video track and starts a new chunk
//! under it; the file stays one file. The track header keeps the first
//! description's size, and players scale later frames to it or follow the
//! description, depending on the player. Repeated identical sets (every
//! keyframe, as cameras send them) add nothing. Callers that need one
//! resolution per file can compare [`Summary::video_entries`] or split at
//! the change themselves.
//!
//! # Memory
//!
//! Sample data goes to the sink as it arrives, in chunks of about one second
//! per track (at most 1 MiB buffered per track). Only the index stays in
//! memory: about 12 bytes per video sample (size 4, duration run 8; the
//! `stts`/`ctts` runs collapse when the frame rate is steady, and keyframes
//! add 4) and 4 bytes per audio frame, plus 8 bytes per chunk. At 30 fps
//! that is about 1.3 MB per hour of video and 0.2 MB per hour of 16 kHz
//! audio; with a steady frame rate it is about 0.6 MB per hour.
//!
//! # Limitations
//!
//! * Audio has no encoder-delay (priming) edit; the first 1024 AAC samples of
//!   a stream from an encoder that primes play as decoded.
//! * Gaps in audio are filled with silent frames only for mono AAC-LC.
//! * Timestamps must increase by at least one 90 kHz tick (11 µs).
//! * Fragmented MP4 is not produced; a crash before `finish` leaves a file
//!   without `moov`.

mod adts;
mod bits;
mod boxes;
mod concat;
mod error;
mod finalize;
mod fmp4_file;
mod fragment;
mod nal;
mod writer;

pub use adts::{AacConfig, SAMPLES_PER_FRAME, parse_adts};
pub use concat::{Chunk, ChunkStats, Concatenator, TimedAudio};
pub use error::{Error, Result};
pub use finalize::finalize_to_mp4;
pub use fmp4_file::{FileOptions, FileSummary, FragmentedFile};
pub use fragment::{
    AUDIO_TRACK_ID, AudioLayout, ChangePolicy, FragmentOptions, FragmentStats, Fragmenter,
    InitInfo, Segment, SegmentKind, SegmentTrack, VIDEO_TRACK_ID,
};
pub use nal::VideoCodec;
pub use writer::{Mp4Writer, Options, Summary, TIMESCALE, VideoFrame, us_to_ticks};
