//! The streaming MP4 writer.

use std::io::{Read, Seek, SeekFrom, Write};

use crate::adts::{AacConfig, SAMPLES_PER_FRAME, parse_adts};
use crate::boxes::{
    ChunkRec, Tables, avc1_entry, bx, full, hvc1_entry, mp4a_entry, put_u16, put_u32, put_u64,
    stbl, stsd,
};
use crate::error::{Error, Result};
use crate::nal::{
    NalKind, ParamSets, VideoCodec, classify, parse_h264_sps, parse_h265_sps, split_annexb,
};

/// Movie and video media timescale: ticks per second.
pub const TIMESCALE: u32 = 90_000;
/// Chunks are flushed after this much media time per track.
const CHUNK_TICKS: i64 = TIMESCALE as i64;
/// Chunks are also flushed when this many bytes are buffered.
const CHUNK_BYTES: usize = 1 << 20;
/// Duration assumed for a lone video sample (25 fps).
const DEFAULT_SAMPLE_TICKS: u32 = TIMESCALE / 25;
/// Seconds from 1904-01-01 to 1970-01-01.
pub(crate) const EPOCH_1904: u64 = 2_082_844_800;

/// Converts microseconds to 90 kHz ticks, rounding to nearest.
pub fn us_to_ticks(us: i64) -> i64 {
    (us * 9 + 50).div_euclid(100)
}

/// Writer options.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Put `moov` before `mdat` (progressive playback). Costs one extra
    /// sequential copy of the media data when the file is finished.
    pub faststart: bool,
    /// Creation time recorded in the headers, seconds since the Unix epoch
    /// (0 leaves the headers at the 1904 epoch, meaning unknown).
    pub creation_unix_secs: u64,
    /// Presentation time (µs, same clock as the video timestamps) of the
    /// first audio frame. `None` aligns it with the first video frame.
    pub audio_start_us: Option<i64>,
}

/// One video access unit.
#[derive(Clone, Copy, Debug)]
pub struct VideoFrame<'a> {
    /// Annex-B access unit. Parameter sets may be in band; AUD and
    /// parameter-set NAL units are removed from the stored sample.
    pub data: &'a [u8],
    pub keyframe: bool,
    /// Decode time in microseconds.
    pub dts_us: i64,
    /// Presentation time in microseconds (at least `dts_us`).
    pub pts_us: i64,
}

impl<'a> VideoFrame<'a> {
    /// A frame whose presentation time equals its decode time (no B-frames).
    pub fn new(data: &'a [u8], keyframe: bool, ts_us: i64) -> Self {
        Self {
            data,
            keyframe,
            dts_us: ts_us,
            pts_us: ts_us,
        }
    }
}

/// What a finished file contains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    pub file_size: u64,
    pub video_samples: u64,
    pub audio_samples: u64,
    /// Number of video sample descriptions (more than one means the
    /// parameter sets, and possibly the resolution, changed mid-stream).
    pub video_entries: usize,
    pub uses_co64: bool,
    pub moov_size: u64,
}

struct Pending {
    buf: Vec<u8>,
    samples: u32,
    entry: u32,
    first_ticks: i64,
}

impl Pending {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            samples: 0,
            entry: 1,
            first_ticks: 0,
        }
    }
}

pub(crate) struct VideoEntry {
    pub(crate) sets: ParamSets,
    pub(crate) sample_entry: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

struct VideoTrack {
    codec: VideoCodec,
    entries: Vec<VideoEntry>,
    sizes: Vec<u32>,
    stts: Vec<(u32, u32)>,
    ctts: Vec<(u32, u32)>,
    ctts_any: bool,
    sync: Vec<u32>,
    chunks: Vec<ChunkRec>,
    pending: Pending,
    first_dts_ticks: i64,
    last_dts_ticks: i64,
    last_dts_us: i64,
    last_delta: u32,
    min_pts_ticks: i64,
    min_pts_us: i64,
    /// Latest presentation time seen; with `min_pts_ticks` it bounds the
    /// presentation span the edit list covers.
    max_pts_ticks: i64,
}

fn push_run(runs: &mut Vec<(u32, u32)>, value: u32) {
    match runs.last_mut() {
        Some((c, v)) if *v == value => *c += 1,
        _ => runs.push((1, value)),
    }
}

struct AudioTrack {
    config: AacConfig,
    sizes: Vec<u32>,
    chunks: Vec<ChunkRec>,
    pending: Pending,
    /// Presentation time of the first frame; `None` means "with the video".
    start_us: Option<i64>,
    total_bytes: u64,
    max_frame: u32,
}

/// One raw AAC-LC mono frame of silence: a single channel element with global
/// gain 0, no scale-factor bands, and the end marker (32 bits).
const SILENT_AAC_LC_MONO: [u8; 4] = [0, 0, 0, 7];
/// Longest gap (in frames, about an hour at 16 kHz) filled with silence.
const MAX_GAP_FRAMES: i64 = 56_250;

impl AudioTrack {
    fn push<W: Write>(&mut self, out: &mut W, pos: &mut u64, payload: &[u8]) -> Result<()> {
        let rate = i64::from(self.config.sample_rate);
        let total = self.sizes.len() as i64 * i64::from(SAMPLES_PER_FRAME);
        if self.pending.samples == 0 {
            self.pending.first_ticks = total * i64::from(TIMESCALE) / rate;
        }
        self.pending.buf.extend_from_slice(payload);
        self.pending.samples += 1;
        let size = payload.len() as u32;
        self.sizes.push(size);
        self.total_bytes += u64::from(size);
        self.max_frame = self.max_frame.max(size);
        let now = (total + i64::from(SAMPLES_PER_FRAME)) * i64::from(TIMESCALE) / rate;
        if now - self.pending.first_ticks >= CHUNK_TICKS || self.pending.buf.len() >= CHUNK_BYTES {
            flush(out, pos, &mut self.pending, &mut self.chunks)?;
        }
        Ok(())
    }
}

/// Streaming MP4 muxer. See the crate documentation.
pub struct Mp4Writer<W: Read + Write + Seek> {
    out: W,
    opts: Options,
    pos: u64,
    mdat_start: u64,
    video: Option<VideoTrack>,
    audio: Option<AudioTrack>,
    codec: VideoCodec,
}

impl<W: Read + Write + Seek> Mp4Writer<W> {
    /// Starts a file at the sink's position 0.
    pub fn new(mut out: W, codec: VideoCodec, opts: Options) -> Result<Self> {
        out.seek(SeekFrom::Start(0))?;
        let mut head = Vec::new();
        bx(&mut head, b"ftyp", |b| {
            b.extend_from_slice(b"isom");
            put_u32(b, 512);
            for brand in [b"isom", b"iso2", b"mp41"] {
                b.extend_from_slice(brand);
            }
        });
        let mdat_start = head.len() as u64;
        // 64-bit mdat header: size field 1, then the real size, patched at finish.
        put_u32(&mut head, 1);
        head.extend_from_slice(b"mdat");
        put_u64(&mut head, 16);
        out.write_all(&head)?;
        Ok(Self {
            out,
            opts,
            pos: head.len() as u64,
            mdat_start,
            video: None,
            audio: None,
            codec,
        })
    }

    /// Decode time of the last video sample written, in microseconds.
    pub fn last_video_dts_us(&self) -> Option<i64> {
        self.video.as_ref().map(|v| v.last_dts_us)
    }

    /// Adds one video access unit.
    pub fn write_video(&mut self, f: VideoFrame<'_>) -> Result<()> {
        if f.pts_us < f.dts_us {
            return Err(Error::NonMonotonicTimestamp);
        }
        let codec = self.codec;
        let nals = split_annexb(f.data)?;
        let mut sets = ParamSets::default();
        let mut kept: Vec<&[u8]> = Vec::new();
        for n in nals {
            match classify(codec, n) {
                NalKind::Vps => ParamSets::push_unique(&mut sets.vps, n),
                NalKind::Sps => ParamSets::push_unique(&mut sets.sps, n),
                NalKind::Pps => ParamSets::push_unique(&mut sets.pps, n),
                NalKind::Drop => {}
                NalKind::Keep => kept.push(n),
            }
        }
        let dts = us_to_ticks(f.dts_us);
        let pts = us_to_ticks(f.pts_us);
        if let Some(v) = &self.video
            && dts <= v.last_dts_ticks
        {
            return Err(Error::NonMonotonicTimestamp);
        }

        // Sample description: new one when the in-band sets differ.
        let mut new_entry = None;
        {
            let current = self.video.as_ref().and_then(|v| v.entries.last());
            let mut cand = ParamSets {
                vps: sets.vps.clone(),
                sps: sets.sps.clone(),
                pps: sets.pps.clone(),
            };
            let have_any = !(cand.sps.is_empty() && cand.pps.is_empty() && cand.vps.is_empty());
            if let Some(cur) = current {
                if cand.vps.is_empty() {
                    cand.vps = cur.sets.vps.clone();
                }
                if cand.sps.is_empty() {
                    cand.sps = cur.sets.sps.clone();
                }
                if cand.pps.is_empty() {
                    cand.pps = cur.sets.pps.clone();
                }
            } else if cand.sps.is_empty() || cand.pps.is_empty() {
                return Err(Error::MissingParameterSets);
            }
            if have_any && current.is_none_or(|c| c.sets != cand) {
                new_entry = Some(build_entry(codec, cand)?);
            }
        }
        if kept.is_empty() {
            // Parameter sets only (or nothing): nothing to store as a sample.
            return Ok(());
        }

        let mut flush_old = false;
        let v = self.video.get_or_insert_with(|| VideoTrack {
            codec,
            entries: Vec::new(),
            sizes: Vec::new(),
            stts: Vec::new(),
            ctts: Vec::new(),
            ctts_any: false,
            sync: Vec::new(),
            chunks: Vec::new(),
            pending: Pending::new(),
            first_dts_ticks: dts,
            last_dts_ticks: dts,
            last_dts_us: f.dts_us,
            last_delta: 0,
            min_pts_ticks: pts,
            min_pts_us: f.pts_us,
            max_pts_ticks: pts,
        });
        debug_assert_eq!(v.codec, codec);
        if let Some(e) = new_entry {
            if !v.entries.is_empty() {
                flush_old = true;
            }
            if flush_old {
                flush(&mut self.out, &mut self.pos, &mut v.pending, &mut v.chunks)?;
            }
            v.entries.push(e);
        }
        let first = v.sizes.is_empty();
        if !first {
            let delta = u32::try_from(dts - v.last_dts_ticks).map_err(|_| Error::TooLarge)?;
            push_run(&mut v.stts, delta);
            v.last_delta = delta;
        }
        if v.pending.samples == 0 {
            v.pending.first_ticks = dts;
            v.pending.entry = v.entries.len() as u32;
        }
        let before = v.pending.buf.len();
        for n in &kept {
            put_u32(&mut v.pending.buf, n.len() as u32);
            v.pending.buf.extend_from_slice(n);
        }
        let size = u32::try_from(v.pending.buf.len() - before).map_err(|_| Error::TooLarge)?;
        v.pending.samples += 1;
        v.sizes.push(size);
        if f.keyframe {
            v.sync.push(v.sizes.len() as u32);
        }
        let off = u32::try_from(pts - dts).map_err(|_| Error::TooLarge)?;
        v.ctts_any |= off != 0;
        push_run(&mut v.ctts, off);
        v.last_dts_ticks = dts;
        v.last_dts_us = f.dts_us;
        if pts < v.min_pts_ticks {
            v.min_pts_ticks = pts;
            v.min_pts_us = f.pts_us;
        }
        v.max_pts_ticks = v.max_pts_ticks.max(pts);
        if dts - v.pending.first_ticks >= CHUNK_TICKS || v.pending.buf.len() >= CHUNK_BYTES {
            flush(&mut self.out, &mut self.pos, &mut v.pending, &mut v.chunks)?;
        }
        Ok(())
    }

    /// Appends one ADTS frame, timed by sample count right after the
    /// previous audio frame.
    pub fn write_audio(&mut self, adts: &[u8]) -> Result<()> {
        self.audio_frame(adts, None).map(|_| ())
    }

    /// Appends one ADTS frame with a presentation time (µs, video clock).
    ///
    /// A frame earlier than the end of the audio written so far (by more than
    /// half a frame) is a duplicate and is dropped. A frame later than that
    /// by more than half a frame leaves a gap, which is filled with silent
    /// AAC frames so the audio track stays one continuous run (mono AAC-LC
    /// only; other formats get [`Error::UnsupportedAudioGap`]). Returns
    /// whether the frame was stored.
    pub fn write_audio_at(&mut self, adts: &[u8], ts_us: i64) -> Result<bool> {
        self.audio_frame(adts, Some(ts_us))
    }

    fn audio_frame(&mut self, adts: &[u8], ts_us: Option<i64>) -> Result<bool> {
        let (cfg, payload) = parse_adts(adts)?;
        let default_start = self.opts.audio_start_us;
        let a = self.audio.get_or_insert_with(|| AudioTrack {
            config: cfg,
            sizes: Vec::new(),
            chunks: Vec::new(),
            pending: Pending::new(),
            start_us: ts_us.or(default_start),
            total_bytes: 0,
            max_frame: 0,
        });
        if a.config != cfg {
            return Err(Error::AudioConfigChanged);
        }
        let rate = i64::from(cfg.sample_rate);
        let frame_us = i64::from(SAMPLES_PER_FRAME) * 1_000_000 / rate;
        let total = a.sizes.len() as i64 * i64::from(SAMPLES_PER_FRAME);
        let mut silent_frames = 0;
        if let (Some(ts), Some(start)) = (ts_us, a.start_us) {
            let expected = start + total * 1_000_000 / rate;
            if ts < expected - frame_us / 2 {
                return Ok(false);
            }
            if ts > expected + frame_us / 2 {
                silent_frames = (ts - expected + frame_us / 2) / frame_us;
                if silent_frames > MAX_GAP_FRAMES {
                    return Err(Error::TooLarge);
                }
                if cfg.object_type != 2 || cfg.channels != 1 {
                    return Err(Error::UnsupportedAudioGap);
                }
            }
        }
        for _ in 0..silent_frames {
            a.push(&mut self.out, &mut self.pos, &SILENT_AAC_LC_MONO)?;
        }
        a.push(&mut self.out, &mut self.pos, payload)?;
        Ok(true)
    }

    /// Writes the index, finalizes the file, and returns the sink.
    pub fn finish(mut self) -> Result<(W, Summary)> {
        let mut video = self.video.take().ok_or(Error::NoVideo)?;
        flush(
            &mut self.out,
            &mut self.pos,
            &mut video.pending,
            &mut video.chunks,
        )?;
        if let Some(a) = &mut self.audio {
            flush(&mut self.out, &mut self.pos, &mut a.pending, &mut a.chunks)?;
        }
        if video.sizes.len() == 1 {
            video.last_delta = DEFAULT_SAMPLE_TICKS;
        }
        push_run(&mut video.stts, video.last_delta);
        let plan = Plan::new(&video, self.audio.as_ref(), &self.opts)?;

        // mdat size
        let mdat_len = self.pos - self.mdat_start;
        self.out.seek(SeekFrom::Start(self.mdat_start + 8))?;
        self.out.write_all(&mdat_len.to_be_bytes())?;

        let max_offset = video
            .chunks
            .iter()
            .chain(self.audio.iter().flat_map(|a| a.chunks.iter()))
            .map(|c| c.offset)
            .max()
            .unwrap_or(0);
        let (mut shift, mut co64) = (0u64, false);
        let mut moov;
        let mut rounds = 0;
        loop {
            moov = build_moov(&video, self.audio.as_ref(), &plan, &self.opts, shift, co64);
            let new_shift = if self.opts.faststart {
                moov.len() as u64
            } else {
                0
            };
            let new_co64 = max_offset + new_shift > u64::from(u32::MAX) || co64;
            if new_shift == shift && new_co64 == co64 {
                break;
            }
            shift = new_shift;
            co64 = new_co64;
            rounds += 1;
            assert!(rounds < 8, "moov size did not converge");
        }

        let end = self.pos;
        if self.opts.faststart {
            let len = end - self.mdat_start;
            let mut buf = vec![0u8; 1 << 20];
            let mut remaining = len;
            while remaining > 0 {
                let n = remaining.min(buf.len() as u64) as usize;
                let from = self.mdat_start + remaining - n as u64;
                self.out.seek(SeekFrom::Start(from))?;
                self.out.read_exact(&mut buf[..n])?;
                self.out.seek(SeekFrom::Start(from + shift))?;
                self.out.write_all(&buf[..n])?;
                remaining -= n as u64;
            }
            self.out.seek(SeekFrom::Start(self.mdat_start))?;
            self.out.write_all(&moov)?;
        } else {
            self.out.seek(SeekFrom::Start(end))?;
            self.out.write_all(&moov)?;
        }
        self.out.flush()?;
        let summary = Summary {
            file_size: end + moov.len() as u64,
            video_samples: video.sizes.len() as u64,
            audio_samples: self.audio.as_ref().map_or(0, |a| a.sizes.len() as u64),
            video_entries: video.entries.len(),
            uses_co64: co64,
            moov_size: moov.len() as u64,
        };
        Ok((self.out, summary))
    }
}

pub(crate) fn build_entry(codec: VideoCodec, sets: ParamSets) -> Result<VideoEntry> {
    let sps = sets.sps.first().ok_or(Error::MissingParameterSets)?;
    if sets.pps.is_empty() {
        return Err(Error::MissingParameterSets);
    }
    let (sample_entry, width, height) = match codec {
        VideoCodec::H264 => {
            let p = parse_h264_sps(sps)?;
            (avc1_entry(&sets, &p), p.width, p.height)
        }
        VideoCodec::H265 => {
            let p = parse_h265_sps(sps)?;
            (hvc1_entry(&sets, &p), p.width, p.height)
        }
    };
    Ok(VideoEntry {
        sets,
        sample_entry,
        width,
        height,
    })
}

fn flush<W: Write>(
    out: &mut W,
    pos: &mut u64,
    p: &mut Pending,
    chunks: &mut Vec<ChunkRec>,
) -> Result<()> {
    if p.samples == 0 {
        return Ok(());
    }
    out.write_all(&p.buf)?;
    chunks.push(ChunkRec {
        offset: *pos,
        samples: p.samples,
        entry: p.entry,
    });
    *pos += p.buf.len() as u64;
    p.buf.clear();
    p.samples = 0;
    Ok(())
}

/// One edit list entry in movie ticks.
#[derive(Clone, Copy)]
pub(crate) struct Edit {
    pub(crate) duration: u64,
    /// -1 for an empty edit.
    pub(crate) media_time: i64,
}

struct Plan {
    video_edits: Vec<Edit>,
    audio_edits: Vec<Edit>,
    video_media_duration: u64,
    audio_media_duration: u64,
}

impl Plan {
    fn new(video: &VideoTrack, audio: Option<&AudioTrack>, _opts: &Options) -> Result<Self> {
        let video_media_duration =
            (video.last_dts_ticks - video.first_dts_ticks) as u64 + u64::from(video.last_delta);
        let media_time = video.min_pts_ticks - video.first_dts_ticks;
        // The presented span: from the earliest presentation time to the
        // latest plus one frame. (Media duration minus the offset underflows
        // when presentation offsets exceed the media duration.)
        let video_presented =
            (video.max_pts_ticks - video.min_pts_ticks) as u64 + u64::from(video.last_delta);
        let mut audio_media_duration = 0;
        let mut audio_start = None;
        if let Some(a) = audio {
            audio_media_duration = a.sizes.len() as u64 * u64::from(SAMPLES_PER_FRAME);
            audio_start = Some(us_to_ticks(a.start_us.unwrap_or(video.min_pts_us)));
        }
        let t0 = video.min_pts_ticks.min(audio_start.unwrap_or(i64::MAX));
        let mut video_edits = Vec::new();
        if video.min_pts_ticks > t0 {
            video_edits.push(Edit {
                duration: (video.min_pts_ticks - t0) as u64,
                media_time: -1,
            });
        }
        video_edits.push(Edit {
            duration: video_presented,
            media_time,
        });
        let mut audio_edits = Vec::new();
        if let Some(start) = audio_start {
            if start > t0 {
                audio_edits.push(Edit {
                    duration: (start - t0) as u64,
                    media_time: -1,
                });
            }
            let rate = u64::from(audio.map_or(1, |a| a.config.sample_rate));
            audio_edits.push(Edit {
                duration: audio_media_duration * u64::from(TIMESCALE) / rate,
                media_time: 0,
            });
        }
        Ok(Plan {
            video_edits,
            audio_edits,
            video_media_duration,
            audio_media_duration,
        })
    }
}

fn edts(buf: &mut Vec<u8>, edits: &[Edit]) {
    let big = edits
        .iter()
        .any(|e| e.duration > u64::from(u32::MAX) || e.media_time > i64::from(i32::MAX));
    bx(buf, b"edts", |b| {
        full(b, b"elst", u8::from(big), 0, |b| {
            put_u32(b, edits.len() as u32);
            for e in edits {
                if big {
                    put_u64(b, e.duration);
                    put_u64(b, e.media_time as u64);
                } else {
                    put_u32(b, e.duration as u32);
                    put_u32(b, e.media_time as i32 as u32);
                }
                put_u16(b, 1); // media_rate integer
                put_u16(b, 0);
            }
        });
    });
}

pub(crate) const MATRIX: [u32; 9] = [0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000];

pub(crate) struct TrackInfo<'a> {
    pub(crate) id: u32,
    pub(crate) kind: &'a [u8; 4],
    pub(crate) handler_name: &'a [u8],
    pub(crate) timescale: u32,
    pub(crate) media_duration: u64,
    pub(crate) edits: &'a [Edit],
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) volume: u16,
    pub(crate) tables: Tables<'a>,
}

pub(crate) fn trak(buf: &mut Vec<u8>, t: &TrackInfo<'_>, created: u64) {
    let track_dur: u64 = t.edits.iter().map(|e| e.duration).sum();
    let big = track_dur > u64::from(u32::MAX) || t.media_duration > u64::from(u32::MAX);
    let v = u8::from(big);
    bx(buf, b"trak", |b| {
        full(b, b"tkhd", v, 3, |b| {
            if big {
                put_u64(b, created);
                put_u64(b, created);
                put_u32(b, t.id);
                put_u32(b, 0);
                put_u64(b, track_dur);
            } else {
                put_u32(b, created as u32);
                put_u32(b, created as u32);
                put_u32(b, t.id);
                put_u32(b, 0);
                put_u32(b, track_dur as u32);
            }
            put_u64(b, 0);
            put_u16(b, 0); // layer
            put_u16(b, 0); // alternate group
            put_u16(b, t.volume);
            put_u16(b, 0);
            for m in MATRIX {
                put_u32(b, m);
            }
            put_u32(b, t.width << 16);
            put_u32(b, t.height << 16);
        });
        if !t.edits.is_empty() && !(t.edits.len() == 1 && t.edits[0].media_time == 0) {
            edts(b, t.edits);
        }
        bx(b, b"mdia", |b| {
            full(b, b"mdhd", v, 0, |b| {
                if big {
                    put_u64(b, created);
                    put_u64(b, created);
                    put_u32(b, t.timescale);
                    put_u64(b, t.media_duration);
                } else {
                    put_u32(b, created as u32);
                    put_u32(b, created as u32);
                    put_u32(b, t.timescale);
                    put_u32(b, t.media_duration as u32);
                }
                put_u16(b, 0x55c4); // "und"
                put_u16(b, 0);
            });
            full(b, b"hdlr", 0, 0, |b| {
                put_u32(b, 0);
                b.extend_from_slice(t.kind);
                b.extend_from_slice(&[0; 12]);
                b.extend_from_slice(t.handler_name);
                b.push(0);
            });
            bx(b, b"minf", |b| {
                if t.kind == b"vide" {
                    full(b, b"vmhd", 0, 1, |b| b.extend_from_slice(&[0; 8]));
                } else {
                    full(b, b"smhd", 0, 0, |b| b.extend_from_slice(&[0; 4]));
                }
                bx(b, b"dinf", |b| {
                    full(b, b"dref", 0, 0, |b| {
                        put_u32(b, 1);
                        full(b, b"url ", 0, 1, |_| {});
                    });
                });
                stbl(b, &t.tables);
            });
        });
    });
}

fn build_moov(
    video: &VideoTrack,
    audio: Option<&AudioTrack>,
    plan: &Plan,
    opts: &Options,
    shift: u64,
    co64: bool,
) -> Vec<u8> {
    let created = if opts.creation_unix_secs == 0 {
        0
    } else {
        opts.creation_unix_secs + EPOCH_1904
    };
    let entries: Vec<Vec<u8>> = video
        .entries
        .iter()
        .map(|e| e.sample_entry.clone())
        .collect();
    let vstsd = stsd(&entries);
    let all_sync = video.sync.len() == video.sizes.len();
    let first = &video.entries[0];
    let vtrack = TrackInfo {
        id: 1,
        kind: b"vide",
        handler_name: b"VideoHandler",
        timescale: TIMESCALE,
        media_duration: plan.video_media_duration,
        edits: &plan.video_edits,
        width: first.width,
        height: first.height,
        volume: 0,
        tables: Tables {
            stsd: &vstsd,
            stts: &video.stts,
            ctts: video.ctts_any.then_some(&video.ctts[..]),
            sizes: &video.sizes,
            sync: (!all_sync).then_some(&video.sync[..]),
            chunks: &video.chunks,
            shift,
            co64,
        },
    };
    let audio = audio.filter(|a| !a.sizes.is_empty());
    let audio_stsd = audio.map(|a| {
        let secs =
            (a.sizes.len() as f64 * f64::from(SAMPLES_PER_FRAME)) / f64::from(a.config.sample_rate);
        let avg = (a.total_bytes as f64 * 8.0 / secs) as u32;
        let max = (f64::from(a.max_frame) * 8.0 * f64::from(a.config.sample_rate)
            / f64::from(SAMPLES_PER_FRAME)) as u32;
        stsd(&[mp4a_entry(&a.config, avg, max)])
    });
    let audio_stts = audio.map(|a| [(a.sizes.len() as u32, SAMPLES_PER_FRAME)]);
    let atrack = audio.map(|a| TrackInfo {
        id: 2,
        kind: b"soun",
        handler_name: b"SoundHandler",
        timescale: a.config.sample_rate,
        media_duration: plan.audio_media_duration,
        edits: &plan.audio_edits,
        width: 0,
        height: 0,
        volume: 0x0100,
        tables: Tables {
            stsd: audio_stsd.as_deref().expect("audio entry"),
            stts: audio_stts.as_ref().expect("audio stts"),
            ctts: None,
            sizes: &a.sizes,
            sync: None,
            chunks: &a.chunks,
            shift,
            co64,
        },
    });
    let movie_dur = [
        Some(&plan.video_edits),
        atrack.as_ref().map(|_| &plan.audio_edits),
    ]
    .into_iter()
    .flatten()
    .map(|e| e.iter().map(|x| x.duration).sum::<u64>())
    .max()
    .unwrap_or(0);
    let big = movie_dur > u64::from(u32::MAX);
    let mut out = Vec::new();
    bx(&mut out, b"moov", |b| {
        full(b, b"mvhd", u8::from(big), 0, |b| {
            if big {
                put_u64(b, created);
                put_u64(b, created);
                put_u32(b, TIMESCALE);
                put_u64(b, movie_dur);
            } else {
                put_u32(b, created as u32);
                put_u32(b, created as u32);
                put_u32(b, TIMESCALE);
                put_u32(b, movie_dur as u32);
            }
            put_u32(b, 0x0001_0000); // rate
            put_u16(b, 0x0100); // volume
            b.extend_from_slice(&[0; 10]);
            for m in MATRIX {
                put_u32(b, m);
            }
            b.extend_from_slice(&[0; 24]);
            put_u32(b, if atrack.is_some() { 3 } else { 2 }); // next_track_ID
        });
        trak(b, &vtrack, created);
        if let Some(a) = &atrack {
            trak(b, a, created);
        }
    });
    out
}
