import CoreGraphics
import Foundation

// Regions of interest and zones drawn on a still of a camera (docs/UX.md
// "Smart alerts", "Regions"): polygons in normalized picture coordinates,
// the same the core tests a box's foot point against (crates/alerts
// `zone`). Pure geometry, tested headless.

public enum ROIGeometry {
    /// The most points a polygon keeps (the core's limit).
    public static let maxPoints = 32
    /// How close (points) a click must be to a corner to grab it.
    public static let grabDistance: CGFloat = 10

    /// Where a picture of `picture` size sits in a view of `view` size when
    /// fitted whole and centered (letterboxed).
    public static func fitRect(picture: CGSize, in view: CGSize) -> CGRect {
        guard picture.width > 0, picture.height > 0, view.width > 0, view.height > 0 else {
            return CGRect(origin: .zero, size: view)
        }
        let scale = min(view.width / picture.width, view.height / picture.height)
        let size = CGSize(width: picture.width * scale, height: picture.height * scale)
        return CGRect(x: (view.width - size.width) / 2, y: (view.height - size.height) / 2, width: size.width, height: size.height)
    }

    /// A view point (top-left origin) as a picture point, clamped to the
    /// picture.
    public static func toPicture(_ p: CGPoint, fit: CGRect) -> ROIPoint {
        guard fit.width > 0, fit.height > 0 else { return ROIPoint(x: 0, y: 0) }
        let x = Double((p.x - fit.minX) / fit.width)
        let y = Double((p.y - fit.minY) / fit.height)
        return ROIPoint(x: min(max(x, 0), 1), y: min(max(y, 0), 1))
    }

    /// A picture point in the view (top-left origin).
    public static func toView(_ p: ROIPoint, fit: CGRect) -> CGPoint {
        CGPoint(x: fit.minX + CGFloat(p.x) * fit.width, y: fit.minY + CGFloat(p.y) * fit.height)
    }

    /// A normalized box in the view.
    public static func toView(x: Double, y: Double, w: Double, h: Double, fit: CGRect) -> CGRect {
        CGRect(
            x: fit.minX + CGFloat(x) * fit.width, y: fit.minY + CGFloat(y) * fit.height, width: CGFloat(w) * fit.width,
            height: CGFloat(h) * fit.height)
    }

    /// The corner within `grabDistance` of a view point, nearest first.
    public static func vertex(near p: CGPoint, in points: [ROIPoint], fit: CGRect) -> Int? {
        var best: (Int, CGFloat)?
        for (i, q) in points.enumerated() {
            let v = toView(q, fit: fit)
            let d = hypot(v.x - p.x, v.y - p.y)
            if d <= grabDistance, d < (best?.1 ?? .infinity) { best = (i, d) }
        }
        return best?.0
    }

    /// Even-odd point in polygon (the core's test); fewer than 3 points
    /// enclose nothing.
    public static func contains(_ polygon: [ROIPoint], _ p: ROIPoint) -> Bool {
        guard polygon.count >= 3, p.x.isFinite, p.y.isFinite else { return false }
        var inside = false
        var j = polygon.count - 1
        for i in polygon.indices {
            let (xi, yi) = (polygon[i].x, polygon[i].y)
            let (xj, yj) = (polygon[j].x, polygon[j].y)
            if (yi > p.y) != (yj > p.y), p.x < (xj - xi) * (p.y - yi) / (yj - yi) + xi { inside.toggle() }
            j = i
        }
        return inside
    }

    /// A box's foot point: the bottom center, where it meets the ground.
    public static func foot(x: Double, y: Double, w: Double, h: Double) -> ROIPoint { ROIPoint(x: x + w / 2, y: y + h) }

    /// The polygon's area (shoelace), in picture fractions.
    public static func area(_ polygon: [ROIPoint]) -> Double {
        guard polygon.count >= 3 else { return 0 }
        var s = 0.0
        for i in polygon.indices {
            let a = polygon[i], b = polygon[(i + 1) % polygon.count]
            s += a.x * b.y - b.x * a.y
        }
        return abs(s) / 2
    }
}

/// The region editor's state: click to add a corner, drag a corner to move
/// it, Delete removes the last one (or the grabbed one), Clear starts over.
public struct ROIEditor: Equatable, Sendable {
    public private(set) var points: [ROIPoint]
    /// The corner being dragged.
    public private(set) var grabbed: Int?

    public init(points: [ROIPoint] = []) { self.points = Array(points.prefix(ROIGeometry.maxPoints)) }

    /// Usable as a region (three corners or more).
    public var isClosed: Bool { points.count >= 3 }

    /// A press at a view point: grabs a corner near it, or adds one (at most
    /// `maxPoints`).
    public mutating func press(at p: CGPoint, fit: CGRect) {
        if let i = ROIGeometry.vertex(near: p, in: points, fit: fit) {
            grabbed = i
        } else if points.count < ROIGeometry.maxPoints, fit.contains(p) {
            points.append(ROIGeometry.toPicture(p, fit: fit))
            grabbed = points.count - 1
        } else {
            grabbed = nil
        }
    }

    /// Moves the grabbed corner (clamped to the picture).
    public mutating func drag(to p: CGPoint, fit: CGRect) {
        guard let i = grabbed, points.indices.contains(i) else { return }
        points[i] = ROIGeometry.toPicture(p, fit: fit)
    }

    public mutating func release() { grabbed = nil }

    /// Removes the grabbed corner, else the last one.
    public mutating func deleteCorner() {
        if let i = grabbed, points.indices.contains(i) {
            points.remove(at: i)
        } else if !points.isEmpty {
            points.removeLast()
        }
        grabbed = nil
    }

    public mutating func clear() {
        points = []
        grabbed = nil
    }

    /// What is saved: the polygon when it has three corners or more, else
    /// nothing (the whole picture).
    public var saved: [ROIPoint] { isClosed ? points : [] }
}
