//! A simulated camera, network, and audio device driving one tile on a
//! virtual host clock, and a measurement of when each source sample is heard.
//!
//! Camera: video pictures at `fps` stamped with camera time; audio at
//! `audio_rate` samples per camera second, sent as 1024-sample records once
//! their last sample is captured, after every picture captured before then.
//! One camera microsecond lasts `camera_skew` host microseconds.
//!
//! Network: every message (picture or audio record) is delayed by
//! `base_delay_us` plus a deviation from [`Jitter`], delivered in order (TCP).
//! During a stall nothing arrives; everything sent during it arrives in a
//! burst at its end.
//!
//! Device: `out_rate` frames per device second, where a device second lasts
//! `1 / device_skew` host seconds; callbacks of `callback_frames`; the host
//! time passed to the renderer is the callback time plus `output_latency_us`.
//!
//! Measurement: the source has a single-sample impulse every `click_every`
//! samples. The renderer's output is scanned for peaks; for each, the
//! latency is the host time it is heard minus the host time at which its
//! sample was captured. Playout aims at the delivery delay reference (the
//! base delay plus the percentile of the jitter that the presentation
//! estimator uses) plus the playout delay in effect for that click (adaptive
//! unless the config fixes it). [`SimResult::errors`] gives each click's
//! deviation from that.

#![allow(dead_code)]

use audio_pipeline::presentation::DELAY_PERCENTILE;
use audio_pipeline::{
    AudioOutput, OutputConfig, OutputFormat, PcmFormat, TileAudio, TileConfig, output,
};

/// Delivery-delay deviation per message.
#[derive(Clone, Copy, Debug)]
pub enum Jitter {
    /// Uniform in `0..max_us`.
    Uniform { max_us: f64 },
    /// A slowly wandering component (first-order autoregressive, standard
    /// deviation `slow_sd_us`, correlation time `slow_tau_s`) plus a
    /// heavy-tailed fast one (Laplace with scale `fast_scale_us`), the sum
    /// clipped to `clip_us`. [`Jitter::measured`] has a standard deviation
    /// of about 65 ms and extremes a little beyond those measured on a LAN
    /// (67 ms, -131 ms and +177 ms in 10 min).
    Heavy {
        slow_sd_us: f64,
        slow_tau_s: f64,
        fast_scale_us: f64,
        clip_us: (f64, f64),
    },
}

impl Jitter {
    pub fn measured() -> Jitter {
        Jitter::Heavy {
            slow_sd_us: 55_000.0,
            slow_tau_s: 2.0,
            fast_scale_us: 20_000.0,
            clip_us: (-160_000.0, 215_000.0),
        }
    }
}

pub struct SimConfig {
    pub audio_rate: f64,
    pub nominal_rate: u32,
    pub fps: f64,
    pub camera_skew: f64,
    pub device_skew: f64,
    pub base_delay_us: f64,
    pub jitter: Jitter,
    pub out_rate: u32,
    pub out_channels: usize,
    pub callback_frames: usize,
    pub output_latency_us: f64,
    pub click_every: u64,
    pub duration_s: f64,
    pub playout_delay_us: Option<f64>,
    /// (start, length) in camera seconds: the camera sends no audio.
    pub audio_gaps: Vec<(f64, f64)>,
    /// (start, length) in camera seconds: nothing arrives until the end.
    pub stalls: Vec<(f64, f64)>,
    pub seed: u64,
}

impl Default for SimConfig {
    /// The measured case: audio about 20 ppm fast in camera time, camera and
    /// host clocks 8 ppm apart, measured jitter.
    fn default() -> Self {
        SimConfig {
            audio_rate: 16_000.33,
            nominal_rate: 16_000,
            fps: 7.5,
            camera_skew: 1.0 + 8e-6,
            device_skew: 1.0 - 20e-6,
            base_delay_us: 200_000.0,
            jitter: Jitter::measured(),
            out_rate: 48_000,
            out_channels: 2,
            callback_frames: 512,
            output_latency_us: 10_000.0,
            click_every: 16_000,
            duration_s: 60.0,
            playout_delay_us: None,
            audio_gaps: Vec::new(),
            stalls: Vec::new(),
            seed: 0x1234_5678_9ABC_DEF1,
        }
    }
}

struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        ((self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn normal(&mut self) -> f64 {
        let (u, v) = (self.unit(), self.unit());
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }
    fn laplace(&mut self, scale: f64) -> f64 {
        let u = self.unit() - 0.5;
        -scale * u.signum() * (1.0 - 2.0 * u.abs()).ln()
    }
}

struct JitterGen {
    model: Jitter,
    slow: f64,
    last_t: f64,
}

impl JitterGen {
    fn next(&mut self, rng: &mut Rng, t_s: f64) -> f64 {
        match self.model {
            Jitter::Uniform { max_us } => rng.unit() * max_us,
            Jitter::Heavy {
                slow_sd_us,
                slow_tau_s,
                fast_scale_us,
                clip_us,
            } => {
                let a = (-(t_s - self.last_t).max(0.0) / slow_tau_s).exp();
                self.last_t = t_s;
                self.slow = a * self.slow + (1.0 - a * a).sqrt() * slow_sd_us * rng.normal();
                (self.slow + rng.laplace(fast_scale_us)).clamp(clip_us.0, clip_us.1)
            }
        }
    }
}

/// One heard click.
pub struct Heard {
    /// Camera second of the click's source sample.
    pub camera_s: f64,
    /// Host time heard minus host time captured, µs.
    pub latency_us: f64,
}

pub struct SimResult {
    pub heard: Vec<Heard>,
    pub clicks_sent: u64,
    pub tile: TileAudio,
    pub output: AudioOutput,
    /// The latency playout aims at at the end, µs.
    pub expected_latency_us: f64,
    /// The delivery delay reference, µs: the latency without playout delay.
    pub reference_us: f64,
    /// (camera second, playout delay in µs) after every record.
    pub delays: Vec<(f64, f64)>,
    /// Jitter deviations generated (for reporting).
    pub jitter_sd_us: f64,
    pub jitter_range_us: (f64, f64),
}

impl SimResult {
    /// (camera second, deviation from the expected latency in µs) per click.
    pub fn errors(&self) -> impl Iterator<Item = (f64, f64)> + '_ {
        self.heard.iter().map(|h| {
            (
                h.camera_s,
                h.latency_us - self.reference_us - self.delay_at(h.camera_s),
            )
        })
    }

    /// The playout delay used for the record holding camera second `t`.
    pub fn delay_at(&self, t: f64) -> f64 {
        let i = self.delays.partition_point(|&(s, _)| s < t);
        self.delays
            .get(i)
            .or(self.delays.last())
            .map_or(0.0, |&(_, d)| d)
    }

    /// Largest absolute deviation after `settle_s` and the number of clicks.
    pub fn worst_after(&self, settle_s: f64) -> (f64, usize) {
        let mut worst = 0.0f64;
        let mut n = 0;
        for (_, e) in self.errors().filter(|(t, _)| *t >= settle_s) {
            worst = worst.max(e.abs());
            n += 1;
        }
        (worst, n)
    }
}

pub const HOST0_US: f64 = 1_000_000_000.0;
pub const PTS0_US: f64 = 7_000_000.0;

pub fn run(cfg: &SimConfig, observe: impl FnMut(f64, &TileAudio)) -> SimResult {
    run_with(cfg, |_| {}, observe)
}

/// Runs the simulation. `setup` can attach taps; `observe` is called after
/// every record with the camera second and the tile.
pub fn run_with(
    cfg: &SimConfig,
    setup: impl FnOnce(&mut TileAudio),
    mut observe: impl FnMut(f64, &TileAudio),
) -> SimResult {
    let (out, mut renderer) = output(
        OutputFormat {
            sample_rate: cfg.out_rate,
            channels: cfg.out_channels,
        },
        OutputConfig::default(),
    );
    let tile_cfg = TileConfig {
        playout_delay_us: cfg.playout_delay_us,
    };
    let mut tile = TileAudio::new(tile_cfg);
    tile.set_output(Some(out.clone()));
    tile.set_audible(true);
    setup(&mut tile);

    let mut rng = Rng(cfg.seed);
    let mut jit = JitterGen {
        model: cfg.jitter,
        slow: 0.0,
        last_t: 0.0,
    };
    let host_of_cam = |cam_s: f64| HOST0_US + cam_s * 1e6 * cfg.camera_skew;
    let fmt = PcmFormat {
        sample_rate: cfg.nominal_rate,
        channels: 1,
    };
    let frame_s = 1.0 / cfg.fps;
    let rec_s = 1024.0 / cfg.audio_rate;

    let mut next_video = 0u64;
    let mut next_audio = 0u64;
    let mut last_pts: Option<u64> = None;
    let mut last_arrival = 0.0f64;
    let us_per_frame = 1e6 / (f64::from(cfg.out_rate) * cfg.device_skew);
    let mut next_cb = 0u64;
    let mut buf = vec![0.0f32; cfg.callback_frames * cfg.out_channels];
    let mut pcm = vec![0.0f32; 1024];
    let mut heard = Vec::new();
    let mut detector = Detector::default();
    let mut clicks_sent = 0u64;
    let mut devs = Vec::new();
    let mut delays = Vec::new();

    let cb_start = HOST0_US + 50_000.0;
    loop {
        let t_video = next_video as f64 * frame_s;
        let t_audio = (next_audio + 1) as f64 * rec_s;
        if t_video.min(t_audio) > cfg.duration_s {
            break;
        }
        let is_video = t_video <= t_audio;
        let t_cam = if is_video { t_video } else { t_audio };
        let dev = jit.next(&mut rng, t_cam);
        devs.push(dev);
        let mut arrival = host_of_cam(t_cam) + (cfg.base_delay_us + dev).max(1_000.0);
        for &(s, len) in &cfg.stalls {
            if t_cam >= s && t_cam < s + len {
                arrival = arrival.max(host_of_cam(s + len) + cfg.base_delay_us);
            }
        }
        arrival = arrival.max(last_arrival);
        last_arrival = arrival;

        // Render every callback due before this arrival.
        loop {
            let cb_time = cb_start + next_cb as f64 * cfg.callback_frames as f64 * us_per_frame;
            if cb_time > arrival {
                break;
            }
            let start_us = cb_time + cfg.output_latency_us;
            renderer.render(&mut buf, (start_us * 1e3) as u64);
            detector.scan(&buf, cfg.out_channels, start_us, us_per_frame, |host_us| {
                heard.push(host_us);
            });
            next_cb += 1;
        }

        if is_video {
            last_pts = Some((PTS0_US + t_video * 1e6).round() as u64);
            next_video += 1;
        } else {
            let k = next_audio;
            next_audio += 1;
            if cfg
                .audio_gaps
                .iter()
                .any(|&(s, len)| t_audio >= s && t_audio < s + len)
            {
                continue;
            }
            pcm.fill(0.0);
            for (i, s) in pcm.iter_mut().enumerate() {
                let n = k * 1024 + i as u64;
                if n.is_multiple_of(cfg.click_every) && n > 0 {
                    *s = 0.9;
                    clicks_sent += 1;
                }
            }
            // The delay this record is played with (before the push moves it).
            delays.push((t_audio - rec_s, tile.stats().playout_delay_us));
            tile.push_pcm(fmt, &pcm, last_pts, arrival as u64, None);
            observe(t_audio, &tile);
        }
    }

    let mut sorted = devs.clone();
    sorted.sort_by(f64::total_cmp);
    let p_low = sorted[((sorted.len() - 1) as f64 * DELAY_PERCENTILE) as usize];
    let mean = devs.iter().sum::<f64>() / devs.len() as f64;
    let sd = (devs.iter().map(|d| (d - mean).powi(2)).sum::<f64>() / devs.len() as f64).sqrt();
    let reference = cfg.base_delay_us + p_low;
    let delay_at = |t: f64| {
        let i = delays.partition_point(|&(s, _)| s < t);
        delays.get(i).or(delays.last()).map_or(0.0, |&(_, d)| d)
    };

    // Attribute each heard peak to the nearest click. The delay changes by
    // at most a few ms per second, so the one at the time heard is close
    // enough to pick the click.
    let heard = heard
        .into_iter()
        .map(|host_us| {
            let rough_s = (host_us - HOST0_US) / 1e6 / cfg.camera_skew;
            let expected = reference + delay_at(rough_s);
            let cam_s = (host_us - expected - HOST0_US) / 1e6 / cfg.camera_skew;
            let k = (cam_s * cfg.audio_rate / cfg.click_every as f64)
                .round()
                .max(1.0);
            let n = k as u64 * cfg.click_every;
            let camera_s = n as f64 / cfg.audio_rate;
            Heard {
                camera_s,
                latency_us: host_us - host_of_cam(camera_s),
            }
        })
        .collect();
    let expected = reference + delays.last().map_or(0.0, |&(_, d)| d);
    SimResult {
        heard,
        clicks_sent,
        tile,
        output: out,
        expected_latency_us: expected,
        reference_us: reference,
        delays,
        jitter_sd_us: sd,
        jitter_range_us: (sorted[0], sorted[sorted.len() - 1]),
    }
}

/// Finds peaks of channel 0 across render calls.
#[derive(Default)]
struct Detector {
    prev: [f32; 2],
    prev_time: f64,
    cooldown: usize,
}

impl Detector {
    fn scan(
        &mut self,
        buf: &[f32],
        ch: usize,
        start_us: f64,
        us_per_frame: f64,
        mut found: impl FnMut(f64),
    ) {
        for (k, f) in buf.chunks_exact(ch).enumerate() {
            let v = f[0];
            // prev[1] is a peak if it exceeds both neighbors.
            if self.cooldown == 0
                && self.prev[1] > 0.3
                && self.prev[1] >= self.prev[0]
                && self.prev[1] > v
            {
                found(self.prev_time);
                self.cooldown = 48;
            }
            self.cooldown = self.cooldown.saturating_sub(1);
            self.prev = [self.prev[1], v];
            self.prev_time = start_us + k as f64 * us_per_frame;
        }
    }
}
