//! The start of `vui_parameters()`, which H.264 (E.1.1) and H.265 (E.2.1)
//! share up to and including the color description.

use crate::bits::{BitReader, Result};
use crate::color::{ColorDescription, VideoSignal};

/// Reads the VUI up to the color description. Returns `None` when no video
/// signal type is present. The reader is left in the middle of the VUI.
pub(crate) fn video_signal(r: &mut BitReader<'_>) -> Result<Option<VideoSignal>> {
    if r.flag()? {
        // aspect_ratio_info_present_flag
        if r.u8(8)? == 255 {
            // aspect_ratio_idc == Extended_SAR: sar_width, sar_height
            r.skip(32)?;
        }
    }
    if r.flag()? {
        // overscan_info_present_flag
        r.skip(1)?; // overscan_appropriate_flag
    }
    if !r.flag()? {
        // video_signal_type_present_flag
        return Ok(None);
    }
    r.skip(3)?; // video_format
    let full_range = r.flag()?;
    let description = if r.flag()? {
        Some(ColorDescription {
            primaries: r.u8(8)?,
            transfer: r.u8(8)?,
            matrix_coefficients: r.u8(8)?,
        })
    } else {
        None
    };
    Ok(Some(VideoSignal {
        full_range,
        description,
    }))
}
