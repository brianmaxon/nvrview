import CoreGraphics
import Foundation

// Where a split tree's tiles and dividers sit in a rectangle, how a picture
// fits a tile, and how tile rectangles animate (focus and back). Pure, so the
// designer's hit testing and the renderer's rectangles are tested headless.

/// One tile's rectangle.
public struct LeafFrame: Sendable, Equatable {
    public var path: NodePath
    public var camera: CameraRef?
    public var rect: CGRect
}

/// One divider: the gap between a split's two children, and the split's
/// whole rectangle (what a drag's position is measured against).
public struct DividerFrame: Sendable, Equatable {
    public var path: NodePath
    public var axis: SplitAxis
    /// The gap between the two children (zero thickness with no gap).
    public var rect: CGRect
    public var region: CGRect

    /// Where the pointer can grab it: the gap widened to at least `minimum`.
    public func hitRect(minimum: CGFloat = 8) -> CGRect {
        switch axis {
        case .row:
            let grow = max(minimum - rect.width, 0) / 2
            return rect.insetBy(dx: -grow, dy: 0)
        case .col:
            let grow = max(minimum - rect.height, 0) / 2
            return rect.insetBy(dx: 0, dy: -grow)
        }
    }
}

public struct LayoutFrames: Sendable, Equatable {
    public var leaves: [LeafFrame]
    public var dividers: [DividerFrame]

    public init(leaves: [LeafFrame], dividers: [DividerFrame]) {
        self.leaves = leaves
        self.dividers = dividers
    }

    public func leaf(at point: CGPoint) -> LeafFrame? { leaves.first { $0.rect.contains(point) } }

    public func leaf(_ path: NodePath) -> LeafFrame? { leaves.first { $0.path == path } }

    /// The divider under `point`, the innermost first (it is the smallest).
    public func divider(at point: CGPoint, minimum: CGFloat = 8) -> DividerFrame? {
        dividers.filter { $0.hitRect(minimum: minimum).contains(point) }
            .min { $0.region.width * $0.region.height < $1.region.width * $1.region.height }
    }
}

extension LayoutNode {
    /// Tile and divider rectangles in `bounds`, `gap` points between tiles.
    public func frames(in bounds: CGRect, gap: CGFloat = 0) -> LayoutFrames {
        var leaves: [LeafFrame] = []
        var dividers: [DividerFrame] = []
        func walk(_ n: LayoutNode, _ path: NodePath, _ r: CGRect) {
            switch n {
            case .leaf(let c):
                leaves.append(LeafFrame(path: path, camera: c, rect: r))
            case .split(let axis, let ratio, let a, let b):
                switch axis {
                case .row:
                    let g = min(gap, r.width)
                    let aw = ((r.width - g) * ratio).rounded()
                    let ra = CGRect(x: r.minX, y: r.minY, width: aw, height: r.height)
                    let rb = CGRect(x: r.minX + aw + g, y: r.minY, width: max(r.width - aw - g, 0), height: r.height)
                    dividers.append(
                        DividerFrame(
                            path: path, axis: axis, rect: CGRect(x: ra.maxX, y: r.minY, width: g, height: r.height),
                            region: r))
                    walk(a, path + [.a], ra)
                    walk(b, path + [.b], rb)
                case .col:
                    let g = min(gap, r.height)
                    let ah = ((r.height - g) * ratio).rounded()
                    let ra = CGRect(x: r.minX, y: r.minY, width: r.width, height: ah)
                    let rb = CGRect(x: r.minX, y: r.minY + ah + g, width: r.width, height: max(r.height - ah - g, 0))
                    dividers.append(
                        DividerFrame(
                            path: path, axis: axis, rect: CGRect(x: r.minX, y: ra.maxY, width: r.width, height: g),
                            region: r))
                    walk(a, path + [.a], ra)
                    walk(b, path + [.b], rb)
                }
            }
        }
        walk(self, [], bounds)
        return LayoutFrames(leaves: leaves, dividers: dividers)
    }

    /// The ratio that puts `divider`'s middle at `point` (clamped).
    public static func ratio(dragging divider: DividerFrame, to point: CGPoint, gap: CGFloat = 0) -> Double {
        let r = divider.region
        switch divider.axis {
        case .row:
            let g = min(gap, r.width)
            guard r.width - g > 0 else { return 0.5 }
            return clamp(Double((point.x - r.minX - g / 2) / (r.width - g)))
        case .col:
            let g = min(gap, r.height)
            guard r.height - g > 0 else { return 0.5 }
            return clamp(Double((point.y - r.minY - g / 2) / (r.height - g)))
        }
    }
}

/// How a picture sits in its tile: `destination` in the tile's coordinate
/// space, `source` in pixels of the picture (both origin top left).
public struct PictureFit: Sendable, Equatable {
    public var destination: CGRect
    public var source: CGRect

    /// Fits (letterboxed) or fills (cropped, centered) `picture` in `cell`.
    /// Whole units, so pixel-aligned cells give pixel-aligned pictures.
    public static func fit(picture: CGSize, in cell: CGRect, fill: Bool) -> PictureFit {
        let full = CGRect(origin: .zero, size: picture)
        guard picture.width > 0, picture.height > 0, cell.width > 0, cell.height > 0 else {
            return PictureFit(destination: cell, source: full)
        }
        let pictureAspect = picture.width / picture.height
        let cellAspect = cell.width / cell.height
        if fill {
            var s = full
            if pictureAspect > cellAspect {
                s.size.width = (picture.height * cellAspect).rounded()
                s.origin.x = ((picture.width - s.width) / 2).rounded(.down)
            } else {
                s.size.height = (picture.width / cellAspect).rounded()
                s.origin.y = ((picture.height - s.height) / 2).rounded(.down)
            }
            return PictureFit(destination: cell, source: s)
        }
        var d = cell
        if pictureAspect > cellAspect {
            d.size.height = (cell.width / pictureAspect).rounded()
            d.origin.y = (cell.minY + (cell.height - d.height) / 2).rounded(.down)
        } else {
            d.size.width = (cell.height * pictureAspect).rounded()
            d.origin.x = (cell.minX + (cell.width - d.width) / 2).rounded(.down)
        }
        return PictureFit(destination: d, source: full)
    }
}

/// Rectangle animation for focus and full screen: the same live tiles move,
/// nothing is recreated.
public enum RectAnimation {
    /// Ease in and out (smoothstep), `t` clamped to 0...1.
    public static func ease(_ t: Double) -> Double {
        let x = min(max(t, 0), 1)
        return x * x * (3 - 2 * x)
    }

    public static func interpolate(_ a: CGRect, _ b: CGRect, _ t: Double) -> CGRect {
        let k = CGFloat(min(max(t, 0), 1))
        return CGRect(
            x: a.minX + (b.minX - a.minX) * k, y: a.minY + (b.minY - a.minY) * k,
            width: a.width + (b.width - a.width) * k, height: a.height + (b.height - a.height) * k)
    }

    /// Where every tile is at `progress` of focusing `focused` (0: the grid,
    /// 1: the focused tile fills `bounds`). The other tiles stay where they
    /// are; the focused one grows over them and is drawn last.
    public static func focusFrames(
        grid: LayoutFrames, focused: NodePath?, bounds: CGRect, progress: Double
    ) -> [LeafFrame] {
        guard let focused, let target = grid.leaf(focused) else { return grid.leaves }
        let t = ease(progress)
        var others = grid.leaves.filter { $0.path != focused }
        var moving = target
        moving.rect = interpolate(target.rect, bounds, t)
        if t >= 1 { others.removeAll() }
        return others + [moving]
    }
}

extension CGRect {
    /// The rectangle in pixels of a `scale` backing, rounded to whole pixels.
    public func pixels(scale: CGFloat) -> CGRect { pixels(scaleX: scale, scaleY: scale) }

    /// With a scale per axis: a drawable that has not yet followed a resize
    /// is stretched to the view, so points map to its pixels unevenly.
    public func pixels(scaleX: CGFloat, scaleY: CGFloat) -> CGRect {
        let x0 = (minX * scaleX).rounded(), y0 = (minY * scaleY).rounded()
        let x1 = (maxX * scaleX).rounded(), y1 = (maxY * scaleY).rounded()
        return CGRect(x: x0, y: y0, width: max(x1 - x0, 0), height: max(y1 - y0, 0))
    }
}
