//! IMA ADPCM and G.711 µ-law, the non-AAC audio payloads.
//!
//! **Reported, unconfirmed on hardware** (docs/protocol.md, "Audio in the
//! official client"): a record whose magic's second character is not `5` is
//! decoded as IMA ADPCM when its payload is under 480 bytes and as 8-bit
//! µ-law otherwise, both 8 kHz mono. The ADPCM payload has a 4-byte prefix,
//! the predictor as i16 (little-endian) at offset 4, the step index as u8 at
//! offset 6, and 4-bit samples from offset 8. No device we have seen sends
//! these. Two details are assumptions until one does: samples are taken low
//! nibble first, as in the common IMA ADPCM formats, and the header predictor
//! is decoder state only (not emitted as a sample).
//!
//! Both decoders are written from the public algorithm descriptions (the IMA
//! Digital Audio Focus and Technical Working Group's ADPCM recommendation and
//! ITU-T G.711).

/// Sample rate of both formats.
pub const RATE: u32 = 8_000;
/// Payloads shorter than this are ADPCM; longer ones µ-law.
pub const ADPCM_MAX_LEN: usize = 480;
/// Header bytes in front of the ADPCM nibbles.
pub const ADPCM_HEADER_LEN: usize = 8;

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

const INDEX_ADJUST: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// IMA ADPCM decoder state.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ImaState {
    pub predictor: i32,
    pub index: i32,
}

impl ImaState {
    /// Decodes one 4-bit code.
    pub fn decode(&mut self, code: u8) -> i16 {
        let step = STEPS[self.index as usize];
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
        if code & 8 != 0 {
            self.predictor -= diff;
        } else {
            self.predictor += diff;
        }
        self.predictor = self.predictor.clamp(-32768, 32767);
        self.index = (self.index + INDEX_ADJUST[usize::from(code & 7)]).clamp(0, 88);
        self.predictor as i16
    }

    /// Encodes one sample (used by tests to build synthetic payloads).
    #[cfg(test)]
    pub fn encode(&mut self, sample: i16) -> u8 {
        let step = STEPS[self.index as usize];
        let mut diff = i32::from(sample) - self.predictor;
        let mut code = 0u8;
        if diff < 0 {
            code = 8;
            diff = -diff;
        }
        let mut s = step;
        for bit in [4u8, 2, 1] {
            if diff >= s {
                code |= bit;
                diff -= s;
            }
            s >>= 1;
        }
        // Keep encoder and decoder state identical.
        self.decode(code);
        code
    }
}

/// Why an ADPCM payload cannot be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdpcmError {
    /// Shorter than its header.
    Truncated,
    /// The header's step index is outside 0..=88.
    BadStepIndex(u8),
}

/// Decodes an IMA ADPCM payload, appending f32 samples to `out`.
pub fn decode_ima(payload: &[u8], out: &mut Vec<f32>) -> Result<usize, AdpcmError> {
    if payload.len() < ADPCM_HEADER_LEN {
        return Err(AdpcmError::Truncated);
    }
    let predictor = i16::from_le_bytes([payload[4], payload[5]]);
    let index = payload[6];
    if index > 88 {
        return Err(AdpcmError::BadStepIndex(index));
    }
    let mut st = ImaState {
        predictor: i32::from(predictor),
        index: i32::from(index),
    };
    let body = &payload[ADPCM_HEADER_LEN..];
    out.reserve(body.len() * 2);
    for &b in body {
        for code in [b & 0x0F, b >> 4] {
            out.push(f32::from(st.decode(code)) / 32768.0);
        }
    }
    Ok(body.len() * 2)
}

/// Decodes one G.711 µ-law byte to a 16-bit linear sample.
pub fn mulaw_to_linear(byte: u8) -> i16 {
    let u = !byte;
    let exponent = (u >> 4) & 0x07;
    let mantissa = i32::from(u & 0x0F);
    let magnitude = (((mantissa << 3) + 0x84) << exponent) - 0x84;
    (if u & 0x80 != 0 { -magnitude } else { magnitude }) as i16
}

/// Decodes a µ-law payload, appending f32 samples to `out`.
pub fn decode_mulaw(payload: &[u8], out: &mut Vec<f32>) -> usize {
    out.extend(
        payload
            .iter()
            .map(|&b| f32::from(mulaw_to_linear(b)) / 32768.0),
    );
    payload.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mulaw_known_values() {
        assert_eq!(mulaw_to_linear(0xFF), 0);
        assert_eq!(mulaw_to_linear(0x7F), 0);
        assert_eq!(mulaw_to_linear(0x80), 32124);
        assert_eq!(mulaw_to_linear(0x00), -32124);
        assert_eq!(mulaw_to_linear(0xFE), 8);
        assert_eq!(mulaw_to_linear(0x7E), -8);
        // Monotonic over the positive half (0xFF down to 0x80).
        let mut last = -1;
        for b in (0x80..=0xFFu8).rev() {
            let v = i32::from(mulaw_to_linear(b));
            assert!(v > last || (b == 0xFF && v == 0));
            last = v;
        }
    }

    #[test]
    fn ima_round_trip_of_a_tone() {
        let src: Vec<i16> = (0..940)
            .map(|n| {
                (8000.0 * (2.0 * std::f64::consts::PI * 440.0 * n as f64 / 8000.0).sin()) as i16
            })
            .collect();
        let mut enc = ImaState {
            predictor: 0,
            index: 0,
        };
        let mut payload = vec![0u8, 1, 0, 0];
        payload.extend(0i16.to_le_bytes());
        payload.extend([0u8, 0]);
        for pair in src.chunks(2) {
            let lo = enc.encode(pair[0]);
            let hi = enc.encode(pair[1]);
            payload.push(lo | (hi << 4));
        }
        assert!(payload.len() < ADPCM_MAX_LEN);
        let mut out = Vec::new();
        assert_eq!(decode_ima(&payload, &mut out), Ok(940));
        // Skip the attack while the step size adapts.
        let (mut sig, mut err) = (0.0f64, 0.0f64);
        for (a, b) in src.iter().zip(&out).skip(100) {
            let a = f64::from(*a) / 32768.0;
            sig += a * a;
            err += (a - f64::from(*b)).powi(2);
        }
        let snr = 10.0 * (sig / err).log10();
        assert!(snr > 20.0, "SNR {snr:.1} dB");
    }

    #[test]
    fn ima_rejects_bad_headers() {
        let mut out = Vec::new();
        assert_eq!(decode_ima(&[0; 7], &mut out), Err(AdpcmError::Truncated));
        assert_eq!(
            decode_ima(&[0, 0, 0, 0, 0, 0, 89, 0, 0x77], &mut out),
            Err(AdpcmError::BadStepIndex(89))
        );
        assert!(out.is_empty());
    }
}
