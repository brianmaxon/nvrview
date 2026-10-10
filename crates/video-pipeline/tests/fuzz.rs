//! Randomized inputs: nothing panics, every protocol invariant holds, and all
//! state stays bounded. Deterministic seeds, so failures reproduce.

mod common;

use common::*;
use video_pipeline::{
    AccessUnit, Codec, DecodeFailure, ParameterSets, PipelineConfig, VideoFormat, analyze, h264,
    h265, nal_units,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Random bytes, or random NAL units behind start codes with headers that
/// hit the interesting types, or a fixture access unit with flipped bits.
fn random_access_unit(rng: &mut Rng, fixtures: &[(Codec, Vec<Vec<u8>>)]) -> AccessUnit {
    let codec = if rng.below(2) == 0 {
        Codec::H264
    } else {
        Codec::H265
    };
    let data = match rng.below(4) {
        0 => {
            let n = rng.below(300) as usize;
            rng.bytes(n)
        }
        1 => {
            let mut v = Vec::new();
            for _ in 0..rng.below(6) {
                v.extend_from_slice(&[0, 0, 1]);
                match codec {
                    Codec::H264 => {
                        let t = [1u8, 5, 6, 7, 8, 9][rng.below(6) as usize];
                        v.push(0x60 | t);
                    }
                    Codec::H265 => {
                        let t = [1u8, 8, 19, 21, 32, 33, 34, 39][rng.below(8) as usize];
                        v.push(t << 1);
                        v.push(1);
                    }
                }
                let n = rng.below(64) as usize;
                v.extend(rng.bytes(n));
            }
            v
        }
        _ => {
            let (c, aus) = &fixtures[rng.below(fixtures.len() as u64) as usize];
            let mut v = aus[rng.below(aus.len() as u64) as usize].clone();
            for _ in 0..rng.below(4) {
                let i = rng.below(v.len() as u64) as usize;
                v[i] ^= 1 << rng.below(8);
            }
            return AccessUnit {
                codec: *c,
                keyframe: rng.below(2) == 0,
                pts: rng.below(1000),
                data: v,
            };
        }
    };
    AccessUnit {
        codec,
        keyframe: rng.below(2) == 0,
        pts: rng.below(1000),
        data,
    }
}

fn fixtures() -> Vec<(Codec, Vec<Vec<u8>>)> {
    [
        "h264-basic.h264",
        "h264-resize.h264",
        "h265-basic.h265",
        "h265-resize.h265",
    ]
    .iter()
    .map(|n| load(n))
    .collect()
}

#[test]
fn random_inputs_never_break_the_protocol() {
    let fx = fixtures();
    let mut total = video_pipeline::TileStats::default();
    for seed in 1..=16u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let config = PipelineConfig {
            max_queued_units: 1 + rng.below(10) as u32,
            max_in_flight: 1 + rng.below(5) as u32,
            failures_before_reset: rng.below(4) as u32,
            max_resets_per_window: rng.below(5) as u32,
            ..PipelineConfig::default()
        };
        let mut h = Harness::new(config);
        let mut answers = Rng(seed);
        h.decide = Box::new(move |_, _| match answers.below(10) {
            0 => Answer::Fail(DecodeFailure::Corrupt),
            1 if answers.below(20) == 0 => Answer::Fail(DecodeFailure::DecoderLost),
            2 if answers.below(50) == 0 => Answer::Fail(DecodeFailure::Unsupported),
            3 => Answer::Silent,
            _ => Answer::Ok,
        });
        for _ in 0..4000 {
            match rng.below(20) {
                0..=11 => {
                    let au = random_access_unit(&mut rng, &fx);
                    h.p.push(au);
                    h.drain();
                }
                12 | 13 => h.run(),
                14 => {
                    h.p.decoded(rng.below(1000));
                    h.drain();
                }
                15 => {
                    let kind = [
                        DecodeFailure::Corrupt,
                        DecodeFailure::DecoderLost,
                        DecodeFailure::Unsupported,
                    ][rng.below(3) as usize];
                    h.p.decode_failed(kind);
                    h.drain();
                }
                16 => {
                    h.p.discontinuity();
                    h.drain();
                }
                _ => {
                    let dt = rng.below(3_000_000);
                    h.advance(dt);
                }
            }
        }
        let s = h.p.stats();
        eprintln!("seed {seed}: {s:?}");
        total.decoded += s.decoded;
        total.configures += s.configures;
        total.format_changes += s.format_changes;
        total.resets += s.resets;
        total.dropped += s.dropped;
        total.failures += s.failures;
        // Decoder rebuilds stay within the per-window cap whatever happens.
        assert!(
            h.max_in_window(config.window_us, |e| matches!(e, Event::Configure { .. }))
                <= config.max_configures_per_window as usize
        );
    }
    // The random walk reached the interesting states.
    assert!(total.decoded > 0 && total.configures > 0 && total.format_changes > 0);
    assert!(total.resets > 0 && total.dropped > 0 && total.failures > 0);
}

#[test]
fn whole_fixture_streams_with_random_corruption_play_on() {
    let fx = fixtures();
    let mut rng = Rng(0xDEAD_BEEF);
    for (codec, aus) in &fx {
        let mut h = Harness::new(PipelineConfig::default());
        for round in 0..3u64 {
            for (i, data) in aus.iter().enumerate() {
                let mut v = data.clone();
                if rng.below(10) == 0 {
                    let i = rng.below(v.len() as u64) as usize;
                    v[i] ^= 0xFF;
                }
                h.feed(au(*codec, (round * 1000 + i as u64) * FRAME_US, v));
            }
        }
        assert!(h.p.stats().decoded > 0);
    }
}

#[test]
fn parsers_survive_garbage() {
    let mut rng = Rng(42);
    for _ in 0..20_000 {
        let n = rng.below(80) as usize;
        let mut v = rng.bytes(n);
        if let Some(b) = v.first_mut() {
            *b = 0x67;
        }
        let _ = h264::parse_sps(&v);
        if v.len() > 1 {
            v[0] = 33 << 1;
            v[1] = 1;
        }
        let _ = h265::parse_sps(&v);
        if v.len() > 1 {
            v[0] = 32 << 1;
        }
        let _ = h265::parse_vps(&v);
        for codec in [Codec::H264, Codec::H265] {
            let _ = analyze(codec, &v);
            let _ = nal_units(&v).count();
            let p = ParameterSets::from_access_unit(codec, &v);
            assert!(p.matches_access_unit(codec, &v));
            let _ = VideoFormat::from_parameter_sets(p);
        }
    }
    // Bit flips in real parameter sets.
    for (codec, aus) in fixtures() {
        let params = ParameterSets::from_access_unit(codec, &aus[0]);
        for set in &params {
            for i in 0..set.data.len() * 8 {
                let mut v = set.data.to_vec();
                v[i / 8] ^= 1 << (i % 8);
                match codec {
                    Codec::H264 => {
                        let _ = h264::parse_sps(&v);
                    }
                    Codec::H265 => {
                        let _ = h265::parse_sps(&v);
                        let _ = h265::parse_vps(&v);
                    }
                }
            }
        }
    }
}
