//! End to end on the mock NVR's stream: its interleaving schedule and real
//! AAC records (the clicks fixture, looping) through ADTS parsing, decoding,
//! timestamping, playout, and the renderer, on a simulated clock. The audio
//! clock runs at 16,013 samples per second of video time.

use audio_pipeline::adts::split_frames;
use audio_pipeline::decode::Decoder;
use audio_pipeline::{AudioRecord, OutputConfig, OutputFormat, TileAudio, TileConfig, output};
use mock_nvr::audio::{AudioConfig, AudioFixture, Item, Schedule};
use std::path::PathBuf;

const CLOCK_HZ: f64 = 16_013.0;
const FPS: u64 = 12;
const BASE_DELAY_US: f64 = 30_000.0;
const JITTER_US: f64 = 40_000.0;
const SECONDS: f64 = 180.0;
const THRESHOLD: f32 = 0.4;

fn fixture() -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/aac-clicks.aac"),
    )
    .unwrap()
}

/// Decoded-sample offsets (within one loop of the fixture) where each click
/// first crosses the threshold, from decoding the fixture directly.
fn click_onsets(frames: &[&[u8]]) -> Vec<usize> {
    let mut d = Decoder::new();
    let mut pcm = Vec::new();
    for f in frames {
        d.decode(5, f, &mut pcm).unwrap();
    }
    let mut onsets = Vec::new();
    let mut quiet = 0usize;
    for (i, s) in pcm.iter().enumerate() {
        if s.abs() > THRESHOLD && quiet > 1000 {
            onsets.push(i);
        }
        quiet = if s.abs() > THRESHOLD { 0 } else { quiet + 1 };
    }
    onsets
}

#[test]
fn clicks_from_the_mock_stream_play_at_their_camera_time() {
    let bytes = fixture();
    let frames = split_frames(&bytes);
    let onsets = click_onsets(&frames);
    assert_eq!(onsets.len(), 8, "one click every 0.5 s in 4 s");
    let loop_samples = frames.len() * 1024;

    let mut acfg = AudioConfig::new(AudioFixture::from_adts(&bytes).unwrap());
    acfg.clock_hz = CLOCK_HZ;
    let schedule = Schedule::new(1_000_000 / FPS, Some(&acfg));

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

    let host0 = 1e9f64;
    let mut seed = 77u64;
    let mut last_arrival = 0.0f64;
    let mut last_pts = None;
    let mut delays = Vec::new();
    let mut next_cb_us = host0;
    let mut buf = vec![0.0f32; 2 * 480];
    let mut quiet = 0usize;
    let mut heard = Vec::new();
    for (item, send_us) in schedule {
        if send_us > SECONDS * 1e6 {
            break;
        }
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let jitter = (seed % 1_000_000) as f64 / 1e6 * JITTER_US;
        let arrival = (host0 + send_us + BASE_DELAY_US + jitter).max(last_arrival);
        last_arrival = arrival;
        while next_cb_us <= arrival {
            let start_us = next_cb_us + 8_000.0;
            renderer.render(&mut buf, (start_us * 1e3) as u64);
            for (k, f) in buf.chunks_exact(2).enumerate() {
                if f[0].abs() > THRESHOLD && quiet > 3000 {
                    heard.push(start_us + k as f64 * 1e6 / 48_000.0);
                }
                quiet = if f[0].abs() > THRESHOLD { 0 } else { quiet + 1 };
            }
            next_cb_us += 10_000.0;
        }
        match item {
            Item::Video { pts_us, .. } => last_pts = Some(pts_us),
            Item::Audio { k } => {
                delays.push(arrival - host0 - send_us);
                let f = frames[(k as usize) % frames.len()];
                tile.push(AudioRecord {
                    stream_index: 5,
                    data: f,
                    after_video_pts_us: last_pts,
                    arrived_us: arrival as u64,
                    wall_clock_us: None,
                });
            }
        }
    }
    let s = tile.stats();
    println!("{s:?}");
    assert_eq!(s.concealed + s.rejected, 0);
    assert!((s.rate_estimate.unwrap() - CLOCK_HZ).abs() < 1.0);

    // Every click that should be heard, by decoded sample number, and its
    // target: camera time (sample / clock) on the host clock plus the median
    // delivery delay plus the playout delay.
    delays.sort_by(f64::total_cmp);
    let median = delays[delays.len() / 2];
    let latency = median + TileConfig::default().playout_delay_us;
    let mut targets = Vec::new();
    let mut base = 0usize;
    while (base as f64) < SECONDS * CLOCK_HZ {
        for o in &onsets {
            targets.push(host0 + (base + o) as f64 / CLOCK_HZ * 1e6 + latency);
        }
        base += loop_samples;
    }
    let mut worst = 0.0f64;
    let mut n = 0;
    for h in &heard {
        let t = targets
            .iter()
            .min_by(|a, b| (*a - h).abs().total_cmp(&(*b - h).abs()))
            .unwrap();
        if *t > host0 + 60e6 {
            worst = worst.max((h - t).abs());
            n += 1;
        }
    }
    println!("{n} clicks after 60 s, worst {:.1} ms", worst / 1e3);
    assert!(n > 200);
    assert!(worst < 40_000.0, "worst {worst} µs");
}
