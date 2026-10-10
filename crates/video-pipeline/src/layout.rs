//! Grid layout: where each tile goes in the viewport and which part of its
//! picture to sample, in whole pixels.
//!
//! Tiles fill a grid row by row, left to right. By default the grid has
//! `ceil(sqrt(n))` columns and as many rows as needed; a short last row stays
//! left-aligned. Cells share the space left after the gaps, with any
//! remainder pixels spread so cell sizes differ by at most one pixel.

use crate::geometry::{Rect, Size};

/// How a picture is placed in its cell.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FitMode {
    /// Show the whole picture, letterboxed or pillarboxed inside the cell.
    Fit = 0,
    /// Cover the whole cell, cropping the picture's edges.
    Fill = 1,
}

/// Placement of one tile.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct TileLayout {
    /// The tile's grid cell in the viewport.
    pub cell: Rect,
    /// Where to draw the picture, in viewport pixels (inside `cell`).
    pub dest: Rect,
    /// What part of the picture to sample, in picture pixels relative to the
    /// visible picture (add the format's `visible_rect` origin when sampling
    /// a coded texture). Empty when the picture size is unknown: draw nothing
    /// (black) in `cell`.
    pub source: Rect,
}

/// Columns and rows of the default grid for `tile_count` tiles.
pub fn grid_dimensions(tile_count: usize) -> (u32, u32) {
    if tile_count == 0 {
        return (0, 0);
    }
    let n = tile_count.min(u32::MAX as usize) as u64;
    let mut cols = (n as f64).sqrt() as u64;
    while cols * cols < n {
        cols += 1;
    }
    while cols > 1 && (cols - 1) * (cols - 1) >= n {
        cols -= 1;
    }
    let rows = n.div_ceil(cols);
    (cols as u32, rows as u32)
}

/// Lays out `tile_count` tiles in the default grid. `picture_sizes[i]` is the
/// visible size of tile `i`'s picture; missing or empty entries mean unknown.
pub fn grid_layout(
    tile_count: usize,
    viewport: Size,
    gap: u32,
    picture_sizes: &[Size],
    mode: FitMode,
) -> Vec<TileLayout> {
    let mut out = vec![TileLayout::default(); tile_count];
    grid_layout_into(0, viewport, gap, picture_sizes, mode, &mut out);
    out
}

/// Like [`grid_layout`], writing one entry per element of `out` (so the tile
/// count is `out.len()`) and allocating nothing. `columns` of 0 means the
/// default `ceil(sqrt(n))`. Returns the number of rows.
pub fn grid_layout_into(
    columns: u32,
    viewport: Size,
    gap: u32,
    picture_sizes: &[Size],
    mode: FitMode,
    out: &mut [TileLayout],
) -> u32 {
    let n = out.len();
    if n == 0 {
        return 0;
    }
    let cols = if columns == 0 {
        grid_dimensions(n).0
    } else {
        columns.min(n.min(u32::MAX as usize) as u32)
    };
    let rows = (n as u64).div_ceil(u64::from(cols)) as u32;
    for (i, tile) in out.iter_mut().enumerate() {
        let col = (i as u64 % u64::from(cols)) as u32;
        let row = (i as u64 / u64::from(cols)) as u32;
        let (x, w) = span(viewport.width, gap, cols, col);
        let (y, h) = span(viewport.height, gap, rows, row);
        let cell = Rect::new(x, y, w, h);
        let picture = picture_sizes.get(i).copied().unwrap_or_default();
        *tile = place(cell, picture, mode);
    }
    rows
}

/// Start and length of cell `index` of `count` along an axis of `total`
/// pixels with `gap` pixels between cells.
fn span(total: u32, gap: u32, count: u32, index: u32) -> (u32, u32) {
    let total = u64::from(total);
    let gaps = u64::from(gap) * u64::from(count - 1);
    let avail = total.saturating_sub(gaps);
    let i = u64::from(index);
    let c = u64::from(count);
    let start = i * avail / c;
    let end = (i + 1) * avail / c;
    let pos = (start + i * u64::from(gap)).min(total);
    (pos as u32, (end - start) as u32)
}

/// Places a picture in a cell.
fn place(cell: Rect, picture: Size, mode: FitMode) -> TileLayout {
    if picture.is_empty() {
        return TileLayout {
            cell,
            dest: cell,
            source: Rect::ZERO,
        };
    }
    let full = Rect::new(0, 0, picture.width, picture.height);
    if cell.is_empty() {
        return TileLayout {
            cell,
            dest: Rect::new(cell.x, cell.y, 0, 0),
            source: full,
        };
    }
    let (pw, ph) = (u64::from(picture.width), u64::from(picture.height));
    let (cw, ch) = (u64::from(cell.width), u64::from(cell.height));
    // Rounded a * b / c, at least 1, at most `max`.
    let scale = |a: u64, b: u64, c: u64, max: u64| ((a * b + c / 2) / c).clamp(1, max);
    match mode {
        FitMode::Fit => {
            let (w, h) = if pw * ch > ph * cw {
                // Picture is wider than the cell: full width, bars above/below.
                (cw, scale(ph, cw, pw, ch))
            } else {
                (scale(pw, ch, ph, cw), ch)
            };
            let dest = Rect::new(
                cell.x + ((cw - w) / 2) as u32,
                cell.y + ((ch - h) / 2) as u32,
                w as u32,
                h as u32,
            );
            TileLayout {
                cell,
                dest,
                source: full,
            }
        }
        FitMode::Fill => {
            let (sw, sh) = if pw * ch > ph * cw {
                // Picture is wider: crop left and right.
                (scale(ph, cw, ch, pw), ph)
            } else {
                (pw, scale(pw, ch, cw, ph))
            };
            let source = Rect::new(
                ((pw - sw) / 2) as u32,
                ((ph - sh) / 2) as u32,
                sw as u32,
                sh as u32,
            );
            TileLayout {
                cell,
                dest: cell,
                source,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains(outer: Rect, inner: Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && u64::from(inner.x) + u64::from(inner.width)
                <= u64::from(outer.x) + u64::from(outer.width)
            && u64::from(inner.y) + u64::from(inner.height)
                <= u64::from(outer.y) + u64::from(outer.height)
    }

    #[test]
    fn dimensions() {
        let d: Vec<(u32, u32)> = (0..=10).map(grid_dimensions).collect();
        assert_eq!(
            d,
            [
                (0, 0),
                (1, 1),
                (2, 1),
                (2, 2),
                (2, 2),
                (3, 2),
                (3, 2),
                (3, 3),
                (3, 3),
                (3, 3),
                (4, 3)
            ]
        );
        assert_eq!(grid_dimensions(16), (4, 4));
        assert_eq!(grid_dimensions(17), (5, 4));
    }

    #[test]
    fn cells_tile_the_viewport_exactly() {
        let vp = Size::new(1001, 601);
        let l = grid_layout(9, vp, 3, &[], FitMode::Fill);
        // Columns: 1001 - 6 = 995 px over 3 cells: 331, 332, 332.
        assert_eq!(l[0].cell, Rect::new(0, 0, 331, 198));
        assert_eq!(l[1].cell.x, 334);
        assert_eq!(l[2].cell.x + l[2].cell.width, 1001);
        assert_eq!(l[8].cell.y + l[8].cell.height, 601);
        for t in &l {
            assert!(contains(Rect::new(0, 0, 1001, 601), t.cell));
            // Unknown picture: draw black over the cell.
            assert_eq!(t.dest, t.cell);
            assert!(t.source.is_empty());
        }
    }

    #[test]
    fn odd_count_leaves_last_row_short() {
        let sizes = vec![Size::new(640, 360); 5];
        let l = grid_layout(5, Size::new(1920, 1080), 0, &sizes, FitMode::Fit);
        assert_eq!(l.len(), 5);
        // 3 columns x 2 rows; tiles 3 and 4 in the second row.
        assert_eq!(l[3].cell, Rect::new(0, 540, 640, 540));
        assert_eq!(l[4].cell, Rect::new(640, 540, 640, 540));
        // 16:9 in a 640x540 cell: 640x360, centered vertically.
        assert_eq!(l[0].dest, Rect::new(0, 90, 640, 360));
        assert_eq!(l[0].source, Rect::new(0, 0, 640, 360));
    }

    #[test]
    fn fit_and_fill() {
        let cell_vp = Size::new(400, 300);
        let pic = [Size::new(1920, 1080)];
        let fit = grid_layout(1, cell_vp, 0, &pic, FitMode::Fit)[0];
        assert_eq!(fit.dest, Rect::new(0, 37, 400, 225));
        let fill = grid_layout(1, cell_vp, 0, &pic, FitMode::Fill)[0];
        assert_eq!(fill.dest, Rect::new(0, 0, 400, 300));
        // 4:3 crop of 1920x1080: 1440x1080 centered.
        assert_eq!(fill.source, Rect::new(240, 0, 1440, 1080));

        // Tall picture in a wide cell.
        let pic = [Size::new(480, 1920)];
        let fit = grid_layout(1, Size::new(1000, 500), 0, &pic, FitMode::Fit)[0];
        assert_eq!(fit.dest, Rect::new(437, 0, 125, 500));
        let fill = grid_layout(1, Size::new(1000, 500), 0, &pic, FitMode::Fill)[0];
        assert_eq!(fill.source, Rect::new(0, 840, 480, 240));
    }

    #[test]
    fn extreme_aspect_ratios_stay_inside() {
        let cases = [
            Size::new(100_000, 1),
            Size::new(1, 100_000),
            Size::new(u32::MAX, 1),
            Size::new(1, u32::MAX),
            Size::new(u32::MAX, u32::MAX),
        ];
        for vp in [
            Size::new(1, 1),
            Size::new(7, 3),
            Size::new(3840, 2160),
            Size::new(u32::MAX, u32::MAX),
        ] {
            for mode in [FitMode::Fit, FitMode::Fill] {
                let l = grid_layout(cases.len(), vp, 1, &cases, mode);
                for (t, pic) in l.iter().zip(cases.iter()) {
                    assert!(
                        contains(t.cell, t.dest) || t.cell.is_empty(),
                        "{vp:?} {pic:?} {t:?}"
                    );
                    assert!(
                        contains(Rect::new(0, 0, pic.width, pic.height), t.source),
                        "{t:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn zero_sizes() {
        assert!(grid_layout(0, Size::new(100, 100), 4, &[], FitMode::Fit).is_empty());
        let l = grid_layout(
            4,
            Size::new(0, 0),
            4,
            &[Size::new(640, 480); 4],
            FitMode::Fit,
        );
        for t in &l {
            assert!(t.cell.is_empty() && t.dest.is_empty());
        }
        // Gap larger than the viewport: cells are empty and stay in bounds.
        let l = grid_layout(9, Size::new(10, 10), 100, &[], FitMode::Fill);
        for t in &l {
            assert!(t.cell.is_empty());
            assert!(t.cell.x <= 10 && t.cell.y <= 10);
        }
        // Zero-size picture: black cell.
        let l = grid_layout(1, Size::new(10, 10), 0, &[Size::new(0, 480)], FitMode::Fit);
        assert_eq!(l[0].dest, Rect::new(0, 0, 10, 10));
        assert!(l[0].source.is_empty());
    }

    #[test]
    fn explicit_columns() {
        let mut out = [TileLayout::default(); 5];
        let rows = grid_layout_into(5, Size::new(500, 100), 0, &[], FitMode::Fit, &mut out);
        assert_eq!(rows, 1);
        assert_eq!(out[4].cell, Rect::new(400, 0, 100, 100));
        let rows = grid_layout_into(1, Size::new(100, 500), 0, &[], FitMode::Fit, &mut out);
        assert_eq!(rows, 5);
        assert_eq!(out[4].cell, Rect::new(0, 400, 100, 100));
        // More columns than tiles collapses to one row of n.
        let rows = grid_layout_into(50, Size::new(500, 100), 0, &[], FitMode::Fit, &mut out);
        assert_eq!(rows, 1);
        assert_eq!(out[0].cell.width, 100);
    }
}
