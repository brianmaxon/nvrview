//! Streaming fragmenter: init segment plus `moof`/`mdat` media segments.
//!
//! Written from ISO/IEC 14496-12 (movie fragments), -14 and -15 (sample
//! entries). See the crate documentation for the layout decisions.

use crate::adts::{AacConfig, SAMPLES_PER_FRAME, parse_adts};
use crate::boxes::mp4a_entry;
use crate::boxes::{Tables, bx, full, put_u16, put_u32, put_u64, stsd};
use crate::error::{Error, Result};
use crate::nal::{
    NalKind, ParamSets, VideoCodec, classify, parse_h264_sps, parse_h265_sps, split_annexb,
};
use crate::track::{AudioTrackSpec, VIDEO_HANDLER_NAME};
use crate::writer::{
    EPOCH_1904, Edit, MATRIX, TIMESCALE, TrackInfo, VideoEntry, VideoFrame, build_entry,
    check_raw_au, check_track_count, trak, us_to_ticks,
};

/// Track ID of the video track in every init and media segment.
pub const VIDEO_TRACK_ID: u32 = 1;
/// Track ID of the first audio track; further audio tracks follow (3, 4,
/// ...) in declaration order.
pub const AUDIO_TRACK_ID: u32 = 2;

/// Sample flags (ISO/IEC 14496-12, sample flags): depends on no other sample.
const FLAGS_SYNC: u32 = 0x0200_0000;
/// Depends on others and is not a sync sample.
const FLAGS_NON_SYNC: u32 = 0x0101_0000;
/// Duration assumed for a lone final video sample (25 fps).
const DEFAULT_SAMPLE_TICKS: u32 = TIMESCALE / 25;
/// Most audio bytes held (all tracks together) while waiting for video to
/// cut a fragment.
const MAX_AUDIO_BYTES: usize = 1 << 20;

/// Where audio goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AudioLayout {
    /// One init segment with both tracks; each media segment is one `moof`
    /// with a `traf` per track and one `mdat` (video samples, then audio).
    /// One `SourceBuffer`.
    #[default]
    Muxed,
    /// Two streams: a video-only init and segments, an audio-only init and
    /// segments (CMAF style, one track per fragment). Two `SourceBuffer`s;
    /// segments carry [`SegmentTrack`] so a sender can route them.
    Separate,
}

/// What to do when video parameter sets change at a keyframe.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChangePolicy {
    /// Close the fragment, emit a new init segment (flagged `reset`), and go
    /// on. Right for live delivery.
    #[default]
    NewInit,
    /// Return [`Error::ParameterSetsChanged`] without consuming the frame.
    /// Right for files, which have one `moov`.
    Reject,
}

/// Fragmenter options.
#[derive(Clone, Debug)]
pub struct FragmentOptions {
    /// A fragment is closed when the next frame is a keyframe or when it
    /// would span at least this long (microseconds).
    pub max_fragment_us: i64,
    /// A fragment is also closed when its video data reaches this many bytes.
    pub max_fragment_bytes: usize,
    /// Audio format of the first audio track, as [`AudioTrackSpec::new`]
    /// (the single-track API); `None` means no such track. ADTS frames
    /// pushed while there is no audio track at all are ignored.
    pub audio: Option<AacConfig>,
    /// Further audio tracks, declared up front: they follow the `audio`
    /// track if there is one (track IDs continue from it) and are fed with
    /// [`Fragmenter::push_audio_raw`] by index. At most
    /// [`MAX_AUDIO_TRACKS`](crate::MAX_AUDIO_TRACKS) audio tracks in all.
    pub audio_tracks: Vec<AudioTrackSpec>,
    pub audio_layout: AudioLayout,
    pub on_change: ChangePolicy,
    /// Creation time for the headers, seconds since the Unix epoch.
    pub creation_unix_secs: u64,
}

impl Default for FragmentOptions {
    /// Live delivery: fragments of at most half a second.
    fn default() -> Self {
        Self {
            max_fragment_us: 500_000,
            max_fragment_bytes: 4 << 20,
            audio: None,
            audio_tracks: Vec::new(),
            audio_layout: AudioLayout::Muxed,
            on_change: ChangePolicy::NewInit,
            creation_unix_secs: 0,
        }
    }
}

impl FragmentOptions {
    /// Files: fragments of up to two seconds, so the per-fragment overhead
    /// stays small while a crash loses at most that much.
    pub fn for_download() -> Self {
        Self {
            max_fragment_us: 2_000_000,
            on_change: ChangePolicy::Reject,
            ..Self::default()
        }
    }
}

/// Init segment or media segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentKind {
    Init,
    Media,
}

/// Which `SourceBuffer` a segment belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentTrack {
    /// Video and audio in one buffer ([`AudioLayout::Muxed`], or video-only
    /// streams).
    Muxed,
    Video,
    Audio,
}

/// Facts about an init segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitInfo {
    /// MIME type with codecs for `MediaSource.isTypeSupported` and
    /// `addSourceBuffer`, such as `video/mp4; codecs="avc1.640028,mp4a.40.2"`.
    pub mime: String,
    /// Picture size (video inits); 0 for an audio-only init.
    pub width: u32,
    pub height: u32,
    /// True when this init replaces an earlier one (parameter sets or
    /// resolution changed). The client appends it to the same
    /// `SourceBuffer` in stream order, after the previous generation's media
    /// segments, calling `changeType` first when `mime` differs from the
    /// previous init's (see the crate documentation).
    pub reset: bool,
}

/// One unit to send: bytes ready for `appendBuffer`.
#[derive(Clone, Debug)]
pub struct Segment {
    pub kind: SegmentKind,
    pub track: SegmentTrack,
    /// Increases by one at each new init segment; media segments carry the
    /// generation of the init they belong to.
    pub generation: u32,
    pub data: Vec<u8>,
    /// Media: start and length of the video (or audio, for audio-only
    /// segments) in microseconds since the first frame.
    pub start_us: i64,
    pub duration_us: i64,
    /// Media: the first video sample is a sync sample, so a client that
    /// joins late can start here (after the current init). Audio-only
    /// segments are always true.
    pub starts_with_keyframe: bool,
    /// Media: `tfdt` base decode times (video in 90 kHz ticks, audio in
    /// sample-rate ticks) of the track fragments present. `audio_tfdt` is
    /// the first audio track's ([`AUDIO_TRACK_ID`]).
    pub video_tfdt: Option<u64>,
    pub audio_tfdt: Option<u64>,
    /// Media: (track ID, `tfdt`) of every audio track fragment present, in
    /// the order of the `traf` boxes.
    pub audio_tfdts: Vec<(u32, u64)>,
    /// Init only.
    pub init: Option<InitInfo>,
}

/// Counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FragmentStats {
    pub video_samples: u64,
    pub audio_samples: u64,
    pub fragments: u64,
    pub inits: u32,
    /// Video frames dropped before the first keyframe.
    pub leading_skipped: u64,
    /// Audio frames dropped: before video started, duplicates, no audio
    /// track configured, or buffer full.
    pub audio_dropped: u64,
}

struct VSample {
    size: u32,
    dts: i64,
    pts: i64,
    key: bool,
}

struct ASample {
    start: u64,
    dur: u32,
    size: u32,
}

/// Audio of one track waiting for video to cut a fragment.
#[derive(Default)]
struct AState {
    samples: Vec<ASample>,
    data: Vec<u8>,
    /// Sample time where the next frame is expected.
    next: Option<u64>,
}

/// Cuts a stream of access units and ADTS frames into fragments.
///
/// At most one fragment of video (see [`FragmentOptions`]) and up to 1 MiB
/// of audio (all tracks together) are held at any time.
pub struct Fragmenter {
    codec: VideoCodec,
    opts: FragmentOptions,
    entry: Option<VideoEntry>,
    video_codec_string: String,
    generation: u32,
    inits: Vec<Segment>,
    seq: u32,
    /// Decode time (90 kHz) of the first frame; all times are relative.
    origin_ticks: i64,
    origin_us: i64,
    started: bool,
    vsamples: Vec<VSample>,
    vdata: Vec<u8>,
    last_dts: i64,
    last_delta: u32,
    /// Every audio track: `opts.audio` first, then `opts.audio_tracks`.
    tracks: Vec<AudioTrackSpec>,
    audio: Vec<AState>,
    stats: FragmentStats,
}

impl Fragmenter {
    pub fn new(codec: VideoCodec, opts: FragmentOptions) -> Self {
        let tracks: Vec<AudioTrackSpec> = opts
            .audio
            .map(AudioTrackSpec::new)
            .into_iter()
            .chain(opts.audio_tracks.iter().copied())
            .collect();
        let audio = tracks.iter().map(|_| AState::default()).collect();
        Self {
            codec,
            opts,
            entry: None,
            video_codec_string: String::new(),
            generation: 0,
            inits: Vec::new(),
            seq: 0,
            origin_ticks: 0,
            origin_us: 0,
            started: false,
            vsamples: Vec::new(),
            vdata: Vec::new(),
            last_dts: 0,
            last_delta: 0,
            tracks,
            audio,
            stats: FragmentStats::default(),
        }
    }

    pub fn stats(&self) -> FragmentStats {
        self.stats
    }

    /// Every audio track, in track order (IDs from [`AUDIO_TRACK_ID`]).
    pub fn audio_tracks(&self) -> &[AudioTrackSpec] {
        &self.tracks
    }

    /// The init segment(s) in effect (one, or two for separate audio), for
    /// replay to a client that joins later. Empty before the first keyframe.
    pub fn current_init(&self) -> &[Segment] {
        &self.inits
    }

    /// Adds one video access unit; returns the segments that became ready,
    /// in the order to send them.
    ///
    /// Frames before the first keyframe are dropped. The first keyframe must
    /// carry parameter sets in band.
    pub fn push_video(&mut self, f: VideoFrame<'_>) -> Result<Vec<Segment>> {
        if f.pts_us < f.dts_us {
            return Err(Error::NonMonotonicTimestamp);
        }
        let codec = self.codec;
        let mut sets = ParamSets::default();
        let mut kept: Vec<&[u8]> = Vec::new();
        for n in split_annexb(f.data)? {
            match classify(codec, n) {
                NalKind::Vps => ParamSets::push_unique(&mut sets.vps, n),
                NalKind::Sps => ParamSets::push_unique(&mut sets.sps, n),
                NalKind::Pps => ParamSets::push_unique(&mut sets.pps, n),
                NalKind::Drop => {}
                NalKind::Keep => kept.push(n),
            }
        }
        let dts_abs = us_to_ticks(f.dts_us);
        if self.started && dts_abs - self.origin_ticks <= self.last_dts {
            return Err(Error::NonMonotonicTimestamp);
        }
        if kept.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut cut_done = false;
        if !self.started {
            check_track_count(self.tracks.len())?;
            if !f.keyframe {
                self.stats.leading_skipped += 1;
                return Ok(out);
            }
            if sets.sps.is_empty() || sets.pps.is_empty() {
                return Err(Error::MissingParameterSets);
            }
            let entry = build_entry(codec, sets)?;
            self.origin_ticks = dts_abs;
            self.origin_us = f.dts_us;
            self.started = true;
            self.install_entry(entry, false)?;
            out.extend(self.inits.iter().cloned());
        } else if f.keyframe
            && (!sets.sps.is_empty() || !sets.pps.is_empty() || !sets.vps.is_empty())
        {
            let cur = &self.entry.as_ref().expect("started").sets;
            let mut cand = sets;
            if cand.vps.is_empty() {
                cand.vps = cur.vps.clone();
            }
            if cand.sps.is_empty() {
                cand.sps = cur.sps.clone();
            }
            if cand.pps.is_empty() {
                cand.pps = cur.pps.clone();
            }
            if &cand != cur {
                if self.opts.on_change == ChangePolicy::Reject {
                    return Err(Error::ParameterSetsChanged);
                }
                let entry = build_entry(codec, cand)?;
                self.flush_fragment(dts_abs - self.origin_ticks, false, &mut out)?;
                self.install_entry(entry, true)?;
                out.extend(self.inits.iter().cloned());
                cut_done = true;
            }
        }
        let rel = dts_abs - self.origin_ticks;
        if !cut_done && !self.vsamples.is_empty() {
            let span = rel - self.vsamples[0].dts;
            let max = us_to_ticks(self.opts.max_fragment_us).max(1);
            if f.keyframe || span >= max || self.vdata.len() >= self.opts.max_fragment_bytes {
                self.flush_fragment(rel, false, &mut out)?;
            }
        }
        let before = self.vdata.len();
        for n in &kept {
            put_u32(&mut self.vdata, n.len() as u32);
            self.vdata.extend_from_slice(n);
        }
        let size = u32::try_from(self.vdata.len() - before).map_err(|_| Error::TooLarge)?;
        if self.stats.video_samples > 0 {
            self.last_delta = u32::try_from(rel - self.last_dts).map_err(|_| Error::TooLarge)?;
        }
        self.vsamples.push(VSample {
            size,
            dts: rel,
            pts: us_to_ticks(f.pts_us) - self.origin_ticks,
            key: f.keyframe,
        });
        self.last_dts = rel;
        self.stats.video_samples += 1;
        Ok(out)
    }

    /// Adds one ADTS frame, timed right after the previous audio frame.
    /// Returns whether it was kept.
    pub fn push_audio(&mut self, adts: &[u8]) -> Result<bool> {
        self.audio_frame(adts, None)
    }

    /// Adds one ADTS frame with a time on the video clock (microseconds).
    ///
    /// A frame earlier than the end of the audio so far (by more than half a
    /// frame) is a duplicate and is dropped. A later one leaves a gap: the
    /// previous frame's duration is extended over it (when it is in the same
    /// fragment) or the next fragment's `tfdt` jumps; no silence is made.
    pub fn push_audio_at(&mut self, adts: &[u8], ts_us: i64) -> Result<bool> {
        self.audio_frame(adts, Some(ts_us))
    }

    /// Adds one raw AAC access unit to audio track `track` (an index into
    /// [`audio_tracks`](Self::audio_tracks)), with a time on the video clock
    /// (microseconds) or, with `None`, right after the track's previous
    /// frame. Duplicates and gaps are handled per track as in
    /// [`push_audio_at`](Self::push_audio_at). Returns whether it was kept.
    pub fn push_audio_raw(&mut self, track: usize, au: &[u8], ts_us: Option<i64>) -> Result<bool> {
        if track >= self.tracks.len() {
            return Err(Error::InvalidTrack("audio track not declared"));
        }
        check_raw_au(au)?;
        self.audio_payload(track, au, ts_us)
    }

    fn audio_frame(&mut self, adts: &[u8], ts_us: Option<i64>) -> Result<bool> {
        let Some(spec) = self.tracks.first() else {
            self.stats.audio_dropped += 1;
            return Ok(false);
        };
        let (got, payload) = parse_adts(adts)?;
        if got != spec.config {
            return Err(Error::AudioConfigChanged);
        }
        self.audio_payload(0, payload, ts_us)
    }

    fn audio_payload(&mut self, track: usize, payload: &[u8], ts_us: Option<i64>) -> Result<bool> {
        let buffered: usize = self.audio.iter().map(|a| a.data.len()).sum();
        if !self.started || buffered >= MAX_AUDIO_BYTES {
            self.stats.audio_dropped += 1;
            return Ok(false);
        }
        let rate = i64::from(self.tracks[track].config.sample_rate);
        let half = i64::from(SAMPLES_PER_FRAME) / 2;
        let to_samples = |us: i64| us * rate / 1_000_000;
        let st = &mut self.audio[track];
        let start = match (ts_us, st.next) {
            (Some(ts), expected) => {
                let rel = to_samples(ts - self.origin_us);
                if rel < -half {
                    self.stats.audio_dropped += 1;
                    return Ok(false);
                }
                let rel = rel.max(0) as u64;
                match expected {
                    Some(e) if (rel as i64) < e as i64 - half => {
                        self.stats.audio_dropped += 1;
                        return Ok(false);
                    }
                    Some(e) if (rel as i64) <= e as i64 + half => e,
                    _ => rel,
                }
            }
            (None, Some(e)) => e,
            (None, None) => {
                let now = to_samples(self.last_dts * 1_000_000 / i64::from(TIMESCALE));
                now.max(0) as u64
            }
        };
        if let Some(prev) = st.samples.last_mut() {
            prev.dur = u32::try_from(start - prev.start).map_err(|_| Error::TooLarge)?;
        }
        st.data.extend_from_slice(payload);
        st.samples.push(ASample {
            start,
            dur: SAMPLES_PER_FRAME,
            size: payload.len() as u32,
        });
        st.next = Some(start + u64::from(SAMPLES_PER_FRAME));
        self.stats.audio_samples += 1;
        Ok(true)
    }

    /// Closes the current fragment and returns it (with the audio still
    /// buffered). Call at the end of the stream.
    pub fn finish(mut self) -> Result<Vec<Segment>> {
        let mut out = Vec::new();
        if !self.vsamples.is_empty() {
            let delta = if self.last_delta == 0 {
                DEFAULT_SAMPLE_TICKS
            } else {
                self.last_delta
            };
            let end = self.last_dts + i64::from(delta);
            self.flush_fragment(end, true, &mut out)?;
        }
        Ok(out)
    }

    fn install_entry(&mut self, entry: VideoEntry, reset: bool) -> Result<()> {
        let sps = entry.sets.sps.first().ok_or(Error::MissingParameterSets)?;
        self.video_codec_string = match self.codec {
            VideoCodec::H264 => {
                format!("avc1.{:02X}{:02X}{:02X}", sps[1], sps[2], sps[3])
            }
            VideoCodec::H265 => hevc_codec_string(&parse_h265_sps(sps)?),
        };
        if self.codec == VideoCodec::H264 {
            // Validates the SPS (and keeps the error early).
            parse_h264_sps(sps)?;
        }
        if reset {
            self.generation += 1;
        }
        self.entry = Some(entry);
        self.inits = self.build_inits(reset);
        self.stats.inits += 1;
        Ok(())
    }

    fn build_inits(&self, reset: bool) -> Vec<Segment> {
        let entry = self.entry.as_ref().expect("entry");
        let mut audio_cs: Vec<String> = Vec::new();
        for t in &self.tracks {
            let c = format!("mp4a.40.{}", t.config.object_type);
            if !audio_cs.contains(&c) {
                audio_cs.push(c);
            }
        }
        let mk = |track: SegmentTrack, video: bool, audio: bool| {
            let mut cs = Vec::new();
            if video {
                cs.push(self.video_codec_string.clone());
            }
            if audio {
                cs.extend(audio_cs.clone());
            }
            let mime = format!(
                "{}/mp4; codecs=\"{}\"",
                if video { "video" } else { "audio" },
                cs.join(",")
            );
            Segment {
                kind: SegmentKind::Init,
                track,
                generation: self.generation,
                data: init_segment(
                    video.then_some(entry),
                    if audio { &self.tracks } else { &[] },
                    self.opts.creation_unix_secs,
                ),
                start_us: 0,
                duration_us: 0,
                starts_with_keyframe: false,
                video_tfdt: None,
                audio_tfdt: None,
                audio_tfdts: Vec::new(),
                init: Some(InitInfo {
                    mime,
                    width: if video { entry.width } else { 0 },
                    height: if video { entry.height } else { 0 },
                    reset,
                }),
            }
        };
        match (!self.tracks.is_empty(), self.opts.audio_layout) {
            (false, _) => vec![mk(SegmentTrack::Muxed, true, false)],
            (true, AudioLayout::Muxed) => vec![mk(SegmentTrack::Muxed, true, true)],
            (true, AudioLayout::Separate) => vec![
                mk(SegmentTrack::Video, true, false),
                mk(SegmentTrack::Audio, false, true),
            ],
        }
    }

    /// Emits the buffered video (ending at `end` ticks) and the audio that
    /// starts before it (all audio when `all_audio`).
    fn flush_fragment(&mut self, end: i64, all_audio: bool, out: &mut Vec<Segment>) -> Result<()> {
        if self.vsamples.is_empty() {
            return Ok(());
        }
        let vs = std::mem::take(&mut self.vsamples);
        let vdata = std::mem::take(&mut self.vdata);
        let mut vrecs = Vec::with_capacity(vs.len());
        for (i, s) in vs.iter().enumerate() {
            let next = vs.get(i + 1).map_or(end, |n| n.dts);
            vrecs.push(SampleRec {
                dur: u32::try_from(next - s.dts).map_err(|_| Error::TooLarge)?,
                size: s.size,
                flags: if s.key { FLAGS_SYNC } else { FLAGS_NON_SYNC },
                cto: i32::try_from(s.pts - s.dts).map_err(|_| Error::TooLarge)?,
            });
        }
        let vtfdt = vs[0].dts as u64;
        let mut audio_parts: Vec<AudioPart> = Vec::new();
        for (i, (spec, st)) in self.tracks.iter().zip(&mut self.audio).enumerate() {
            let rate = u64::from(spec.config.sample_rate);
            let take = if all_audio {
                st.samples.len()
            } else {
                let end = end as u64;
                st.samples
                    .partition_point(|a| a.start * u64::from(TIMESCALE) / rate < end)
            };
            if take == 0 {
                continue;
            }
            let asel: Vec<ASample> = st.samples.drain(..take).collect();
            let abytes: usize = asel.iter().map(|a| a.size as usize).sum();
            let adata: Vec<u8> = st.data.drain(..abytes).collect();
            let arecs: Vec<SampleRec> = asel
                .iter()
                .map(|a| SampleRec {
                    dur: a.dur,
                    size: a.size,
                    flags: FLAGS_SYNC,
                    cto: 0,
                })
                .collect();
            let tfdt = asel[0].start;
            let total: u64 = arecs.iter().map(|r| u64::from(r.dur)).sum();
            audio_parts.push(AudioPart {
                track_id: AUDIO_TRACK_ID + i as u32,
                recs: arecs,
                data: adata,
                tfdt,
                start_us: (tfdt * 1_000_000 / rate) as i64,
                duration_us: (total * 1_000_000 / rate) as i64,
            });
        }
        let starts_key = vs[0].key;
        let start_us = vtfdt as i64 * 1_000_000 / i64::from(TIMESCALE);
        let duration_us = (end - vs[0].dts) * 1_000_000 / i64::from(TIMESCALE);
        let video = TrafSpec {
            track_id: VIDEO_TRACK_ID,
            tfdt: vtfdt,
            samples: &vrecs,
            data: &vdata,
            cto: vrecs.iter().any(|r| r.cto != 0),
            flags: true,
        };
        let audio: Vec<TrafSpec<'_>> = audio_parts
            .iter()
            .map(|p| TrafSpec {
                track_id: p.track_id,
                tfdt: p.tfdt,
                samples: &p.recs,
                data: &p.data,
                cto: false,
                flags: false,
            })
            .collect();
        let generation = self.generation;
        let seg = |track, trafs: &[&TrafSpec<'_>], seq, start_us, duration_us, key| {
            Ok::<_, Error>(Segment {
                kind: SegmentKind::Media,
                track,
                generation,
                data: media_segment(seq, trafs)?,
                start_us,
                duration_us,
                starts_with_keyframe: key,
                video_tfdt: trafs
                    .iter()
                    .find(|t| t.track_id == VIDEO_TRACK_ID)
                    .map(|t| t.tfdt),
                audio_tfdt: trafs
                    .iter()
                    .find(|t| t.track_id == AUDIO_TRACK_ID)
                    .map(|t| t.tfdt),
                audio_tfdts: trafs
                    .iter()
                    .filter(|t| t.track_id != VIDEO_TRACK_ID)
                    .map(|t| (t.track_id, t.tfdt))
                    .collect(),
                init: None,
            })
        };
        match (audio.is_empty(), self.opts.audio_layout) {
            (false, AudioLayout::Muxed) => {
                self.seq += 1;
                let trafs: Vec<&TrafSpec<'_>> = std::iter::once(&video).chain(&audio).collect();
                out.push(seg(
                    SegmentTrack::Muxed,
                    &trafs,
                    self.seq,
                    start_us,
                    duration_us,
                    starts_key,
                )?);
            }
            (_, layout) => {
                self.seq += 1;
                let track = if !self.tracks.is_empty() && layout == AudioLayout::Separate {
                    SegmentTrack::Video
                } else {
                    SegmentTrack::Muxed
                };
                out.push(seg(
                    track,
                    &[&video],
                    self.seq,
                    start_us,
                    duration_us,
                    starts_key,
                )?);
                if !audio.is_empty() {
                    self.seq += 1;
                    // The span of the audio track fragments.
                    let (a_start, a_dur) = if let [one] = &audio_parts[..] {
                        (one.start_us, one.duration_us)
                    } else {
                        let first = audio_parts.iter().map(|p| p.start_us).min();
                        let last = audio_parts.iter().map(|p| p.start_us + p.duration_us).max();
                        let first = first.unwrap_or(0);
                        (first, last.unwrap_or(0) - first)
                    };
                    let trafs: Vec<&TrafSpec<'_>> = audio.iter().collect();
                    out.push(seg(
                        SegmentTrack::Audio,
                        &trafs,
                        self.seq,
                        a_start,
                        a_dur,
                        true,
                    )?);
                }
            }
        }
        self.stats.fragments += 1;
        Ok(())
    }
}

fn hevc_codec_string(s: &crate::nal::H265Sps) -> String {
    let space = match s.profile_space {
        0 => "",
        1 => "A",
        2 => "B",
        _ => "C",
    };
    let mut out = format!(
        "hvc1.{space}{}.{:X}.{}{}",
        s.profile_idc,
        s.compat_flags.reverse_bits(),
        if s.tier_flag == 1 { 'H' } else { 'L' },
        s.level_idc
    );
    let n = s
        .constraint_flags
        .iter()
        .rposition(|&b| b != 0)
        .map_or(0, |i| i + 1);
    for b in &s.constraint_flags[..n] {
        out.push_str(&format!(".{b:X}"));
    }
    out
}

// ---- box construction -------------------------------------------------------

/// One audio track's part of a fragment.
struct AudioPart {
    track_id: u32,
    recs: Vec<SampleRec>,
    data: Vec<u8>,
    tfdt: u64,
    /// Start and length in microseconds.
    start_us: i64,
    duration_us: i64,
}

struct SampleRec {
    dur: u32,
    size: u32,
    flags: u32,
    cto: i32,
}

struct TrafSpec<'a> {
    track_id: u32,
    tfdt: u64,
    samples: &'a [SampleRec],
    data: &'a [u8],
    /// Write composition time offsets.
    cto: bool,
    /// Write per-sample flags.
    flags: bool,
}

/// `ftyp` + `moov` with `mvex`; no samples.
pub(crate) fn init_segment(
    video: Option<&VideoEntry>,
    audio: &[AudioTrackSpec],
    creation_unix_secs: u64,
) -> Vec<u8> {
    let created = if creation_unix_secs == 0 {
        0
    } else {
        creation_unix_secs + EPOCH_1904
    };
    let mut out = Vec::new();
    bx(&mut out, b"ftyp", |b| {
        b.extend_from_slice(b"iso6");
        put_u32(b, 0);
        for brand in [b"iso6", b"iso5", b"isom", b"mp41"] {
            b.extend_from_slice(brand);
        }
    });
    let vstsd = video.map(|v| stsd(std::slice::from_ref(&v.sample_entry)));
    let astsd: Vec<Vec<u8>> = audio
        .iter()
        .map(|a| stsd(&[mp4a_entry(&a.config, 0, 0)]))
        .collect();
    bx(&mut out, b"moov", |b| {
        full(b, b"mvhd", 0, 0, |b| {
            put_u32(b, created as u32);
            put_u32(b, created as u32);
            put_u32(b, TIMESCALE);
            put_u32(b, 0); // duration unknown
            put_u32(b, 0x0001_0000);
            put_u16(b, 0x0100);
            b.extend_from_slice(&[0; 10]);
            for m in MATRIX {
                put_u32(b, m);
            }
            b.extend_from_slice(&[0; 24]);
            // next_track_ID (3 with or without one audio track).
            put_u32(
                b,
                (AUDIO_TRACK_ID + audio.len() as u32).max(AUDIO_TRACK_ID + 1),
            );
        });
        if let (Some(v), Some(s)) = (video, &vstsd) {
            trak(
                b,
                &TrackInfo {
                    id: VIDEO_TRACK_ID,
                    kind: b"vide",
                    handler_name: VIDEO_HANDLER_NAME.as_bytes(),
                    timescale: TIMESCALE,
                    media_duration: 0,
                    edits: &[],
                    width: v.width,
                    height: v.height,
                    volume: 0,
                    flags: 3,
                    alternate_group: 0,
                    tables: empty_tables(s),
                },
                created,
            );
        }
        for (i, (a, s)) in audio.iter().zip(&astsd).enumerate() {
            // Encoder delay: the whole track (an edit of duration 0 extends
            // over every fragment) starts this many samples in.
            let priming = [Edit {
                duration: 0,
                media_time: i64::from(a.priming_samples),
            }];
            trak(
                b,
                &TrackInfo {
                    id: AUDIO_TRACK_ID + i as u32,
                    kind: b"soun",
                    handler_name: a.handler.name().as_bytes(),
                    timescale: a.config.sample_rate,
                    media_duration: 0,
                    edits: if a.priming_samples > 0 { &priming } else { &[] },
                    width: 0,
                    height: 0,
                    volume: 0x0100,
                    flags: a.tkhd_flags(),
                    alternate_group: a.alternate_group,
                    tables: empty_tables(s),
                },
                created,
            );
        }
        bx(b, b"mvex", |b| {
            let trex = |b: &mut Vec<u8>, id: u32, flags: u32| {
                full(b, b"trex", 0, 0, |b| {
                    put_u32(b, id);
                    put_u32(b, 1); // default sample description index
                    put_u32(b, 0); // default duration
                    put_u32(b, 0); // default size
                    put_u32(b, flags);
                });
            };
            if video.is_some() {
                trex(b, VIDEO_TRACK_ID, FLAGS_NON_SYNC);
            }
            for i in 0..audio.len() {
                trex(b, AUDIO_TRACK_ID + i as u32, FLAGS_SYNC);
            }
        });
    });
    out
}

/// Sample tables with no samples (all samples live in fragments).
fn empty_tables(stsd: &[u8]) -> Tables<'_> {
    Tables {
        stsd,
        stts: &[],
        ctts: None,
        sizes: &[],
        sync: None,
        chunks: &[],
        shift: 0,
        co64: false,
    }
}

/// `moof` + `mdat` for one fragment of one or more tracks.
fn media_segment(seq: u32, trafs: &[&TrafSpec<'_>]) -> Result<Vec<u8>> {
    let data_len: usize = trafs.iter().map(|t| t.data.len()).sum();
    let probe = moof(seq, trafs, &vec![0; trafs.len()]);
    let mdat_header = 8u64;
    let mut offsets = Vec::new();
    let mut acc = probe.len() as u64 + mdat_header;
    for t in trafs {
        offsets.push(u32::try_from(acc).map_err(|_| Error::TooLarge)?);
        acc += t.data.len() as u64;
    }
    let total = probe.len() as u64 + mdat_header + data_len as u64;
    u32::try_from(total).map_err(|_| Error::TooLarge)?;
    let mut out = moof(seq, trafs, &offsets);
    debug_assert_eq!(out.len(), probe.len());
    out.reserve(8 + data_len);
    put_u32(&mut out, 8 + data_len as u32);
    out.extend_from_slice(b"mdat");
    for t in trafs {
        out.extend_from_slice(t.data);
    }
    Ok(out)
}

fn moof(seq: u32, trafs: &[&TrafSpec<'_>], offsets: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    bx(&mut out, b"moof", |b| {
        full(b, b"mfhd", 0, 0, |b| put_u32(b, seq));
        for (t, &off) in trafs.iter().zip(offsets) {
            bx(b, b"traf", |b| {
                // default-base-is-moof
                full(b, b"tfhd", 0, 0x02_0000, |b| put_u32(b, t.track_id));
                full(b, b"tfdt", 1, 0, |b| put_u64(b, t.tfdt));
                let flags = 0x1
                    | 0x100
                    | 0x200
                    | if t.flags { 0x400 } else { 0 }
                    | if t.cto { 0x800 } else { 0 };
                full(b, b"trun", u8::from(t.cto), flags, |b| {
                    put_u32(b, t.samples.len() as u32);
                    put_u32(b, off);
                    for s in t.samples {
                        put_u32(b, s.dur);
                        put_u32(b, s.size);
                        if t.flags {
                            put_u32(b, s.flags);
                        }
                        if t.cto {
                            put_u32(b, s.cto as u32);
                        }
                    }
                });
            });
        }
    });
    out
}
