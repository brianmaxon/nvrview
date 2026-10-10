//! The AAC decoder against FFmpeg's decode of the same synthetic frames, and
//! malformed input that must never panic.

use audio_pipeline::adts::split_frames;
use audio_pipeline::decode::{AudioCodec, DecodeError, Decoder};
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/synthetic")
        .join(name);
    std::fs::read(p).unwrap()
}

fn decode_all(file: &str) -> (Vec<f32>, usize, u32) {
    let bytes = fixture(file);
    let mut d = Decoder::new();
    let mut out = Vec::new();
    let mut fmt = None;
    for f in split_frames(&bytes) {
        let r = d.decode(5, f, &mut out).unwrap();
        assert_eq!(r.codec, AudioCodec::Aac);
        assert_eq!(r.frames, 1024);
        fmt = Some(r.format);
    }
    let fmt = fmt.unwrap();
    (out, fmt.channels, fmt.sample_rate)
}

fn reference(file: &str) -> Vec<f32> {
    fixture(file)
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0)
        .collect()
}

/// SNR in dB of `ours` against `reference`, both interleaved, skipping the
/// first decoded frame (FFmpeg's encoder priming, near silence).
fn snr_db(ours: &[f32], reference: &[f32], skip: usize) -> f64 {
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for (a, b) in reference.iter().zip(ours).skip(skip) {
        sig += f64::from(*a).powi(2);
        err += f64::from(a - b).powi(2);
    }
    10.0 * (sig / err.max(1e-30)).log10()
}

/// The lag (in samples, interleaved) of `ours` against `reference` with the
/// highest correlation, searched within one frame either way.
fn best_lag(ours: &[f32], reference: &[f32], channels: usize) -> isize {
    let mut best = (f64::MIN, 0isize);
    for lag in -1024isize..=1024 {
        let lag = lag * channels as isize;
        let mut c = 0.0f64;
        for (i, r) in reference.iter().enumerate().skip(4096) {
            let j = i as isize + lag;
            if j >= 0 && (j as usize) < ours.len() {
                c += f64::from(*r) * f64::from(ours[j as usize]);
            }
        }
        if c > best.0 {
            best = (c, lag);
        }
    }
    best.1
}

#[test]
fn mono_16k_matches_ffmpeg() {
    let (ours, ch, rate) = decode_all("aac-tone.aac");
    let reference = reference("aac-tone.s16");
    assert_eq!((ch, rate), (1, 16_000));
    assert_eq!(ours.len(), reference.len(), "same number of samples");
    assert_eq!(best_lag(&ours, &reference, 1), 0, "sample-aligned");
    let snr = snr_db(&ours, &reference, 1024);
    println!("16 kHz mono: SNR against FFmpeg {snr:.1} dB");
    // FFmpeg's output is rounded to 16 bits; anything this close is the
    // same decode up to floating-point detail.
    assert!(snr > 60.0, "SNR {snr:.1} dB");
}

#[test]
fn stereo_48k_matches_ffmpeg() {
    let (ours, ch, rate) = decode_all("aac-stereo-48k.aac");
    let reference = reference("aac-stereo-48k.s16");
    assert_eq!((ch, rate), (2, 48_000));
    assert_eq!(ours.len(), reference.len());
    assert_eq!(best_lag(&ours, &reference, 2), 0);
    let snr = snr_db(&ours, &reference, 2048);
    println!("48 kHz stereo: SNR against FFmpeg {snr:.1} dB");
    assert!(snr > 60.0, "SNR {snr:.1} dB");
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[test]
fn garbage_never_panics() {
    let bytes = fixture("aac-tone.aac");
    let frames: Vec<Vec<u8>> = split_frames(&bytes).iter().map(|f| f.to_vec()).collect();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut d = Decoder::new();
    let (mut ok, mut concealable, mut rejected) = (0u32, 0u32, 0u32);
    for i in 0..20_000 {
        let index = [5u8, 15, 1, 0, 25][rng.below(5) as usize];
        let data: Vec<u8> = match rng.below(4) {
            // Random bytes.
            0 => (0..rng.below(700)).map(|_| rng.next() as u8).collect(),
            // A real frame with flipped bits in the payload.
            1 | 2 => {
                let mut f = frames[i % frames.len()].clone();
                for _ in 0..1 + rng.below(8) {
                    let p = 7 + rng.below(f.len() as u64 - 7) as usize;
                    f[p] ^= 1 << rng.below(8);
                }
                f
            }
            // A real header on a random body of the declared length.
            _ => {
                let mut f = frames[i % frames.len()].clone();
                for b in f.iter_mut().skip(7) {
                    *b = rng.next() as u8;
                }
                f
            }
        };
        let mut out = vec![0.5f32; 3];
        match d.decode(index, &data, &mut out) {
            Ok(r) => {
                ok += 1;
                assert_eq!(out.len(), 3 + r.frames * r.format.channels);
                assert!(out.iter().all(|s| s.is_finite()));
            }
            Err(DecodeError::Aac { frames, .. }) => {
                concealable += 1;
                assert_eq!(frames, 1024);
                assert_eq!(out.len(), 3);
            }
            Err(_) => {
                rejected += 1;
                assert_eq!(out.len(), 3);
            }
        }
    }
    println!("garbage: {ok} decoded, {concealable} AAC errors, {rejected} rejected");
    // After all that, a clean stream still decodes.
    d.reset();
    let mut out = Vec::new();
    for f in &frames {
        d.decode(5, f, &mut out).unwrap();
    }
}
