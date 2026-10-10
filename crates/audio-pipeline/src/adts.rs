//! ADTS frame headers (ISO/IEC 13818-7 and 14496-3).
//!
//! Every audio record a device sends is exactly one ADTS frame
//! (docs/protocol.md, "Audio payloads"). The header says the profile, sample
//! rate, channel configuration, and frame length; the decoder needs those, and
//! anything that does not look like a frame we can decode is rejected here,
//! without panicking, before it reaches the decoder.

use std::fmt;

/// Samples per channel in one AAC-LC raw data block.
pub const SAMPLES_PER_BLOCK: usize = 1024;

/// Sample rates by `sampling_frequency_index` (indexes 13 to 15 are reserved
/// or escape values that ADTS cannot use).
const SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

/// A parsed ADTS header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdtsHeader {
    /// MPEG-4 audio object type (`profile + 1`); 2 is AAC-LC.
    pub object_type: u8,
    /// Index into the standard sample-rate table.
    pub sample_rate_index: u8,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Channel configuration (1 = mono, 2 = stereo, ...).
    pub channel_config: u8,
    /// Header length in bytes: 7, or 9 with a CRC.
    pub header_len: usize,
    /// Whole frame length in bytes, header included.
    pub frame_len: usize,
    /// Raw data blocks in the frame (1 to 4).
    pub raw_blocks: u8,
}

/// Why bytes are not a usable ADTS frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdtsError {
    /// Fewer bytes than a header, or than the header's frame length.
    Truncated,
    /// The 12-bit sync word is not `0xFFF`, or the layer field is not 0.
    NoSync,
    /// Frame length smaller than its own header.
    BadLength,
    /// A reserved sample-rate index.
    BadSampleRate,
    /// Not AAC-LC (another profile, for example Main or SSR).
    UnsupportedProfile(u8),
    /// Channel configuration 0 (layout in a program config element) or more
    /// than two channels; the decoder handles mono and stereo.
    UnsupportedChannels(u8),
    /// More than one raw data block per frame.
    MultipleBlocks(u8),
    /// The frame is shorter than the payload it was found in (trailing
    /// bytes), which the protocol says never happens.
    TrailingBytes,
}

impl fmt::Display for AdtsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdtsError::Truncated => write!(f, "truncated ADTS frame"),
            AdtsError::NoSync => write!(f, "no ADTS sync word"),
            AdtsError::BadLength => write!(f, "ADTS frame length shorter than its header"),
            AdtsError::BadSampleRate => write!(f, "reserved ADTS sample-rate index"),
            AdtsError::UnsupportedProfile(p) => write!(f, "unsupported AAC object type {p}"),
            AdtsError::UnsupportedChannels(c) => {
                write!(f, "unsupported ADTS channel configuration {c}")
            }
            AdtsError::MultipleBlocks(n) => write!(f, "{n} raw data blocks in one ADTS frame"),
            AdtsError::TrailingBytes => write!(f, "bytes after the ADTS frame"),
        }
    }
}

impl std::error::Error for AdtsError {}

impl AdtsHeader {
    /// Parses and validates the header at the start of `b`. `b` must hold at
    /// least the whole frame.
    pub fn parse(b: &[u8]) -> Result<AdtsHeader, AdtsError> {
        if b.len() < 7 {
            return Err(AdtsError::Truncated);
        }
        // syncword(12) id(1) layer(2) protection_absent(1)
        if b[0] != 0xFF || b[1] & 0xF6 != 0xF0 {
            return Err(AdtsError::NoSync);
        }
        let protection_absent = b[1] & 0x01 != 0;
        // profile(2) sampling_frequency_index(4) private(1) channel_config(3, spans bytes)
        let object_type = (b[2] >> 6) + 1;
        let sample_rate_index = (b[2] >> 2) & 0x0F;
        let channel_config = ((b[2] & 0x01) << 2) | (b[3] >> 6);
        let frame_len =
            (usize::from(b[3] & 0x03) << 11) | (usize::from(b[4]) << 3) | usize::from(b[5] >> 5);
        let raw_blocks = (b[6] & 0x03) + 1;
        let header_len = if protection_absent { 7 } else { 9 };

        if frame_len < header_len {
            return Err(AdtsError::BadLength);
        }
        if b.len() < frame_len.max(header_len) {
            return Err(AdtsError::Truncated);
        }
        let sample_rate = *SAMPLE_RATES
            .get(usize::from(sample_rate_index))
            .ok_or(AdtsError::BadSampleRate)?;
        if object_type != 2 {
            return Err(AdtsError::UnsupportedProfile(object_type));
        }
        if !(1..=2).contains(&channel_config) {
            return Err(AdtsError::UnsupportedChannels(channel_config));
        }
        if raw_blocks != 1 {
            return Err(AdtsError::MultipleBlocks(raw_blocks));
        }
        Ok(AdtsHeader {
            object_type,
            sample_rate_index,
            sample_rate,
            channel_config,
            header_len,
            frame_len,
            raw_blocks,
        })
    }

    /// Parses `record`, which must be exactly one frame (one audio record).
    pub fn parse_record(record: &[u8]) -> Result<AdtsHeader, AdtsError> {
        let h = AdtsHeader::parse(record)?;
        if h.frame_len != record.len() {
            return Err(AdtsError::TrailingBytes);
        }
        Ok(h)
    }

    /// Number of channels.
    pub fn channels(&self) -> usize {
        usize::from(self.channel_config)
    }

    /// Samples per channel the frame decodes to.
    pub fn samples(&self) -> usize {
        SAMPLES_PER_BLOCK * usize::from(self.raw_blocks)
    }

    /// The two-byte MPEG-4 AudioSpecificConfig equivalent to this header:
    /// object type (5 bits), sample-rate index (4), channel configuration (4),
    /// and three zero bits (1024-sample frames, no core coder, no extension).
    pub fn audio_specific_config(&self) -> [u8; 2] {
        let v: u16 = (u16::from(self.object_type) << 11)
            | (u16::from(self.sample_rate_index) << 7)
            | (u16::from(self.channel_config) << 3);
        v.to_be_bytes()
    }

    /// True when `other` decodes with the same decoder configuration.
    pub fn same_format(&self, other: &AdtsHeader) -> bool {
        self.object_type == other.object_type
            && self.sample_rate_index == other.sample_rate_index
            && self.channel_config == other.channel_config
    }
}

/// Splits a raw ADTS stream (such as a fixture file) into frames. Stops at
/// the first byte that does not start a frame.
pub fn split_frames(mut b: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    while b.len() >= 7 {
        if b[0] != 0xFF || b[1] & 0xF0 != 0xF0 {
            break;
        }
        let len =
            (usize::from(b[3] & 0x03) << 11) | (usize::from(b[4]) << 3) | usize::from(b[5] >> 5);
        if len < 7 || len > b.len() {
            break;
        }
        out.push(&b[..len]);
        b = &b[len..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a header for a frame of `len` bytes.
    pub(crate) fn header(object_type: u8, rate_index: u8, channels: u8, len: usize) -> [u8; 7] {
        let profile = object_type - 1;
        [
            0xFF,
            0xF1,
            (profile << 6) | (rate_index << 2) | (channels >> 2),
            ((channels & 3) << 6) | ((len >> 11) as u8 & 3),
            (len >> 3) as u8,
            ((len as u8 & 7) << 5) | 0x1F,
            0xFC,
        ]
    }

    #[test]
    fn parses_16k_mono_lc() {
        let mut f = header(2, 8, 1, 300).to_vec();
        f.resize(300, 0);
        let h = AdtsHeader::parse_record(&f).unwrap();
        assert_eq!(h.sample_rate, 16_000);
        assert_eq!(h.channels(), 1);
        assert_eq!(h.frame_len, 300);
        assert_eq!(h.header_len, 7);
        assert_eq!(h.samples(), 1024);
        // AAC-LC (2), index 8, mono: 00010 1000 0001 000.
        assert_eq!(h.audio_specific_config(), [0x14, 0x08]);
    }

    #[test]
    fn rejects_bad_headers() {
        let mut f = header(2, 8, 1, 20).to_vec();
        f.resize(20, 0);
        assert_eq!(AdtsHeader::parse(&f[..6]), Err(AdtsError::Truncated));
        assert_eq!(AdtsHeader::parse(&f[..19]), Err(AdtsError::Truncated));
        let mut g = f.clone();
        g[0] = 0xFE;
        assert_eq!(AdtsHeader::parse(&g), Err(AdtsError::NoSync));
        let mut g = header(2, 13, 1, 20).to_vec();
        g.resize(20, 0);
        assert_eq!(AdtsHeader::parse(&g), Err(AdtsError::BadSampleRate));
        let mut g = header(1, 8, 1, 20).to_vec();
        g.resize(20, 0);
        assert_eq!(AdtsHeader::parse(&g), Err(AdtsError::UnsupportedProfile(1)));
        let mut g = header(2, 8, 0, 20).to_vec();
        g.resize(20, 0);
        assert_eq!(
            AdtsHeader::parse(&g),
            Err(AdtsError::UnsupportedChannels(0))
        );
        let mut g = header(2, 8, 6, 20).to_vec();
        g.resize(20, 0);
        assert_eq!(
            AdtsHeader::parse(&g),
            Err(AdtsError::UnsupportedChannels(6))
        );
        let g = header(2, 8, 1, 5);
        assert_eq!(AdtsHeader::parse(&g), Err(AdtsError::BadLength));
        let mut g = f.clone();
        g[6] = 0xFD;
        assert_eq!(AdtsHeader::parse(&g), Err(AdtsError::MultipleBlocks(2)));
        let mut g = f.clone();
        g.push(0);
        assert_eq!(AdtsHeader::parse_record(&g), Err(AdtsError::TrailingBytes));
    }

    #[test]
    fn crc_header_is_nine_bytes() {
        let mut f = header(2, 3, 2, 40).to_vec();
        f[1] = 0xF0;
        f.resize(40, 0);
        let h = AdtsHeader::parse_record(&f).unwrap();
        assert_eq!(h.header_len, 9);
        assert_eq!(h.sample_rate, 48_000);
        assert_eq!(h.channels(), 2);
    }

    #[test]
    fn splits_a_stream() {
        let mut s = Vec::new();
        for len in [10usize, 30, 8] {
            let mut f = header(2, 8, 1, len).to_vec();
            f.resize(len, 0);
            s.extend(f);
        }
        s.extend([0xFF, 0x00]);
        let frames = split_frames(&s);
        assert_eq!(
            frames.iter().map(|f| f.len()).collect::<Vec<_>>(),
            [10, 30, 8]
        );
    }
}
