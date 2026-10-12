//! One file of a recording: the partial fragmented MP4 being written, its
//! audio tracks (camera AAC passed through, or PCM sent to the app's
//! encoder and answered with access units), and the finalize step.
//!
//! # Rules
//!
//! - **Files.** The partial file is created new (never over an existing
//!   file), written as 2 s fragments, and each fragment is flushed and
//!   synced (`sync_data`) before the next one starts.
//! - **Video.** Positions are microseconds after the segment's keyframe. A
//!   frame not after the last one written (a camera reordering step the
//!   epoch timeline kept) is dropped and counted rather than failing the
//!   file.
//! - **Encoded tracks.** Mixer frames go into an [`EncodeQueue`] per track
//!   (10 s, overflow becomes silence). At most 10 s of frames are with the
//!   encoder at once (handed over and not yet answered); the rest wait in
//!   the queue. The `k`th access unit the app returns for a track is placed
//!   at the time of the `k`th 1024-sample block it was given, as the
//!   muxer's priming rule requires; units the encoder adds when flushed
//!   follow at the same spacing.
//! - **Closing.** The mixer is flushed to the last video frame, every
//!   queued frame is handed to the encoder (without the 10 s bound), each
//!   encoded track is closed with flush, the units that answer are placed,
//!   and the file's last fragment is written.
//! - **Finalizing.** With room for it, the fragmented file is remuxed to a
//!   standard MP4 under a temporary name, renamed into place, and deleted
//!   (`recordings::finalize::recover_fragmented`). Without room, or if the
//!   remux fails, the fragmented file itself is renamed to the final name
//!   (it plays) and the event says `"fragmented": true`.

use super::AudioEncoder;
use capture::caps::FinalizePlan;
use capture::mixer::{
    CallMixer, EncodeQueue, FRAME, MixedFrame, MixerConfig, OUT_RATE, Side, TEN_SECONDS_OF_FRAMES,
};
use capture::tracks::AudioTrackPlan;
use mp4_remux::{AacConfig, FileOptions, FragmentOptions, FragmentedFile, VideoCodec, VideoFrame};
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use tracing::warn;

/// The format the app's encoder is asked for (docs/CAPTURE.md, "Encoding").
pub(crate) const ENCODE_RATE: u32 = OUT_RATE;
/// Mono.
pub(crate) const ENCODE_CHANNELS: u32 = 1;
/// 96 kbps.
pub(crate) const ENCODE_BIT_RATE: u32 = 96_000;
/// The rate of our sent audio (the talk sender's PCM).
pub(crate) const YOU_RATE: u32 = 16_000;
/// Frames the mixer emits per call, so one step never does unbounded work.
const MIX_BATCH: usize = 64;

/// Which mixer output an encoded track carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MixOut {
    /// Visitor plus you.
    Conversation,
    /// The camera's microphone alone (also the live track of a camera
    /// whose audio is not AAC).
    Visitor,
    /// Our sent audio alone.
    You,
}

/// Where an audio track's samples come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// The camera's AAC, written as received.
    Passthrough(AacConfig),
    /// PCM from the mixer, encoded by the app.
    Mix(MixOut),
}

/// One planned audio track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TrackPlan {
    /// Handler, flags, group.
    pub(crate) plan: AudioTrackPlan,
    /// Its samples.
    pub(crate) source: Source,
}

/// A file whose flush also syncs the data to disk, so every complete
/// fragment survives a crash or a power loss.
pub(crate) struct SyncFile(BufWriter<File>);

impl Write for SyncFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()?;
        self.0.get_ref().sync_data()
    }
}

struct EncTrack {
    /// The audio track index the encoder knows it by.
    track: u32,
    out: MixOut,
    queue: EncodeQueue,
    submitted: u64,
    received: u64,
}

/// Why a segment could not be opened.
#[derive(Debug)]
pub(crate) enum OpenError {
    /// The app's encoder refused a track (or gave an unusable config).
    Encoder,
    /// The partial file could not be created.
    Io(io::Error),
}

/// One open segment.
pub(crate) struct Segment {
    /// The part number, 1 for the first file.
    pub(crate) number: u32,
    /// The file being written.
    pub(crate) partial: PathBuf,
    /// The file it becomes.
    pub(crate) target: PathBuf,
    file: Option<FragmentedFile<SyncFile>>,
    last_rel_us: Option<i64>,
    enc: Vec<EncTrack>,
    mixer: Option<CallMixer>,
    /// Video frames dropped as not after the previous one.
    pub(crate) reordered: u64,
    /// Audio units the muxer refused (format change, malformed).
    pub(crate) audio_refused: u64,
}

/// What a segment needs to open.
pub(crate) struct OpenArgs<'a> {
    pub(crate) recording: u64,
    pub(crate) number: u32,
    pub(crate) partial: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) codec: VideoCodec,
    pub(crate) tracks: &'a [TrackPlan],
    pub(crate) encoder: Option<&'a dyn AudioEncoder>,
    pub(crate) visitor_rate: u32,
    pub(crate) creation_unix_secs: u64,
}

impl Segment {
    /// Opens every encoded track, then creates the partial file.
    pub(crate) fn open(a: OpenArgs<'_>) -> Result<Segment, OpenError> {
        let mut specs = Vec::with_capacity(a.tracks.len());
        let mut enc: Vec<EncTrack> = Vec::new();
        let close_all = |enc: &[EncTrack]| {
            if let Some(e) = a.encoder {
                for t in enc {
                    e.close(a.recording, t.track, false);
                }
            }
        };
        for (i, t) in a.tracks.iter().enumerate() {
            let track = i as u32;
            match t.source {
                Source::Passthrough(cfg) => specs.push(t.plan.spec(cfg, 0)),
                Source::Mix(out) => {
                    let Some(e) = a.encoder else {
                        close_all(&enc);
                        return Err(OpenError::Encoder);
                    };
                    let opened = e.open(
                        a.recording,
                        track,
                        ENCODE_RATE,
                        ENCODE_CHANNELS,
                        ENCODE_BIT_RATE,
                    );
                    let cfg = opened.as_ref().and_then(|o| {
                        AacConfig::from_raw_audio_specific_config(&o.audio_specific_config).ok()
                    });
                    let (Some(o), Some(cfg)) = (opened, cfg) else {
                        // A track that did open is closed again.
                        close_all(&enc);
                        if let Some(e) = a.encoder {
                            e.close(a.recording, track, false);
                        }
                        return Err(OpenError::Encoder);
                    };
                    specs.push(t.plan.spec(cfg, o.priming_frames));
                    enc.push(EncTrack {
                        track,
                        out,
                        queue: EncodeQueue::new(TEN_SECONDS_OF_FRAMES),
                        submitted: 0,
                        received: 0,
                    });
                }
            }
        }
        let f = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&a.partial)
        {
            Ok(f) => f,
            Err(e) => {
                close_all(&enc);
                return Err(OpenError::Io(e));
            }
        };
        let opts = FileOptions {
            fragment: FragmentOptions {
                audio_tracks: specs,
                creation_unix_secs: a.creation_unix_secs,
                ..FragmentOptions::for_download()
            },
            mfra: true,
        };
        let mixer = (!enc.is_empty())
            .then(|| CallMixer::new(MixerConfig::new(a.visitor_rate.max(1), YOU_RATE)));
        Ok(Segment {
            number: a.number,
            partial: a.partial,
            target: a.target,
            file: Some(FragmentedFile::new(
                SyncFile(BufWriter::with_capacity(1 << 20, f)),
                a.codec,
                opts,
            )),
            last_rel_us: None,
            enc,
            mixer,
            reordered: 0,
            audio_refused: 0,
        })
    }

    /// Bytes written to the partial file so far.
    pub(crate) fn bytes(&self) -> u64 {
        self.file.as_ref().map_or(0, FragmentedFile::bytes_written)
    }

    /// The last video position written.
    pub(crate) fn last_rel_us(&self) -> Option<i64> {
        self.last_rel_us
    }

    /// Writes a video frame at `rel_us`. `Ok(None)`: dropped as not after
    /// the previous frame; `Ok(Some(true))`: a fragment was completed (and
    /// synced).
    pub(crate) fn write_video(
        &mut self,
        data: &[u8],
        keyframe: bool,
        rel_us: i64,
    ) -> Result<Option<bool>, mp4_remux::Error> {
        if self.last_rel_us.is_some_and(|l| rel_us <= l) {
            self.reordered += 1;
            return Ok(None);
        }
        let Some(f) = self.file.as_mut() else {
            return Ok(None);
        };
        let done = f.push_video(VideoFrame::new(data, keyframe, rel_us))?;
        self.last_rel_us = Some(rel_us);
        Ok(Some(done))
    }

    /// Adds one raw AAC unit of the camera to passthrough track `track`.
    pub(crate) fn passthrough(&mut self, track: usize, payload: &[u8], rel_us: i64) {
        let Some(f) = self.file.as_mut() else {
            return;
        };
        if f.push_audio_raw(track, payload, Some(rel_us)).is_err() {
            self.audio_refused += 1;
        }
    }

    /// Whether this segment mixes PCM (it has an encoded track).
    pub(crate) fn mixes(&self) -> bool {
        self.mixer.is_some()
    }

    /// Adds PCM of one side of the mix at `rel_us`.
    pub(crate) fn mix(&mut self, side: Side, rel_us: i64, samples: &[f32]) {
        if let Some(m) = self.mixer.as_mut() {
            m.push(side, rel_us, samples);
        }
    }

    fn dispatch(&mut self, frames: Vec<MixedFrame>) {
        for mut f in frames {
            for e in &mut self.enc {
                let pcm = match e.out {
                    MixOut::Conversation => std::mem::take(&mut f.conversation),
                    MixOut::Visitor => std::mem::take(&mut f.visitor),
                    MixOut::You => std::mem::take(&mut f.you),
                };
                e.queue.push(pcm);
            }
        }
    }

    fn pump(&mut self, encoder: &dyn AudioEncoder, recording: u64, bounded: bool) {
        let limit = TEN_SECONDS_OF_FRAMES as u64;
        for e in &mut self.enc {
            while !bounded || e.submitted.saturating_sub(e.received) < limit {
                let Some(pcm) = e.queue.pop() else {
                    break;
                };
                encoder.encode(recording, e.track, &pcm);
                e.submitted += 1;
            }
        }
    }

    /// The recording reached `rel_us`: emits mixer frames that are due and
    /// hands what the encoder has room for.
    pub(crate) fn advance(&mut self, rel_us: i64, encoder: Option<&dyn AudioEncoder>, rec: u64) {
        let Some(m) = self.mixer.as_mut() else {
            return;
        };
        let mut frames = Vec::new();
        while m.advance(rel_us, MIX_BATCH, &mut frames) == MIX_BATCH {}
        self.dispatch(frames);
        if let Some(e) = encoder {
            self.pump(e, rec, true);
        }
    }

    /// Places one access unit the app's encoder returned for `track`.
    /// False when the segment has no such encoded track.
    pub(crate) fn accept_unit(&mut self, track: u32, unit: &[u8]) -> bool {
        let Some(e) = self.enc.iter_mut().find(|e| e.track == track) else {
            return false;
        };
        let ts = (e.received as i64) * FRAME as i64 * 1_000_000 / i64::from(OUT_RATE);
        e.received += 1;
        if let Some(f) = self.file.as_mut()
            && f.push_audio_raw(track as usize, unit, Some(ts)).is_err()
        {
            self.audio_refused += 1;
        }
        true
    }

    /// Milliseconds of encoded audio replaced by silence (the worst track).
    pub(crate) fn encode_dropped_ms(&self) -> u64 {
        self.enc
            .iter()
            .map(|e| e.queue.encode_dropped_ms())
            .max()
            .unwrap_or(0)
    }

    /// First step of closing: flushes the mixer to the end of the video,
    /// hands every queued frame to the encoder, and closes each encoded
    /// track with flush. The units that answer arrive through the inbox;
    /// place them with [`Segment::accept_unit`] before [`Segment::finish`].
    pub(crate) fn flush_audio(&mut self, encoder: Option<&dyn AudioEncoder>, rec: u64) {
        let end = self.last_rel_us.unwrap_or(0);
        if let Some(m) = self.mixer.as_mut() {
            let mut frames = Vec::new();
            while m.flush(end, MIX_BATCH, &mut frames) == MIX_BATCH {}
            self.dispatch(frames);
        }
        if let Some(e) = encoder {
            self.pump(e, rec, false);
            for t in &self.enc {
                e.close(rec, t.track, true);
            }
        }
    }

    /// Writes the last fragment and the index; returns the partial file's
    /// size.
    pub(crate) fn finish(&mut self) -> io::Result<u64> {
        let Some(f) = self.file.take() else {
            return file_len(&self.partial);
        };
        match f.finish() {
            Ok((mut w, summary)) => {
                w.flush()?;
                Ok(summary.file_size)
            }
            Err(e) => Err(io::Error::other(e.to_string())),
        }
    }
}

fn file_len(p: &Path) -> io::Result<u64> {
    std::fs::metadata(p).map(|m| m.len())
}

/// Turns a finished partial file into its final file; returns true when
/// the fragmented file itself was published (no room to remux, or the
/// remux failed).
pub(crate) fn finalize(partial: &Path, target: &Path, plan: FinalizePlan) -> io::Result<bool> {
    if plan == FinalizePlan::Normal {
        match recordings::finalize::recover_fragmented(partial, target, true) {
            Ok(_) => return Ok(false),
            Err(e) => {
                warn!(error = %e, "a recording could not be finalized; publishing it as written")
            }
        }
    }
    std::fs::rename(partial, target)?;
    Ok(true)
}
