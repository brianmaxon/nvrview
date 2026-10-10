//! Raw chunk files: what one 143 download of one camera's time window is
//! written as, before the MP4 step.
//!
//! In a camera's parts directory, chunk `n` is:
//!
//! - `chunk-NNNNN.h264` or `.h265`: the video records' payloads, Annex-B, in
//!   order, starting at a keyframe (anything before the first keyframe is
//!   dropped);
//! - `chunk-NNNNN.aac`: the audio records' payloads, one ADTS frame each,
//!   from the first keyframe on (audio ahead of the first picture is
//!   dropped: it has no timestamp to align it, docs/protocol.md);
//! - `chunk-NNNNN.json`: a [`ChunkIndex`] with every frame's timestamp,
//!   byte range, and keyframe flag, and for every audio frame the video frame
//!   it followed in the stream (its only timing information).
//!
//! The data files are written under `.part` names and renamed once flushed;
//! the index is written last, so a chunk is complete exactly when its index
//! exists.
//!
//! Consecutive chunks overlap: the device starts every window at the
//! keyframe at or before its start. The MP4 step drops frames of a chunk
//! whose timestamp is at or before the previous chunk's last one; what
//! remains continues the previous chunk's group of pictures.

use crate::Secs;
use crate::store::{read_json, write_json};
use bc_protocol::media::{Codec, Record};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

/// One video frame in a chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoEntry {
    /// Timestamp in microseconds, as the device sent it.
    pub pts_us: u64,
    /// Keyframe (its data starts with the parameter sets).
    pub key: bool,
    /// Byte offset in the video file.
    pub offset: u64,
    /// Length in bytes.
    pub len: u32,
}

/// One audio frame in a chunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioEntry {
    /// Byte offset in the audio file.
    pub offset: u64,
    /// Length in bytes (one ADTS frame).
    pub len: u32,
    /// Index into [`ChunkIndex::video`] of the last video frame received
    /// before this audio frame.
    pub after_video: u32,
}

/// The index of one chunk.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkIndex {
    /// Format version (1).
    pub version: u32,
    /// Chunk number within the camera job.
    pub chunk: u32,
    /// Requested window start (naive seconds).
    pub start: Secs,
    /// Requested window end.
    pub end: Secs,
    /// `H264` or `H265`; `None` when the window had no video.
    pub codec: Option<String>,
    /// Width from the stream info record (may be 0).
    pub width: u32,
    /// Height from the stream info record (may be 0).
    pub height: u32,
    /// Advertised frame rate (not the real rate; use the timestamps).
    pub fps: u8,
    /// Video file name, if any video.
    pub video_file: Option<String>,
    /// Audio file name, if any audio.
    pub audio_file: Option<String>,
    /// Video frames in order.
    pub video: Vec<VideoEntry>,
    /// Audio frames in order.
    pub audio: Vec<AudioEntry>,
    /// Records dropped because they came before the first keyframe.
    pub dropped_before_keyframe: u32,
    /// Bytes of payload written.
    pub bytes: u64,
}

impl ChunkIndex {
    /// First and last video timestamps.
    pub fn pts_range(&self) -> Option<(u64, u64)> {
        Some((self.video.first()?.pts_us, self.video.last()?.pts_us))
    }
}

/// File stem of chunk `n`.
pub fn chunk_stem(n: u32) -> String {
    format!("chunk-{n:05}")
}

/// Path of chunk `n`'s index in `dir`.
pub fn index_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("{}.json", chunk_stem(n)))
}

/// Reads chunk `n`'s index; `Ok(None)` if the chunk is not complete.
pub fn read_index(dir: &Path, n: u32) -> io::Result<Option<ChunkIndex>> {
    read_json(&index_path(dir, n))
}

/// Whether chunk `n` is complete: its index exists and every data file it
/// names is present.
pub fn is_complete(dir: &Path, n: u32) -> bool {
    match read_index(dir, n) {
        Ok(Some(i)) => [&i.video_file, &i.audio_file]
            .into_iter()
            .flatten()
            .all(|f| dir.join(f).is_file()),
        _ => false,
    }
}

/// Removes every file of chunk `n` (complete or partial).
pub fn remove_chunk(dir: &Path, n: u32) -> io::Result<()> {
    let stem = chunk_stem(n);
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let p = entry?.path();
        if p.file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f.starts_with(&format!("{stem}.")))
        {
            fs::remove_file(&p)?;
        }
    }
    Ok(())
}

/// Writes one chunk.
pub struct ChunkWriter {
    dir: PathBuf,
    video: Option<BufWriter<File>>,
    audio: Option<BufWriter<File>>,
    video_len: u64,
    audio_len: u64,
    codec: Option<Codec>,
    seen_key: bool,
    idx: ChunkIndex,
}

impl ChunkWriter {
    /// Starts chunk `n` of window `start..=end` in `dir`, replacing any
    /// partial files of an earlier attempt.
    pub fn create(dir: &Path, n: u32, start: Secs, end: Secs) -> io::Result<ChunkWriter> {
        fs::create_dir_all(dir)?;
        remove_chunk(dir, n)?;
        Ok(ChunkWriter {
            dir: dir.to_owned(),
            video: None,
            audio: None,
            video_len: 0,
            audio_len: 0,
            codec: None,
            seen_key: false,
            idx: ChunkIndex {
                version: 1,
                chunk: n,
                start,
                end,
                ..ChunkIndex::default()
            },
        })
    }

    fn part(&self, ext: &str) -> PathBuf {
        self.dir
            .join(format!("{}.{ext}.part", chunk_stem(self.idx.chunk)))
    }

    /// Adds a record in stream order.
    pub fn write(&mut self, rec: &Record) -> io::Result<()> {
        match rec {
            Record::Info { width, height, fps } => {
                if self.idx.width == 0 && self.idx.height == 0 {
                    (self.idx.width, self.idx.height, self.idx.fps) = (*width, *height, *fps);
                }
            }
            Record::Video {
                codec,
                keyframe,
                pts_us,
                data,
            } => {
                if !self.seen_key && !keyframe {
                    self.idx.dropped_before_keyframe += 1;
                    return Ok(());
                }
                self.seen_key = true;
                if self.codec.is_none() {
                    self.codec = Some(*codec);
                }
                if self.video.is_none() {
                    self.video = Some(BufWriter::new(File::create(self.part("video"))?));
                }
                if let Some(w) = self.video.as_mut() {
                    w.write_all(data)?;
                }
                self.idx.video.push(VideoEntry {
                    pts_us: *pts_us,
                    key: *keyframe,
                    offset: self.video_len,
                    len: data.len() as u32,
                });
                self.video_len += data.len() as u64;
                self.idx.bytes += data.len() as u64;
            }
            Record::Audio { data, .. } => {
                if !self.seen_key {
                    self.idx.dropped_before_keyframe += 1;
                    return Ok(());
                }
                if self.audio.is_none() {
                    self.audio = Some(BufWriter::new(File::create(self.part("audio"))?));
                }
                if let Some(w) = self.audio.as_mut() {
                    w.write_all(data)?;
                }
                self.idx.audio.push(AudioEntry {
                    offset: self.audio_len,
                    len: data.len() as u32,
                    after_video: self.idx.video.len().saturating_sub(1) as u32,
                });
                self.audio_len += data.len() as u64;
                self.idx.bytes += data.len() as u64;
            }
        }
        Ok(())
    }

    /// Payload bytes written so far.
    pub fn bytes(&self) -> u64 {
        self.idx.bytes
    }

    /// Flushes the data files to disk, gives them their final names, and
    /// writes the index (which marks the chunk complete).
    pub fn finish(mut self) -> io::Result<ChunkIndex> {
        let stem = chunk_stem(self.idx.chunk);
        if let Some(w) = self.video.take() {
            let f = w.into_inner().map_err(|e| e.into_error())?;
            f.sync_all()?;
            let ext = match self.codec {
                Some(Codec::H265) => "h265",
                _ => "h264",
            };
            let name = format!("{stem}.{ext}");
            fs::rename(self.part("video"), self.dir.join(&name))?;
            self.idx.video_file = Some(name);
            self.idx.codec = self.codec.map(|c| match c {
                Codec::H264 => "H264".to_owned(),
                Codec::H265 => "H265".to_owned(),
            });
        }
        if let Some(w) = self.audio.take() {
            let f = w.into_inner().map_err(|e| e.into_error())?;
            f.sync_all()?;
            let name = format!("{stem}.aac");
            fs::rename(self.part("audio"), self.dir.join(&name))?;
            self.idx.audio_file = Some(name);
        }
        write_json(&index_path(&self.dir, self.idx.chunk), &self.idx)?;
        Ok(self.idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(key: bool, pts: u64, n: usize) -> Record {
        Record::Video {
            codec: Codec::H265,
            keyframe: key,
            pts_us: pts,
            data: vec![key as u8; n],
        }
    }

    fn audio(n: usize) -> Record {
        Record::Audio {
            stream_index: 5,
            data: vec![0xff; n],
        }
    }

    #[test]
    fn writes_aligned_chunk_and_index() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = ChunkWriter::create(dir.path(), 3, 100, 160).unwrap();
        w.write(&Record::Info {
            width: 640,
            height: 360,
            fps: 15,
        })
        .unwrap();
        w.write(&audio(10)).unwrap(); // before the first picture: dropped
        w.write(&video(false, 1, 5)).unwrap(); // dropped
        w.write(&video(true, 10, 100)).unwrap();
        w.write(&audio(20)).unwrap();
        w.write(&video(false, 20, 30)).unwrap();
        w.write(&audio(21)).unwrap();
        assert!(!is_complete(dir.path(), 3));
        let idx = w.finish().unwrap();
        assert!(is_complete(dir.path(), 3));
        assert_eq!(idx.codec.as_deref(), Some("H265"));
        assert_eq!(idx.dropped_before_keyframe, 2);
        assert_eq!(idx.pts_range(), Some((10, 20)));
        assert_eq!(idx.video[1].offset, 100);
        assert_eq!(
            idx.audio
                .iter()
                .map(|a| (a.offset, a.after_video))
                .collect::<Vec<_>>(),
            vec![(0, 0), (20, 1)]
        );
        assert_eq!(
            fs::read(dir.path().join("chunk-00003.h265")).unwrap().len(),
            130
        );
        assert_eq!(
            fs::read(dir.path().join("chunk-00003.aac")).unwrap().len(),
            41
        );
        assert_eq!(read_index(dir.path(), 3).unwrap().unwrap(), idx);
        assert_eq!((idx.width, idx.height), (640, 360));

        // A retry replaces everything, including a stale partial file.
        fs::write(dir.path().join("chunk-00003.video.part"), b"stale").unwrap();
        let w = ChunkWriter::create(dir.path(), 3, 100, 160).unwrap();
        assert!(!is_complete(dir.path(), 3));
        assert!(!dir.path().join("chunk-00003.video.part").exists());
        let idx = w.finish().unwrap();
        assert!(idx.video.is_empty() && idx.video_file.is_none());
        assert!(is_complete(dir.path(), 3));
    }
}
