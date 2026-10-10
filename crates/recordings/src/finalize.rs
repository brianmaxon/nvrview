//! The final step of a camera job: join the downloaded chunks into one MP4.
//!
//! [`Mp4Finalizer`] is the scheduler's [`Finalizer`]. It runs in two steps,
//! each safe against a killed process:
//!
//! 1. **Join.** Chunks `0..n` are read in order and written to a fragmented
//!    MP4 (`joined.fmp4` in the parts directory) by
//!    [`mp4_remux::FragmentedFile`]. A fragment (up to 2 s) is flushed and
//!    synced as soon as it is complete, so a process killed here leaves a
//!    file that plays up to its last complete fragment, and
//!    [`recover_fragmented`] turns it into a normal MP4.
//! 2. **Finalize.** [`mp4_remux::finalize_to_mp4`] remuxes the fragmented
//!    file into a standard faststart MP4 under a temporary name; a rename
//!    publishes it, and the fragmented file is removed last. The output name
//!    therefore never holds a partial file.
//!
//! The raw chunks stay in the parts directory (the scheduler owns their
//! lifetime), so an interrupted run is simply redone from them.
//!
//! # Joining rules
//!
//! - **Overlap.** Each window starts at the keyframe at or before its start,
//!   so a chunk repeats the end of the previous one. A video frame whose
//!   timestamp is not after the last written one is dropped; the rest
//!   continues the previous group of pictures. A gap (a missing stretch)
//!   just lengthens the previous frame, as in `mp4_remux`.
//! - **Audio** has no timestamp. An audio frame is written right after the
//!   video frame it followed in the stream ([`AudioEntry::after_video`]), so
//!   its time is its position (the AAC sample count then spaces it). Audio
//!   that followed a dropped (duplicate) video frame is dropped with it: the
//!   overlap carries the previous chunk's audio again. This can lose the
//!   few milliseconds of audio between the overlap's last frame and the next
//!   new one when the previous chunk ended before delivering them; keeping
//!   them would risk repeating a frame of audio instead.
//! - A chunk without video contributes nothing. One stream's codec and
//!   parameter sets must not change within one file; if they do, the join
//!   fails with the chunk number (the file would need a second `moov`).

use crate::chunk::{self, AudioEntry, ChunkIndex};
use crate::scheduler::Finalizer;
use mp4_remux::{
    AacConfig, FileOptions, FileSummary, FragmentedFile, Options, Summary, VideoCodec, VideoFrame,
    finalize_to_mp4, parse_adts,
};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Name of the fragmented file in the parts directory.
pub const JOINED_NAME: &str = "joined.fmp4";

/// What a finished join did, for logs and tests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JoinStats {
    /// Video frames written.
    pub video_written: u64,
    /// Video frames dropped as overlap with the previous chunk.
    pub video_duplicates: u64,
    /// Audio frames written.
    pub audio_written: u64,
    /// Audio frames dropped with the overlap.
    pub audio_duplicates: u64,
    /// Chunks that held no video.
    pub empty_chunks: u32,
}

/// Joins chunks into a standard MP4.
#[derive(Clone, Debug)]
pub struct Mp4Finalizer {
    /// Put `moov` first (progressive playback). On by default.
    pub faststart: bool,
    /// Sync the fragmented file after every fragment, so a power loss keeps
    /// what was joined. On by default; a killed process does not need it.
    pub sync_fragments: bool,
}

impl Default for Mp4Finalizer {
    fn default() -> Self {
        Mp4Finalizer {
            faststart: true,
            sync_fragments: true,
        }
    }
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn mux(e: mp4_remux::Error) -> io::Error {
    match e {
        mp4_remux::Error::Io(e) => e,
        e => invalid(e.to_string()),
    }
}

/// A sibling path with `suffix` appended to the file name.
fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let mut n = p.file_name().unwrap_or_default().to_os_string();
    n.push(suffix);
    p.with_file_name(n)
}

impl Finalizer for Mp4Finalizer {
    fn finalize(&self, parts_dir: &Path, chunks: u32, output: &Path) -> io::Result<()> {
        self.finalize_with_stats(parts_dir, chunks, output)
            .map(drop)
    }
}

impl Mp4Finalizer {
    /// [`Finalizer::finalize`] that also reports what the join did.
    pub fn finalize_with_stats(
        &self,
        parts_dir: &Path,
        chunks: u32,
        output: &Path,
    ) -> io::Result<JoinStats> {
        let joined = parts_dir.join(JOINED_NAME);
        let (_, stats) = join_chunks(parts_dir, chunks, &joined, self.sync_fragments)?;
        recover_fragmented(&joined, output, self.faststart)?;
        Ok(stats)
    }
}

/// Remuxes a fragmented file written by [`join_chunks`] (complete, or cut
/// short by a crash: everything up to the last complete fragment is kept)
/// into a standard MP4 at `output`, atomically, and removes the fragmented
/// file. This is the recovery path after a killed process.
pub fn recover_fragmented(
    fragmented: &Path,
    output: &Path,
    faststart: bool,
) -> io::Result<Summary> {
    let tmp = with_suffix(output, ".tmp");
    let result = (|| {
        let src = BufReader::new(File::open(fragmented)?);
        let out = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        let opts = Options {
            faststart,
            ..Options::default()
        };
        let (out, summary) = finalize_to_mp4(src, out, opts).map_err(mux)?;
        out.sync_all()?;
        drop(out);
        fs::rename(&tmp, output)?;
        Ok(summary)
    })();
    match result {
        Ok(s) => {
            fs::remove_file(fragmented)?;
            Ok(s)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Opens a chunk's data file for sequential reads of its entries.
struct Reader {
    file: BufReader<File>,
    pos: u64,
}

impl Reader {
    fn open(dir: &Path, name: &str) -> io::Result<Reader> {
        Ok(Reader {
            file: BufReader::new(File::open(dir.join(name))?),
            pos: 0,
        })
    }

    fn read(&mut self, offset: u64, len: u32, buf: &mut Vec<u8>) -> io::Result<()> {
        if offset != self.pos {
            self.file.seek(SeekFrom::Start(offset))?;
        }
        buf.clear();
        buf.resize(len as usize, 0);
        self.file.read_exact(buf)?;
        self.pos = offset + u64::from(len);
        Ok(())
    }
}

fn video_codec(name: &str) -> io::Result<VideoCodec> {
    match name {
        "H264" => Ok(VideoCodec::H264),
        "H265" => Ok(VideoCodec::H265),
        other => Err(invalid(format!("unknown codec {other}"))),
    }
}

/// Loads the indexes of chunks `0..n`; every one must be complete.
fn load_indexes(dir: &Path, n: u32) -> io::Result<Vec<ChunkIndex>> {
    (0..n)
        .map(|i| {
            if !chunk::is_complete(dir, i) {
                return Err(invalid(format!("chunk {i} is missing or incomplete")));
            }
            chunk::read_index(dir, i)?.ok_or_else(|| invalid(format!("chunk {i} has no index")))
        })
        .collect()
}

/// The audio format: from the first audio frame of any chunk that has video.
fn audio_config(dir: &Path, idx: &[ChunkIndex]) -> io::Result<Option<AacConfig>> {
    let mut buf = Vec::new();
    for c in idx.iter().filter(|c| !c.video.is_empty()) {
        let (Some(name), Some(a)) = (&c.audio_file, c.audio.first()) else {
            continue;
        };
        Reader::open(dir, name)?.read(a.offset, a.len, &mut buf)?;
        return Ok(Some(parse_adts(&buf).map_err(mux)?.0));
    }
    Ok(None)
}

/// Joins chunks `0..n` of `dir` into the fragmented file `out` (replacing
/// it), deduplicating the overlap between chunks by timestamp and aligning
/// audio by position. See the module documentation.
pub fn join_chunks(
    dir: &Path,
    n: u32,
    out: &Path,
    sync_fragments: bool,
) -> io::Result<(FileSummary, JoinStats)> {
    let idx = load_indexes(dir, n)?;
    let codec = idx
        .iter()
        .find_map(|c| c.codec.as_deref())
        .ok_or_else(|| invalid("no chunk holds video"))
        .and_then(video_codec)?;
    let audio = audio_config(dir, &idx)?;
    let file = File::create(out)?;
    let mut w = FragmentedFile::new(
        BufWriter::new(file),
        codec,
        FileOptions::default().with_audio(audio),
    );
    let mut stats = JoinStats::default();
    let mut last_pts: Option<u64> = None;
    let (mut vbuf, mut abuf) = (Vec::new(), Vec::new());
    for c in &idx {
        let (Some(vname), true) = (&c.video_file, !c.video.is_empty()) else {
            stats.empty_chunks += 1;
            continue;
        };
        if video_codec(c.codec.as_deref().unwrap_or(""))? != codec {
            return Err(invalid(format!(
                "chunk {} changes the video codec",
                c.chunk
            )));
        }
        let mut video = Reader::open(dir, vname)?;
        let mut aud = c
            .audio_file
            .as_deref()
            .map(|f| Reader::open(dir, f))
            .transpose()?;
        let mut audio_iter = c.audio.iter().peekable();
        // Frames up to this index repeat the previous chunk.
        let mut dup_through: Option<usize> = None;
        for (i, v) in c.video.iter().enumerate() {
            let dup = last_pts.is_some_and(|l| v.pts_us <= l);
            if dup {
                dup_through = Some(i);
                stats.video_duplicates += 1;
            } else {
                video.read(v.offset, v.len, &mut vbuf)?;
                let wrote = w
                    .push_video(VideoFrame::new(&vbuf, v.key, v.pts_us as i64))
                    .map_err(|e| match e {
                        mp4_remux::Error::ParameterSetsChanged => invalid(format!(
                            "chunk {} changes the stream's parameter sets",
                            c.chunk
                        )),
                        e => mux(e),
                    })?;
                if wrote && sync_fragments {
                    w.get_mut().get_ref().sync_data()?;
                }
                last_pts = Some(v.pts_us);
                stats.video_written += 1;
            }
            // Audio that followed this frame in the stream.
            while let Some(a) = audio_iter.next_if(|a: &&AudioEntry| a.after_video as usize <= i) {
                if dup_through.is_some_and(|d| a.after_video as usize <= d) {
                    stats.audio_duplicates += 1;
                    continue;
                }
                if let Some(r) = aud.as_mut() {
                    r.read(a.offset, a.len, &mut abuf)?;
                    // Audio never completes a fragment by itself.
                    w.push_audio(&abuf).map_err(mux)?;
                    stats.audio_written += 1;
                }
            }
        }
    }
    let (writer, summary) = w.finish().map_err(mux)?;
    writer
        .into_inner()
        .map_err(|e| e.into_error())?
        .sync_all()?;
    Ok((summary, stats))
}
