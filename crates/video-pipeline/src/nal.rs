//! Annex-B NAL unit splitting and per-access-unit classification.
//!
//! An Annex-B byte stream (H.264 Annex B, H.265 Annex B) separates NAL units
//! with start codes `00 00 01`, optionally preceded by more zero bytes. The
//! splitter borrows each NAL unit from the input; nothing is copied.

use crate::bits::BitReader;

/// Video codec of a stream.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    /// H.264 / AVC.
    H264 = 1,
    /// H.265 / HEVC.
    H265 = 2,
}

/// Iterator over the NAL units of an Annex-B buffer, each without its start
/// code and without trailing zero bytes. Bytes before the first start code
/// are ignored.
#[derive(Clone, Debug)]
pub struct NalUnits<'a> {
    data: &'a [u8],
    /// Index just past the start code of the next NAL unit.
    next: Option<usize>,
}

/// Splits an Annex-B buffer into NAL units.
pub fn nal_units(data: &[u8]) -> NalUnits<'_> {
    NalUnits {
        data,
        next: find_start_code(data, 0).map(|i| i + 3),
    }
}

/// Index of the next `00 00 01` at or after `from`.
fn find_start_code(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 2 < data.len() {
        let c = data[i + 2];
        if c > 1 {
            // Neither a third byte nor the start of a code can be here.
            i += 3;
        } else if c == 1 && data[i] == 0 && data[i + 1] == 0 {
            return Some(i);
        } else {
            i += 1;
        }
    }
    None
}

impl<'a> Iterator for NalUnits<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        loop {
            let start = self.next?;
            let end = match find_start_code(self.data, start) {
                Some(i) => {
                    self.next = Some(i + 3);
                    i
                }
                None => {
                    self.next = None;
                    self.data.len()
                }
            };
            let mut nal = &self.data[start..end];
            while let [rest @ .., 0] = nal {
                nal = rest;
            }
            if !nal.is_empty() {
                return Some(nal);
            }
        }
    }
}

/// How a picture allows decoding to start.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RandomAccess {
    /// Not a random access point.
    None = 0,
    /// H.264 IDR, or H.265 IDR_W_RADL / IDR_N_LP.
    Idr = 1,
    /// H.265 clean random access (CRA_NUT).
    Cra = 2,
    /// H.265 broken link access (BLA_W_LP, BLA_W_RADL, BLA_N_LP).
    Bla = 3,
    /// H.264 non-IDR picture with a recovery point SEI whose
    /// `recovery_frame_cnt` is 0, so it is correct on its own.
    RecoveryPoint = 4,
    /// H.265 reserved IRAP types (22, 23).
    OtherIrap = 5,
}

impl RandomAccess {
    /// True for every random access point.
    pub const fn is_random_access(self) -> bool {
        !matches!(self, RandomAccess::None)
    }

    /// True when pictures that lead this one in output order may reference
    /// pictures before it (CRA, BLA), so they must be skipped when decoding
    /// starts here.
    pub const fn may_have_skipped_leading(self) -> bool {
        matches!(
            self,
            RandomAccess::Cra | RandomAccess::Bla | RandomAccess::OtherIrap
        )
    }
}

/// Kind of a parameter set NAL unit.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ParameterSetKind {
    /// H.265 video parameter set.
    Vps = 0,
    /// Sequence parameter set.
    Sps = 1,
    /// Picture parameter set.
    Pps = 2,
}

/// What one NAL unit is, as far as the pipeline cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NalClass {
    ParameterSet(ParameterSetKind),
    Sei,
    Slice {
        random_access: RandomAccess,
        /// H.265 RASL picture.
        rasl: bool,
        /// Not a leading picture (H.265 trailing or IRAP; every H.264 slice).
        trailing: bool,
    },
    Other,
}

/// Classifies a NAL unit (with its header, without start code).
pub(crate) fn classify(codec: Codec, nal: &[u8]) -> NalClass {
    match codec {
        Codec::H264 => {
            let Some(&h) = nal.first() else {
                return NalClass::Other;
            };
            if h & 0x80 != 0 {
                return NalClass::Other;
            }
            match h & 0x1F {
                5 => NalClass::Slice {
                    random_access: RandomAccess::Idr,
                    rasl: false,
                    trailing: true,
                },
                1..=4 => NalClass::Slice {
                    random_access: RandomAccess::None,
                    rasl: false,
                    trailing: true,
                },
                6 => NalClass::Sei,
                7 => NalClass::ParameterSet(ParameterSetKind::Sps),
                8 => NalClass::ParameterSet(ParameterSetKind::Pps),
                _ => NalClass::Other,
            }
        }
        Codec::H265 => {
            let [h0, h1, ..] = *nal else {
                return NalClass::Other;
            };
            let forbidden = h0 & 0x80 != 0;
            let layer = ((h0 & 1) << 5) | (h1 >> 3);
            let temporal_plus1 = h1 & 7;
            if forbidden || layer != 0 || temporal_plus1 == 0 {
                return NalClass::Other;
            }
            let t = (h0 >> 1) & 0x3F;
            let slice = |random_access, rasl, trailing| NalClass::Slice {
                random_access,
                rasl,
                trailing,
            };
            match t {
                // TRAIL, TSA, STSA, and reserved non-IRAP sub-layer types.
                0..=5 | 10..=15 => slice(RandomAccess::None, false, true),
                // RADL
                6 | 7 => slice(RandomAccess::None, false, false),
                // RASL
                8 | 9 => slice(RandomAccess::None, true, false),
                16..=18 => slice(RandomAccess::Bla, false, true),
                19 | 20 => slice(RandomAccess::Idr, false, true),
                21 => slice(RandomAccess::Cra, false, true),
                22 | 23 => slice(RandomAccess::OtherIrap, false, true),
                24..=31 => slice(RandomAccess::None, false, true),
                32 => NalClass::ParameterSet(ParameterSetKind::Vps),
                33 => NalClass::ParameterSet(ParameterSetKind::Sps),
                34 => NalClass::ParameterSet(ParameterSetKind::Pps),
                39 | 40 => NalClass::Sei,
                _ => NalClass::Other,
            }
        }
    }
}

/// What the pipeline needs to know about one access unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessUnitInfo {
    /// At least one slice NAL unit is present.
    pub has_picture: bool,
    /// Judged from the first slice's NAL type (and, for H.264, a recovery
    /// point SEI before it).
    pub random_access: RandomAccess,
    /// The picture is an H.265 RASL picture.
    pub rasl: bool,
    /// The picture is not a leading picture.
    pub trailing: bool,
    /// Number of parameter set NAL units present.
    pub parameter_sets: u32,
}

/// Classifies an Annex-B access unit by its NAL unit types.
pub fn analyze(codec: Codec, data: &[u8]) -> AccessUnitInfo {
    let mut info = AccessUnitInfo {
        has_picture: false,
        random_access: RandomAccess::None,
        rasl: false,
        trailing: false,
        parameter_sets: 0,
    };
    let mut recovery_point = false;
    for nal in nal_units(data) {
        match classify(codec, nal) {
            NalClass::ParameterSet(_) => {
                info.parameter_sets = info.parameter_sets.saturating_add(1);
            }
            NalClass::Sei if codec == Codec::H264 && !info.has_picture => {
                recovery_point |= h264_sei_has_exact_recovery_point(nal);
            }
            NalClass::Slice {
                random_access,
                rasl,
                trailing,
            } if !info.has_picture => {
                info.has_picture = true;
                info.random_access = random_access;
                info.rasl = rasl;
                info.trailing = trailing;
                if random_access == RandomAccess::None
                    && recovery_point
                    && codec == Codec::H264
                    && nal[0] & 0x1F == 1
                {
                    info.random_access = RandomAccess::RecoveryPoint;
                }
            }
            _ => {}
        }
    }
    info
}

/// True when an H.264 SEI NAL unit carries a recovery point message
/// (payload type 6) with `recovery_frame_cnt == 0`.
fn h264_sei_has_exact_recovery_point(nal: &[u8]) -> bool {
    let mut r = BitReader::new(&nal[1..]);
    // sei_message() repeats until rbsp_trailing_bits; a garbage or truncated
    // message just ends the scan.
    for _ in 0..16 {
        let Ok(payload_type) = read_sei_number(&mut r) else {
            return false;
        };
        let Ok(payload_size) = read_sei_number(&mut r) else {
            return false;
        };
        if payload_type == 6 {
            return r.ue() == Ok(0);
        }
        // Skip the payload; truncation ends the scan.
        if payload_size > nal.len() as u32 || r.skip(payload_size * 8).is_err() {
            return false;
        }
    }
    false
}

/// `payloadType` / `payloadSize`: a run of `0xFF` bytes plus a final byte.
fn read_sei_number(r: &mut BitReader<'_>) -> crate::bits::Result<u32> {
    let mut v: u32 = 0;
    for _ in 0..64 {
        let b = r.bits(8)?;
        v += b;
        if b != 0xFF {
            return Ok(v);
        }
    }
    Err(crate::bits::ParseError::OutOfRange("SEI payload number"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let data = [
            0xAA, 0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 0, 1, 0x65, 4, 5, 0, 0,
        ];
        let nals: Vec<&[u8]> = nal_units(&data).collect();
        assert_eq!(nals, [&[0x67, 1, 2][..], &[0x68, 3], &[0x65, 4, 5]]);
    }

    #[test]
    fn no_start_code_means_no_nal_units() {
        assert_eq!(nal_units(&[0x65, 1, 2, 3]).count(), 0);
        assert_eq!(nal_units(&[]).count(), 0);
        assert_eq!(nal_units(&[0, 0, 1]).count(), 0);
        assert_eq!(nal_units(&[0, 0, 1, 0, 0, 1]).count(), 0);
    }

    #[test]
    fn h264_types() {
        let au = [
            0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0xCE, 0, 0, 1, 0x65, 0x88,
        ];
        let i = analyze(Codec::H264, &au);
        assert!(i.has_picture);
        assert_eq!(i.random_access, RandomAccess::Idr);
        assert_eq!(i.parameter_sets, 2);
        let p = analyze(Codec::H264, &[0, 0, 1, 0x41, 0x9A]);
        assert!(p.has_picture);
        assert_eq!(p.random_access, RandomAccess::None);
        let forbidden = analyze(Codec::H264, &[0, 0, 1, 0xE5, 0x9A]);
        assert!(!forbidden.has_picture);
    }

    #[test]
    fn h264_recovery_point_sei() {
        // SEI: type 6, size 1, recovery_frame_cnt ue(0) = '1', exact_match 1,
        // broken_link 0, changing_slice_group_idc 00, then a stop bit.
        let sei = [0x06, 0x06, 0x01, 0b1100_0100, 0x80];
        let mut au = vec![0, 0, 1];
        au.extend_from_slice(&sei);
        au.extend_from_slice(&[0, 0, 1, 0x41, 0x88]);
        assert_eq!(
            analyze(Codec::H264, &au).random_access,
            RandomAccess::RecoveryPoint
        );
        // recovery_frame_cnt = 1 (ue '010') is gradual refresh: not a start.
        let sei = [0x06, 0x06, 0x01, 0b0101_0010, 0x80];
        let mut au = vec![0, 0, 1];
        au.extend_from_slice(&sei);
        au.extend_from_slice(&[0, 0, 1, 0x41, 0x88]);
        assert_eq!(analyze(Codec::H264, &au).random_access, RandomAccess::None);
        // A different SEI first, then the recovery point.
        let sei = [0x06, 0x05, 0x02, 0xAB, 0xCD, 0x06, 0x01, 0b1100_0100, 0x80];
        let mut au = vec![0, 0, 1];
        au.extend_from_slice(&sei);
        au.extend_from_slice(&[0, 0, 1, 0x41, 0x88]);
        assert_eq!(
            analyze(Codec::H264, &au).random_access,
            RandomAccess::RecoveryPoint
        );
    }

    #[test]
    fn h265_types() {
        let nal = |t: u8| [0u8, 0, 1, t << 1, 1, 0xAF];
        let ra = |t: u8| analyze(Codec::H265, &nal(t)).random_access;
        assert_eq!(ra(19), RandomAccess::Idr);
        assert_eq!(ra(20), RandomAccess::Idr);
        assert_eq!(ra(21), RandomAccess::Cra);
        assert_eq!(ra(16), RandomAccess::Bla);
        assert_eq!(ra(1), RandomAccess::None);
        assert!(analyze(Codec::H265, &nal(8)).rasl);
        assert!(!analyze(Codec::H265, &nal(6)).trailing);
        assert!(analyze(Codec::H265, &nal(1)).trailing);
        assert!(!analyze(Codec::H265, &nal(32)).has_picture);
        assert_eq!(analyze(Codec::H265, &nal(32)).parameter_sets, 1);
        // nuh_layer_id != 0 is ignored.
        assert!(!analyze(Codec::H265, &[0, 0, 1, (19 << 1) | 1, 1, 0xAF]).has_picture);
        // nuh_temporal_id_plus1 == 0 is invalid.
        assert!(!analyze(Codec::H265, &[0, 0, 1, 19 << 1, 0, 0xAF]).has_picture);
    }
}
