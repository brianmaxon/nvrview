//! A camera-day's audio from downloaded files: the core's own source for
//! `sound_analyze_day` when no other [`super::DayAudioSource`] was set.
//!
//! **Which files.** Finished camera downloads (`done`, file present) of the
//! camera whose time range touches the day. A file's start is the start of
//! its download range on the index's clock (the same seam as the edit
//! planners: [`recordings::edl::fallback_file_start_ms`]), plus the audio
//! track's own delay; its audio runs for as many samples as it has frames.
//! Where two files overlap, the later one is cut so no second is counted
//! twice. Files without an AAC track are skipped.
//!
//! **Decoding.** Each frame goes through the same AAC decoder the live
//! audio uses, is mixed down to mono and resampled to 16 kHz when it is
//! not already, and is handed over in chunks of about a second. A frame
//! that fails to decode is replaced by silence of its length so the times
//! after it stay right.
//!
//! **Not here: replay from the device.** When nothing is downloaded the day
//! could be replayed through the playback path, but audio is carried only at
//! 1x (docs/protocol.md, "Rates, keyframes, and how a play ends"), so a day
//! would take up to a day of real time on one of the device's few play
//! slots. That is not done; the apps offer the analysis for days that were
//! downloaded (see docs/FFI-RECORDINGS.md, section 2a).

use super::sound_work::AudioChunk;
use super::{Hub, lock, query};
use audio_pipeline::decode::Decoder;
use audio_pipeline::resample::Resampler;
use recordings::audio_track::{AacTrack, SAMPLES_PER_FRAME, read_aac_track};
use recordings::edl::fallback_file_start_ms;
use recordings::scheduler::CameraState;
use recordings::{DeviceCamera, Secs};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

/// 16 kHz samples in the day.
const RATE: i64 = 16_000;
const DAY_SAMPLES: i64 = 86_400 * RATE;
/// Samples handed over at a time (one second).
const CHUNK: usize = RATE as usize;
/// Most files read for one day.
const MAX_FILES: usize = 256;

/// A finished download's audio file and where it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DayFile {
    pub path: PathBuf,
    /// Milliseconds on the index's clock (device-local naive time).
    pub start_ms: i64,
    /// Milliseconds on the same clock where the download's range ends.
    pub end_ms: i64,
}

/// The finished downloads of `camera` on `device` whose file can reach
/// into the day starting at `day`, in time order; `None` when downloads
/// are not set up. Only the download's range is consulted here; the files
/// are opened later.
pub(super) fn files_for_day(
    h: &Arc<Hub>,
    device: &str,
    camera: DeviceCamera,
    day: Secs,
) -> Option<Vec<DayFile>> {
    let mut st = h.st();
    let sched = h.scheduler(&st).ok()?;
    h.fresh_incidents(&mut st);
    let progress = sched.progress().borrow().clone();
    let trashing = lock(&h.trashing);
    let mut out = Vec::new();
    for p in progress.iter().filter(|p| !trashing.contains_key(&p.id)) {
        let Some(inc) = query::find(&st, &p.key) else {
            continue;
        };
        for c in &p.cameras {
            let id = &c.camera;
            if c.state != CameraState::Done
                || id.device != device
                || id.channel != camera.channel
                || id.lens != camera.lens
            {
                continue;
            }
            let Some(start_ms) = fallback_file_start_ms(inc, id) else {
                continue;
            };
            // A file can start before the day and run into it, or start
            // inside it: its range decides.
            let end_ms = inc
                .download_ranges()
                .get(id)
                .map_or(start_ms, |r| r.1.saturating_mul(1000));
            if end_ms < day.saturating_mul(1000) || start_ms >= (day + 86_400).saturating_mul(1000)
            {
                continue;
            }
            out.push(DayFile {
                path: c.output.clone(),
                start_ms,
                end_ms,
            });
        }
    }
    out.sort_by_key(|f| f.start_ms);
    out.dedup();
    out.truncate(MAX_FILES);
    Some(out)
}

/// The day's audio from `files`, as chunks in time order.
pub(super) fn chunks(files: Vec<DayFile>, day: Secs) -> impl Iterator<Item = AudioChunk> + Send {
    DayAudio {
        files: files.into(),
        day_ms: day.saturating_mul(1000),
        cur: None,
        emitted: 0,
        done: false,
    }
}

struct DayAudio {
    files: VecDeque<DayFile>,
    day_ms: i64,
    cur: Option<Open>,
    /// Day-sample number after the last sample handed over.
    emitted: i64,
    done: bool,
}

/// A file being read.
struct Open {
    track: AacTrack,
    reader: BufReader<File>,
    pos: u64,
    next_frame: usize,
    decoder: Decoder,
    resampler: Option<(Resampler, u32)>,
    /// Input samples fed to the resampler.
    fed: i64,
    /// Day-sample number of the first sample in `pending`.
    first: i64,
    pending: Vec<f32>,
    scratch: Vec<f32>,
}

impl Open {
    fn new(f: &DayFile, day_ms: i64) -> Option<Open> {
        let track = read_aac_track(&f.path).ok()??;
        let reader = BufReader::new(File::open(&f.path).ok()?);
        let delay = i64::try_from(track.start_ms).unwrap_or(0);
        // The sample of the day at which the file's first audio sample is.
        let first = (f.start_ms.saturating_sub(day_ms).saturating_add(delay)) * RATE / 1000;
        Some(Open {
            track,
            reader,
            pos: 0,
            next_frame: 0,
            decoder: Decoder::new(),
            resampler: None,
            fed: 0,
            first,
            pending: Vec::new(),
            scratch: Vec::new(),
        })
    }

    /// Reads and decodes frames until a chunk's worth is pending or the
    /// file ends; false at the end.
    fn fill(&mut self) -> bool {
        while self.pending.len() < CHUNK {
            let Some(&(offset, size)) = self.track.frames.get(self.next_frame) else {
                return false;
            };
            self.next_frame += 1;
            let mut raw = vec![0u8; size as usize];
            let read = (|| {
                if self.pos != offset {
                    self.reader.seek(SeekFrom::Start(offset))?;
                }
                self.reader.read_exact(&mut raw)?;
                self.pos = offset + u64::from(size);
                Ok::<_, std::io::Error>(())
            })();
            if read.is_err() {
                self.next_frame = self.track.frames.len();
                return false;
            }
            self.decode(&raw);
        }
        true
    }

    fn decode(&mut self, raw: &[u8]) {
        let adts = self.track.adts_frame(raw);
        self.scratch.clear();
        match self.decoder.decode(5, &adts, &mut self.scratch) {
            Ok(d) => {
                let ch = d.format.channels.max(1);
                let mono: Vec<f32> = if ch == 1 {
                    std::mem::take(&mut self.scratch)
                } else {
                    self.scratch
                        .chunks_exact(ch)
                        .map(|f| f.iter().sum::<f32>() / ch as f32)
                        .collect()
                };
                self.push_mono(d.format.sample_rate, &mono);
            }
            Err(_) => {
                // Silence of the frame's length keeps later times right.
                let n = (u64::from(SAMPLES_PER_FRAME) * RATE as u64
                    / u64::from(self.track.sample_rate.max(1))) as usize;
                self.pending.resize(self.pending.len() + n, 0.0);
            }
        }
    }

    fn push_mono(&mut self, rate: u32, mono: &[f32]) {
        if rate == RATE as u32 {
            self.pending.extend_from_slice(mono);
            return;
        }
        let rs = match &mut self.resampler {
            Some((r, in_rate)) if *in_rate == rate => r,
            slot => {
                // A new rate (not expected within a file) starts afresh.
                *slot = Some((Resampler::new(1, rate, RATE as u32), rate));
                self.fed = 0;
                &mut slot.as_mut().expect("just set").0
            }
        };
        rs.push(self.fed, mono);
        self.fed += mono.len() as i64;
        rs.produce(f64::from(rate) / RATE as f64, usize::MAX, &mut self.pending);
    }
}

impl Iterator for DayAudio {
    type Item = AudioChunk;

    fn next(&mut self) -> Option<AudioChunk> {
        while !self.done {
            if self.cur.is_none() {
                let f = self.files.pop_front()?;
                self.cur = Open::new(&f, self.day_ms);
                continue;
            }
            let open = self.cur.as_mut().expect("checked");
            let more = open.fill();
            let samples = std::mem::take(&mut open.pending);
            let first = open.first;
            open.first += samples.len() as i64;
            if !more {
                self.cur = None;
            }
            if let Some(c) = self.clip(first, samples) {
                return Some(c);
            }
        }
        None
    }
}

impl DayAudio {
    /// Cuts samples to the day and to what was not handed over yet.
    fn clip(&mut self, first: i64, mut samples: Vec<f32>) -> Option<AudioChunk> {
        let mut first = first;
        let from = self.emitted.max(0);
        if first < from {
            let cut = usize::try_from(from - first).unwrap_or(usize::MAX);
            if cut >= samples.len() {
                return None;
            }
            samples.drain(..cut);
            first = from;
        }
        if first >= DAY_SAMPLES {
            self.done = true;
            return None;
        }
        let room = usize::try_from(DAY_SAMPLES - first).unwrap_or(usize::MAX);
        if samples.len() >= room {
            samples.truncate(room);
            self.done = true;
        }
        if samples.is_empty() {
            return None;
        }
        self.emitted = first + samples.len() as i64;
        Some(AudioChunk {
            first_sample: first as u64,
            samples,
        })
    }
}
