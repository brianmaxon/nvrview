//! The 16 kHz mono form of a tile's audio (`TileAudio::mono_block`): the
//! same samples the tap carries, with or without a tap, audible or not.

use audio_pipeline::adts::split_frames;
use audio_pipeline::{AudioRecord, TileAudio, TileConfig, tap};
use std::path::PathBuf;

fn frames(name: &str) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/synthetic")
            .join(name),
    )
    .unwrap();
    split_frames(&bytes).iter().map(|f| f.to_vec()).collect()
}

fn record(data: &[u8], k: u64) -> AudioRecord<'_> {
    AudioRecord {
        stream_index: 5,
        data,
        after_video_pts_us: Some(k * 64_000),
        arrived_us: 1_000_000 + k * 64_000,
        wall_clock_us: Some(1_700_000_000_000_000 + k as i64 * 64_000),
    }
}

/// Runs a fixture through a tile with mono output only and one with a tap
/// only; returns (mono blocks, tap blocks) as (first sample, samples,
/// discontinuity).
type Blocks = Vec<(u64, Vec<f32>, bool)>;

fn run(name: &str) -> (Blocks, Blocks) {
    let mut a = TileAudio::new(TileConfig::default());
    a.set_mono_output(true);
    let mut b = TileAudio::new(TileConfig::default());
    let (tx, rx) = tap(10_000);
    b.set_tap(Some(tx));
    let mut mono = Vec::new();
    for (k, f) in frames(name).iter().enumerate() {
        a.push(record(f, k as u64));
        b.push(record(f, k as u64));
        if let Some(m) = a.mono_block() {
            assert_eq!(m.arrived_us, 1_000_000 + k as u64 * 64_000);
            assert!(m.wall_clock_us.is_some());
            mono.push((m.first_sample, m.samples.to_vec(), m.discontinuity));
        }
    }
    let tapped = std::iter::from_fn(|| rx.pop())
        .map(|t| (t.first_sample, t.samples, t.discontinuity))
        .collect();
    (mono, tapped)
}

#[test]
fn mono_output_matches_the_tap_at_16k() {
    let (mono, tapped) = run("aac-tone.aac");
    assert!(mono.len() > 20);
    assert_eq!(mono, tapped);
    assert!(mono[0].2 && mono[1..].iter().all(|m| !m.2));
    assert!(mono.iter().all(|m| m.1.len() == 1024));
}

#[test]
fn mono_output_matches_the_tap_when_resampled() {
    let (mono, tapped) = run("aac-stereo-48k.aac");
    assert!(mono.len() > 10);
    assert_eq!(mono, tapped);
    // 48 kHz stereo in: about a third as many samples, contiguous.
    for w in mono.windows(2) {
        assert_eq!(w[0].0 + w[0].1.len() as u64, w[1].0);
    }
}

#[test]
fn mono_output_is_off_by_default() {
    let mut t = TileAudio::new(TileConfig::default());
    let f = frames("aac-tone.aac");
    t.push(record(&f[0], 0));
    assert!(t.mono_block().is_none());
    t.set_mono_output(true);
    t.push(record(&f[1], 1));
    let m = t.mono_block().unwrap();
    assert!(m.discontinuity, "the first block after switching on");
    // A record that is dropped (nothing decodable before any format is
    // known) leaves no block behind.
    let mut fresh = TileAudio::new(TileConfig::default());
    fresh.set_mono_output(true);
    fresh.push(record(&f[0], 0));
    assert!(fresh.mono_block().is_some());
    let mut other = TileAudio::new(TileConfig::default());
    other.set_mono_output(true);
    other.push(record(&[0u8; 3], 0));
    assert!(other.mono_block().is_none());
    t.set_mono_output(false);
    t.push(record(&f[2], 2));
    assert!(t.mono_block().is_none());
}
