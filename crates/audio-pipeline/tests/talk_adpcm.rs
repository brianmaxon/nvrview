//! The talk-back encoder (bc-protocol) against this crate's IMA ADPCM
//! decoder. Talk-back records are coded high nibble first
//! (docs/protocol.md, "Talk-back (motion)") and the received-audio decoder
//! reads low nibble first, so the samples bytes are swapped before
//! decoding; the step tables, state header, and arithmetic are shared.

use audio_pipeline::adpcm::decode_ima;
use bc_protocol::talk::{ADPCM_HEADER_LEN, AdpcmEncoder, SAMPLES_PER_RECORD, decode_record};

#[test]
fn talk_records_round_trip_through_the_receive_decoder() {
    let src: Vec<i16> = (0..SAMPLES_PER_RECORD * 6)
        .map(|n| {
            let t = n as f64 / 16_000.0;
            (9000.0 * (2.0 * std::f64::consts::PI * 523.0 * t).sin()
                + 3000.0 * (2.0 * std::f64::consts::PI * 1700.0 * t).sin()) as i16
        })
        .collect();
    let mut enc = AdpcmEncoder::new();
    let mut theirs = Vec::new();
    let mut ours = Vec::new();
    for block in src.as_chunks::<SAMPLES_PER_RECORD>().0 {
        let rec = enc.encode_record(block);
        ours.extend(decode_record(&rec).unwrap().samples);
        let mut swapped = rec.clone();
        for b in &mut swapped[ADPCM_HEADER_LEN..] {
            *b = b.rotate_left(4);
        }
        assert_eq!(decode_ima(&swapped, &mut theirs), Ok(SAMPLES_PER_RECORD));
    }
    assert_eq!(theirs.len(), ours.len());
    for (a, b) in ours.iter().zip(&theirs) {
        assert_eq!(f32::from(*a) / 32768.0, *b);
    }
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for (a, b) in src.iter().zip(&ours).skip(200) {
        sig += f64::from(*a).powi(2);
        err += (f64::from(*a) - f64::from(*b)).powi(2);
    }
    let snr = 10.0 * (sig / err).log10();
    assert!(snr > 20.0, "SNR {snr:.1} dB");
}
