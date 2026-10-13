//! One tile's audio: decode, timestamp, play out, tap, and 16 kHz mono for
//! sound analysis.

use crate::decode::{AudioCodec, DecodeError, Decoder, PcmFormat};
use crate::delay::DelayControl;
use crate::mono::{Mono16k, MonoBlock};
use crate::output::AudioOutput;
use crate::playout::{Block, Playout, PlayoutStats};
use crate::presentation::{PresentationEstimator, PresentationMap};
use crate::tap::{TapBlock, TapSender, TapStats};
use crate::timestamp::{Timeline, TimestampStats, Timestamper};

/// Settings of a tile.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TileConfig {
    /// How long after its picture is shown each sample is heard, µs: with the
    /// estimated presentation map, after the *median* delivery of what the
    /// camera sent at that moment. `None` (the default) adapts it to the
    /// stream: a high percentile of recent delivery lateness plus the record
    /// length, bounded and changing slowly ([`crate::delay`]). A fixed value
    /// must cover the record length (64 ms for device AAC) plus the late
    /// tail of the delivery jitter, or late records cause underruns (which
    /// drop the late audio and keep the timing).
    pub playout_delay_us: Option<f64>,
}

/// One audio record with the timing context the stream gives it.
#[derive(Clone, Copy, Debug)]
pub struct AudioRecord<'a> {
    /// The two digits of the record magic (`05wb` is 5).
    pub stream_index: u8,
    /// The record's payload.
    pub data: &'a [u8],
    /// Timestamp of the last video picture before this record, if any since
    /// the last discontinuity.
    pub after_video_pts_us: Option<u64>,
    /// Arrival time on the host clock (µs), the same clock as the render
    /// callback's host times.
    pub arrived_us: u64,
    /// Arrival wall-clock time (µs since the Unix epoch), for tap blocks.
    pub wall_clock_us: Option<i64>,
}

/// Everything a tile reports (the `audio_stats` event).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioStats {
    /// Records received.
    pub records: u64,
    /// Payload bytes received.
    pub bytes: u64,
    /// Records decoded.
    pub decoded: u64,
    /// Records replaced by silence (a valid frame the decoder rejected, or a
    /// corrupt record in an AAC stream), so the sample count stays right.
    pub concealed: u64,
    /// Records dropped (corrupt, oversized, or before any format was known).
    pub rejected: u64,
    /// Codec of the last decoded record.
    pub codec: Option<AudioCodec>,
    /// Format of the last decoded record.
    pub format: Option<PcmFormat>,
    /// Current timestamp epoch.
    pub epoch: u32,
    /// Estimated source rate in samples per camera second.
    pub rate_estimate: Option<f64>,
    /// Timestamper counters.
    pub timestamps: TimestampStats,
    /// Playout state, while the tile is audible.
    pub playout: Option<PlayoutStats>,
    /// The playout delay in use, µs (see [`TileConfig::playout_delay_us`]).
    pub playout_delay_us: f64,
    /// The playout delay it is moving to, once enough has been observed.
    pub playout_delay_target_us: Option<f64>,
    /// True when the tile should be audible but every output slot is taken.
    pub no_free_slot: bool,
    /// Tap counters, while a tap is attached.
    pub tap: Option<TapStats>,
}

struct TapState {
    sender: TapSender,
    /// No block sent yet: the first one is marked as a discontinuity.
    fresh: bool,
}

/// The per-tile audio pipeline. Lives on the tile's thread; every call is
/// non-blocking and bounded.
pub struct TileAudio {
    decoder: Decoder,
    format: Option<PcmFormat>,
    timestamper: Option<Timestamper>,
    explicit_map: Option<PresentationMap>,
    estimator: PresentationEstimator,
    estimator_settled: bool,
    delay: DelayControl,
    output: Option<AudioOutput>,
    audible: bool,
    volume: f32,
    muted: bool,
    playout: Option<Playout>,
    tap: Option<TapState>,
    /// The 16 kHz mono conversion, run while a tap or `mono_output` reads it.
    mono: Mono16k,
    mono_output: bool,
    pcm: Vec<f32>,
    stats: AudioStats,
    owner: u64,
    level: Option<std::sync::Arc<crate::DecodeLevel>>,
    /// Number of the first sample of every epoch (0 except in tests).
    sample_base: u64,
}

impl std::fmt::Debug for TileAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TileAudio")
            .field("stats", &self.stats())
            .finish()
    }
}

impl TileAudio {
    /// A tile with nothing attached: it decodes and timestamps, and plays or
    /// taps once an output or tap is attached.
    pub fn new(config: TileConfig) -> TileAudio {
        TileAudio {
            decoder: Decoder::new(),
            format: None,
            timestamper: None,
            explicit_map: None,
            estimator: PresentationEstimator::new(),
            estimator_settled: false,
            delay: DelayControl::new(config.playout_delay_us),
            output: None,
            audible: false,
            volume: 1.0,
            muted: false,
            playout: None,
            tap: None,
            mono: Mono16k::default(),
            mono_output: false,
            pcm: Vec::new(),
            stats: AudioStats::default(),
            owner: 0,
            level: None,
            sample_base: 0,
        }
    }

    /// Numbers the samples of every epoch from `base` instead of 0, as if
    /// the counters had run for a long time. For tests of long-running
    /// numerics; call before the first record.
    #[doc(hidden)]
    pub fn set_sample_base(&mut self, base: u64) {
        self.sample_base = base;
    }

    /// Tags the playout slots this tile claims from now on, so the
    /// renderer's levels ([`crate::Renderer::levels`]) name it.
    pub fn set_owner(&mut self, owner: u64) {
        self.owner = owner;
    }

    /// Measures every decoded record into `level` (`None` stops), audible
    /// or not, at its arrival time.
    pub fn set_level_meter(&mut self, level: Option<std::sync::Arc<crate::DecodeLevel>>) {
        self.level = level;
    }

    /// Keeps the 16 kHz mono form of every record available through
    /// [`Self::mono_block`] (for sound analysis), audible or not, tapped or
    /// not. Off by default; a tap turns the conversion on by itself.
    pub fn set_mono_output(&mut self, enabled: bool) {
        self.mono_output = enabled;
        if !self.mono_wanted() {
            self.mono.reset();
        }
    }

    /// The 16 kHz mono samples of the last record pushed, while
    /// [`Self::set_mono_output`] or a tap is on; `None` when that record
    /// produced none (rejected, or still inside the resampler's look-ahead).
    /// The same samples the tap carries.
    pub fn mono_block(&self) -> Option<MonoBlock<'_>> {
        self.mono.block()
    }

    fn mono_wanted(&self) -> bool {
        self.mono_output || self.tap.is_some()
    }

    /// One audio record, in stream order.
    pub fn push(&mut self, rec: AudioRecord<'_>) {
        self.mono.clear_ready();
        self.stats.records += 1;
        self.stats.bytes += rec.data.len() as u64;
        let mut pcm = std::mem::take(&mut self.pcm);
        pcm.clear();
        let format = match self.decoder.decode(rec.stream_index, rec.data, &mut pcm) {
            Ok(d) => {
                self.stats.decoded += 1;
                self.stats.codec = Some(d.codec);
                Some(d.format)
            }
            Err(DecodeError::Aac { format, frames, .. }) => {
                self.stats.concealed += 1;
                pcm.resize(frames * format.channels, 0.0);
                Some(format)
            }
            Err(_) => match self.format {
                // A corrupt record in an AAC stream still stood for one frame.
                Some(f) if self.stats.codec == Some(AudioCodec::Aac) => {
                    self.stats.concealed += 1;
                    pcm.resize(crate::adts::SAMPLES_PER_BLOCK * f.channels, 0.0);
                    Some(f)
                }
                _ => {
                    self.stats.rejected += 1;
                    None
                }
            },
        };
        if let Some(f) = format {
            self.push_pcm(
                f,
                &pcm,
                rec.after_video_pts_us,
                rec.arrived_us,
                rec.wall_clock_us,
            );
        }
        self.pcm = pcm;
    }

    /// Decoded samples (interleaved) standing for one record. [`Self::push`]
    /// calls this; it is public for callers that decode elsewhere and for
    /// simulations.
    pub fn push_pcm(
        &mut self,
        format: PcmFormat,
        pcm: &[f32],
        after_video_pts_us: Option<u64>,
        arrived_us: u64,
        wall_clock_us: Option<i64>,
    ) {
        self.mono.clear_ready();
        let channels = format.channels.max(1);
        let frames = pcm.len() / channels;
        if let Some(level) = &self.level {
            level.observe(pcm, channels, format.sample_rate, arrived_us);
        }
        if self.format != Some(format) {
            if self.format.is_some() {
                self.decoder.reset();
            }
            self.format = Some(format);
            match self.timestamper.as_mut() {
                Some(t) => t.reset_with_rate(format.sample_rate),
                None => {
                    self.timestamper =
                        Some(Timestamper::with_base(format.sample_rate, self.sample_base));
                }
            }
        }
        let Some(ts) = self.timestamper.as_mut() else {
            return;
        };
        let stamp = ts.push(frames, after_video_pts_us);
        // Samples since the epoch started.
        let into_epoch = stamp.first_sample - ts.base();
        self.stats.format = Some(format);
        self.stats.epoch = stamp.epoch;
        self.stats.timestamps = ts.stats();
        self.stats.rate_estimate = stamp.timeline.map(|t| t.rate());

        if stamp.reset {
            self.estimator.reset();
            self.estimator_settled = false;
            self.delay.restart();
        }
        let mut sent = None;
        if let Some(tl) = stamp.timeline {
            // The first second's timeline is rough; start the delay envelope
            // over once it has settled.
            if !self.estimator_settled && into_epoch >= u64::from(format.sample_rate) * 2 {
                self.estimator.reset();
                self.estimator_settled = true;
            }
            let s = tl.at((stamp.first_sample + frames as u64) as f64);
            self.estimator.observe(s, arrived_us as f64);
            sent = Some(s);
        }
        let map = self.explicit_map.or_else(|| self.estimator.map());
        let record_s = frames as f64 / f64::from(format.sample_rate.max(1));
        let delay = match sent {
            Some(s) => {
                self.delay
                    .observe(s, arrived_us as f64, record_s * 1e6, map.as_ref(), record_s)
            }
            None => crate::delay::Delay {
                us: self.delay.current(),
                rate: 0.0,
            },
        };
        if let Some(p) = self.playout.as_mut() {
            p.push(Block {
                epoch: stamp.epoch,
                first_sample: stamp.first_sample,
                format,
                pcm,
                timeline: stamp.timeline,
                // Playout starts once the map has settled (the first two
                // seconds of an estimated one are rough, and it starts over
                // then) and has a second of deliveries for its median:
                // placing audio on the rough map and correcting it a second
                // later cost two or three audible jumps after every
                // (re)start in a replayed LAN recording; waiting costs three
                // seconds of audio once.
                map: map.filter(|_| {
                    self.explicit_map.is_some()
                        || (self.estimator_settled
                            && into_epoch >= u64::from(format.sample_rate) * 3)
                }),
                playout_delay_us: delay.us,
                delay_rate: delay.rate,
                now_us: arrived_us as f64,
            });
        }
        if self.mono_wanted() {
            self.mono
                .push(format, pcm, &stamp, wall_clock_us, arrived_us);
            if let (Some(t), Some(b)) = (self.tap.as_mut(), self.mono.block()) {
                t.push(&b);
            }
        }
    }

    /// The stream restarted (reconnect, overflow upstream): timing, the
    /// decoder, and playout start over.
    pub fn discontinuity(&mut self) {
        self.decoder.reset();
        if let Some(t) = self.timestamper.as_mut() {
            t.reset();
        }
        self.estimator.reset();
        self.estimator_settled = false;
        self.delay.restart();
        if let Some(p) = self.playout.as_mut() {
            p.restart();
        }
    }

    /// Supplies the video presentation map (`None` returns to estimating it
    /// from arrival times).
    pub fn set_presentation(&mut self, map: Option<PresentationMap>) {
        self.explicit_map = map;
    }

    /// Attaches the audio output (or detaches with `None`).
    pub fn set_output(&mut self, output: Option<AudioOutput>) {
        let same = match (&self.output, &output) {
            (Some(a), Some(b)) => a.same_as(b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.playout = None;
            self.output = output;
            self.update_playout();
        }
    }

    /// Makes the tile audible or silent. A silent tile releases its output
    /// slot; taps keep running either way.
    pub fn set_audible(&mut self, audible: bool) {
        self.audible = audible;
        self.update_playout();
    }

    /// Volume, 0 to 1 (more amplifies).
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = if volume.is_finite() {
            volume.max(0.0)
        } else {
            0.0
        };
        self.apply_gain();
    }

    /// Mutes without releasing the slot (playout keeps its timing).
    pub fn set_muted(&mut self, muted: bool) {
        self.muted = muted;
        self.apply_gain();
    }

    /// Attaches a tap (or detaches with `None`).
    pub fn set_tap(&mut self, tap: Option<TapSender>) {
        self.tap = tap.map(|sender| TapState {
            sender,
            fresh: true,
        });
        if !self.mono_wanted() {
            self.mono.reset();
        }
    }

    /// Current counters and state.
    pub fn stats(&self) -> AudioStats {
        AudioStats {
            playout: self.playout.as_ref().map(Playout::stats),
            tap: self.tap.as_ref().map(|t| t.sender.stats()),
            no_free_slot: self.audible && self.output.is_some() && self.playout.is_none(),
            playout_delay_us: self.delay.current(),
            playout_delay_target_us: self.delay.target(),
            ..self.stats
        }
    }

    /// The current sample-to-camera-time mapping.
    pub fn timeline(&self) -> Option<Timeline> {
        self.timestamper.as_ref()?.timeline()
    }

    /// The presentation map in use.
    pub fn presentation(&self) -> Option<PresentationMap> {
        self.explicit_map.or_else(|| self.estimator.map())
    }

    /// For tests and diagnostics: the next ring frame playout will write,
    /// the source sample position it stands for, and its epoch.
    pub fn playout_probe(&self) -> Option<(u64, f64, u32)> {
        self.playout.as_ref()?.probe()
    }

    fn update_playout(&mut self) {
        match (&self.output, self.audible) {
            (Some(o), true) => {
                if self.playout.is_none() {
                    self.playout = Playout::new(o, self.owner);
                    if let Some(p) = &self.playout {
                        p.set_playing(true);
                    }
                    self.apply_gain();
                }
            }
            _ => self.playout = None,
        }
    }

    fn apply_gain(&self) {
        if let Some(p) = &self.playout {
            p.set_gain(if self.muted { 0.0 } else { self.volume });
        }
    }
}

impl TapState {
    fn push(&mut self, b: &MonoBlock<'_>) {
        let fresh = std::mem::take(&mut self.fresh);
        self.sender.push(TapBlock {
            samples: b.samples.to_vec(),
            epoch: b.epoch,
            first_sample: b.first_sample,
            camera_time_us: b.camera_time_us,
            wall_clock_us: b.wall_clock_us,
            discontinuity: b.discontinuity || fresh,
        });
    }
}
