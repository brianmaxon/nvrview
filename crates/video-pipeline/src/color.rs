//! YCbCr to RGB conversion, decided once here so every platform's shader only
//! multiplies.
//!
//! The stream's VUI (video usability information) may carry a video signal
//! type: a full-range flag and, optionally, a color description with
//! ITU-T H.273 code points for primaries, transfer characteristics, and
//! matrix coefficients. Only the matrix coefficients and the range affect the
//! YCbCr to RGB step; primaries and transfer are passed through for platforms
//! that want to tag their output.
//!
//! Without a color description the defaults follow `docs/RENDERING.md`:
//! BT.709 for pictures 720 lines high or more, BT.601 below, video range.

/// H.273 code point meaning "unspecified".
pub const UNSPECIFIED: u8 = 2;

/// The YCbCr matrix family used for conversion.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorMatrix {
    /// ITU-R BT.601 (Kr = 0.299, Kb = 0.114).
    Bt601 = 0,
    /// ITU-R BT.709 (Kr = 0.2126, Kb = 0.0722).
    Bt709 = 1,
    /// ITU-R BT.2020 non-constant luminance (Kr = 0.2627, Kb = 0.0593).
    Bt2020 = 2,
}

impl ColorMatrix {
    /// The luma weights `(Kr, Kb)` of this matrix.
    pub const fn weights(self) -> (f64, f64) {
        match self {
            ColorMatrix::Bt601 => (0.299, 0.114),
            ColorMatrix::Bt709 => (0.2126, 0.0722),
            ColorMatrix::Bt2020 => (0.2627, 0.0593),
        }
    }

    /// Maps an H.273 `matrix_coefficients` code point to the matrix used here,
    /// or `None` when the code is unspecified or not supported (identity/GBR,
    /// YCgCo, ...), in which case the size-based default applies.
    ///
    /// Close relatives are folded in: FCC (4) and BT.470 System B/G (5) and
    /// SMPTE 170M (6) use BT.601; SMPTE 240M (7) uses BT.709; BT.2020
    /// constant luminance (10) is approximated by the non-constant form.
    pub const fn from_h273(matrix_coefficients: u8) -> Option<ColorMatrix> {
        match matrix_coefficients {
            1 | 7 => Some(ColorMatrix::Bt709),
            4..=6 => Some(ColorMatrix::Bt601),
            9 | 10 => Some(ColorMatrix::Bt2020),
            _ => None,
        }
    }
}

/// The video signal type as read from a sequence parameter set's VUI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct VideoSignal {
    /// `video_full_range_flag`.
    pub full_range: bool,
    /// Present when `colour_description_present_flag` was set.
    pub description: Option<ColorDescription>,
}

/// H.273 code points from a VUI color description.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ColorDescription {
    /// `colour_primaries`.
    pub primaries: u8,
    /// `transfer_characteristics`.
    pub transfer: u8,
    /// `matrix_coeffs` / `matrix_coefficients`.
    pub matrix_coefficients: u8,
}

/// A row-major 3x4 matrix taking `(Y, Cb, Cr, 1)`, each sample an 8-bit value
/// normalized to `0..=1` (as a GPU samples a `unorm8` texture), to
/// gamma-encoded (non-linear) `(R, G, B)` in `0..=1`. The fourth column holds the range
/// offsets, so a shader computes `rgb = M * vec4(y, cb, cr, 1)`.
pub type YcbcrToRgb = [[f32; 4]; 3];

/// Everything a platform needs to convert one stream's pictures to RGB.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorInfo {
    /// The matrix family the conversion uses.
    pub matrix: ColorMatrix,
    /// Full range (`0..=255`) rather than video range (`16..=235` luma,
    /// `16..=240` chroma).
    pub full_range: bool,
    /// True when `matrix` came from the stream's color description, false when
    /// it is the size-based default.
    pub from_stream: bool,
    /// Raw H.273 code points as signaled (all [`UNSPECIFIED`] when absent).
    pub description: ColorDescription,
    /// The ready-to-use conversion; see [`YcbcrToRgb`].
    pub ycbcr_to_rgb: YcbcrToRgb,
}

impl ColorInfo {
    /// Conversion for an explicit matrix and range.
    pub fn new(matrix: ColorMatrix, full_range: bool) -> ColorInfo {
        ColorInfo {
            matrix,
            full_range,
            from_stream: false,
            description: ColorDescription {
                primaries: UNSPECIFIED,
                transfer: UNSPECIFIED,
                matrix_coefficients: UNSPECIFIED,
            },
            ycbcr_to_rgb: ycbcr_to_rgb(matrix, full_range),
        }
    }

    /// The default for a picture of this visible height with no color
    /// description: BT.709 at 720 lines and above, BT.601 below.
    pub fn default_for_height(height: u32, full_range: bool) -> ColorInfo {
        let matrix = if height >= 720 {
            ColorMatrix::Bt709
        } else {
            ColorMatrix::Bt601
        };
        ColorInfo::new(matrix, full_range)
    }

    /// Resolves the conversion from an optional VUI signal type.
    ///
    /// The range comes from the VUI whenever a video signal type is present,
    /// even without a color description. The matrix comes from the color
    /// description when it names a supported matrix, else the height default.
    pub fn from_signal(signal: Option<&VideoSignal>, visible_height: u32) -> ColorInfo {
        let full_range = signal.is_some_and(|s| s.full_range);
        let description = signal.and_then(|s| s.description);
        let matrix = description.and_then(|d| ColorMatrix::from_h273(d.matrix_coefficients));
        let mut info = match matrix {
            Some(m) => ColorInfo {
                from_stream: true,
                ..ColorInfo::new(m, full_range)
            },
            None => ColorInfo::default_for_height(visible_height, full_range),
        };
        if let Some(d) = description {
            info.description = d;
        }
        info
    }
}

/// Builds the YCbCr to RGB matrix for 8-bit samples normalized to `0..=1`.
///
/// With `Kg = 1 - Kr - Kb` and `Y` in `0..=1`, `Cb`/`Cr` in `-0.5..=0.5`:
///
/// ```text
/// R = Y + 2(1 - Kr) Cr
/// G = Y - 2 Kb (1 - Kb) / Kg Cb - 2 Kr (1 - Kr) / Kg Cr
/// B = Y + 2(1 - Kb) Cb
/// ```
///
/// Range expansion from a texture value `t` (= code / 255): video range
/// `Y = (255 t - 16) / 219`, `C = (255 t - 128) / 224`; full range `Y = t`,
/// `C = (255 t - 128) / 255`. Both are folded into the matrix. Pictures with
/// more than 8 bits per sample that the decoder outputs as 8-bit (or as
/// normalized 16-bit) differ from this by well under one 8-bit step.
pub fn ycbcr_to_rgb(matrix: ColorMatrix, full_range: bool) -> YcbcrToRgb {
    let (kr, kb) = matrix.weights();
    let kg = 1.0 - kr - kb;
    // Coefficients for (Y, Cb, Cr) in their nominal ranges.
    let base: [[f64; 3]; 3] = [
        [1.0, 0.0, 2.0 * (1.0 - kr)],
        [
            1.0,
            -2.0 * kb * (1.0 - kb) / kg,
            -2.0 * kr * (1.0 - kr) / kg,
        ],
        [1.0, 2.0 * (1.0 - kb), 0.0],
    ];
    // nominal = scale * t + offset
    let (y_scale, y_offset, c_scale, c_offset) = if full_range {
        (1.0, 0.0, 1.0, -128.0 / 255.0)
    } else {
        (255.0 / 219.0, -16.0 / 219.0, 255.0 / 224.0, -128.0 / 224.0)
    };
    let mut out = [[0f32; 4]; 3];
    for (row, b) in out.iter_mut().zip(base.iter()) {
        row[0] = (b[0] * y_scale) as f32;
        row[1] = (b[1] * c_scale) as f32;
        row[2] = (b[2] * c_scale) as f32;
        row[3] = (b[0] * y_offset + (b[1] + b[2]) * c_offset) as f32;
    }
    out
}

/// Applies a matrix to normalized `(y, cb, cr)`; used by tests and by
/// platforms that want a CPU reference.
pub fn apply(m: &YcbcrToRgb, y: f32, cb: f32, cr: f32) -> [f32; 3] {
    let mut rgb = [0f32; 3];
    for (out, row) in rgb.iter_mut().zip(m.iter()) {
        *out = row[0] * y + row[1] * cb + row[2] * cr + row[3];
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(v: u8) -> f32 {
        f32::from(v) / 255.0
    }

    fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| (x - y).abs() <= tol)
    }

    #[test]
    fn video_range_black_and_white() {
        for m in [ColorMatrix::Bt601, ColorMatrix::Bt709, ColorMatrix::Bt2020] {
            let mx = ycbcr_to_rgb(m, false);
            let black = apply(&mx, code(16), code(128), code(128));
            let white = apply(&mx, code(235), code(128), code(128));
            assert!(close(black, [0.0; 3], 1e-5), "{m:?} black {black:?}");
            assert!(close(white, [1.0; 3], 1e-5), "{m:?} white {white:?}");
        }
    }

    #[test]
    fn full_range_black_and_white() {
        for m in [ColorMatrix::Bt601, ColorMatrix::Bt709, ColorMatrix::Bt2020] {
            let mx = ycbcr_to_rgb(m, true);
            let black = apply(&mx, code(0), code(128), code(128));
            let white = apply(&mx, code(255), code(128), code(128));
            assert!(close(black, [0.0; 3], 1e-5), "{m:?} black {black:?}");
            assert!(close(white, [1.0; 3], 1e-5), "{m:?} white {white:?}");
        }
    }

    /// Published 8-bit video-range code values for 100% red.
    #[test]
    fn known_red_code_values() {
        // BT.709: Y 63, Cb 102, Cr 240.
        let red = apply(
            &ycbcr_to_rgb(ColorMatrix::Bt709, false),
            code(63),
            code(102),
            code(240),
        );
        assert!(close(red, [1.0, 0.0, 0.0], 0.01), "709 red {red:?}");
        // BT.601: Y 81, Cb 90, Cr 240.
        let red = apply(
            &ycbcr_to_rgb(ColorMatrix::Bt601, false),
            code(81),
            code(90),
            code(240),
        );
        assert!(close(red, [1.0, 0.0, 0.0], 0.01), "601 red {red:?}");
    }

    /// Encodes pure colors with the forward equations, quantizes to 8 bits,
    /// and checks the matrix brings them back.
    #[test]
    fn pure_color_round_trip() {
        let colors = [
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 1.0],
            [1.0, 0.0, 1.0],
            [0.5, 0.25, 0.75],
        ];
        for m in [ColorMatrix::Bt601, ColorMatrix::Bt709, ColorMatrix::Bt2020] {
            let (kr, kb) = m.weights();
            let kg = 1.0 - kr - kb;
            for full in [false, true] {
                let mx = ycbcr_to_rgb(m, full);
                for c in colors {
                    let (r, g, b) = (c[0], c[1], c[2]);
                    let y = kr * r + kg * g + kb * b;
                    let cb = (b - y) / (2.0 * (1.0 - kb));
                    let cr = (r - y) / (2.0 * (1.0 - kr));
                    let q = |v: f64| v.round().clamp(0.0, 255.0) as u8;
                    let (yq, cbq, crq) = if full {
                        (q(y * 255.0), q(cb * 255.0 + 128.0), q(cr * 255.0 + 128.0))
                    } else {
                        (
                            q(y * 219.0 + 16.0),
                            q(cb * 224.0 + 128.0),
                            q(cr * 224.0 + 128.0),
                        )
                    };
                    let rgb = apply(&mx, code(yq), code(cbq), code(crq));
                    let want = [r as f32, g as f32, b as f32];
                    assert!(
                        close(rgb, want, 0.012),
                        "{m:?} full={full} {c:?} -> {rgb:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn defaults_and_vui() {
        assert_eq!(ColorInfo::from_signal(None, 240).matrix, ColorMatrix::Bt601);
        assert_eq!(ColorInfo::from_signal(None, 719).matrix, ColorMatrix::Bt601);
        assert_eq!(ColorInfo::from_signal(None, 720).matrix, ColorMatrix::Bt709);
        assert!(!ColorInfo::from_signal(None, 1080).full_range);
        assert!(!ColorInfo::from_signal(None, 1080).from_stream);

        // Range without a color description still counts.
        let s = VideoSignal {
            full_range: true,
            description: None,
        };
        let c = ColorInfo::from_signal(Some(&s), 1080);
        assert!(c.full_range && !c.from_stream);
        assert_eq!(c.matrix, ColorMatrix::Bt709);

        // An explicit BT.601 matrix wins over the size default.
        let s = VideoSignal {
            full_range: false,
            description: Some(ColorDescription {
                primaries: 1,
                transfer: 1,
                matrix_coefficients: 6,
            }),
        };
        let c = ColorInfo::from_signal(Some(&s), 2160);
        assert_eq!(c.matrix, ColorMatrix::Bt601);
        assert!(c.from_stream);
        assert_eq!(c.description.primaries, 1);
        assert_eq!(c.ycbcr_to_rgb, ycbcr_to_rgb(ColorMatrix::Bt601, false));

        // Unsupported (identity) falls back to the default but keeps the codes.
        let s = VideoSignal {
            full_range: true,
            description: Some(ColorDescription {
                primaries: 9,
                transfer: 16,
                matrix_coefficients: 0,
            }),
        };
        let c = ColorInfo::from_signal(Some(&s), 480);
        assert_eq!(c.matrix, ColorMatrix::Bt601);
        assert!(!c.from_stream);
        assert_eq!(c.description.matrix_coefficients, 0);
    }
}
