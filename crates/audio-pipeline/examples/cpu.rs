//! Measures the CPU time of one tile's audio: ADTS parsing and AAC decoding,
//! timestamping, resampling to 48 kHz stereo into the ring, and rendering.
//!
//! Run optimized: `cargo run --release -p audio-pipeline --example cpu`.

use audio_pipeline::adts::split_frames;
use audio_pipeline::decode::Decoder;
use audio_pipeline::{AudioRecord, OutputConfig, OutputFormat, TileAudio, TileConfig, output};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let bytes = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/aac-tone.aac"),
    )
    .expect("fixture");
    let frames = split_frames(&bytes);
    let records = 15_625usize; // 1000 s of 16 kHz audio

    // Decoding alone.
    let mut d = Decoder::new();
    let mut pcm = Vec::with_capacity(1024);
    let t = Instant::now();
    for k in 0..records {
        pcm.clear();
        d.decode(5, frames[k % frames.len()], &mut pcm)
            .expect("decodes");
    }
    let decode = t.elapsed().as_secs_f64();

    // The whole tile, with the renderer pulling 10 ms at a time.
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
    let mut buf = vec![0.0f32; 2 * 480];
    let mut host_us = 1e9;
    let mut produce = 0.0;
    let mut render = 0.0;
    let mut clock = 0.0;
    for k in 0..records {
        let pts = (k as u64 * 64_000 / 100_000) * 100_000;
        let t = Instant::now();
        tile.push(AudioRecord {
            stream_index: 5,
            data: frames[k % frames.len()],
            after_video_pts_us: Some(pts),
            arrived_us: host_us as u64,
            wall_clock_us: None,
        });
        produce += t.elapsed().as_secs_f64();
        clock += 64_000.0;
        while clock >= 10_000.0 {
            clock -= 10_000.0;
            let t = Instant::now();
            renderer.render(&mut buf, ((host_us + 5_000.0) * 1e3) as u64);
            render += t.elapsed().as_secs_f64();
            host_us += 10_000.0;
        }
    }
    let audio_s = records as f64 * 1024.0 / 16_000.0;
    let pct = |s: f64| s / audio_s * 100.0;
    println!("{audio_s:.0} s of 16 kHz mono AAC, one tile, 48 kHz stereo output:");
    println!(
        "  decode only:                    {:.3}% of one core",
        pct(decode)
    );
    println!(
        "  decode + timestamp + resample:  {:.3}% of one core",
        pct(produce)
    );
    println!(
        "  render (one tile):              {:.3}% of one core",
        pct(render)
    );
    println!("{:?}", tile.stats().playout);
}
