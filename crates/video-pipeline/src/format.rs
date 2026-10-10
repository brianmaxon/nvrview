//! Parameter sets and the decoder format derived from them.

use crate::bits::ParseError;
use crate::color::{ColorInfo, VideoSignal};
use crate::geometry::{Rect, Size};
use crate::nal::{Codec, NalClass, ParameterSetKind, classify, nal_units};
use crate::{h264, h265};

/// Largest parameter set NAL unit kept, in bytes. Real ones are tens to a few
/// hundred bytes; anything larger is ignored.
pub const MAX_PARAMETER_SET_BYTES: usize = 4096;
/// Most parameter sets of one kind kept from one access unit.
pub const MAX_PARAMETER_SETS_PER_KIND: usize = 8;

const KINDS: usize = 3;

/// What an SPS says about the pictures that follow it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceInfo {
    /// Codec of the SPS.
    pub codec: Codec,
    /// `profile_idc` / `general_profile_idc`.
    pub profile_idc: u8,
    /// `level_idc` / `general_level_idc`.
    pub level_idc: u8,
    /// 0 monochrome, 1 4:2:0, 2 4:2:2, 3 4:4:4.
    pub chroma_format_idc: u8,
    /// Luma bit depth.
    pub bit_depth_luma: u8,
    /// Chroma bit depth.
    pub bit_depth_chroma: u8,
    /// Decoded picture size before cropping (H.264: whole macroblocks).
    pub coded_size: Size,
    /// The part of the coded picture to show (cropping / conformance window).
    pub visible_rect: Rect,
    /// VUI video signal type, when present.
    pub signal: Option<VideoSignal>,
}

/// One parameter set NAL unit, header included, start code excluded.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParameterSet {
    /// VPS, SPS, or PPS.
    pub kind: ParameterSetKind,
    /// The NAL unit bytes, emulation prevention included (as sent).
    pub data: Box<[u8]>,
}

/// The parameter sets a decoder is configured with, ordered VPS, SPS, PPS
/// (the order `CMVideoFormatDescriptionCreateFromHEVCParameterSets` and
/// similar APIs expect), without duplicates.
///
/// These are the only bytes the pipeline copies out of an access unit: a few
/// small NAL units once per format, kept so a later `Reset` can be followed by
/// a `Configure` without waiting for them again.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParameterSets {
    codec: Codec,
    sets: Vec<ParameterSet>,
}

impl ParameterSets {
    /// An empty collection for `codec`.
    pub fn new(codec: Codec) -> ParameterSets {
        ParameterSets {
            codec,
            sets: Vec::new(),
        }
    }

    /// Copies the parameter sets out of an Annex-B access unit. Oversized
    /// NAL units and more than [`MAX_PARAMETER_SETS_PER_KIND`] of one kind
    /// are ignored.
    pub fn from_access_unit(codec: Codec, data: &[u8]) -> ParameterSets {
        let mut out = ParameterSets::new(codec);
        for nal in nal_units(data) {
            if let NalClass::ParameterSet(kind) = classify(codec, nal) {
                out.insert(kind, nal);
            }
        }
        out
    }

    /// Adds one parameter set, keeping the order and the bounds. Returns
    /// whether it was added.
    pub fn insert(&mut self, kind: ParameterSetKind, nal: &[u8]) -> bool {
        if nal.len() > MAX_PARAMETER_SET_BYTES
            || self.count(kind) >= MAX_PARAMETER_SETS_PER_KIND
            || self.sets.iter().any(|s| s.kind == kind && *s.data == *nal)
        {
            return false;
        }
        let at = self
            .sets
            .iter()
            .position(|s| s.kind > kind)
            .unwrap_or(self.sets.len());
        self.sets.insert(
            at,
            ParameterSet {
                kind,
                data: nal.into(),
            },
        );
        true
    }

    /// Codec these parameter sets belong to.
    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// Number of parameter sets.
    pub fn len(&self) -> usize {
        self.sets.len()
    }

    /// True when there are none.
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    /// The `i`th parameter set, in VPS, SPS, PPS order.
    pub fn get(&self, i: usize) -> Option<&ParameterSet> {
        self.sets.get(i)
    }

    /// All parameter sets, in VPS, SPS, PPS order.
    pub fn iter(&self) -> std::slice::Iter<'_, ParameterSet> {
        self.sets.iter()
    }

    /// Parameter sets of one kind.
    pub fn of_kind(&self, kind: ParameterSetKind) -> impl Iterator<Item = &[u8]> {
        self.sets
            .iter()
            .filter(move |s| s.kind == kind)
            .map(|s| &*s.data)
    }

    fn count(&self, kind: ParameterSetKind) -> usize {
        self.sets.iter().filter(|s| s.kind == kind).count()
    }

    /// True when [`ParameterSets::from_access_unit`] on this access unit
    /// would produce exactly these sets. Allocates nothing.
    pub fn matches_access_unit(&self, codec: Codec, data: &[u8]) -> bool {
        if codec != self.codec {
            return false;
        }
        let mut seen = [false; KINDS * MAX_PARAMETER_SETS_PER_KIND];
        let mut seen_per_kind = [0usize; KINDS];
        let mut matched = 0;
        for nal in nal_units(data) {
            let NalClass::ParameterSet(kind) = classify(codec, nal) else {
                continue;
            };
            if nal.len() > MAX_PARAMETER_SET_BYTES {
                continue;
            }
            match self
                .sets
                .iter()
                .position(|s| s.kind == kind && *s.data == *nal)
            {
                Some(i) => {
                    if !seen[i] {
                        seen[i] = true;
                        seen_per_kind[kind as usize] += 1;
                        matched += 1;
                    }
                }
                // Extraction would have kept this one unless its kind was full.
                None if seen_per_kind[kind as usize] < MAX_PARAMETER_SETS_PER_KIND => {
                    return false;
                }
                None => {}
            }
        }
        matched == self.sets.len()
    }
}

impl<'a> IntoIterator for &'a ParameterSets {
    type Item = &'a ParameterSet;
    type IntoIter = std::slice::Iter<'a, ParameterSet>;
    fn into_iter(self) -> Self::IntoIter {
        self.sets.iter()
    }
}

/// Everything a platform needs to create a decoder for a stream.
#[derive(Clone, Debug, PartialEq)]
pub struct VideoFormat {
    /// The codec.
    pub codec: Codec,
    /// Parameter sets to build the decoder's format description from.
    pub parameter_sets: ParameterSets,
    /// Decoded picture size before cropping.
    pub coded_size: Size,
    /// Region of the decoded picture to show.
    pub visible_rect: Rect,
    /// YCbCr to RGB conversion for this stream.
    pub color: ColorInfo,
    /// The SPS fields everything above came from.
    pub sequence: SequenceInfo,
}

impl VideoFormat {
    /// Derives the format from a complete set of parameter sets: H.264 needs
    /// an SPS and a PPS, H.265 a VPS, an SPS, and a PPS. The first SPS that
    /// parses decides size and color.
    pub fn from_parameter_sets(parameter_sets: ParameterSets) -> Result<VideoFormat, ParseError> {
        let codec = parameter_sets.codec();
        if codec == Codec::H265 {
            let vps = parameter_sets
                .of_kind(ParameterSetKind::Vps)
                .next()
                .ok_or(ParseError::Missing("VPS"))?;
            h265::parse_vps(vps)?;
        }
        if parameter_sets
            .of_kind(ParameterSetKind::Pps)
            .next()
            .is_none()
        {
            return Err(ParseError::Missing("PPS"));
        }
        let mut first_error = ParseError::Missing("SPS");
        let mut sequence = None;
        for (i, sps) in parameter_sets.of_kind(ParameterSetKind::Sps).enumerate() {
            let parsed = match codec {
                Codec::H264 => h264::parse_sps(sps),
                Codec::H265 => h265::parse_sps(sps),
            };
            match parsed {
                Ok(s) => {
                    sequence = Some(s);
                    break;
                }
                Err(e) if i == 0 => first_error = e,
                Err(_) => {}
            }
        }
        let sequence = sequence.ok_or(first_error)?;
        Ok(VideoFormat {
            codec,
            coded_size: sequence.coded_size,
            visible_rect: sequence.visible_rect,
            color: ColorInfo::from_signal(sequence.signal.as_ref(), sequence.visible_rect.height),
            sequence,
            parameter_sets,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in nals {
            v.extend_from_slice(&[0, 0, 0, 1]);
            v.extend_from_slice(n);
        }
        v
    }

    #[test]
    fn extraction_orders_dedupes_and_bounds() {
        let pps: &[u8] = &[0x68, 0xCE, 0x38];
        let sps: &[u8] = &[0x67, 0x42, 0x00, 0x1E];
        let big = {
            let mut b = vec![0x67u8];
            b.resize(MAX_PARAMETER_SET_BYTES + 1, 0x11);
            b
        };
        let au = annexb(&[pps, sps, sps, &big, &[0x65, 0x88]]);
        let p = ParameterSets::from_access_unit(Codec::H264, &au);
        assert_eq!(p.len(), 2);
        assert_eq!(p.get(0).unwrap().kind, ParameterSetKind::Sps);
        assert_eq!(&*p.get(1).unwrap().data, pps);
        assert!(p.matches_access_unit(Codec::H264, &au));
        assert!(p.matches_access_unit(Codec::H264, &annexb(&[sps, pps])));
        assert!(!p.matches_access_unit(Codec::H264, &annexb(&[sps])));
        assert!(!p.matches_access_unit(Codec::H265, &au));
        assert!(!p.matches_access_unit(Codec::H264, &annexb(&[sps, pps, &[0x68, 1]])));

        // Many distinct PPSs: only the first MAX are kept, and matching agrees.
        let mut nals: Vec<Vec<u8>> = vec![sps.to_vec()];
        for i in 0..(MAX_PARAMETER_SETS_PER_KIND as u8 + 4) {
            nals.push(vec![0x68, 0x80 | i]);
        }
        let refs: Vec<&[u8]> = nals.iter().map(|v| v.as_slice()).collect();
        let au = annexb(&refs);
        let p = ParameterSets::from_access_unit(Codec::H264, &au);
        assert_eq!(p.len(), 1 + MAX_PARAMETER_SETS_PER_KIND);
        assert!(p.matches_access_unit(Codec::H264, &au));
    }

    #[test]
    fn missing_sets_are_reported() {
        let p = ParameterSets::from_access_unit(Codec::H264, &annexb(&[&[0x68, 0xCE]]));
        assert_eq!(
            VideoFormat::from_parameter_sets(p),
            Err(ParseError::Missing("SPS"))
        );
        let p = ParameterSets::new(Codec::H265);
        assert_eq!(
            VideoFormat::from_parameter_sets(p),
            Err(ParseError::Missing("VPS"))
        );
    }
}
