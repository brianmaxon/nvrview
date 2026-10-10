import CoreGraphics
import Foundation

// Ready-made split trees ("New Layout from Template") and the geometry of
// layout icons. Pure: no drawing here. docs/UX.md "Layouts" describes the
// shapes; apps/windows/NvrView.Shell/LayoutTemplate.cs mirrors them.

extension LayoutNode {
    /// Every tile's rectangle in the unit square, in reading order. No
    /// rounding and no gaps: the rectangles tile the square exactly.
    public var unitRects: [CGRect] {
        var out: [CGRect] = []
        func walk(_ n: LayoutNode, _ r: CGRect) {
            switch n {
            case .leaf:
                out.append(r)
            case .split(let axis, let ratio, let a, let b, _):
                switch axis {
                case .row:
                    let w = r.width * ratio
                    walk(a, CGRect(x: r.minX, y: r.minY, width: w, height: r.height))
                    walk(b, CGRect(x: r.minX + w, y: r.minY, width: r.width - w, height: r.height))
                case .col:
                    let h = r.height * ratio
                    walk(a, CGRect(x: r.minX, y: r.minY, width: r.width, height: h))
                    walk(b, CGRect(x: r.minX, y: r.minY + h, width: r.width, height: r.height - h))
                }
            }
        }
        walk(self, CGRect(x: 0, y: 0, width: 1, height: 1))
        return out
    }

    /// The tiles of a layout icon `size` points across: each tile's unit
    /// rectangle scaled to `size` and pulled in by half of `gap` on every
    /// side, so neighbors are `gap` apart. A tile never shrinks below one
    /// point.
    public func iconRects(size: CGSize, gap: CGFloat) -> [CGRect] {
        unitRects.map { u in
            let r = CGRect(x: u.minX * size.width, y: u.minY * size.height, width: u.width * size.width, height: u.height * size.height)
            let dx = min(gap / 2, max((r.width - 1) / 2, 0))
            let dy = min(gap / 2, max((r.height - 1) / 2, 0))
            return r.insetBy(dx: dx, dy: dy)
        }
    }
}

/// A ready-made arrangement: its shape, with the first cameras of a list in
/// its tiles in reading order and the rest empty.
public enum LayoutTemplate: String, CaseIterable, Sendable, Identifiable {
    case single, sideBySide, stacked
    case grid4, grid9, grid16, grid25, grid36
    case big1Small5, big1Small7, big2Small8, big1Small12, big2Small12, big1Small31

    public var id: String { rawValue }

    /// Smallest first.
    public static let ordered: [LayoutTemplate] = LayoutTemplate.allCases.sorted {
        ($0.tileCount, $0.rawValue) < ($1.tileCount, $1.rawValue)
    }

    public var tileCount: Int {
        switch self {
        case .single: 1
        case .sideBySide, .stacked: 2
        case .grid4: 4
        case .big1Small5: 6
        case .big1Small7: 8
        case .grid9: 9
        case .big2Small8: 10
        case .big1Small12: 13
        case .big2Small12: 14
        case .grid16: 16
        case .grid25: 25
        case .big1Small31: 32
        case .grid36: 36
        }
    }

    /// The menu title: the tile count, then the shape.
    public var title: String {
        switch self {
        case .single: "1 · Single"
        case .sideBySide: "2 · Side by Side"
        case .stacked: "2 · Stacked"
        case .grid4: "4 · Grid 2 × 2"
        case .grid9: "9 · Grid 3 × 3"
        case .grid16: "16 · Grid 4 × 4"
        case .grid25: "25 · Grid 5 × 5"
        case .grid36: "36 · Grid 6 × 6"
        case .big1Small5: "6 · One Large, 5 Small"
        case .big1Small7: "8 · One Large, 7 Small"
        case .big2Small8: "10 · Two Large, 8 Small"
        case .big1Small12: "13 · One Large, 12 Small"
        case .big2Small12: "14 · Two Large, 12 Small"
        case .big1Small31: "32 · One Large, 31 Small"
        }
    }

    /// The shape with empty tiles.
    public var shape: LayoutNode {
        let e = LayoutNode.empty
        func row(_ n: Int) -> LayoutNode { LayoutNode.chain(.row, Array(repeating: e, count: n)) }
        func rows(_ n: Int, of columns: Int) -> LayoutNode { LayoutNode.chain(.col, Array(repeating: row(columns), count: n)) }
        /// Two parts along `axis`, the first `first` of `total` units.
        func cut(_ axis: SplitAxis, _ first: Double, of total: Double, _ a: LayoutNode, _ b: LayoutNode) -> LayoutNode {
            .split(axis, ratio: first / total, a: a, b: b)
        }
        switch self {
        case .single: return e
        case .sideBySide: return row(2)
        case .stacked: return LayoutNode.chain(.col, [e, e])
        case .grid4: return rows(2, of: 2)
        case .grid9: return rows(3, of: 3)
        case .grid16: return rows(4, of: 4)
        case .grid25: return rows(5, of: 5)
        case .grid36: return rows(6, of: 6)
        case .big1Small5:
            // 3 x 3 units: a 2 x 2 tile, a column of 2, a row of 3.
            return cut(.col, 2, of: 3, cut(.row, 2, of: 3, e, LayoutNode.chain(.col, [e, e])), row(3))
        case .big1Small7:
            // 4 x 4 units: a 3 x 3 tile, a column of 3, a row of 4.
            return cut(.col, 3, of: 4, cut(.row, 3, of: 4, e, LayoutNode.chain(.col, [e, e, e])), row(4))
        case .big2Small8:
            // Two large tiles across the top half, two rows of 4 below.
            return cut(.col, 1, of: 2, row(2), rows(2, of: 4))
        case .big1Small12:
            // 4 x 4 units: a 2 x 2 tile, a 2 x 2 block of small tiles, two rows of 4.
            return cut(.col, 1, of: 2, cut(.row, 1, of: 2, e, rows(2, of: 2)), rows(2, of: 4))
        case .big2Small12:
            // Two large tiles across the top (2 of 5 rows), three rows of 4 below.
            return cut(.col, 2, of: 5, row(2), rows(3, of: 4))
        case .big1Small31:
            // 7 x 5 units: a 2 x 2 tile with a 5 x 2 block of small tiles
            // beside it, three rows of 7 below.
            return cut(.col, 2, of: 5, cut(.row, 2, of: 7, e, rows(2, of: 5)), rows(3, of: 7))
        }
    }

    /// The shape with `cameras` in its tiles in reading order; tiles beyond
    /// the list stay empty and cameras beyond the tile count are not used.
    public func tree(cameras: [CameraRef] = []) -> LayoutNode {
        var next = 0
        func fill(_ n: LayoutNode) -> LayoutNode {
            switch n {
            case .leaf(_, let extras):
                defer { next += 1 }
                return .leaf(next < cameras.count ? cameras[next] : nil, extras: extras)
            case .split(let axis, let ratio, let a, let b, let extras):
                let fa = fill(a)
                return .split(axis, ratio: ratio, a: fa, b: fill(b), extras: extras)
            }
        }
        return fill(shape)
    }
}
