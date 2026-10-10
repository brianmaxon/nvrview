//! What sound analysis adds to a tile's audio thread: decoding 10 minutes
//! of the synthetic smoke-alarm AAC (fixtures/synthetic/aac-smoke-t3.aac,
//! looped) through `TileAudio`, without and with the 16 kHz mono form and
//! the analyzers (alarm detector and loudness meter).
//!
//! Run optimized: `cargo run --release -p viewer-core --example sound_cost`.

use audio_pipeline::adts::split_frames;
use audio_pipeline::{AudioRecord, TileAudio, TileConfig};
use std::hint::black_box;
use std::time::{Duration, Instant};
use viewer_core::sound::{SoundOutput, SoundSettings, TileSound};

const SECONDS: usize = 600;

fn run(frames: &[Vec<u8>], analyze: bool) -> (Duration, usize, usize) {
    let mut tile = TileAudio::new(TileConfig::default());
    tile.set_mono_output(analyze);
    let mut sound = analyze
        .then(|| TileSound::new(&SoundSettings::default()))
        .flatten();
    let records = SECONDS * 16_000 / 1024;
    let mut alarms = 0;
    let start = Instant::now();
    for k in 0..records {
        let t_us = k as u64 * 64_000;
        tile.push(AudioRecord {
            stream_index: 5,
            data: black_box(&frames[k % frames.len()]),
            after_video_pts_us: Some(t_us),
            arrived_us: 1_000_000 + t_us,
            wall_clock_us: Some(1_700_000_000_000_000 + t_us as i64),
        });
        if let (Some(s), Some(b)) = (sound.as_mut(), tile.mono_block()) {
            s.push(&b, 0, |o| {
                if let SoundOutput::Alarm { .. } = o {
                    alarms += 1;
                }
            });
        }
    }
    (start.elapsed(), records, alarms)
}

fn main() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic/aac-smoke-t3.aac");
    let bytes = std::fs::read(path).unwrap();
    let frames: Vec<Vec<u8>> = split_frames(&bytes).iter().map(|f| f.to_vec()).collect();
    let best = |analyze| {
        (0..3)
            .map(|_| run(&frames, analyze))
            .min_by_key(|r| r.0)
            .unwrap()
    };
    let (plain, records, _) = best(false);
    let (with, _, alarms) = best(true);
    let per = |d: Duration| d.as_secs_f64() * 1e6 / records as f64;
    let added = with.saturating_sub(plain);
    println!("{SECONDS} s of 16 kHz mono AAC, {records} records of 64 ms");
    println!(
        "  decode and timestamp:          {:7.2} µs per record ({:.3}% of one core)",
        per(plain),
        plain.as_secs_f64() / SECONDS as f64 * 100.0
    );
    println!(
        "  with mono form and analyzers:  {:7.2} µs per record ({:.3}% of one core)",
        per(with),
        with.as_secs_f64() / SECONDS as f64 * 100.0
    );
    println!(
        "  added by sound analysis:       {:7.2} µs per record ({:.4}% of one core), {alarms} alarm events",
        per(added),
        added.as_secs_f64() / SECONDS as f64 * 100.0
    );
}
