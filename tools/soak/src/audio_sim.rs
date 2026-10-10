//! The audio leg: a replayed stream's audio records through one
//! `audio_pipeline::TileAudio` (decode, timestamp, drift-corrected playout to
//! a 48 kHz stereo output) in *simulated* time.
//!
//! Arrival jitter is what the audio pipeline is built for, and accelerating
//! it on the wall clock would change it, so this leg uses the recording's own
//! arrival times as the host clock and pulls the renderer every 10 ms of
//! simulated time, like an audio device. Any simulated duration costs only
//! CPU time.

use audio_pipeline::{AudioRecord, OutputConfig, OutputFormat, TileAudio, TileConfig, output};
use bc_protocol::media::{Demuxer, Record};
use mock_nvr::replay::{Replay, Step};

/// Stream time to ignore after every (re)start of the stream, the first one
/// and each loop of the recording: the controller is still converging.
pub const WARMUP_US: u64 = 60_000_000;
const RENDER_STEP_US: u64 = 10_000;
const HOST0_US: u64 = 1_000_000_000;

/// What the audio leg saw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioReport {
    /// Simulated stream time covered, microseconds.
    pub simulated_us: u64,
    /// Audio records fed.
    pub records: u64,
    /// Records decoded, concealed with silence, and rejected.
    pub decoded: u64,
    /// Records concealed.
    pub concealed: u64,
    /// Records rejected.
    pub rejected: u64,
    /// Offset samples taken after the warm-up.
    pub offset_samples: u64,
    /// Filtered A/V error (positive: audio late against its target) over the
    /// run after the warm-up, microseconds: smallest, largest, mean.
    pub offset_min_us: f64,
    /// Largest.
    pub offset_max_us: f64,
    /// Mean.
    pub offset_mean_us: f64,
    /// Stream time of the smallest and the largest offset, seconds.
    pub offset_min_at_s: f64,
    /// See `offset_min_at_s`.
    pub offset_max_at_s: f64,
    /// Largest absolute offset in the warm-up, for reference.
    pub warmup_abs_max_us: f64,
    /// Offset sampled at the end of each simulated hour, microseconds.
    pub hourly_us: Vec<f64>,
    /// Times the recording looped, each treated as a stream restart.
    pub restarts: u64,
    /// Hard resyncs, starts, underruns, ring overflow frames (final counters).
    pub resyncs: u64,
    /// Playout starts.
    pub starts: u64,
    /// Underrun events.
    pub underruns: u64,
    /// Frames that did not fit the ring.
    pub overflow_frames: u64,
    /// Final estimate of the camera's audio sample rate.
    pub rate_estimate: Option<f64>,
    /// Timestamper epochs started.
    pub epoch: u32,
}

/// Simulates `simulated_us` of stream time (looping the replay as needed).
pub fn simulate(replay: &Replay, simulated_us: u64) -> AudioReport {
    let (out, mut renderer) = output(
        OutputFormat {
            sample_rate: 48_000,
            channels: 2,
        },
        OutputConfig::default(),
    );
    let mut tile = TileAudio::new(TileConfig::default());
    tile.set_output(Some(out));
    tile.set_audible(true);

    let mut demux = Demuxer::new();
    let mut buf = vec![0.0f32; 2 * 480];
    let mut next_render = 0u64;
    let mut last_pts: Option<u64> = None;
    let mut rep = AudioReport::default();
    let (mut sum, mut min, mut max) = (0.0f64, f64::MAX, f64::MIN);
    let mut hour = 1u64;
    let mut pass = 0u64;
    let mut restart_at = 0u64;

    for (at, step) in replay.playback(true) {
        if at >= simulated_us {
            break;
        }
        // The wire has no marker for a loop; a viewer would see the stream
        // restart (the recording's audio and video do not line up across the
        // seam), so each loop is a discontinuity here.
        if at / replay.pass_us() > pass {
            pass = at / replay.pass_us();
            restart_at = pass * replay.pass_us();
            tile.discontinuity();
            last_pts = None;
            rep.restarts += 1;
        }
        while next_render + RENDER_STEP_US <= at {
            next_render += RENDER_STEP_US;
            let host_us = HOST0_US + next_render;
            renderer.render(&mut buf, (host_us + 5_000) * 1000);
        }
        let bytes = match step {
            Step::Discontinuity => {
                tile.discontinuity();
                last_pts = None;
                restart_at = at;
                continue;
            }
            Step::Record(b) => b,
        };
        demux.push(&bytes);
        while let Ok(Some(r)) = demux.next_record() {
            match r {
                Record::Video { pts_us, .. } => last_pts = Some(pts_us),
                Record::Audio { stream_index, data } => {
                    tile.push(AudioRecord {
                        stream_index,
                        data: &data,
                        after_video_pts_us: last_pts,
                        arrived_us: HOST0_US + at,
                        wall_clock_us: None,
                    });
                    rep.records += 1;
                    let off = tile.stats().playout.and_then(|p| p.av_offset_us);
                    if let Some(o) = off {
                        if at - restart_at < WARMUP_US {
                            rep.warmup_abs_max_us = rep.warmup_abs_max_us.max(o.abs());
                        } else {
                            rep.offset_samples += 1;
                            sum += o;
                            if o < min {
                                min = o;
                                rep.offset_min_at_s = at as f64 / 1e6;
                            }
                            if o > max {
                                max = o;
                                rep.offset_max_at_s = at as f64 / 1e6;
                            }
                        }
                        if at >= hour * 3_600_000_000 {
                            rep.hourly_us.push(o);
                            hour += 1;
                        }
                    }
                }
                Record::Info { .. } => {}
            }
        }
    }
    let s = tile.stats();
    rep.simulated_us = simulated_us;
    rep.decoded = s.decoded;
    rep.concealed = s.concealed;
    rep.rejected = s.rejected;
    if rep.offset_samples > 0 {
        rep.offset_min_us = min;
        rep.offset_max_us = max;
        rep.offset_mean_us = sum / rep.offset_samples as f64;
    }
    if let Some(p) = s.playout {
        rep.resyncs = p.resyncs;
        rep.starts = p.starts;
        rep.underruns = p.underruns;
        rep.overflow_frames = p.overflow_frames;
    }
    rep.rate_estimate = s.rate_estimate;
    rep.epoch = s.epoch;
    rep
}
