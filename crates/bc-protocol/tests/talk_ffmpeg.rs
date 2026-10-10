//! Talk-back audio against FFmpeg's IMA ADPCM (skipped when ffmpeg is not
//! installed).
//!
//! With the 8-byte record headers removed, talk-back records join into a
//! plain IMA ADPCM stream coded high nibble first from state 0, which is
//! what FFmpeg's `adpcm_ima_ssi` codec reads and writes in a KVAG file
//! (docs/protocol.md, "Talk-back (motion)"). So:
//!
//! - our records, joined, decode in FFmpeg to exactly what our decoder gives;
//! - FFmpeg's encoding of the same input, cut into records, decodes in our
//!   decoder to exactly what FFmpeg gives.

use bc_protocol::talk::{
    ADPCM_DATA_LEN, AdpcmEncoder, AdpcmState, RECORD_PREFIX, SAMPLES_PER_RECORD, decode_record,
};
use std::path::PathBuf;
use std::process::Command;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bc-protocol-talk-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// A KVAG file: `KVAG`, data size, sample rate, stereo flag, data.
fn kvag(data: &[u8]) -> Vec<u8> {
    let mut v = b"KVAG".to_vec();
    v.extend((data.len() as u32).to_le_bytes());
    v.extend(16_000u32.to_le_bytes());
    v.extend(0u16.to_le_bytes());
    v.extend_from_slice(data);
    v
}

fn ffmpeg(args: &[&str]) {
    let o = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "ffmpeg {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn read_s16(path: &PathBuf) -> Vec<i16> {
    std::fs::read(path)
        .unwrap()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect()
}

/// Speech-like test input: a gliding tone with a slow envelope plus a
/// little deterministic noise, 0.5 s of silence at the start.
fn input(records: usize) -> Vec<i16> {
    let mut seed = 0x1234_5678u32;
    (0..records * SAMPLES_PER_RECORD)
        .map(|i| {
            let t = i as f64 / 16_000.0;
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = f64::from(seed % 512) - 256.0;
            if t < 0.5 {
                return 0;
            }
            let env = (std::f64::consts::PI * t * 1.5).sin().abs();
            let f = 300.0 + 900.0 * t;
            (env * 12_000.0 * (2.0 * std::f64::consts::PI * f * t).sin() + noise) as i16
        })
        .collect()
}

#[test]
fn our_records_decode_identically_in_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not installed; skipping");
        return;
    }
    let src = input(40);
    let mut enc = AdpcmEncoder::new();
    let mut joined = Vec::new();
    let mut ours = Vec::new();
    for block in src.as_chunks::<SAMPLES_PER_RECORD>().0 {
        let rec = enc.encode_record(block);
        assert_eq!(&rec[..4], &RECORD_PREFIX);
        joined.extend_from_slice(&rec[8..]);
        ours.extend(decode_record(&rec).unwrap().samples);
    }
    let vag = temp("ours.vag");
    let raw = temp("ours.raw");
    std::fs::write(&vag, kvag(&joined)).unwrap();
    ffmpeg(&[
        "-f",
        "kvag",
        "-i",
        vag.to_str().unwrap(),
        "-f",
        "s16le",
        "-ac",
        "1",
        raw.to_str().unwrap(),
    ]);
    let theirs = read_s16(&raw);
    assert_eq!(theirs.len(), ours.len());
    assert!(ours == theirs, "FFmpeg decodes our records differently");
}

#[test]
fn ffmpeg_encoding_decodes_identically_in_ours() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not installed; skipping");
        return;
    }
    let src = input(40);
    let pcm = temp("in.raw");
    let vag = temp("theirs.vag");
    let raw = temp("theirs.raw");
    let bytes: Vec<u8> = src.iter().flat_map(|s| s.to_le_bytes()).collect();
    std::fs::write(&pcm, bytes).unwrap();
    ffmpeg(&[
        "-f",
        "s16le",
        "-ar",
        "16000",
        "-ac",
        "1",
        "-i",
        pcm.to_str().unwrap(),
        "-c:a",
        "adpcm_ima_ssi",
        "-f",
        "kvag",
        vag.to_str().unwrap(),
    ]);
    ffmpeg(&[
        "-f",
        "kvag",
        "-i",
        vag.to_str().unwrap(),
        "-f",
        "s16le",
        "-ac",
        "1",
        raw.to_str().unwrap(),
    ]);
    let file = std::fs::read(&vag).unwrap();
    assert_eq!(&file[..4], b"KVAG");
    let data = &file[14..];
    let theirs = read_s16(&raw);
    // Cut FFmpeg's stream into records, carrying the state as a sender
    // would, and decode each.
    let mut state = AdpcmState::default();
    let mut ours = Vec::new();
    for chunk in data.as_chunks::<ADPCM_DATA_LEN>().0 {
        let mut rec = RECORD_PREFIX.to_vec();
        rec.extend(state.predictor.to_le_bytes());
        rec.push(state.index);
        rec.push(0);
        rec.extend_from_slice(chunk);
        let d = decode_record(&rec).unwrap();
        state = d.end;
        ours.extend(d.samples);
    }
    assert!(ours.len() >= src.len() - SAMPLES_PER_RECORD);
    assert!(ours.len() <= theirs.len());
    assert!(
        ours[..] == theirs[..ours.len()],
        "our decoder disagrees with FFmpeg's"
    );
    // And both codings are faithful to the input once the step size has
    // adapted (4-bit ADPCM of this noisy, gliding input: about 19 dB).
    let mut enc = AdpcmEncoder::new();
    let mut mine = Vec::new();
    for block in src.as_chunks::<SAMPLES_PER_RECORD>().0 {
        mine.extend(decode_record(&enc.encode_record(block)).unwrap().samples);
    }
    for (name, out, min) in [("ffmpeg", &ours, 15.0), ("ours", &mine, 15.0)] {
        let (mut sig, mut err) = (0.0f64, 0.0f64);
        for (a, b) in src.iter().zip(out.iter()).skip(9000) {
            sig += f64::from(*a).powi(2);
            err += (f64::from(*a) - f64::from(*b)).powi(2);
        }
        let snr = 10.0 * (sig / err).log10();
        assert!(snr > min, "{name}: SNR {snr:.1} dB");
    }
}
