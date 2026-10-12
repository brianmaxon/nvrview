//! Crash-safe fragmented MP4 files.

use std::io::Write;

use crate::adts::AacConfig;
use crate::boxes::{bx, full, put_u32, put_u64};
use crate::error::Result;
use crate::fragment::{
    AUDIO_TRACK_ID, AudioLayout, ChangePolicy, FragmentOptions, Fragmenter, Segment, SegmentKind,
    VIDEO_TRACK_ID,
};
use crate::nal::VideoCodec;
use crate::track::AudioTrackSpec;
use crate::writer::VideoFrame;

/// Options for [`FragmentedFile`].
#[derive(Clone, Debug)]
pub struct FileOptions {
    /// Fragment cutting. The layout is forced to muxed and parameter-set
    /// changes to [`ChangePolicy::Reject`] (a file has one `moov`).
    pub fragment: FragmentOptions,
    /// Write a `mfra` random-access index at the end of [`FragmentedFile::finish`].
    pub mfra: bool,
}

impl Default for FileOptions {
    fn default() -> Self {
        Self {
            fragment: FragmentOptions::for_download(),
            mfra: true,
        }
    }
}

impl FileOptions {
    /// With the given audio format (or none).
    pub fn with_audio(mut self, audio: Option<AacConfig>) -> Self {
        self.fragment.audio = audio;
        self
    }

    /// With these audio tracks declared (after the `with_audio` track, if
    /// any), fed with [`FragmentedFile::push_audio_raw`].
    pub fn with_audio_tracks(mut self, tracks: Vec<AudioTrackSpec>) -> Self {
        self.fragment.audio_tracks = tracks;
        self
    }
}

/// What a finished file contains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileSummary {
    pub file_size: u64,
    pub fragments: u64,
    pub video_samples: u64,
    /// Audio samples in all audio tracks.
    pub audio_samples: u64,
    pub has_mfra: bool,
}

struct IndexEntry {
    moof_offset: u64,
    video_tfdt: Option<u64>,
    video_sync: bool,
    /// (track ID, `tfdt`) of each audio `traf`, in `traf` order.
    audio: Vec<(u32, u64)>,
}

/// Writes a fragmented MP4 file progressively to any `Write` sink (no seeking).
///
/// The file is `ftyp`, `moov` (no samples), then one `moof`+`mdat` pair per
/// fragment, and optionally `mfra`. Each fragment is written whole and the
/// sink is flushed after it, so a file cut at any fragment boundary, or
/// after a crash, plays everything before the cut. A partial last fragment is
/// ignored by players or at worst plays a few frames of garbage-free media
/// (see the tests for what was measured).
///
/// For durability across power loss, pass a sink whose `flush` syncs (or
/// call `File::sync_data` through [`get_mut`](Self::get_mut) after each
/// [`push_video`](Self::push_video) that returned true).
pub struct FragmentedFile<W: Write> {
    out: W,
    frag: Fragmenter,
    pos: u64,
    mfra: bool,
    index: Vec<IndexEntry>,
    audio_tracks: u32,
}

impl<W: Write> FragmentedFile<W> {
    pub fn new(out: W, codec: VideoCodec, mut opts: FileOptions) -> Self {
        opts.fragment.audio_layout = AudioLayout::Muxed;
        opts.fragment.on_change = ChangePolicy::Reject;
        let frag = Fragmenter::new(codec, opts.fragment);
        let audio_tracks = frag.audio_tracks().len() as u32;
        Self {
            out,
            frag,
            pos: 0,
            mfra: opts.mfra,
            index: Vec::new(),
            audio_tracks,
        }
    }

    /// Bytes written so far (a truncation point at a fragment boundary is
    /// any value this returned after a call that returned `true`).
    pub fn bytes_written(&self) -> u64 {
        self.pos
    }

    /// The sink, for syncing to disk.
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.out
    }

    /// Adds a video access unit. Returns whether a fragment was completed
    /// and written (the file is then valid up to [`bytes_written`](Self::bytes_written)).
    ///
    /// [`Error::ParameterSetsChanged`](crate::Error::ParameterSetsChanged)
    /// means the file cannot hold this frame; call [`finish`](Self::finish)
    /// and start a new file with it.
    pub fn push_video(&mut self, f: VideoFrame<'_>) -> Result<bool> {
        let segs = self.frag.push_video(f)?;
        self.write_all(segs)
    }

    pub fn push_audio(&mut self, adts: &[u8]) -> Result<bool> {
        self.frag.push_audio(adts)
    }

    pub fn push_audio_at(&mut self, adts: &[u8], ts_us: i64) -> Result<bool> {
        self.frag.push_audio_at(adts, ts_us)
    }

    /// Adds one raw AAC access unit to audio track `track` (see
    /// [`Fragmenter::push_audio_raw`]). Audio never completes a fragment by
    /// itself.
    pub fn push_audio_raw(&mut self, track: usize, au: &[u8], ts_us: Option<i64>) -> Result<bool> {
        self.frag.push_audio_raw(track, au, ts_us)
    }

    /// Writes the last fragment, the `mfra` index if enabled, and flushes.
    pub fn finish(self) -> Result<(W, FileSummary)> {
        let Self {
            mut out,
            frag,
            mut pos,
            mfra: want_mfra,
            mut index,
            audio_tracks,
        } = self;
        let stats = frag.stats();
        let segs = frag.finish()?;
        emit(&mut out, &mut pos, &mut index, segs)?;
        let has_mfra = want_mfra && !index.is_empty();
        if has_mfra {
            let m = mfra(&index, audio_tracks);
            out.write_all(&m)?;
            pos += m.len() as u64;
        }
        out.flush()?;
        Ok((
            out,
            FileSummary {
                file_size: pos,
                fragments: index.len() as u64,
                video_samples: stats.video_samples,
                audio_samples: stats.audio_samples,
                has_mfra,
            },
        ))
    }

    fn write_all(&mut self, segs: Vec<Segment>) -> Result<bool> {
        emit(&mut self.out, &mut self.pos, &mut self.index, segs)
    }
}

fn emit<W: Write>(
    out: &mut W,
    pos: &mut u64,
    index: &mut Vec<IndexEntry>,
    segs: Vec<Segment>,
) -> Result<bool> {
    let mut wrote_media = false;
    for s in segs {
        if s.kind == SegmentKind::Media {
            index.push(IndexEntry {
                moof_offset: *pos,
                video_tfdt: s.video_tfdt,
                video_sync: s.starts_with_keyframe,
                audio: s.audio_tfdts,
            });
            wrote_media = true;
        }
        out.write_all(&s.data)?;
        *pos += s.data.len() as u64;
    }
    if wrote_media {
        out.flush()?;
    }
    Ok(wrote_media)
}

/// `mfra` with one `tfra` per track that has fragments, and the closing
/// `mfro`.
fn mfra(index: &[IndexEntry], audio_tracks: u32) -> Vec<u8> {
    let tfra = |b: &mut Vec<u8>, id: u32, entries: Vec<(u64, u64, u8)>| {
        if entries.is_empty() {
            return;
        }
        full(b, b"tfra", 1, 0, |b| {
            put_u32(b, id);
            put_u32(b, 0); // one-byte traf, trun, and sample numbers
            put_u32(b, entries.len() as u32);
            for (time, off, traf) in entries {
                put_u64(b, time);
                put_u64(b, off);
                b.extend_from_slice(&[traf, 1, 1]);
            }
        });
    };
    let mut out = Vec::new();
    bx(&mut out, b"mfra", |b| {
        tfra(
            b,
            VIDEO_TRACK_ID,
            index
                .iter()
                .filter(|e| e.video_sync)
                .filter_map(|e| Some((e.video_tfdt?, e.moof_offset, 1)))
                .collect(),
        );
        for id in AUDIO_TRACK_ID..AUDIO_TRACK_ID + audio_tracks {
            // The video traf is first in every fragment of a file.
            tfra(
                b,
                id,
                index
                    .iter()
                    .filter_map(|e| {
                        let k = e.audio.iter().position(|&(t, _)| t == id)?;
                        Some((e.audio[k].1, e.moof_offset, 2 + k as u8))
                    })
                    .collect(),
            );
        }
        full(b, b"mfro", 0, 0, |b| put_u32(b, 0));
    });
    let n = out.len() as u32;
    let at = out.len() - 4;
    out[at..].copy_from_slice(&n.to_be_bytes());
    out
}
