//! ADTS frame parsing for AAC.

use crate::error::{Error, Result};

const RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

/// Samples per AAC-LC frame.
pub const SAMPLES_PER_FRAME: u32 = 1024;

/// Stream parameters from an ADTS header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AacConfig {
    /// MPEG-4 audio object type (2 = AAC-LC).
    pub object_type: u8,
    pub sample_rate_index: u8,
    pub sample_rate: u32,
    pub channels: u8,
}

impl AacConfig {
    /// The 2-byte AudioSpecificConfig for this stream.
    pub(crate) fn audio_specific_config(&self) -> [u8; 2] {
        let v = (u16::from(self.object_type) << 11)
            | (u16::from(self.sample_rate_index) << 7)
            | (u16::from(self.channels) << 3);
        v.to_be_bytes()
    }
}

/// Parses one complete ADTS frame; returns the config and the raw AAC payload
/// (header and optional CRC removed).
pub fn parse_adts(frame: &[u8]) -> Result<(AacConfig, &[u8])> {
    if frame.len() < 7 {
        return Err(Error::InvalidAdts("shorter than a header"));
    }
    if frame[0] != 0xff || frame[1] & 0xf6 != 0xf0 {
        return Err(Error::InvalidAdts("bad sync or layer"));
    }
    let protection_absent = frame[1] & 1 == 1;
    let profile = frame[2] >> 6;
    let sf = (frame[2] >> 2) & 0xf;
    let channels = ((frame[2] & 1) << 2) | (frame[3] >> 6);
    let len = (usize::from(frame[3] & 3) << 11)
        | (usize::from(frame[4]) << 3)
        | usize::from(frame[5] >> 5);
    let blocks = frame[6] & 3;
    let header = if protection_absent { 7 } else { 9 };
    let sample_rate = *RATES
        .get(usize::from(sf))
        .ok_or(Error::InvalidAdts("reserved sample rate index"))?;
    if channels == 0 {
        return Err(Error::InvalidAdts("channel configuration in stream"));
    }
    if blocks != 0 {
        return Err(Error::InvalidAdts("more than one raw data block"));
    }
    if len != frame.len() || len < header {
        return Err(Error::InvalidAdts("frame length does not match payload"));
    }
    Ok((
        AacConfig {
            object_type: profile + 1,
            sample_rate_index: sf,
            sample_rate,
            channels,
        },
        &frame[header..],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an ADTS frame around a payload.
    pub(crate) fn frame(payload: &[u8], sf: u8, ch: u8) -> Vec<u8> {
        let len = payload.len() + 7;
        let mut v = vec![
            0xff,
            0xf1,
            (1 << 6) | (sf << 2) | (ch >> 2),
            ((ch & 3) << 6) | ((len >> 11) as u8 & 3),
            (len >> 3) as u8,
            ((len & 7) as u8) << 5 | 0x1f,
            0xfc,
        ];
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn parses_lc_16k_mono() {
        let f = frame(&[1, 2, 3], 8, 1);
        let (c, p) = parse_adts(&f).unwrap();
        assert_eq!((c.object_type, c.sample_rate, c.channels), (2, 16000, 1));
        assert_eq!(p, [1, 2, 3]);
        // AOT 2, index 8, channels 1 -> 0x1408
        assert_eq!(c.audio_specific_config(), [0x14, 0x08]);
    }

    #[test]
    fn rejects_bad_frames() {
        let mut f = frame(&[1, 2, 3], 8, 1);
        f.push(0);
        assert!(parse_adts(&f).is_err());
        assert!(parse_adts(&[0xff, 0xf1]).is_err());
        let mut g = frame(&[1], 8, 1);
        g[0] = 0;
        assert!(parse_adts(&g).is_err());
    }
}
