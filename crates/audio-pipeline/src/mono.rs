//! A tile's decoded audio as 16 kHz mono: what taps carry and what sound
//! analysis reads (docs/AUDIO.md, docs/PLAYBACK.md "Sound").
//!
//! Channels are averaged and other rates are resampled to [`TAP_RATE`].
//! Buffers are reused, so after the first records of a stream converting a
//! record allocates nothing.

use crate::decode::PcmFormat;
use crate::resample::Resampler;
use crate::tap::TAP_RATE;
use crate::timestamp::Stamp;

/// The 16 kHz mono samples standing for one record, borrowed from the tile
/// ([`crate::TileAudio::mono_block`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonoBlock<'a> {
    /// 16 kHz mono samples in -1..1. Empty never: a record that produced
    /// no output yet (the resampler's look-ahead) gives no block.
    pub samples: &'a [f32],
    /// Timestamp epoch; numbering and camera times restart with each.
    pub epoch: u32,
    /// Number of the first sample in this epoch, at 16 kHz.
    pub first_sample: u64,
    /// Camera time (µs) of the first sample, once a video timestamp has
    /// been seen in this epoch.
    pub camera_time_us: Option<f64>,
    /// Arrival wall-clock time (µs since the Unix epoch) of the record, if
    /// the caller supplied one.
    pub wall_clock_us: Option<i64>,
    /// Arrival time of the record on the host clock (µs).
    pub arrived_us: u64,
    /// True for the first block after a reset (stream restart, format
    /// change, epoch change) or after the conversion was switched on.
    pub discontinuity: bool,
}

/// The converter. Runs only while something reads its output.
#[derive(Default)]
pub(crate) struct Mono16k {
    key: Option<(PcmFormat, u32)>,
    resampler: Option<Resampler>,
    mono: Vec<f32>,
    out: Vec<f32>,
    epoch: u32,
    first_sample: u64,
    camera: Option<f64>,
    wall: Option<i64>,
    arrived_us: u64,
    /// A discontinuity not yet reported (kept until a block has samples).
    pending_discontinuity: bool,
    discontinuity: bool,
    ready: bool,
}

impl Mono16k {
    /// Forgets the stream: the next record starts afresh.
    pub(crate) fn reset(&mut self) {
        self.key = None;
        self.resampler = None;
        self.ready = false;
    }

    /// Marks that the last record gave no block (yet).
    pub(crate) fn clear_ready(&mut self) {
        self.ready = false;
    }

    /// Converts one record's interleaved samples.
    pub(crate) fn push(
        &mut self,
        format: PcmFormat,
        pcm: &[f32],
        stamp: &Stamp,
        wall: Option<i64>,
        arrived_us: u64,
    ) {
        self.ready = false;
        let ch = format.channels.max(1);
        let key = (format, stamp.epoch);
        if self.key != Some(key) {
            self.key = Some(key);
            self.pending_discontinuity = true;
            self.resampler = (format.sample_rate != TAP_RATE).then(|| {
                let mut r = Resampler::new(1, format.sample_rate, TAP_RATE);
                r.reset(stamp.first_sample as f64);
                r
            });
        }
        let mono = match self.resampler {
            None => &mut self.out,
            Some(_) => &mut self.mono,
        };
        mono.clear();
        if ch == 1 {
            mono.extend_from_slice(pcm);
        } else {
            mono.extend(
                pcm.chunks_exact(ch)
                    .map(|f| f.iter().sum::<f32>() / ch as f32),
            );
        }
        let (first_sample, camera) = match self.resampler.as_mut() {
            None => (
                stamp.first_sample,
                stamp.timeline.map(|t| t.at(stamp.first_sample as f64)),
            ),
            Some(r) => {
                r.push(stamp.first_sample as i64, &self.mono);
                let pos = r.position();
                let step = f64::from(format.sample_rate) / f64::from(TAP_RATE);
                self.out.clear();
                r.produce(step, usize::MAX, &mut self.out);
                (
                    (pos / step).round() as u64,
                    stamp.timeline.map(|t| t.at(pos)),
                )
            }
        };
        if self.out.is_empty() {
            return;
        }
        self.epoch = stamp.epoch;
        self.first_sample = first_sample;
        self.camera = camera;
        self.wall = wall;
        self.arrived_us = arrived_us;
        self.discontinuity = std::mem::take(&mut self.pending_discontinuity);
        self.ready = true;
    }

    /// The block of the last record, if it produced one.
    pub(crate) fn block(&self) -> Option<MonoBlock<'_>> {
        self.ready.then_some(MonoBlock {
            samples: &self.out,
            epoch: self.epoch,
            first_sample: self.first_sample,
            camera_time_us: self.camera,
            wall_clock_us: self.wall,
            arrived_us: self.arrived_us,
            discontinuity: self.discontinuity,
        })
    }
}
