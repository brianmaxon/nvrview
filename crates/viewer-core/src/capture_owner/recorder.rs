//! One recording: its own subscription to the camera's stream, the epoch
//! timeline, the segment policy, the disk guard, the call mixer, and the
//! segments it writes, on a thread of its own (or a task on a test
//! runtime).
//!
//! # Rules
//!
//! - **Audio probe.** A file's audio tracks are declared before its first
//!   frame, so the recorder first learns what audio the camera sends: it
//!   holds the stream from the first keyframe until the first audio record
//!   (its codec and AAC format decide), or 1.5 s of video, 3 s of host
//!   time, or 8 MiB, whichever comes first (then the camera sends no
//!   audio). The held records are then handled in order. Later parts reuse
//!   the decision.
//! - **Camera audio time.** Audio records carry no timestamp: they are
//!   stamped with `audio_pipeline::timestamp` (the playout's own method)
//!   from the video they follow, then placed with the epoch timeline.
//! - **Our audio.** Talk records from the session's observer are mapped
//!   from their send instant (the core's clock) with the timeline's
//!   presentation map.
//! - **Ends.** A stop request, a cap, low disk, a write or encoder failure,
//!   no video, or the end of the session (a refused login) ends the loop;
//!   the subscription and the talk observer are released at once, then the
//!   open segment is finalized without the runtime, so finalizing can
//!   finish even while the core shuts down.

use super::segment::{self, MixOut, OpenArgs, OpenError, Segment, Source, TrackPlan};
use super::{AudioEncoder, Control, FreeSpace, Shared, kind_name};
use crate::events::Priority;
use crate::in_callback;
use crate::json::Object;
use audio_pipeline::adts::{AdtsHeader, split_frames};
use audio_pipeline::decode::{AudioCodec, DecodeError, Decoder, PcmFormat};
use audio_pipeline::timestamp::Timestamper;
use bc_protocol::media::{Codec as BcCodec, Record};
use bc_session::{DeviceSession, Item, Subscription, TalkObserver};
use capture::Reason;
use capture::caps::{DiskConfig, DiskDecision, DiskGuard, Limits, RecordingKind};
use capture::epoch::{Timeline, TimelineConfig};
use capture::mixer::{Side, pcm_i16_to_f32};
use capture::naming;
use capture::policy::{
    Codec, PolicyConfig, SegmentPolicy, VideoDecision, VideoFormat, VideoSample,
};
use capture::tracks::{AudioMode, CameraAudio, audio_tracks, call_visitor_mode, live_audio_mode};
use mp4_remux::{AacConfig, VideoCodec, parse_adts};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

/// How often the recorder checks the clock without media.
const TICK: Duration = Duration::from_millis(250);
/// How often a `recording` event repeats while nothing changes.
const STATUS_EVERY_US: i64 = 1_000_000;
/// Probe limits (see the module documentation).
const PROBE_VIDEO_US: u64 = 1_500_000;
const PROBE_HOST_US: i64 = 3_000_000;
const PROBE_BYTES: usize = 8 << 20;

/// Everything a recorder starts with.
pub(crate) struct Init {
    pub(crate) id: u64,
    pub(crate) kind: RecordingKind,
    pub(crate) device: u64,
    pub(crate) channel: u8,
    pub(crate) lens: u8,
    pub(crate) main: bool,
    pub(crate) dir: PathBuf,
    pub(crate) stem: String,
    pub(crate) limits: Limits,
    pub(crate) disk: DiskConfig,
    pub(crate) encoder: Option<Arc<dyn AudioEncoder>>,
    pub(crate) free: Arc<dyn FreeSpace>,
    pub(crate) sub: Option<Subscription>,
    pub(crate) session: Option<Arc<DeviceSession>>,
    pub(crate) observer: Option<TalkObserver>,
    pub(crate) control: Arc<Control>,
    pub(crate) shared: Arc<Shared>,
}

enum Held {
    Video {
        codec: BcCodec,
        keyframe: bool,
        pts_us: u64,
        data: Vec<u8>,
        host_us: i64,
    },
    Audio {
        stream_index: u8,
        data: Vec<u8>,
    },
}

#[derive(Default)]
struct Probe {
    held: Vec<Held>,
    bytes: usize,
    first_pts: Option<u64>,
    since_host: Option<i64>,
}

/// The audio decision of a recording.
struct AudioPlan {
    mode: AudioMode,
    tracks: Vec<TrackPlan>,
    /// The visitor's PCM rate for the mixer.
    visitor_rate: u32,
}

impl AudioPlan {
    fn mixes_visitor(&self) -> bool {
        self.tracks.iter().any(|t| {
            matches!(
                t.source,
                Source::Mix(MixOut::Conversation | MixOut::Visitor)
            )
        })
    }
}

pub(crate) struct Recorder {
    id: u64,
    kind: RecordingKind,
    device: u64,
    channel: u8,
    lens: u8,
    main: bool,
    dir: PathBuf,
    stem: String,
    encoder: Option<Arc<dyn AudioEncoder>>,
    free: Arc<dyn FreeSpace>,
    sub: Option<Subscription>,
    session: Option<Arc<DeviceSession>>,
    observer: Option<TalkObserver>,
    control: Arc<Control>,
    shared: Arc<Shared>,

    timeline: Timeline,
    policy: SegmentPolicy,
    guard: DiskGuard,
    info: (u32, u32),
    param_hash: u64,
    last_video_pts: Option<u64>,
    probe: Option<Probe>,
    plan: Option<AudioPlan>,
    stamper: Option<Timestamper>,
    decoder: Decoder,
    pcm: Vec<f32>,
    seg: Option<Segment>,
    segment_no: u32,
    target: PathBuf,
    finished_bytes: u64,
    end: Option<Reason>,
    fragmented: bool,
    encode_dropped_done: u64,
    dropped_done: u64,
    audio_unplaced: u64,
    last_status_us: i64,
    free_unknown_logged: bool,
    /// Paths this recording reserved or wrote, released at the end.
    paths: Vec<PathBuf>,
}

/// A hash of the parameter sets (SPS, PPS, and VPS for H.265) carried in
/// an Annex-B access unit, or `None` if it carries none.
fn param_hash(codec: BcCodec, data: &[u8]) -> Option<u64> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut any = false;
    let mut i = 0;
    let n = data.len();
    let mut starts = Vec::new();
    while i + 3 <= n {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(n, |&next| next - 3);
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        let Some(&b0) = data.get(s) else {
            continue;
        };
        let is_set = match codec {
            BcCodec::H264 => matches!(b0 & 0x1f, 7 | 8),
            BcCodec::H265 => matches!((b0 >> 1) & 0x3f, 32..=34),
        };
        if is_set && e > s {
            data[s..e].hash(&mut h);
            any = true;
        }
    }
    any.then(|| h.finish())
}

fn downmix(pcm: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return pcm.to_vec();
    }
    pcm.chunks_exact(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect()
}

impl Recorder {
    pub(crate) fn new(i: Init) -> Recorder {
        let now = i.shared.clock.now_us() as i64;
        let target = i.dir.join(naming::final_file_name(&i.stem));
        let paths = vec![
            target.clone(),
            i.dir.join(naming::partial_file_name(&i.stem)),
        ];
        Recorder {
            id: i.id,
            kind: i.kind,
            device: i.device,
            channel: i.channel,
            lens: i.lens,
            main: i.main,
            dir: i.dir,
            stem: i.stem,
            encoder: i.encoder,
            free: i.free,
            sub: i.sub,
            session: i.session,
            observer: i.observer,
            control: i.control,
            shared: i.shared,
            timeline: Timeline::new(TimelineConfig::default()),
            policy: SegmentPolicy::new(PolicyConfig::new(i.limits), now),
            guard: DiskGuard::new(i.disk),
            info: (0, 0),
            param_hash: 0,
            last_video_pts: None,
            probe: Some(Probe::default()),
            plan: None,
            stamper: None,
            decoder: Decoder::new(),
            pcm: Vec::new(),
            seg: None,
            segment_no: 0,
            target,
            finished_bytes: 0,
            end: None,
            fragmented: false,
            encode_dropped_done: 0,
            dropped_done: 0,
            audio_unplaced: 0,
            last_status_us: now,
            free_unknown_logged: false,
            paths,
        }
    }

    /// On a thread of its own: the loop on the runtime, then the finalize.
    pub(crate) fn run_thread(mut self, runtime: &tokio::runtime::Handle) {
        in_callback::mark_core_thread();
        runtime.block_on(self.serve());
        self.finish();
    }

    /// As a task on a test runtime.
    pub(crate) async fn run_task(mut self) {
        in_callback::mark_core_thread();
        self.serve().await;
        self.finish();
    }

    fn now_us(&self) -> i64 {
        self.shared.clock.now_us() as i64
    }

    fn encoder(&self) -> Option<&dyn AudioEncoder> {
        self.encoder.as_deref()
    }

    async fn serve(&mut self) {
        self.emit("waiting_keyframe", None, Priority::State);
        let mut sub = self.sub.take();
        let control = self.control.clone();
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            if let Some(r) = control.stop_reason() {
                self.end.get_or_insert(r);
            }
            if self.end.is_some() {
                break;
            }
            let Some(s) = sub.as_mut() else {
                // The device refused the login before the start.
                self.end = Some(Reason::LoginFailed);
                break;
            };
            tokio::select! {
                biased;
                () = control.wake.notified() => self.drain_units(),
                item = s.recv() => match item {
                    Some(item) => {
                        self.dropped_done = s.dropped();
                        self.on_item(item);
                    }
                    // The session ended for good on its own: a refused
                    // login (removal and shutdown stop recordings first).
                    None => {
                        self.end.get_or_insert(Reason::LoginFailed);
                    }
                },
                _ = tick.tick() => self.on_tick(),
            }
        }
        if let Some(s) = &sub {
            self.dropped_done = s.dropped();
        }
        // Let go of the stream and the session before finalizing.
        drop(sub);
        if self.observer.take().is_some()
            && let Some(s) = &self.session
        {
            s.detach_talk_observer(self.channel);
        }
        self.session = None;
    }

    fn on_item(&mut self, item: Item) {
        let host = self.now_us();
        match item {
            Item::Media(Record::Info { width, height, .. }) => self.info = (width, height),
            Item::Media(Record::Video {
                codec,
                keyframe,
                pts_us,
                data,
                ..
            }) => {
                if let Some(p) = self.probe.as_mut() {
                    p.first_pts.get_or_insert(pts_us);
                    p.since_host.get_or_insert(host);
                    p.bytes += data.len();
                    let span = pts_us.wrapping_sub(p.first_pts.unwrap_or(pts_us));
                    p.held.push(Held::Video {
                        codec,
                        keyframe,
                        pts_us,
                        data,
                        host_us: host,
                    });
                    if (PROBE_VIDEO_US..1 << 31).contains(&span) || p.bytes >= PROBE_BYTES {
                        self.decide(None);
                    }
                    return;
                }
                self.on_video(codec, keyframe, pts_us, &data, host);
            }
            Item::Media(Record::Audio { stream_index, data }) => {
                if stream_index == crate::audio::TALKBACK_STREAM_INDEX {
                    return;
                }
                if let Some(p) = self.probe.as_mut() {
                    p.since_host.get_or_insert(host);
                    p.bytes += data.len();
                    let first = (stream_index, data.clone());
                    p.held.push(Held::Audio { stream_index, data });
                    self.decide(Some(first));
                    return;
                }
                self.on_audio(stream_index, &data);
            }
            Item::Discontinuity => {
                // The timeline sees the jump in the timestamps; the audio
                // stamps start over.
                self.last_video_pts = None;
                if let Some(t) = self.stamper.as_mut() {
                    t.reset();
                }
                self.decoder.reset();
                if let Some(p) = self.probe.as_mut() {
                    *p = Probe::default();
                }
            }
            Item::WaitingForDevice { .. } | Item::StartRejected { .. } => {}
        }
    }

    /// Ends the probe: `first` is the first camera audio record, if any.
    fn decide(&mut self, first: Option<(u8, Vec<u8>)>) {
        let Some(probe) = self.probe.take() else {
            return;
        };
        let (camera, aac): (CameraAudio, Option<AacConfig>) = match &first {
            None => (CameraAudio::None, None),
            Some((idx, data)) => match AudioCodec::select(*idx, data.len()) {
                AudioCodec::Aac => match parse_adts(data) {
                    Ok((cfg, _)) => (CameraAudio::Aac, Some(cfg)),
                    Err(_) => (CameraAudio::Other, None),
                },
                _ => (CameraAudio::Other, None),
            },
        };
        let have_encoder = self.encoder.is_some();
        let plans = audio_tracks(self.kind);
        let visitor_rate = match (camera, aac) {
            (CameraAudio::Aac, Some(c)) => c.sample_rate,
            (CameraAudio::Other, _) => audio_pipeline::adpcm::RATE,
            _ => segment::YOU_RATE,
        };
        let plan = match self.kind {
            RecordingKind::Live => {
                let mode = live_audio_mode(camera, have_encoder);
                let source = match (mode, aac) {
                    (AudioMode::Passthrough, Some(c)) => Some(Source::Passthrough(c)),
                    (AudioMode::Encoded, _) => Some(Source::Mix(MixOut::Visitor)),
                    _ => None,
                };
                AudioPlan {
                    mode,
                    tracks: source
                        .map(|source| TrackPlan {
                            plan: plans[0],
                            source,
                        })
                        .into_iter()
                        .collect(),
                    visitor_rate,
                }
            }
            RecordingKind::Call => {
                let mode = call_visitor_mode(camera, have_encoder).unwrap_or(AudioMode::None);
                let visitor = match aac {
                    Some(c) if camera == CameraAudio::Aac => Source::Passthrough(c),
                    _ => Source::Mix(MixOut::Visitor),
                };
                let sources = [
                    Source::Mix(MixOut::Conversation),
                    visitor,
                    Source::Mix(MixOut::You),
                ];
                AudioPlan {
                    mode,
                    tracks: plans
                        .into_iter()
                        .zip(sources)
                        .map(|(plan, source)| TrackPlan { plan, source })
                        .collect(),
                    visitor_rate,
                }
            }
        };
        info!(
            recording = self.id,
            audio = plan.mode.as_str(),
            "recording audio decided"
        );
        self.plan = Some(plan);
        for h in probe.held {
            if self.end.is_some() {
                break;
            }
            match h {
                Held::Video {
                    codec,
                    keyframe,
                    pts_us,
                    data,
                    host_us,
                } => self.on_video(codec, keyframe, pts_us, &data, host_us),
                Held::Audio { stream_index, data } => self.on_audio(stream_index, &data),
            }
        }
    }

    fn total_bytes(&self) -> u64 {
        self.finished_bytes + self.seg.as_ref().map_or(0, Segment::bytes)
    }

    fn on_video(&mut self, codec: BcCodec, keyframe: bool, pts_us: u64, data: &[u8], host: i64) {
        if self.end.is_some() {
            return;
        }
        self.last_video_pts = Some(pts_us);
        let pos = self.timeline.push_video(pts_us, host);
        if keyframe && let Some(h) = param_hash(codec, data) {
            self.param_hash = h;
        }
        let format = VideoFormat {
            codec: match codec {
                BcCodec::H264 => Codec::H264,
                BcCodec::H265 => Codec::H265,
            },
            width: self.info.0,
            height: self.info.1,
            param_hash: self.param_hash,
        };
        let d = self.policy.on_video(VideoSample {
            file_us: pos.file_us,
            host_us: host,
            keyframe,
            format,
            total_bytes: self.total_bytes(),
            frame_bytes: data.len() as u64,
        });
        match d {
            VideoDecision::Drop => {}
            VideoDecision::Start { .. } => {
                if self.open_segment(codec) {
                    // A later part (after a long gap) says so.
                    let state = if self.segment_no > 1 {
                        "segment"
                    } else {
                        "recording"
                    };
                    self.emit(state, None, Priority::State);
                    self.write(codec, data, keyframe, 0);
                }
            }
            VideoDecision::Write { rel_us, .. } => self.write(codec, data, keyframe, rel_us),
            VideoDecision::Cut { reason, next } => {
                self.close_segment(reason);
                if next.is_some() && self.open_segment(codec) {
                    self.emit("segment", None, Priority::State);
                    self.write(codec, data, keyframe, 0);
                }
            }
            VideoDecision::Stop(r) => {
                self.end.get_or_insert(r);
            }
        }
        self.drain_you();
        self.drain_units();
        self.advance_mix();
    }

    fn write(&mut self, codec: BcCodec, data: &[u8], keyframe: bool, rel_us: i64) {
        let Some(seg) = self.seg.as_mut() else {
            return;
        };
        match seg.write_video(data, keyframe, rel_us) {
            Ok(Some(true)) => self.disk_check(true),
            Ok(_) => {}
            Err(mp4_remux::Error::ParameterSetsChanged) => {
                // The sets changed in a way the policy's hash did not see:
                // the file cannot hold the frame, so it starts a new part.
                self.close_segment(Reason::FormatChanged);
                if keyframe && self.open_segment(codec) {
                    self.emit("segment", None, Priority::State);
                    self.write(codec, data, keyframe, 0);
                }
            }
            Err(e) => {
                warn!(recording = self.id, error = %e, "a recording could not be written");
                self.end.get_or_insert(Reason::WriteFailed);
            }
        }
    }

    fn on_audio(&mut self, stream_index: u8, data: &[u8]) {
        if self.end.is_some() {
            return;
        }
        let Some(plan) = &self.plan else {
            return;
        };
        let needs_pcm = plan.mixes_visitor();
        let visitor_rate = plan.visitor_rate;
        // Samples and format of the record.
        let (format, frames) = if needs_pcm {
            self.pcm.clear();
            match self.decoder.decode(stream_index, data, &mut self.pcm) {
                Ok(d) => (d.format, d.frames),
                Err(DecodeError::Aac { format, frames, .. }) => {
                    self.pcm.clear();
                    self.pcm.resize(frames * format.channels.max(1), 0.0);
                    (format, frames)
                }
                Err(_) => {
                    self.audio_unplaced += 1;
                    return;
                }
            }
        } else {
            let mut frames = 0;
            let mut format = None;
            for f in split_frames(data) {
                if let Ok(h) = AdtsHeader::parse(f) {
                    frames += h.samples();
                    format.get_or_insert(PcmFormat {
                        sample_rate: h.sample_rate,
                        channels: h.channels(),
                    });
                }
            }
            match format {
                Some(f) => (f, frames),
                None => {
                    self.audio_unplaced += 1;
                    return;
                }
            }
        };
        if frames == 0 {
            return;
        }
        let rate = format.sample_rate.max(1);
        let st = match &mut self.stamper {
            Some(t) if t.nominal_rate() == rate => t,
            other => {
                match other {
                    Some(t) => t.reset_with_rate(rate),
                    None => *other = Some(Timestamper::new(rate)),
                }
                other.as_mut().expect("just set")
            }
        };
        let stamp = st.push(frames, self.last_video_pts);
        let Some(tl) = stamp.timeline else {
            self.audio_unplaced += 1;
            return;
        };
        let cam = tl.at(stamp.first_sample as f64).round();
        if !cam.is_finite() || cam < 0.0 {
            self.audio_unplaced += 1;
            return;
        }
        let Some(file_us) = self.timeline.audio_file_us(cam as u64) else {
            self.audio_unplaced += 1;
            return;
        };
        let Some(rel_us) = self.policy.audio_rel_us(file_us) else {
            return;
        };
        let Some(seg) = self.seg.as_mut() else {
            return;
        };
        let Some(plan) = &self.plan else {
            return;
        };
        for (i, t) in plan.tracks.iter().enumerate() {
            if let Source::Passthrough(cfg) = t.source {
                let mut at = rel_us;
                for f in split_frames(data) {
                    if let Ok((c, payload)) = parse_adts(f) {
                        if c == cfg {
                            seg.passthrough(i, payload, at);
                        } else {
                            seg.audio_refused += 1;
                        }
                        at += i64::from(mp4_remux::SAMPLES_PER_FRAME) * 1_000_000
                            / i64::from(c.sample_rate.max(1));
                    }
                }
            }
        }
        if needs_pcm && seg.mixes() {
            if format.sample_rate == visitor_rate {
                let mono = downmix(&self.pcm, format.channels);
                seg.mix(Side::Visitor, rel_us, &mono);
            } else {
                self.audio_unplaced += 1;
            }
        }
    }

    /// Places our sent talk records (call recordings).
    fn drain_you(&mut self) {
        let Some(obs) = &self.observer else {
            return;
        };
        let records = obs.drain();
        if records.is_empty() {
            return;
        }
        // The send instants are on the runtime's clock; the timeline is on
        // the core's clock. Both run at the same rate, so the age of each
        // instant maps it.
        let now_inst = tokio::time::Instant::now();
        let now = self.now_us();
        for r in records {
            let age = now_inst.saturating_duration_since(r.at).as_micros() as i64;
            let Some(file_us) = self.timeline.sent_file_us(now - age) else {
                continue;
            };
            let Some(rel) = self.policy.audio_rel_us(file_us) else {
                continue;
            };
            if let Some(seg) = self.seg.as_mut() {
                seg.mix(Side::You, rel, &pcm_i16_to_f32(&r.samples));
            }
        }
    }

    /// Places the encoder's answers.
    fn drain_units(&mut self) {
        let units = self.control.take_units();
        if let Some(seg) = self.seg.as_mut() {
            for (track, u) in units {
                seg.accept_unit(track, &u);
            }
        }
    }

    fn advance_mix(&mut self) {
        let enc = self.encoder.clone();
        if let Some(seg) = self.seg.as_mut()
            && let Some(rel) = seg.last_rel_us()
        {
            seg.advance(rel, enc.as_deref(), self.id);
        }
    }

    fn read_free(&mut self) -> u64 {
        match self.free.free_bytes(&self.dir) {
            Some(b) => b,
            None => {
                if !self.free_unknown_logged {
                    self.free_unknown_logged = true;
                    warn!(recording = self.id, "free space is unknown; recording on");
                }
                u64::MAX
            }
        }
    }

    fn disk_check(&mut self, at_fragment: bool) {
        let Some(bytes) = self.seg.as_ref().map(Segment::bytes) else {
            return;
        };
        let now = self.now_us();
        if !self.guard.due(now, at_fragment) {
            return;
        }
        let free = self.read_free();
        if let DiskDecision::Stop(r) = self.guard.check(now, free, bytes) {
            info!(
                recording = self.id,
                "free space is low; stopping the recording"
            );
            self.end.get_or_insert(r);
        }
    }

    fn on_tick(&mut self) {
        let now = self.now_us();
        if let Some(p) = &self.probe
            && p.since_host.is_some_and(|t| now - t >= PROBE_HOST_US)
        {
            self.decide(None);
        }
        match self.policy.tick(now) {
            Some(VideoDecision::Cut { reason, .. }) => self.close_segment(reason),
            Some(VideoDecision::Stop(r)) => {
                self.end.get_or_insert(r);
            }
            _ => {}
        }
        self.drain_you();
        self.drain_units();
        self.advance_mix();
        self.disk_check(false);
        if now - self.last_status_us >= STATUS_EVERY_US && self.end.is_none() {
            let state = if self.seg.is_some() {
                "recording"
            } else {
                "waiting_keyframe"
            };
            self.emit(state, None, Priority::Diagnostics);
        }
    }

    /// Opens the next part; false (and the recording ends) if it cannot.
    fn open_segment(&mut self, codec: BcCodec) -> bool {
        let number = self.segment_no + 1;
        let stem = if number == 1 {
            self.stem.clone()
        } else {
            let base = naming::part_stem(&self.stem, number);
            match naming::unique_stem(&base, |n| self.shared.taken(&self.dir.join(n))) {
                Ok(s) => s,
                Err(_) => {
                    self.end.get_or_insert(Reason::WriteFailed);
                    return false;
                }
            }
        };
        let target = self.dir.join(naming::final_file_name(&stem));
        let partial = self.dir.join(naming::partial_file_name(&stem));
        for p in [&target, &partial] {
            if !self.paths.contains(p) {
                self.shared.reserve(p.clone());
                self.paths.push(p.clone());
            }
        }
        if !self.shared.register(self.id, self.kind, &target) {
            warn!(
                recording = self.id,
                "the partial file could not be listed for recovery"
            );
        }
        let creation = (self.shared.clock.wall_now_us() / 1_000_000).max(0) as u64;
        let video = match codec {
            BcCodec::H264 => VideoCodec::H264,
            BcCodec::H265 => VideoCodec::H265,
        };
        let mut attempt = 0;
        let opened = loop {
            let Some(plan) = &self.plan else {
                return false;
            };
            let r = Segment::open(OpenArgs {
                recording: self.id,
                number,
                partial: partial.clone(),
                target: target.clone(),
                codec: video,
                tracks: &plan.tracks,
                encoder: self.encoder(),
                visitor_rate: plan.visitor_rate,
                creation_unix_secs: creation,
            });
            match r {
                // A live recording goes on without the audio it cannot
                // encode.
                Err(OpenError::Encoder) if self.kind == RecordingKind::Live && attempt == 0 => {
                    warn!(
                        recording = self.id,
                        "the encoder refused the track; recording video only"
                    );
                    if let Some(p) = self.plan.as_mut() {
                        p.tracks
                            .retain(|t| matches!(t.source, Source::Passthrough(_)));
                        p.mode = AudioMode::Unsupported;
                    }
                    attempt += 1;
                }
                other => break other,
            }
        };
        match opened {
            Ok(seg) => {
                self.seg = Some(seg);
                self.segment_no = number;
                self.target = target;
                info!(recording = self.id, part = number, "recording file opened");
                true
            }
            Err(e) => {
                self.shared.unregister(self.id);
                let reason = match e {
                    OpenError::Encoder => Reason::EncoderFailed,
                    OpenError::Io(err) => {
                        warn!(recording = self.id, error = %err, "the recording file could not be created");
                        Reason::WriteFailed
                    }
                };
                self.end.get_or_insert(reason);
                false
            }
        }
    }

    /// Finalizes the open part, if any.
    fn close_segment(&mut self, why: Reason) {
        let Some(mut seg) = self.seg.take() else {
            return;
        };
        let enc = self.encoder.clone();
        seg.flush_audio(enc.as_deref(), self.id);
        for (track, u) in self.control.take_units() {
            seg.accept_unit(track, &u);
        }
        self.encode_dropped_done = self.encode_dropped_done.max(seg.encode_dropped_ms());
        self.dropped_done += seg.reordered;
        let bytes = match seg.finish() {
            Ok(b) => b,
            Err(e) => {
                warn!(recording = self.id, error = %e, "the end of a recording could not be written");
                self.end.get_or_insert(Reason::WriteFailed);
                std::fs::metadata(&seg.partial).map_or(0, |m| m.len())
            }
        };
        let free = self.read_free();
        let plan = DiskGuard::finalize_plan(free, bytes);
        match segment::finalize(&seg.partial, &seg.target, plan) {
            Ok(fragmented) => {
                self.fragmented |= fragmented;
                if fragmented {
                    warn!(
                        recording = self.id,
                        "published the fragmented recording as the final file"
                    );
                }
                self.shared.unregister(self.id);
            }
            // The partial file stays listed, so the next configure tries
            // again.
            Err(e) => {
                warn!(recording = self.id, error = %e, "a recording could not be finalized");
                self.end.get_or_insert(Reason::WriteFailed);
            }
        }
        self.finished_bytes += bytes;
        info!(
            recording = self.id,
            part = seg.number,
            reason = why.as_str(),
            bytes,
            "recording file finished"
        );
    }

    /// After the loop: finalizes and reports the end.
    fn finish(&mut self) {
        let reason = self.end.unwrap_or(Reason::Stopped);
        self.policy.stop(reason);
        self.control.set_finalizing();
        if self.seg.is_some() {
            self.emit("finalizing", None, Priority::State);
            self.close_segment(reason);
        }
        // A failure while finalizing replaces a plain stop.
        let reason = match self.end {
            Some(r) if r.is_failure() => r,
            _ => reason,
        };
        let state = if reason.is_failure() {
            "failed"
        } else {
            "finished"
        };
        self.emit(state, Some(reason), Priority::State);
        info!(
            recording = self.id,
            reason = reason.as_str(),
            "recording ended"
        );
        self.shared.release(&self.paths);
        self.control.mark_done();
    }

    fn emit(&mut self, state: &str, reason: Option<Reason>, priority: Priority) {
        self.last_status_us = self.now_us();
        let seg_dropped = self.seg.as_ref().map_or(0, |s| s.reordered);
        let encode_dropped = self
            .encode_dropped_done
            .max(self.seg.as_ref().map_or(0, Segment::encode_dropped_ms))
            + self.control.inbox_dropped() * 1024 * 1000 / 48_000;
        let (mode, audio_tracks) = match &self.plan {
            Some(p) => (p.mode, p.tracks.len()),
            None => (AudioMode::None, 0),
        };
        let tracks: Vec<&str> = match self.kind {
            RecordingKind::Live if audio_tracks == 0 => vec!["video"],
            k => capture::tracks::track_names(k),
        };
        let mut o = Object::event("recording")
            .u64("recording", self.id)
            .str("kind", kind_name(self.kind))
            .u64("device", self.device)
            .u64("channel", u64::from(self.channel))
            .u64("lens", u64::from(self.lens))
            .str("tier", if self.main { "main" } else { "sub" })
            .str("state", state);
        if let Some(r) = reason {
            o = o.str("reason", r.as_str());
        }
        o = o
            .str("file", &self.target.to_string_lossy())
            .u64("segment", u64::from(self.segment_no.max(1)))
            .u64("bytes", self.total_bytes())
            .u64(
                "duration_ms",
                u64::try_from(self.policy.duration_us() / 1000).unwrap_or(0),
            )
            .u64("gaps", u64::from(self.policy.stats().gaps))
            .str("audio", mode.as_str())
            .strs("tracks", tracks)
            .u64("encode_dropped_ms", encode_dropped)
            .u64("dropped_frames", self.dropped_done + seg_dropped);
        if reason.is_some() {
            o = o.bool("fragmented", self.fragmented);
        }
        let json = o.finish();
        self.control.set_snapshot(&json);
        self.shared.events.send(json, priority);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_sets_are_hashed_and_slices_are_not() {
        let sps = [0, 0, 0, 1, 0x67, 1, 2, 3];
        let pps = [0, 0, 1, 0x68, 4, 5];
        let idr = [0, 0, 1, 0x65, 9, 9, 9];
        let au: Vec<u8> = [&sps[..], &pps[..], &idr[..]].concat();
        let h = param_hash(BcCodec::H264, &au).unwrap();
        // The slice data does not change the hash.
        let au2: Vec<u8> = [&sps[..], &pps[..], &[0, 0, 1, 0x65, 7][..]].concat();
        assert_eq!(param_hash(BcCodec::H264, &au2), Some(h));
        let other: Vec<u8> = [&[0, 0, 0, 1, 0x67, 1, 2, 4][..], &pps[..]].concat();
        assert_ne!(param_hash(BcCodec::H264, &other), Some(h));
        assert_eq!(param_hash(BcCodec::H264, &idr), None);
        // H.265: VPS (32), SPS (33), PPS (34).
        let vps = [0, 0, 1, 32 << 1, 1, 7];
        assert!(param_hash(BcCodec::H265, &vps).is_some());
        assert_eq!(param_hash(BcCodec::H265, &[0, 0, 1, 1 << 1, 1]), None);
    }

    #[test]
    fn downmix_averages_channels() {
        assert_eq!(downmix(&[0.5, -0.5, 1.0, 0.0], 2), vec![0.0, 0.5]);
        assert_eq!(downmix(&[0.25], 1), vec![0.25]);
    }
}
