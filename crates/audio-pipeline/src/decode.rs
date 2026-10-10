//! Codec selection and decoding of one audio record to f32 samples.
//!
//! The codec is chosen per record from the second character of the record
//! magic (`stream_index % 10`): `5` is AAC in ADTS framing (observed); any
//! other value selects IMA ADPCM or µ-law by payload size (reported,
//! unconfirmed; see [`crate::adpcm`]). AAC-LC is decoded by Symphonia's AAC
//! decoder, used unmodified (MPL-2.0).

use crate::adpcm::{self, AdpcmError};
use crate::adts::{AdtsError, AdtsHeader};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use symphonia_codec_aac::AacDecoder as SymphoniaAac;
use symphonia_core::codecs::audio::well_known::CODEC_ID_AAC;
use symphonia_core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia_core::packet::PacketRef;
use symphonia_core::units::{Duration, Timestamp};

/// Payloads larger than this are dropped whatever the codec, as the
/// official client does (reported).
pub const MAX_PAYLOAD: usize = 2048;

/// The codec of one record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCodec {
    /// AAC-LC in an ADTS frame; rate and channels from the header.
    Aac,
    /// IMA ADPCM, 8 kHz mono (reported).
    ImaAdpcm,
    /// G.711 µ-law, 8 kHz mono (reported).
    MuLaw,
}

impl AudioCodec {
    /// The codec for a record with this stream index (the two digits of the
    /// magic, `05wb` = 5) and payload length.
    pub fn select(stream_index: u8, payload_len: usize) -> AudioCodec {
        if stream_index % 10 == 5 {
            AudioCodec::Aac
        } else if payload_len < adpcm::ADPCM_MAX_LEN {
            AudioCodec::ImaAdpcm
        } else {
            AudioCodec::MuLaw
        }
    }
}

/// Sample rate and channel count of decoded audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcmFormat {
    /// Samples per second per channel.
    pub sample_rate: u32,
    /// Interleaved channels (1 or 2).
    pub channels: usize,
}

/// What one record decoded to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decoded {
    /// The codec used.
    pub codec: AudioCodec,
    /// Format of the samples appended.
    pub format: PcmFormat,
    /// Frames (samples per channel) appended.
    pub frames: usize,
}

/// Why a record produced no samples.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// Payload over [`MAX_PAYLOAD`] bytes.
    TooLarge(usize),
    /// Not a usable ADTS frame.
    Adts(AdtsError),
    /// Not a usable ADPCM payload.
    Adpcm(AdpcmError),
    /// A valid ADTS header whose payload the AAC decoder rejected. The frame's
    /// format and length are known, so the caller can put silence in its
    /// place and keep the sample count (the stream's only clock) right.
    Aac {
        /// Format the frame would have decoded to.
        format: PcmFormat,
        /// Frames it would have decoded to.
        frames: usize,
        /// The decoder's message.
        message: String,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::TooLarge(n) => write!(f, "audio payload of {n} bytes is too large"),
            DecodeError::Adts(e) => write!(f, "{e}"),
            DecodeError::Adpcm(e) => write!(f, "bad ADPCM payload: {e:?}"),
            DecodeError::Aac { message, .. } => write!(f, "AAC decode failed: {message}"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decodes records of any supported codec. Keeps one AAC decoder and
/// replaces it when the ADTS format changes.
pub struct Decoder {
    aac: Option<(AdtsHeader, SymphoniaAac)>,
}

impl fmt::Debug for Decoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Decoder")
            .field("aac_format", &self.aac.as_ref().map(|a| a.0))
            .finish()
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Decoder::new()
    }
}

impl Decoder {
    /// A decoder with no state yet.
    pub fn new() -> Decoder {
        Decoder { aac: None }
    }

    /// Forgets decoder state (after a discontinuity), so the next frame
    /// does not overlap-add with audio from before it.
    pub fn reset(&mut self) {
        if let Some((_, d)) = &mut self.aac {
            d.reset();
        }
    }

    /// Decodes one record's payload, appending interleaved f32 samples in
    /// -1..1 to `out`. Never panics on malformed input; on error `out` is
    /// left as it was.
    pub fn decode(
        &mut self,
        stream_index: u8,
        payload: &[u8],
        out: &mut Vec<f32>,
    ) -> Result<Decoded, DecodeError> {
        if payload.len() > MAX_PAYLOAD {
            return Err(DecodeError::TooLarge(payload.len()));
        }
        let codec = AudioCodec::select(stream_index, payload.len());
        let mono8k = PcmFormat {
            sample_rate: adpcm::RATE,
            channels: 1,
        };
        match codec {
            AudioCodec::Aac => self.decode_aac(payload, out),
            AudioCodec::ImaAdpcm => {
                let frames = adpcm::decode_ima(payload, out).map_err(DecodeError::Adpcm)?;
                Ok(Decoded {
                    codec,
                    format: mono8k,
                    frames,
                })
            }
            AudioCodec::MuLaw => Ok(Decoded {
                codec,
                format: mono8k,
                frames: adpcm::decode_mulaw(payload, out),
            }),
        }
    }

    fn decode_aac(&mut self, frame: &[u8], out: &mut Vec<f32>) -> Result<Decoded, DecodeError> {
        let h = AdtsHeader::parse_record(frame).map_err(DecodeError::Adts)?;
        let format = PcmFormat {
            sample_rate: h.sample_rate,
            channels: h.channels(),
        };
        let fail = |message: String| DecodeError::Aac {
            format,
            frames: h.samples(),
            message,
        };
        if !matches!(&self.aac, Some((old, _)) if old.same_format(&h)) {
            let mut params = AudioCodecParameters::new();
            params
                .for_codec(CODEC_ID_AAC)
                .with_extra_data(Box::new(h.audio_specific_config()));
            let d = SymphoniaAac::try_new(&params, &AudioDecoderOptions::default())
                .map_err(|e| fail(e.to_string()))?;
            self.aac = Some((h, d));
        }
        let Some((_, dec)) = &mut self.aac else {
            return Err(fail("no decoder".into()));
        };
        let raw = &frame[h.header_len..];
        let start = out.len();
        // Symphonia's decoder is written to return errors, but a panic on
        // some malformed frame must not take the tile down: treat it as a
        // decode error and start over with a fresh decoder.
        let result = catch_unwind(AssertUnwindSafe(|| {
            let packet = PacketRef::new(0, Timestamp::ZERO, Duration::from(1024u64), raw);
            match dec.decode_ref(&packet) {
                Ok(buf) => {
                    let n = buf.frames() * format.channels;
                    if buf.spec().channels().count() != format.channels {
                        return Err("channel count differs from the header".to_string());
                    }
                    out.resize(start + n, 0.0);
                    buf.copy_to_slice_interleaved::<f32, _>(&mut out[start..]);
                    Ok(buf.frames())
                }
                Err(e) => Err(e.to_string()),
            }
        }));
        match result {
            Ok(Ok(frames)) => Ok(Decoded {
                codec: AudioCodec::Aac,
                format,
                frames,
            }),
            Ok(Err(m)) => {
                out.truncate(start);
                dec.reset();
                Err(fail(m))
            }
            Err(_) => {
                out.truncate(start);
                self.aac = None;
                Err(fail("decoder panicked".into()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_selection_follows_the_magic_and_size() {
        assert_eq!(AudioCodec::select(5, 300), AudioCodec::Aac);
        assert_eq!(AudioCodec::select(15, 300), AudioCodec::Aac);
        assert_eq!(AudioCodec::select(1, 479), AudioCodec::ImaAdpcm);
        assert_eq!(AudioCodec::select(1, 480), AudioCodec::MuLaw);
        assert_eq!(AudioCodec::select(0, 20), AudioCodec::ImaAdpcm);
    }

    #[test]
    fn oversized_payloads_are_dropped() {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        assert_eq!(
            d.decode(1, &[0xFF; 2049], &mut out),
            Err(DecodeError::TooLarge(2049))
        );
        assert!(out.is_empty());
    }

    #[test]
    fn mulaw_records_decode_at_8k() {
        let mut d = Decoder::new();
        let mut out = Vec::new();
        let r = d.decode(1, &[0xFF; 600], &mut out).unwrap();
        assert_eq!(r.codec, AudioCodec::MuLaw);
        assert_eq!(r.format.sample_rate, 8000);
        assert_eq!(r.frames, 600);
        assert!(out.iter().all(|&s| s == 0.0));
    }
}
