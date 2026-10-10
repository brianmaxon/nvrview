//! H.264 sequence parameter set parsing (ITU-T H.264 7.3.2.1.1, E.1.1), as far
//! as the picture size, cropping, and VUI video signal type.

use crate::bits::{BitReader, ParseError, Result};
use crate::geometry::{Rect, Size};
use crate::vui;
use crate::{SequenceInfo, nal::Codec};

/// Upper bound on width or height in pixels accepted from a stream.
pub(crate) const MAX_DIMENSION: u32 = 16_384;

/// Parses an H.264 SPS NAL unit (header byte included, no start code).
pub fn parse_sps(nal: &[u8]) -> Result<SequenceInfo> {
    let (&header, payload) = nal.split_first().ok_or(ParseError::Truncated)?;
    if header & 0x1F != 7 {
        return Err(ParseError::Missing("H.264 SPS"));
    }
    let mut r = BitReader::new(payload);
    let profile_idc = r.u8(8)?;
    r.skip(8)?; // constraint_set0..5_flag, reserved_zero_2bits
    let level_idc = r.u8(8)?;
    r.ue_max(31, "seq_parameter_set_id")?;

    let mut chroma_format_idc = 1;
    let mut separate_colour_plane = false;
    let mut bit_depth_luma = 8;
    let mut bit_depth_chroma = 8;
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format_idc = r.ue_max(3, "chroma_format_idc")?;
        if chroma_format_idc == 3 {
            separate_colour_plane = r.flag()?;
        }
        bit_depth_luma = 8 + r.ue_max(6, "bit_depth_luma_minus8")?;
        bit_depth_chroma = 8 + r.ue_max(6, "bit_depth_chroma_minus8")?;
        r.skip(1)?; // qpprime_y_zero_transform_bypass_flag
        if r.flag()? {
            // seq_scaling_matrix_present_flag
            let lists = if chroma_format_idc == 3 { 12 } else { 8 };
            for i in 0..lists {
                if r.flag()? {
                    skip_scaling_list(&mut r, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    r.ue_max(12, "log2_max_frame_num_minus4")?;
    match r.ue_max(2, "pic_order_cnt_type")? {
        0 => {
            r.ue_max(12, "log2_max_pic_order_cnt_lsb_minus4")?;
        }
        1 => {
            r.skip(1)?; // delta_pic_order_always_zero_flag
            r.se()?; // offset_for_non_ref_pic
            r.se()?; // offset_for_top_to_bottom_field
            let n = r.ue_max(255, "num_ref_frames_in_pic_order_cnt_cycle")?;
            for _ in 0..n {
                r.se()?; // offset_for_ref_frame
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.skip(1)?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = 1 + r.ue_max(MAX_DIMENSION / 16 - 1, "pic_width_in_mbs_minus1")?;
    let height_map_units =
        1 + r.ue_max(MAX_DIMENSION / 16 - 1, "pic_height_in_map_units_minus1")?;
    let frame_mbs_only = r.flag()?;
    if !frame_mbs_only {
        r.skip(1)?; // mb_adaptive_frame_field_flag
    }
    r.skip(1)?; // direct_8x8_inference_flag

    let width = width_mbs * 16;
    let height = height_map_units * 16 * if frame_mbs_only { 1 } else { 2 };
    if height > MAX_DIMENSION {
        return Err(ParseError::OutOfRange("picture height"));
    }

    let mut visible = Rect::new(0, 0, width, height);
    if r.flag()? {
        // frame_cropping_flag; offsets in units of CropUnitX / CropUnitY.
        let chroma_array_type = if separate_colour_plane {
            0
        } else {
            chroma_format_idc
        };
        let (sub_w, sub_h) = match chroma_array_type {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        let unit_x = sub_w;
        let unit_y = sub_h * if frame_mbs_only { 1 } else { 2 };
        let left = r.ue_max(MAX_DIMENSION, "frame_crop_left_offset")?;
        let right = r.ue_max(MAX_DIMENSION, "frame_crop_right_offset")?;
        let top = r.ue_max(MAX_DIMENSION, "frame_crop_top_offset")?;
        let bottom = r.ue_max(MAX_DIMENSION, "frame_crop_bottom_offset")?;
        visible = crop(width, height, unit_x, unit_y, left, right, top, bottom)?;
    }

    let signal = if r.flag()? {
        // vui_parameters_present_flag. A truncated or odd VUI only loses the
        // color information; the size above is still good.
        vui::video_signal(&mut r).ok().flatten()
    } else {
        None
    };

    Ok(SequenceInfo {
        codec: Codec::H264,
        profile_idc,
        level_idc,
        chroma_format_idc: chroma_format_idc as u8,
        bit_depth_luma: bit_depth_luma as u8,
        bit_depth_chroma: bit_depth_chroma as u8,
        coded_size: Size::new(width, height),
        visible_rect: visible,
        signal,
    })
}

/// Applies a crop given in units, rejecting crops that leave nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn crop(
    width: u32,
    height: u32,
    unit_x: u32,
    unit_y: u32,
    left: u32,
    right: u32,
    top: u32,
    bottom: u32,
) -> Result<Rect> {
    let l = u64::from(left) * u64::from(unit_x);
    let r = u64::from(right) * u64::from(unit_x);
    let t = u64::from(top) * u64::from(unit_y);
    let b = u64::from(bottom) * u64::from(unit_y);
    if l + r >= u64::from(width) || t + b >= u64::from(height) {
        return Err(ParseError::OutOfRange("cropping window"));
    }
    Ok(Rect::new(
        l as u32,
        t as u32,
        width - (l + r) as u32,
        height - (t + b) as u32,
    ))
}

/// Reads past one `scaling_list()` (7.3.2.1.1.1) of `size` entries. Only the
/// syntax matters here: a list stops early once a delta sets the next scale
/// to zero.
fn skip_scaling_list(r: &mut BitReader<'_>, size: usize) -> Result<()> {
    let mut last: i32 = 8;
    let mut next: i32 = 8;
    for _ in 0..size {
        if next != 0 {
            let delta = r.se()?;
            if !(-128..=127).contains(&delta) {
                return Err(ParseError::OutOfRange("delta_scale"));
            }
            next = (last + delta + 256).rem_euclid(256);
        }
        if next != 0 {
            last = next;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bits::writer::BitWriter;

    struct Sps {
        profile: u8,
        chroma: u32,
        scaling: bool,
        poc_type: u32,
        width_mbs: u32,
        height_units: u32,
        frame_mbs_only: bool,
        crop: Option<[u32; 4]>,
        vui: Option<(bool, Option<[u8; 3]>)>,
    }

    fn build(s: &Sps) -> Vec<u8> {
        let mut w = BitWriter::default();
        w.bits(8, u64::from(s.profile)).bits(8, 0).bits(8, 40).ue(0);
        if s.profile >= 100 {
            w.ue(s.chroma);
            if s.chroma == 3 {
                w.bit(false);
            }
            w.ue(0).ue(0).bit(false);
            w.bit(s.scaling);
            if s.scaling {
                let lists = if s.chroma == 3 { 12 } else { 8 };
                for i in 0..lists {
                    match i % 3 {
                        // Present, full list of deltas that never hit zero.
                        0 => {
                            w.bit(true);
                            let n = if i < 6 { 16 } else { 64 };
                            for j in 0..n {
                                w.se(if j % 2 == 0 { 3 } else { -2 });
                            }
                        }
                        // Present, ends early: the first delta makes next 0.
                        1 => {
                            w.bit(true).se(-8);
                        }
                        _ => {
                            w.bit(false);
                        }
                    }
                }
            }
        }
        w.ue(0).ue(s.poc_type);
        match s.poc_type {
            0 => {
                w.ue(2);
            }
            1 => {
                w.bit(false).se(-1).se(2).ue(3).se(1).se(-1).se(5);
            }
            _ => {}
        }
        w.ue(4)
            .bit(false)
            .ue(s.width_mbs - 1)
            .ue(s.height_units - 1);
        w.bit(s.frame_mbs_only);
        if !s.frame_mbs_only {
            w.bit(true);
        }
        w.bit(true);
        match s.crop {
            Some(c) => {
                w.bit(true);
                for v in c {
                    w.ue(v);
                }
            }
            None => {
                w.bit(false);
            }
        }
        match s.vui {
            Some((full, desc)) => {
                w.bit(true);
                // aspect_ratio_info: Extended_SAR with explicit size.
                w.bit(true).bits(8, 255).bits(16, 1).bits(16, 1);
                // overscan_info_present + overscan_appropriate.
                w.bit(true).bit(false);
                // video_signal_type_present
                w.bit(true).bits(3, 5).bit(full);
                match desc {
                    Some([p, t, m]) => {
                        w.bit(true)
                            .bits(8, u64::from(p))
                            .bits(8, u64::from(t))
                            .bits(8, u64::from(m));
                    }
                    None => {
                        w.bit(false);
                    }
                }
                // The rest of the VUI is not read.
                w.bit(false).bit(false);
            }
            None => {
                w.bit(false);
            }
        }
        let mut nal = vec![0x67];
        nal.extend(w.finish());
        nal
    }

    #[test]
    fn baseline_1080p_cropped() {
        let nal = build(&Sps {
            profile: 66,
            chroma: 1,
            scaling: false,
            poc_type: 2,
            width_mbs: 120,
            height_units: 68,
            frame_mbs_only: true,
            crop: Some([0, 0, 0, 4]),
            vui: None,
        });
        let s = parse_sps(&nal).unwrap();
        assert_eq!(s.coded_size, Size::new(1920, 1088));
        assert_eq!(s.visible_rect, Rect::new(0, 0, 1920, 1080));
        assert_eq!(s.signal, None);
    }

    #[test]
    fn high_profile_with_scaling_lists_and_vui() {
        for chroma in [1, 2, 3] {
            let nal = build(&Sps {
                profile: if chroma == 1 { 100 } else { 244 },
                chroma,
                scaling: true,
                poc_type: 1,
                width_mbs: 40,
                height_units: 23,
                frame_mbs_only: true,
                crop: Some([1, 2, 0, 3]),
                vui: Some((true, Some([1, 1, 1]))),
            });
            let s = parse_sps(&nal).unwrap();
            let (ux, uy) = match chroma {
                1 => (2, 2),
                2 => (2, 1),
                _ => (1, 1),
            };
            assert_eq!(s.coded_size, Size::new(640, 368), "chroma {chroma}");
            assert_eq!(
                s.visible_rect,
                Rect::new(ux, 0, 640 - 3 * ux, 368 - 3 * uy),
                "chroma {chroma}"
            );
            assert_eq!(s.chroma_format_idc as u32, chroma);
            let sig = s.signal.unwrap();
            assert!(sig.full_range);
            assert_eq!(sig.description.unwrap().matrix_coefficients, 1);
        }
    }

    #[test]
    fn interlaced_doubles_height_and_crop_unit() {
        let nal = build(&Sps {
            profile: 77,
            chroma: 1,
            scaling: false,
            poc_type: 0,
            width_mbs: 45,
            height_units: 18,
            frame_mbs_only: false,
            crop: Some([0, 0, 0, 2]),
            vui: Some((false, None)),
        });
        let s = parse_sps(&nal).unwrap();
        assert_eq!(s.coded_size, Size::new(720, 576));
        assert_eq!(s.visible_rect, Rect::new(0, 0, 720, 568));
        assert_eq!(
            s.signal,
            Some(crate::VideoSignal {
                full_range: false,
                description: None
            })
        );
    }

    #[test]
    fn bad_crop_and_truncation_are_errors() {
        let nal = build(&Sps {
            profile: 66,
            chroma: 1,
            scaling: false,
            poc_type: 2,
            width_mbs: 2,
            height_units: 2,
            frame_mbs_only: true,
            crop: Some([8, 8, 0, 0]),
            vui: None,
        });
        assert!(parse_sps(&nal).is_err());
        let good = build(&Sps {
            profile: 100,
            chroma: 1,
            scaling: true,
            poc_type: 0,
            width_mbs: 20,
            height_units: 15,
            frame_mbs_only: true,
            crop: None,
            vui: Some((false, Some([1, 1, 1]))),
        });
        // Every truncation either errors or (once past the size fields)
        // succeeds; none panic.
        for n in 0..good.len() {
            let _ = parse_sps(&good[..n]);
        }
        assert!(parse_sps(&[]).is_err());
        assert!(parse_sps(&[0x68, 0x42]).is_err());
    }
}
