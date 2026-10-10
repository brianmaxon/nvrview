//! Drift-corrected playout of one tile into its output slot (producer side).
//!
//! Every block of decoded samples arrives with a [`Timeline`] (sample number
//! to camera time) and a [`PresentationMap`] (camera time to the host time its
//! picture is shown). The goal for each sample is
//!
//! ```text
//! heard(sample) = presented(camera_time(sample)) + lip_sync
//! ```
//!
//! The renderer publishes, every call, which ring frame plays at which host
//! time, so the producer can predict when the next frame it writes will be
//! heard. The difference from the goal is the A/V error. A proportional plus
//! small integral controller turns it into a resampling ratio around the
//! feed-forward ratio (the camera's estimated audio rate, through the
//! camera-to-host clock ratio, over the output rate), and the ratio stays
//! within ±[`MAX_CORRECTION`] of nominal, which is inaudible. An error beyond
//! [`RESYNC_US`] (a stall, a reconnect, the first block) is fixed at once:
//! the renderer fades out and drops what is queued, and the producer writes
//! silence or skips input so the next sample lands on time, then fades in.

use crate::decode::PcmFormat;
use crate::output::{AudioOutput, FADE_FRAMES, SlotLease};
use crate::presentation::PresentationMap;
use crate::resample::Resampler;
use crate::timestamp::Timeline;

/// Largest deviation of the resampling ratio from nominal.
pub const MAX_CORRECTION: f64 = 0.005;
/// Errors beyond this are fixed by a jump, not by the ratio.
pub const RESYNC_US: f64 = 250_000.0;
/// For the first [`WARMUP_S`] seconds after a start, while the presentation
/// and timestamp estimates are still settling, errors beyond this are fixed
/// by a jump too: correcting them at the bounded rate would take tens of
/// seconds.
pub const RESYNC_WARMUP_US: f64 = 60_000.0;
/// Length of the warm-up after a start, seconds of audio. During it the
/// controller also reacts [`WARMUP_GAIN`] times faster.
pub const WARMUP_S: f64 = 60.0;
/// Controller speed-up during the warm-up.
const WARMUP_GAIN: f64 = 4.0;
/// Proportional gain: correction per second of error.
const KP: f64 = 1.0 / 60.0;
/// Integral gain: correction per second² of accumulated error.
const KI: f64 = KP / 600.0;
/// Time constant of the error filter, seconds.
const ERROR_TAU_S: f64 = 5.0;
/// The renderer is considered stopped when its last call is older than this.
const STALL_US: f64 = 500_000.0;

/// One block for playout.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Block<'a> {
    pub epoch: u32,
    pub first_sample: u64,
    pub format: PcmFormat,
    pub pcm: &'a [f32],
    pub timeline: Option<Timeline>,
    pub map: Option<PresentationMap>,
    pub playout_delay_us: f64,
    /// Rate of change of the playout delay (µs per µs).
    pub delay_rate: f64,
    pub now_us: f64,
}

/// Playout counters and state.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayoutStats {
    /// Filtered A/V error, µs: positive when audio is late against its target.
    pub av_offset_us: Option<f64>,
    /// Frames queued in the ring at the last block.
    pub buffered_frames: u64,
    /// Deviation of the resampling ratio from nominal (the estimated clock
    /// skew plus the controller's correction).
    pub correction: f64,
    /// Hard resyncs after the first start.
    pub resyncs: u64,
    /// Starts (first block, and after each discontinuity or stall).
    pub starts: u64,
    /// Frames that did not fit in the ring.
    pub overflow_frames: u64,
    /// Blocks dropped because the output was not running or no presentation
    /// map or timeline was known.
    pub blocks_not_played: u64,
    /// Renderer underrun events and the frames of silence they caused.
    pub underruns: u64,
    /// Frames of silence from underruns.
    pub underrun_frames: u64,
}

pub(crate) struct Playout {
    lease: SlotLease,
    out_rate: f64,
    out_channels: usize,
    src: Option<(PcmFormat, u32)>,
    resampler: Option<Resampler>,
    need_start: bool,
    /// Seconds of audio played since the last start.
    since_start_s: f64,
    /// Generation of a skip request the renderer has not carried out yet.
    awaiting_skip: Option<u16>,
    fade_left: usize,
    err_f: Option<f64>,
    integ: f64,
    stats: PlayoutStats,
    scratch: Vec<f32>,
    mapped: Vec<f32>,
}

impl Playout {
    /// Claims a slot on `output`, or `None` when none is free.
    pub fn new(output: &AudioOutput, owner: u64) -> Option<Playout> {
        let lease = output.claim(owner)?;
        let f = lease.format();
        Some(Playout {
            lease,
            out_rate: f64::from(f.sample_rate),
            out_channels: f.channels,
            src: None,
            resampler: None,
            need_start: true,
            since_start_s: 0.0,
            awaiting_skip: None,
            fade_left: 0,
            err_f: None,
            integ: 0.0,
            stats: PlayoutStats::default(),
            scratch: Vec::new(),
            mapped: Vec::new(),
        })
    }

    pub fn set_playing(&self, on: bool) {
        self.lease.set_playing(on);
    }

    pub fn set_gain(&self, gain: f32) {
        self.lease.set_gain(gain);
    }

    /// Starts over (discontinuity): the next block is placed from scratch.
    pub fn restart(&mut self) {
        self.need_start = true;
        self.resampler = None;
    }

    pub fn stats(&self) -> PlayoutStats {
        let (u, uf) = self.lease.underruns();
        PlayoutStats {
            underruns: u,
            underrun_frames: uf,
            ..self.stats
        }
    }

    /// For tests: (next ring frame, input position it stands for, epoch).
    pub fn probe(&self) -> Option<(u64, f64, u32)> {
        let r = self.resampler.as_ref()?;
        Some((
            self.lease.ring().write_index(),
            r.position(),
            self.src.map_or(0, |s| s.1),
        ))
    }

    pub fn push(&mut self, b: Block<'_>) {
        // A new epoch or format starts over.
        if self.src != Some((b.format, b.epoch)) || self.resampler.is_none() {
            self.src = Some((b.format, b.epoch));
            let mut r = Resampler::new(
                b.format.channels,
                b.format.sample_rate,
                self.out_rate as u32,
            );
            r.reset(b.first_sample as f64);
            self.resampler = Some(r);
            self.need_start = true;
        }
        let Some(rs) = self.resampler.as_mut() else {
            return;
        };
        rs.push(b.first_sample as i64, b.pcm);

        let snap = self.lease.snapshot();
        let (Some(tl), Some(map), Some(snap)) = (b.timeline, b.map, snap) else {
            return self.not_played(b);
        };
        let snap_host_us = snap.host_ns as f64 / 1e3;
        // Frames per host second as the renderer runs (nominally the
        // output rate; see the renderer's clock).
        let frame_rate = if snap.ns_per_frame > 0.0 {
            1e9 / snap.ns_per_frame
        } else {
            self.out_rate
        };
        let next_call_us = snap_host_us + snap.frames as f64 * 1e6 / frame_rate;
        if b.now_us - next_call_us > STALL_US {
            return self.not_played(b);
        }
        if let Some(gen_) = self.awaiting_skip
            && snap.skip_gen == gen_
        {
            self.awaiting_skip = None;
        }

        let target_of = |pos: f64| map.host_at(tl.at(pos)) + b.playout_delay_us;
        let ring = self.lease.ring();
        let w = ring.write_index();
        let mut error = None;
        if !self.need_start && self.awaiting_skip.is_none() {
            let heard = snap_host_us + (w as f64 - snap.read as f64) * 1e6 / frame_rate;
            let e = heard - target_of(rs.position());
            let limit = if self.since_start_s < WARMUP_S {
                RESYNC_WARMUP_US
            } else {
                RESYNC_US
            };
            // After a stall longer than the buffer, the renderer has moved
            // on (it skips frames whose time has passed): what we would write
            // now is already late, so jump instead of filling the ring with
            // audio that will only be dropped.
            let late = b.now_us - heard;
            if e.abs() > limit || late > RESYNC_US {
                self.need_start = true;
                self.stats.resyncs += 1;
            } else {
                error = Some(e);
            }
        }

        if self.need_start {
            // Drop what is queued, then line the next frame up with its
            // target by writing silence or skipping input.
            self.awaiting_skip = Some(self.lease.request_skip(w));
            let heard = next_call_us.max(b.now_us) + FADE_FRAMES as f64 * 1e6 / frame_rate;
            let delay_us = target_of(rs.position()) - heard;
            if delay_us >= 0.0 {
                let cap = (ring.capacity() / 2) as f64;
                let frames = (delay_us * frame_rate / 1e6).min(cap) as usize;
                self.scratch.clear();
                self.scratch.resize(frames * self.out_channels, 0.0);
                let n = ring.push(&self.scratch);
                self.stats.overflow_frames += (frames - n) as u64;
            } else {
                // Input samples per host µs.
                let per_us = tl.rate() / map.host_per_camera / 1e6;
                rs.skip_to(rs.position() + -delay_us * per_us);
            }
            self.need_start = false;
            self.fade_left = FADE_FRAMES;
            self.err_f = None;
            self.integ = 0.0;
            self.stats.starts += 1;
            self.since_start_s = 0.0;
        }

        // Ratio: feed-forward plus correction.
        let dt = b.pcm.len() as f64 / b.format.channels as f64 / f64::from(b.format.sample_rate);
        self.since_start_s += dt;
        let mut correction = 0.0;
        if let Some(e) = error {
            let e_s = e / 1e6;
            let a = (dt / ERROR_TAU_S).min(1.0);
            let f = self.err_f.map_or(e_s, |p| p + a * (e_s - p));
            self.err_f = Some(f);
            let kp = if self.since_start_s < WARMUP_S {
                KP * WARMUP_GAIN
            } else {
                KP
            };
            let raw = kp * f + KI * self.integ;
            if raw.abs() < MAX_CORRECTION {
                self.integ += f * dt;
            }
            correction = (kp * f + KI * self.integ).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        }
        let nominal = f64::from(b.format.sample_rate) / frame_rate;
        let feed_forward = tl.rate() / map.host_per_camera / frame_rate;
        // A growing playout delay means each sample is heard that much
        // later than the last: play slower by its rate (and faster while it
        // shrinks), so the change needs no jump and no correction.
        let step = (feed_forward * (1.0 + correction - b.delay_rate)).clamp(
            nominal * (1.0 - MAX_CORRECTION),
            nominal * (1.0 + MAX_CORRECTION),
        );
        // Clock skew and controller only; the delay's own rate is reported
        // with the delay.
        self.stats.correction = step / nominal - 1.0 + b.delay_rate;
        self.stats.av_offset_us = self.err_f.map(|f| f * 1e6);

        self.scratch.clear();
        let frames = rs.produce(step, usize::MAX, &mut self.scratch);
        let src_ch = b.format.channels;
        self.mapped.clear();
        for k in 0..frames {
            let fr = &self.scratch[k * src_ch..(k + 1) * src_ch];
            let mut g = 1.0;
            if self.fade_left > 0 {
                g = 1.0 - self.fade_left as f32 / (FADE_FRAMES + 1) as f32;
                self.fade_left -= 1;
            }
            match (src_ch, self.out_channels) {
                (1, 1) => self.mapped.push(fr[0] * g),
                (1, _) => {
                    self.mapped.push(fr[0] * g);
                    self.mapped.push(fr[0] * g);
                }
                (_, 1) => self.mapped.push(0.5 * (fr[0] + fr[1]) * g),
                _ => {
                    self.mapped.push(fr[0] * g);
                    self.mapped.push(fr[1] * g);
                }
            }
        }
        let n = ring.push(&self.mapped);
        self.stats.overflow_frames += (frames - n) as u64;
        self.stats.buffered_frames = ring.queued();
    }

    fn not_played(&mut self, b: Block<'_>) {
        self.stats.blocks_not_played += 1;
        self.need_start = true;
        // Keep up with the input so nothing backs up.
        if let Some(rs) = self.resampler.as_mut() {
            let end = (b.first_sample as usize + b.pcm.len() / b.format.channels.max(1)) as f64;
            rs.skip_to(end);
        }
    }
}
