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
    /// Reads a raw AudioSpecificConfig (ISO/IEC 14496-3), as an AAC
    /// encoder reports it, for audio that arrives as raw access units rather
    /// than ADTS frames (for example AAC-LC, 48 kHz, mono is `11 88`).
    ///
    /// Only the two-byte form a sample entry built by this crate can carry
    /// is accepted: object types 1 to 4 (those ADTS can also describe), a
    /// sample rate from the table (no explicit 24-bit rate), channel
    /// configurations 1 to 7, 1024-sample frames, no core coder, and no
    /// extension flag or trailing extension bytes.
    pub fn from_raw_audio_specific_config(asc: &[u8]) -> Result<Self> {
        let [a, b] = asc else {
            return Err(Error::InvalidAudioConfig("not two bytes long"));
        };
        let v = u16::from_be_bytes([*a, *b]);
        let object_type = (v >> 11) as u8;
        let sample_rate_index = ((v >> 7) & 0xf) as u8;
        let channels = ((v >> 3) & 0xf) as u8;
        if !(1..=4).contains(&object_type) {
            return Err(Error::InvalidAudioConfig("unsupported audio object type"));
        }
        let sample_rate = *RATES
            .get(usize::from(sample_rate_index))
            .ok_or(Error::InvalidAudioConfig("unsupported sample rate index"))?;
        if !(1..=7).contains(&channels) {
            return Err(Error::InvalidAudioConfig(
                "unsupported channel configuration",
            ));
        }
        if v & 7 != 0 {
            return Err(Error::InvalidAudioConfig(
                "960-sample frames, core coder, or extension flag",
            ));
        }
        Ok(Self {
            object_type,
            sample_rate_index,
            sample_rate,
            channels,
        })
    }

    /// The 2-byte AudioSpecificConfig for this stream, as written into the
    /// sample entry.
    pub fn audio_specific_config(&self) -> [u8; 2] {
        let v = (u16::from(self.object_type) << 11)
            | (u16::from(self.sample_rate_index) << 7)
            | (u16::from(self.channels) << 3);
        v.to_be_bytes()
    }
}

impl AacConfig {
    /// Reads the 2-byte AudioSpecificConfig this crate writes.
    pub(crate) fn from_audio_specific_config(asc: [u8; 2]) -> Option<Self> {
        let v = u16::from_be_bytes(asc);
        let object_type = (v >> 11) as u8;
        let sample_rate_index = ((v >> 7) & 0xf) as u8;
        let channels = ((v >> 3) & 0xf) as u8;
        let sample_rate = *RATES.get(usize::from(sample_rate_index))?;
        (object_type != 0 && channels != 0).then_some(Self {
            object_type,
            sample_rate_index,
            sample_rate,
            channels,
        })
    }

    /// Wraps a raw AAC payload in a single-block ADTS frame without CRC.
    pub(crate) fn adts_frame(&self, payload: &[u8]) -> Vec<u8> {
        let len = payload.len() + 7;
        let mut v = vec![
            0xff,
            0xf1,
            ((self.object_type - 1) << 6) | (self.sample_rate_index << 2) | (self.channels >> 2),
            ((self.channels & 3) << 6) | ((len >> 11) as u8 & 3),
            (len >> 3) as u8,
            (((len & 7) as u8) << 5) | 0x1f,
            0xfc,
        ];
        v.extend_from_slice(payload);
        v
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
    fn adts_round_trip() {
        let f = frame(&[1, 2, 3], 8, 1);
        let (c, p) = parse_adts(&f).unwrap();
        assert_eq!(c.adts_frame(p), f);
        let back = AacConfig::from_audio_specific_config(c.audio_specific_config()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn raw_audio_specific_config() {
        // AAC-LC, 48 kHz, mono: what an app's encoder reports.
        let c = AacConfig::from_raw_audio_specific_config(&[0x11, 0x88]).unwrap();
        assert_eq!((c.object_type, c.sample_rate, c.channels), (2, 48000, 1));
        assert_eq!(c.sample_rate_index, 3);
        assert_eq!(c.audio_specific_config(), [0x11, 0x88]);
        // The ADTS form of the same stream gives the same config.
        let (from_adts, _) = parse_adts(&c.adts_frame(&[1, 2])).unwrap();
        assert_eq!(from_adts, c);
        for bad in [
            &[][..],
            &[0x11],
            &[0x11, 0x88, 0x56], // trailing extension bytes
            &[0x29, 0x88],       // object type 5 (SBR)
            &[0x17, 0x08],       // sample rate index 14
            &[0x11, 0x80],       // channel configuration 0
            &[0x11, 0x8c],       // 960-sample frames
            &[0x00, 0x08],       // object type 0
        ] {
            assert!(
                AacConfig::from_raw_audio_specific_config(bad).is_err(),
                "{bad:02x?}"
            );
        }
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
