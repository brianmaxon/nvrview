//! Annex-B splitting and H.264/H.265 SPS parsing.

use crate::bits::{BitReader, unescape};
use crate::error::{Error, Result};

/// Video codec of the track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    H265,
}

/// Splits an Annex-B buffer into NAL units (start codes and trailing zero
/// bytes removed). Zero-length units are dropped.
pub(crate) fn split_annexb(data: &[u8]) -> Result<Vec<&[u8]>> {
    let mut starts = Vec::new(); // index of first byte after each start code
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    if starts.is_empty() {
        return Err(Error::InvalidAnnexB);
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut e = match starts.get(k + 1) {
            Some(&next) => next - 3,
            None => data.len(),
        };
        while e > s && data[e - 1] == 0 {
            e -= 1;
        }
        if e > s {
            out.push(&data[s..e]);
        }
    }
    Ok(out)
}

/// What a NAL unit means to the muxer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NalKind {
    Vps,
    Sps,
    Pps,
    /// Access unit delimiter, end of sequence/stream, filler: dropped.
    Drop,
    /// Everything else (slices, SEI): kept in the sample.
    Keep,
}

pub(crate) fn classify(codec: VideoCodec, nal: &[u8]) -> NalKind {
    match codec {
        VideoCodec::H264 => match nal[0] & 0x1f {
            7 => NalKind::Sps,
            8 => NalKind::Pps,
            9..=12 => NalKind::Drop,
            _ => NalKind::Keep,
        },
        VideoCodec::H265 => match (nal[0] >> 1) & 0x3f {
            32 => NalKind::Vps,
            33 => NalKind::Sps,
            34 => NalKind::Pps,
            35..=38 => NalKind::Drop,
            _ => NalKind::Keep,
        },
    }
}

/// Parameter sets in effect for a sample entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ParamSets {
    pub vps: Vec<Vec<u8>>,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
}

impl ParamSets {
    pub(crate) fn push_unique(list: &mut Vec<Vec<u8>>, nal: &[u8]) {
        if !list.iter().any(|x| x == nal) {
            list.push(nal.to_vec());
        }
    }
}

/// Facts from an H.264 SPS.
#[derive(Clone, Debug)]
pub(crate) struct H264Sps {
    pub profile_idc: u8,
    pub constraint_flags: u8,
    pub level_idc: u8,
    pub chroma_format_idc: u8,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub width: u32,
    pub height: u32,
}

const BAD: Error = Error::InvalidParameterSet("truncated or malformed SPS");

fn skip_scaling_list(r: &mut BitReader<'_>, size: usize) -> Option<()> {
    let (mut last, mut next) = (8i32, 8i32);
    for _ in 0..size {
        if next != 0 {
            next = (last + r.se()? + 256).rem_euclid(256);
        }
        if next != 0 {
            last = next;
        }
    }
    Some(())
}

pub(crate) fn parse_h264_sps(nal: &[u8]) -> Result<H264Sps> {
    let rbsp = unescape(nal.get(1..).ok_or(BAD)?);
    parse_h264_inner(&rbsp).ok_or(BAD)
}

fn parse_h264_inner(rbsp: &[u8]) -> Option<H264Sps> {
    let profile_idc = *rbsp.first()?;
    let constraint_flags = *rbsp.get(1)?;
    let level_idc = *rbsp.get(2)?;
    let mut r = BitReader::new(rbsp.get(3..)?);
    r.ue()?; // seq_parameter_set_id
    let (mut chroma, mut bd_l, mut bd_c) = (1u32, 8u32, 8u32);
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma = r.ue()?;
        if chroma > 3 {
            return None;
        }
        if chroma == 3 {
            r.bit()?; // separate_colour_plane_flag
        }
        bd_l = r.ue()? + 8;
        bd_c = r.ue()? + 8;
        if bd_l > 14 || bd_c > 14 {
            return None;
        }
        r.bit()?; // qpprime_y_zero_transform_bypass_flag
        if r.bit()? == 1 {
            let n = if chroma != 3 { 8 } else { 12 };
            for i in 0..n {
                if r.bit()? == 1 {
                    skip_scaling_list(&mut r, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.bit()?;
            r.se()?;
            r.se()?;
            let n = r.ue()?;
            if n > 255 {
                return None;
            }
            for _ in 0..n {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.bit()?; // gaps_in_frame_num_value_allowed_flag
    let w_mbs = r.ue()?.checked_add(1)?;
    let h_units = r.ue()?.checked_add(1)?;
    let frame_mbs_only = r.bit()?;
    if frame_mbs_only == 0 {
        r.bit()?; // mb_adaptive_frame_field_flag
    }
    r.bit()?; // direct_8x8_inference_flag
    let (mut cl, mut cr, mut ct, mut cb) = (0u32, 0u32, 0u32, 0u32);
    if r.bit()? == 1 {
        cl = r.ue()?;
        cr = r.ue()?;
        ct = r.ue()?;
        cb = r.ue()?;
    }
    let (sub_w, sub_h) = match chroma {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    let crop_x = sub_w;
    let crop_y = sub_h * (2 - frame_mbs_only);
    let width = (w_mbs * 16).checked_sub(crop_x * cl.checked_add(cr)?)?;
    let height = (h_units * 16 * (2 - frame_mbs_only)).checked_sub(crop_y * ct.checked_add(cb)?)?;
    if width == 0 || height == 0 || width > 65535 || height > 65535 {
        return None;
    }
    Some(H264Sps {
        profile_idc,
        constraint_flags,
        level_idc,
        chroma_format_idc: chroma as u8,
        bit_depth_luma: bd_l as u8,
        bit_depth_chroma: bd_c as u8,
        width,
        height,
    })
}

/// Facts from an H.265 SPS needed for `hvcC` and the sample entry.
#[derive(Clone, Debug)]
pub(crate) struct H265Sps {
    pub profile_space: u8,
    pub tier_flag: u8,
    pub profile_idc: u8,
    pub compat_flags: u32,
    pub constraint_flags: [u8; 6],
    pub level_idc: u8,
    pub chroma_format_idc: u8,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub num_temporal_layers: u8,
    pub temporal_id_nested: u8,
    pub width: u32,
    pub height: u32,
}

pub(crate) fn parse_h265_sps(nal: &[u8]) -> Result<H265Sps> {
    let rbsp = unescape(nal.get(2..).ok_or(BAD)?);
    parse_h265_inner(&rbsp).ok_or(BAD)
}

fn parse_h265_inner(rbsp: &[u8]) -> Option<H265Sps> {
    let mut r = BitReader::new(rbsp);
    r.bits(4)?; // sps_video_parameter_set_id
    let max_sub_minus1 = r.bits(3)?;
    let nested = r.bit()?;
    let profile_space = r.bits(2)? as u8;
    let tier_flag = r.bit()? as u8;
    let profile_idc = r.bits(5)? as u8;
    let compat_flags = r.bits(32)?;
    let mut constraint_flags = [0u8; 6];
    for b in &mut constraint_flags {
        *b = r.bits(8)? as u8;
    }
    let level_idc = r.bits(8)? as u8;
    let mut profile_present = [false; 8];
    let mut level_present = [false; 8];
    for i in 0..max_sub_minus1 as usize {
        profile_present[i] = r.bit()? == 1;
        level_present[i] = r.bit()? == 1;
    }
    if max_sub_minus1 > 0 {
        for _ in max_sub_minus1..8 {
            r.skip(2)?;
        }
    }
    for i in 0..max_sub_minus1 as usize {
        if profile_present[i] {
            r.skip(88)?;
        }
        if level_present[i] {
            r.skip(8)?;
        }
    }
    r.ue()?; // sps_seq_parameter_set_id
    let chroma = r.ue()?;
    if chroma > 3 {
        return None;
    }
    if chroma == 3 {
        r.bit()?;
    }
    let w = r.ue()?;
    let h = r.ue()?;
    let (mut cl, mut cr, mut ct, mut cb) = (0u32, 0u32, 0u32, 0u32);
    if r.bit()? == 1 {
        cl = r.ue()?;
        cr = r.ue()?;
        ct = r.ue()?;
        cb = r.ue()?;
    }
    let bd_l = r.ue()? + 8;
    let bd_c = r.ue()? + 8;
    if bd_l > 16 || bd_c > 16 {
        return None;
    }
    let (sub_w, sub_h) = match chroma {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    let width = w.checked_sub(sub_w * cl.checked_add(cr)?)?;
    let height = h.checked_sub(sub_h * ct.checked_add(cb)?)?;
    if width == 0 || height == 0 || width > 65535 || height > 65535 {
        return None;
    }
    Some(H265Sps {
        profile_space,
        tier_flag,
        profile_idc,
        compat_flags,
        constraint_flags,
        level_idc,
        chroma_format_idc: chroma as u8,
        bit_depth_luma: bd_l as u8,
        bit_depth_chroma: bd_c as u8,
        num_temporal_layers: max_sub_minus1 as u8 + 1,
        temporal_id_nested: nested as u8,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let d = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 0, 1, 0x65, 9,
        ];
        let n = split_annexb(&d).unwrap();
        assert_eq!(n, [&[0x67u8, 1, 2][..], &[0x68, 3], &[0x65, 9]]);
    }

    #[test]
    fn rejects_buffers_without_start_code() {
        assert!(matches!(
            split_annexb(&[1, 2, 3, 4]),
            Err(Error::InvalidAnnexB)
        ));
    }

    #[test]
    fn classifies() {
        assert_eq!(classify(VideoCodec::H264, &[0x67]), NalKind::Sps);
        assert_eq!(classify(VideoCodec::H264, &[0x09]), NalKind::Drop);
        assert_eq!(classify(VideoCodec::H264, &[0x65]), NalKind::Keep);
        assert_eq!(classify(VideoCodec::H265, &[0x40, 1]), NalKind::Vps);
        assert_eq!(classify(VideoCodec::H265, &[0x46, 1]), NalKind::Drop);
        assert_eq!(classify(VideoCodec::H265, &[0x26, 1]), NalKind::Keep);
    }
}
