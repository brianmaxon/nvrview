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
//! * [`Mp4Writer`] does not produce fragmented MP4; a crash before `finish`
//!   leaves a file without `moov`. Use the fragmented API below where that
//!   matters.
//!
//! # Fragmented MP4
//!
//! [`Fragmenter`] turns the same input (Annex-B access units, ADTS frames)
//! into an init segment (`ftyp` + `moov` with `mvex`/`trex`, no samples) and
//! media segments (`moof` + `mdat`), as owned byte vectors: nothing needs a
//! file, so a WebSocket can send each [`Segment`] as one binary message.
//! [`FragmentedFile`] writes the same segments progressively to a file, and
//! [`finalize_to_mp4`] turns such a file into a regular MP4.
//!
//! * **Cutting.** A fragment ends when the next frame is a keyframe (one GOP
//!   per fragment), when it would span [`FragmentOptions::max_fragment_us`]
//!   (default 500 ms live, 2 s for files), or when its video reaches
//!   `max_fragment_bytes`. A fragment closes when the next frame arrives, so
//!   its last sample's duration is exact; the live delay is therefore up to
//!   one fragment. Frames before the first keyframe are dropped; the first
//!   segment after any init always starts with a keyframe, other segments may
//!   start mid-GOP (their samples say so in the `trun` flags). Segments carry
//!   `starts_with_keyframe` so a client that joins late can be sent the
//!   current init ([`Fragmenter::current_init`]) and then segments from the
//!   next one that starts with a keyframe.
//! * **Timing.** Times start at 0 at the first video frame and continue
//!   across inits. `tfdt` is 64-bit; video uses 90 kHz and audio its sample
//!   rate. Every video sample has its own duration, size, and flags (sync is
//!   `0x02000000`, non-sync `0x01010000`); composition offsets are written
//!   only when presentation differs from decode time. Audio is timed by
//!   sample count; gaps extend the previous frame's duration, duplicates are
//!   dropped, no silence is made. There is no edit list: audio and video are
//!   aligned by their `tfdt`.
//! * **Audio.** The AAC format is declared up front
//!   ([`FragmentOptions::audio`], from [`parse_adts`]) so the init can carry
//!   it. [`AudioLayout::Muxed`] (default) puts both tracks in one init and one
//!   `moof` per fragment (video `traf`, then audio `traf`; one MSE
//!   `SourceBuffer`). [`AudioLayout::Separate`] makes a video stream and an
//!   audio stream with their own inits and one-track fragments (CMAF style;
//!   two `SourceBuffer`s; route by [`Segment::track`]).
//! * **Codec strings** for `addSourceBuffer` come with each init
//!   ([`InitInfo::mime`]): `avc1.PPCCLL` (profile, constraint flags, level from
//!   the SPS) and `hvc1.<profile>.<compat>.<L|H><level>[.<constraints>]`
//!   for H.265, `mp4a.40.<object type>` for AAC. Sample entries are `avc1`
//!   and `hvc1` with the sets in `avcC`/`hvcC` only.
//! * **Parameter-set or resolution change.** At a keyframe whose sets differ
//!   ([`ChangePolicy::NewInit`]) the open fragment is closed, a new init is
//!   emitted with `reset: true` and a higher `generation`, and the next
//!   fragment starts with that keyframe. A client resets its buffer
//!   (`changeType`, or a new `SourceBuffer`) before appending that init.
//! * **Memory.** One fragment of video (bounded by time and bytes) and up to
//!   1 MiB of audio waiting for video; segments are handed to the caller and
//!   not kept (only the current init is).
//!
//! ## Files and downloads
//!
//! [`FragmentedFile`] writes `ftyp`, `moov`, then one `moof`+`mdat` per
//! fragment, flushing after each, optionally ending with a `mfra` random
//! access index ([`FileOptions::mfra`]). A file cut at any fragment boundary
//! (or after a crash) plays everything before the cut in ffmpeg; a cut inside
//! a fragment loses at most that fragment. A file has one `moov`, so a
//! parameter-set change returns [`Error::ParameterSetsChanged`]: finish the
//! file and start another with that frame.
//!
//! Recommendation for downloads: write the fragmented file while downloading
//! (crash-safe, nothing to repair, the partial file is playable), and when
//! the download completes run [`finalize_to_mp4`] to produce the standard
//! faststart MP4 the user gets (PLAYBACK.md: "standard MP4 files"), then
//! delete the fragmented file. After a crash, the same function recovers a
//! truncated fragmented file into a normal MP4. Keep the fragmented file as
//! the deliverable only for streaming or progressive-play uses (it plays in
//! players that support fragments, but seeks less well and some editors
//! refuse it). [`finalize_to_mp4`] reads files written by this crate only.
//!
//! The `examples/mse_sample.rs` generator and `tests/mse/` page check
//! playback in a browser by hand; see `tests/mse/README.md`.

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
