//! H.265 parameter set parsing, as far as the picture size, conformance
//! window, and VUI video signal type. Written from the syntax tables of ITU-T
//! H.265 clause 7.3: the VPS and SPS RBSP syntax, profile_tier_level (7.3.3),
//! scaling_list_data (7.3.4), st_ref_pic_set (7.3.7), and VUI (Annex E).

use crate::bits::{BitReader, ParseError, Result};
use crate::geometry::{Rect, Size};
use crate::h264::{MAX_DIMENSION, crop};
use crate::vui;
use crate::{SequenceInfo, nal::Codec};

/// Most short-term reference picture sets an SPS may define (7.4.3.2.1).
const MAX_ST_RPS: usize = 64;
/// Bound on negative or positive pictures in one set (at most
/// `sps_max_dec_pic_buffering_minus1`, itself below 16).
const MAX_RPS_PICS: u32 = 16;

/// The fields of an H.265 VPS header this crate checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VpsInfo {
    /// `vps_video_parameter_set_id`.
    pub id: u8,
    /// `vps_max_sub_layers_minus1 + 1`.
    pub max_sub_layers: u8,
}

/// Parses the start of an H.265 VPS NAL unit (header included). Only the
/// identity and sub-layer count are read; the VPS carries nothing about size
/// or color, but its presence is required to configure a decoder.
pub fn parse_vps(nal: &[u8]) -> Result<VpsInfo> {
    let payload = check_header(nal, 32, "H.265 VPS")?;
    let mut r = BitReader::new(payload);
    let id = r.u8(4)?;
    r.skip(2)?; // vps_base_layer_internal_flag, vps_base_layer_available_flag
    r.skip(6)?; // vps_max_layers_minus1
    let max_sub_layers = r.u8(3)? + 1;
    if max_sub_layers > 7 {
        return Err(ParseError::OutOfRange("vps_max_sub_layers_minus1"));
    }
    r.skip(1)?; // vps_temporal_id_nesting_flag
    if r.bits(16)? != 0xFFFF {
        return Err(ParseError::OutOfRange("vps_reserved_0xffff_16bits"));
    }
    skip_profile_tier_level(&mut r, max_sub_layers - 1)?;
    Ok(VpsInfo { id, max_sub_layers })
}

fn check_header<'a>(nal: &'a [u8], want_type: u8, what: &'static str) -> Result<&'a [u8]> {
    let [h0, h1, ..] = *nal else {
        return Err(ParseError::Truncated);
    };
    if (h0 >> 1) & 0x3F != want_type {
        return Err(ParseError::Missing(what));
    }
    let layer = ((h0 & 1) << 5) | (h1 >> 3);
    if layer != 0 {
        return Err(ParseError::Unsupported("nuh_layer_id other than 0"));
    }
    Ok(&nal[2..])
}

/// Parses an H.265 SPS NAL unit (header included, no start code).
pub fn parse_sps(nal: &[u8]) -> Result<SequenceInfo> {
    let payload = check_header(nal, 33, "H.265 SPS")?;
    let mut r = BitReader::new(payload);
    r.skip(4)?; // sps_video_parameter_set_id
    let max_sub_layers_minus1 = r.u8(3)?;
    if max_sub_layers_minus1 > 6 {
        return Err(ParseError::OutOfRange("sps_max_sub_layers_minus1"));
    }
    r.skip(1)?; // sps_temporal_id_nesting_flag
    let (profile_idc, level_idc) = skip_profile_tier_level(&mut r, max_sub_layers_minus1)?;
    r.ue_max(15, "sps_seq_parameter_set_id")?;
    let chroma_format_idc = r.ue_max(3, "chroma_format_idc")?;
    let separate_colour_plane = chroma_format_idc == 3 && r.flag()?;
    let width = r.ue_max(MAX_DIMENSION, "pic_width_in_luma_samples")?;
    let height = r.ue_max(MAX_DIMENSION, "pic_height_in_luma_samples")?;
    if width == 0 || height == 0 {
        return Err(ParseError::OutOfRange("picture size"));
    }
    let mut visible = Rect::new(0, 0, width, height);
    if r.flag()? {
        // conformance_window_flag; offsets in chroma sample units.
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
        let left = r.ue_max(MAX_DIMENSION, "conf_win_left_offset")?;
        let right = r.ue_max(MAX_DIMENSION, "conf_win_right_offset")?;
        let top = r.ue_max(MAX_DIMENSION, "conf_win_top_offset")?;
        let bottom = r.ue_max(MAX_DIMENSION, "conf_win_bottom_offset")?;
        visible = crop(width, height, sub_w, sub_h, left, right, top, bottom)?;
    }
    let bit_depth_luma = 8 + r.ue_max(8, "bit_depth_luma_minus8")?;
    let bit_depth_chroma = 8 + r.ue_max(8, "bit_depth_chroma_minus8")?;
    let log2_max_poc_lsb = 4 + r.ue_max(12, "log2_max_pic_order_cnt_lsb_minus4")?;
    let ordering_all = r.flag()?; // sps_sub_layer_ordering_info_present_flag
    let first = if ordering_all {
        0
    } else {
        max_sub_layers_minus1
    };
    for _ in first..=max_sub_layers_minus1 {
        r.ue()?; // sps_max_dec_pic_buffering_minus1
        r.ue()?; // sps_max_num_reorder_pics
        r.ue()?; // sps_max_latency_increase_plus1
    }
    r.ue()?; // log2_min_luma_coding_block_size_minus3
    r.ue()?; // log2_diff_max_min_luma_coding_block_size
    r.ue()?; // log2_min_luma_transform_block_size_minus2
    r.ue()?; // log2_diff_max_min_luma_transform_block_size
    r.ue()?; // max_transform_hierarchy_depth_inter
    r.ue()?; // max_transform_hierarchy_depth_intra
    if r.flag()? && r.flag()? {
        // scaling_list_enabled_flag && sps_scaling_list_data_present_flag
        skip_scaling_list_data(&mut r)?;
    }
    r.skip(2)?; // amp_enabled_flag, sample_adaptive_offset_enabled_flag
    if r.flag()? {
        // pcm_enabled_flag
        r.skip(8)?; // pcm_sample_bit_depth_luma_minus1, _chroma_minus1
        r.ue()?; // log2_min_pcm_luma_coding_block_size_minus3
        r.ue()?; // log2_diff_max_min_pcm_luma_coding_block_size
        r.skip(1)?; // pcm_loop_filter_disabled_flag
    }
    let num_sets = r.ue_max(MAX_ST_RPS as u32, "num_short_term_ref_pic_sets")? as usize;
    // NumDeltaPocs of each set parsed so far, needed by inter prediction.
    let mut num_delta_pocs = [0u32; MAX_ST_RPS];
    for idx in 0..num_sets {
        num_delta_pocs[idx] = skip_st_ref_pic_set(&mut r, idx, &num_delta_pocs)?;
    }
    if r.flag()? {
        // long_term_ref_pics_present_flag
        let n = r.ue_max(32, "num_long_term_ref_pics_sps")?;
        for _ in 0..n {
            r.skip(log2_max_poc_lsb)?; // lt_ref_pic_poc_lsb_sps
            r.skip(1)?; // used_by_curr_pic_lt_sps_flag
        }
    }
    r.skip(2)?; // sps_temporal_mvp_enabled_flag, strong_intra_smoothing_enabled_flag
    let signal = if r.flag()? {
        // vui_parameters_present_flag; a bad VUI only loses color.
        vui::video_signal(&mut r).ok().flatten()
    } else {
        None
    };

    Ok(SequenceInfo {
        codec: Codec::H265,
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

/// Reads past `profile_tier_level(1, max_sub_layers_minus1)` (7.3.3) and
/// returns `(general_profile_idc, general_level_idc)`.
fn skip_profile_tier_level(r: &mut BitReader<'_>, max_sub_layers_minus1: u8) -> Result<(u8, u8)> {
    r.skip(3)?; // general_profile_space, general_tier_flag
    let profile_idc = r.u8(5)?;
    // general_profile_compatibility_flag[32], four source flags, 43 bits of
    // constraint flags, and one more flag: 80 bits.
    r.skip(32 + 4 + 43 + 1)?;
    let level_idc = r.u8(8)?;
    let n = usize::from(max_sub_layers_minus1);
    let mut profile_present = [false; 8];
    let mut level_present = [false; 8];
    for i in 0..n {
        profile_present[i] = r.flag()?;
        level_present[i] = r.flag()?;
    }
    if n > 0 {
        for _ in n..8 {
            r.skip(2)?; // reserved_zero_2bits
        }
    }
    for i in 0..n {
        if profile_present[i] {
            r.skip(88)?; // sub_layer profile space .. flags, as for general
        }
        if level_present[i] {
            r.skip(8)?; // sub_layer_level_idc
        }
    }
    Ok((profile_idc, level_idc))
}

/// Reads past `scaling_list_data()` (7.3.4).
fn skip_scaling_list_data(r: &mut BitReader<'_>) -> Result<()> {
    for size_id in 0..4u32 {
        let step = if size_id == 3 { 3 } else { 1 };
        let mut matrix_id = 0;
        while matrix_id < 6 {
            if !r.flag()? {
                // scaling_list_pred_mode_flag == 0
                r.ue_max(matrix_id, "scaling_list_pred_matrix_id_delta")?;
            } else {
                let coefs = 64.min(1u32 << (4 + (size_id << 1)));
                if size_id > 1 {
                    let dc = r.se()?;
                    if !(-7..=247).contains(&dc) {
                        return Err(ParseError::OutOfRange("scaling_list_dc_coef_minus8"));
                    }
                }
                for _ in 0..coefs {
                    let d = r.se()?;
                    if !(-128..=127).contains(&d) {
                        return Err(ParseError::OutOfRange("scaling_list_delta_coef"));
                    }
                }
            }
            matrix_id += step;
        }
    }
    Ok(())
}

/// Reads past `st_ref_pic_set(idx)` (7.3.7) inside an SPS and returns its
/// `NumDeltaPocs`.
fn skip_st_ref_pic_set(r: &mut BitReader<'_>, idx: usize, previous: &[u32]) -> Result<u32> {
    let inter = idx != 0 && r.flag()?; // inter_ref_pic_set_prediction_flag
    if inter {
        // In an SPS the reference is always the previous set
        // (delta_idx_minus1 is only sent in slice headers).
        r.skip(1)?; // delta_rps_sign
        r.ue_max(1 << 15, "abs_delta_rps_minus1")?;
        let ref_pocs = previous[idx - 1];
        let mut count = 0;
        for _ in 0..=ref_pocs {
            let used = r.flag()?; // used_by_curr_pic_flag
            // use_delta_flag is inferred to be 1 when not present.
            let use_delta = used || r.flag()?;
            if use_delta {
                count += 1;
            }
        }
        if count > 2 * MAX_RPS_PICS {
            return Err(ParseError::OutOfRange("NumDeltaPocs"));
        }
        Ok(count)
    } else {
        let negative = r.ue_max(MAX_RPS_PICS, "num_negative_pics")?;
        let positive = r.ue_max(MAX_RPS_PICS, "num_positive_pics")?;
        for _ in 0..negative + positive {
            r.ue_max(1 << 15, "delta_poc_minus1")?;
            r.skip(1)?; // used_by_curr_pic_flag
        }
        Ok(negative + positive)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::bits::writer::BitWriter;

    pub(crate) struct Sps {
        pub sub_layers_minus1: u8,
        pub sub_layer_profile_present: bool,
        pub chroma: u32,
        pub width: u32,
        pub height: u32,
        pub conf: Option<[u32; 4]>,
        pub scaling: bool,
        pub pcm: bool,
        pub long_term: bool,
        pub vui: Option<(bool, Option<[u8; 3]>)>,
    }

    fn ptl(w: &mut BitWriter, n: u8, sub_present: bool) {
        w.bits(2, 0).bit(false).bits(5, 1);
        w.bits(32, 0x6000_0000)
            .bits(4, 0b1001)
            .bits(43, 0)
            .bit(false);
        w.bits(8, 93);
        for _ in 0..n {
            w.bit(sub_present).bit(sub_present);
        }
        if n > 0 {
            for _ in n..8 {
                w.bits(2, 0);
            }
        }
        for _ in 0..n {
            if sub_present {
                w.bits(88, 0x00AB_CDEF);
                w.bits(8, 90);
            }
        }
    }

    pub(crate) fn build(s: &Sps) -> Vec<u8> {
        let mut w = BitWriter::default();
        w.bits(4, 0)
            .bits(3, u64::from(s.sub_layers_minus1))
            .bit(true);
        ptl(&mut w, s.sub_layers_minus1, s.sub_layer_profile_present);
        w.ue(0).ue(s.chroma);
        if s.chroma == 3 {
            w.bit(false);
        }
        w.ue(s.width).ue(s.height);
        match s.conf {
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
        w.ue(0).ue(0).ue(4); // bit depths 8, log2_max_poc_lsb 8
        w.bit(true);
        for _ in 0..=s.sub_layers_minus1 {
            w.ue(4).ue(0).ue(0);
        }
        w.ue(0).ue(3).ue(0).ue(3).ue(2).ue(2);
        w.bit(s.scaling);
        if s.scaling {
            w.bit(true);
            for size_id in 0..4u32 {
                let step = if size_id == 3 { 3 } else { 1 };
                let mut m = 0;
                while m < 6 {
                    if m % 2 == 0 {
                        w.bit(true);
                        if size_id > 1 {
                            w.se(8);
                        }
                        for k in 0..64.min(1u32 << (4 + (size_id << 1))) {
                            w.se(if k % 2 == 0 { 5 } else { -5 });
                        }
                    } else {
                        w.bit(false).ue(1);
                    }
                    m += step;
                }
            }
        }
        w.bit(true).bit(true); // amp, sao
        w.bit(s.pcm);
        if s.pcm {
            w.bits(4, 7).bits(4, 7).ue(0).ue(1).bit(false);
        }
        // Three short-term sets: explicit, inter-predicted, explicit.
        w.ue(3);
        w.ue(2).ue(1); // set 0: two negative, one positive
        for d in [0, 1, 0] {
            w.ue(d).bit(true);
        }
        w.bit(true).bit(false).ue(0); // set 1: predicted from set 0
        // NumDeltaPocs[0] = 3, so four entries: used, unused+use, unused+skip, used.
        w.bit(true)
            .bit(false)
            .bit(true)
            .bit(false)
            .bit(false)
            .bit(true);
        w.bit(false).ue(1).ue(0).ue(3).bit(false); // set 2 (after set 1 with 3 pocs)
        w.bit(s.long_term);
        if s.long_term {
            w.ue(2).bits(8, 17).bit(true).bits(8, 200).bit(false);
        }
        w.bit(true).bit(true);
        match s.vui {
            Some((full, desc)) => {
                w.bit(true);
                w.bit(false).bit(false); // no aspect ratio, no overscan
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
                w.bit(false).bit(false).bit(false).bit(false);
            }
            None => {
                w.bit(false);
            }
        }
        w.bit(false); // sps_extension_present_flag
        let mut nal = vec![33 << 1, 1];
        nal.extend(w.finish());
        nal
    }

    #[test]
    fn plain_1080p() {
        let nal = build(&Sps {
            sub_layers_minus1: 0,
            sub_layer_profile_present: false,
            chroma: 1,
            width: 1920,
            height: 1088,
            conf: Some([0, 0, 0, 4]),
            scaling: false,
            pcm: false,
            long_term: false,
            vui: None,
        });
        let s = parse_sps(&nal).unwrap();
        assert_eq!(s.coded_size, Size::new(1920, 1088));
        assert_eq!(s.visible_rect, Rect::new(0, 0, 1920, 1080));
        assert_eq!(s.profile_idc, 1);
        assert_eq!(s.level_idc, 93);
        assert!(s.signal.is_none());
    }

    #[test]
    fn sub_layers_scaling_pcm_long_term_and_vui() {
        for sub in 0..=6u8 {
            for present in [false, true] {
                for chroma in [0, 1, 2, 3] {
                    let nal = build(&Sps {
                        sub_layers_minus1: sub,
                        sub_layer_profile_present: present,
                        chroma,
                        width: 640,
                        height: 360,
                        conf: Some([2, 1, 1, 0]),
                        scaling: true,
                        pcm: true,
                        long_term: true,
                        vui: Some((true, Some([9, 16, 9]))),
                    });
                    let s = parse_sps(&nal).unwrap_or_else(|e| panic!("{sub} {present} {e}"));
                    let (ux, uy) = match chroma {
                        1 => (2, 2),
                        2 => (2, 1),
                        _ => (1, 1),
                    };
                    assert_eq!(s.coded_size, Size::new(640, 360));
                    assert_eq!(
                        s.visible_rect,
                        Rect::new(2 * ux, uy, 640 - 3 * ux, 360 - uy)
                    );
                    let sig = s.signal.unwrap();
                    assert!(sig.full_range);
                    assert_eq!(
                        sig.description,
                        Some(crate::ColorDescription {
                            primaries: 9,
                            transfer: 16,
                            matrix_coefficients: 9
                        })
                    );
                }
            }
        }
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        let good = build(&Sps {
            sub_layers_minus1: 2,
            sub_layer_profile_present: true,
            chroma: 1,
            width: 320,
            height: 240,
            conf: None,
            scaling: true,
            pcm: false,
            long_term: false,
            vui: Some((false, None)),
        });
        assert!(parse_sps(&good).is_ok());
        for n in 0..good.len() {
            let _ = parse_sps(&good[..n]);
        }
        let mut bad = good.clone();
        for i in 2..bad.len() {
            bad[i] ^= 0x5A;
            let _ = parse_sps(&bad);
        }
        assert!(parse_sps(&[0x40, 0x01, 0x0C]).is_err()); // a VPS header
        assert!(parse_vps(&[0x40]).is_err());
    }

    #[test]
    fn vps_header() {
        let mut w = BitWriter::default();
        w.bits(4, 3)
            .bits(2, 3)
            .bits(6, 0)
            .bits(3, 1)
            .bit(true)
            .bits(16, 0xFFFF);
        ptl(&mut w, 1, true);
        let mut nal = vec![32 << 1, 1];
        nal.extend(w.finish());
        assert_eq!(
            parse_vps(&nal).unwrap(),
            VpsInfo {
                id: 3,
                max_sub_layers: 2
            }
        );
    }
}
