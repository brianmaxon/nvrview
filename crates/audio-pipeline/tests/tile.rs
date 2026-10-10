//! Taps, other codecs, and malformed records through a whole tile.

mod common;

use audio_pipeline::adts::split_frames;
use audio_pipeline::{AudioRecord, OutputConfig, OutputFormat, TileAudio, TileConfig, output, tap};
use common::*;
use std::cell::RefCell;
use std::path::PathBuf;

#[test]
fn tap_blocks_are_timestamped_and_a_slow_consumer_only_loses_old_blocks() {
    let (tx, rx) = tap(16);
    let cfg = SimConfig {
        duration_s: 300.0,
        seed: 21,
        ..SimConfig::default()
    };
    let checked = RefCell::new((0u32, 0.0f64, 0u64));
    let r = run_with(
        &cfg,
        |t| t.set_tap(Some(tx.clone())),
        |t, _| {
            // A consumer that wakes up every 4 s and reads what is queued.
            let k = (t / (1024.0 / cfg.audio_rate)).round() as u64;
            if k % 62 != 0 {
                return;
            }
            let mut c = checked.borrow_mut();
            while let Some(b) = rx.pop() {
                assert_eq!(b.samples.len(), 1024);
                if t > 60.0 {
                    let truth = PTS0_US + b.first_sample as f64 / cfg.audio_rate * 1e6;
                    let err = b.camera_time_us.unwrap() - truth;
                    c.1 = c.1.max(err.abs());
                    c.0 += 1;
                }
                c.2 = c.2.max(b.first_sample);
            }
        },
    );
    let (n, worst, last) = *checked.borrow();
    let s = rx.stats();
    println!(
        "tap: {n} blocks checked, camera time worst {:.2} ms, {s:?}",
        worst / 1e3
    );
    assert!(n > 500);
    assert!(worst < 5_000.0, "camera time off by {worst} µs");
    // Each wake-up finds the newest 16 of about 62 blocks.
    assert!(s.dropped > s.pushed / 2, "{s:?}");
    assert!(last > 16_000 * 290);
    // Playout was not disturbed by the full queue.
    let (worst_play, _) = r.worst_after(60.0);
    assert!(worst_play < 40_000.0, "playout worst {worst_play} µs");
}

/// An IMA ADPCM payload (reported layout) of `samples` 4-bit codes for a
/// tone, encoded with a minimal IMA encoder written for this test.
fn adpcm_record(phase: &mut f64, samples: usize) -> Vec<u8> {
    const STEPS: [i32; 89] = [
        7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60,
        66, 73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371,
        408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878,
        2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845,
        8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086,
        29794, 32767,
    ];
    const ADJ: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];
    let (mut pred, mut idx) = (0i32, 20i32);
    let mut out = vec![0u8, 1, 0, 0];
    out.extend((pred as i16).to_le_bytes());
    out.extend([idx as u8, 0]);
    let mut codes = Vec::new();
    for _ in 0..samples {
        let s = (8000.0 * phase.sin()) as i32;
        *phase += 2.0 * std::f64::consts::PI * 500.0 / 8000.0;
        let step = STEPS[idx as usize];
        let mut d = s - pred;
        let mut code = 0u8;
        if d < 0 {
            code = 8;
            d = -d;
        }
        let mut st = step;
        for bit in [4u8, 2, 1] {
            if d >= st {
                code |= bit;
                d -= st;
            }
            st >>= 1;
        }
        let mut diff = step >> 3;
        if code & 1 != 0 {
            diff += step >> 2;
        }
        if code & 2 != 0 {
            diff += step >> 1;
        }
        if code & 4 != 0 {
            diff += step;
        }
        pred = if code & 8 != 0 {
            pred - diff
        } else {
            pred + diff
        }
        .clamp(-32768, 32767);
        idx = (idx + ADJ[(code & 7) as usize]).clamp(0, 88);
        codes.push(code);
    }
    for pair in codes.chunks(2) {
        out.push(pair[0] | (pair.get(1).copied().unwrap_or(0) << 4));
    }
    out
}

#[test]
fn adpcm_source_plays_and_taps_at_16k() {
    let (out, mut renderer) = output(
        OutputFormat {
            sample_rate: 48_000,
            channels: 1,
        },
        OutputConfig::default(),
    );
    let (tx, rx) = tap(1000);
    let mut tile = TileAudio::new(TileConfig::default());
    tile.set_output(Some(out));
    tile.set_audible(true);
    tile.set_tap(Some(tx));
    let mut phase = 0.0;
    let mut buf = vec![0.0f32; 480];
    let mut host = 1e9f64;
    let mut energy = 0.0f64;
    // 400-sample records (50 ms at 8 kHz), video at 10 fps.
    for k in 0..400u64 {
        let rec = adpcm_record(&mut phase, 400);
        assert!(rec.len() < 480);
        let t_end = (k + 1) as f64 * 0.05;
        let pts = ((t_end * 10.0).floor() * 100_000.0) as u64;
        host = host.max(1e9 + t_end * 1e6 + 30_000.0);
        tile.push(AudioRecord {
            stream_index: 1,
            data: &rec,
            after_video_pts_us: Some(pts),
            arrived_us: host as u64,
            wall_clock_us: Some(1_700_000_000_000_000 + (t_end * 1e6) as i64),
        });
        for _ in 0..5 {
            renderer.render(&mut buf, ((host + 5_000.0) * 1e3) as u64);
            energy += buf.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>();
            host += 10_000.0;
        }
    }
    let s = tile.stats();
    println!("{s:?}");
    assert_eq!(s.decoded, 400);
    assert_eq!(s.format.unwrap().sample_rate, 8_000);
    assert!((s.rate_estimate.unwrap() - 8_000.0).abs() < 5.0);
    assert!(energy > 100.0, "something was played: {energy}");
    let blocks: Vec<_> = std::iter::from_fn(|| rx.pop()).collect();
    let total: usize = blocks.iter().map(|b| b.samples.len()).sum();
    // 8 kHz in, 16 kHz out, minus the resampler's delay.
    assert!(
        (total as i64 - 400 * 800).abs() < 100,
        "{total} tap samples"
    );
    assert!(blocks[0].discontinuity && !blocks[1].discontinuity);
    assert!(blocks.iter().all(|b| b.wall_clock_us.is_some()));
}

#[test]
fn garbage_records_never_panic() {
    let bytes = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/synthetic/aac-tone.aac"),
    )
    .unwrap();
    let frames: Vec<Vec<u8>> = split_frames(&bytes).iter().map(|f| f.to_vec()).collect();
    let (out, mut renderer) = output(
        OutputFormat {
            sample_rate: 44_100,
            channels: 2,
        },
        OutputConfig::default(),
    );
    let mut tile = TileAudio::new(TileConfig::default());
    tile.set_output(Some(out));
    tile.set_audible(true);
    let mut seed = 0xDEAD_BEEF_u64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut buf = vec![0.0f32; 2 * 441];
    let mut host = 1e9;
    for k in 0..5_000u64 {
        let mut f = frames[(k as usize) % frames.len()].clone();
        let (index, data): (u8, Vec<u8>) = match rnd() % 10 {
            0 => (5, (0..rnd() % 3000).map(|_| rnd() as u8).collect()),
            1 => {
                let p = (rnd() as usize) % f.len();
                f[p] ^= rnd() as u8;
                (5, f)
            }
            2 => (
                (rnd() % 100) as u8,
                (0..rnd() % 600).map(|_| rnd() as u8).collect(),
            ),
            _ => (5, f),
        };
        let pts = if rnd() % 50 == 0 { rnd() } else { k * 64_000 };
        tile.push(AudioRecord {
            stream_index: index,
            data: &data,
            after_video_pts_us: (rnd() % 20 != 0).then_some(pts),
            arrived_us: host as u64,
            wall_clock_us: None,
        });
        if rnd() % 200 == 0 {
            tile.discontinuity();
        }
        for _ in 0..6 {
            renderer.render(&mut buf, (host * 1e3) as u64);
            assert!(buf.iter().all(|s| s.is_finite() && s.abs() <= 1.0));
            host += 10_000.0;
        }
    }
    let s = tile.stats();
    println!("{s:?}");
    assert_eq!(s.records, 5_000);
    assert_eq!(s.decoded + s.concealed + s.rejected, 5_000);
    assert!(s.concealed > 0 && s.rejected > 0);
}
