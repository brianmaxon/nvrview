import CoreGraphics
import Foundation
import VideoKit

// Fisheye dewarping (docs/RENDERING.md "Fisheye dewarping", docs/UX.md
// "Fisheye cameras"): the lens model, the views, the shader constants, and
// finding the image circle. Pure code: the Metal shader in VideoKit does
// only the per-pixel part of what is computed here, and the tests check
// this against known directions.

/// How a fisheye camera is mounted (a per-camera setting).
public enum FisheyeMount: String, Sendable, Codable, CaseIterable {
    /// Looking down (the default): the horizon is the rim of the circle.
    case ceiling
    /// Looking ahead: the horizon runs across the middle.
    case wall
    /// Looking up.
    case desk

    public var title: String {
        switch self {
        case .ceiling: "Ceiling"
        case .wall: "Wall"
        case .desk: "Desk"
        }
    }

    /// A direction of the viewer frame (x right, y down, z forward) in the
    /// lens frame (x right and y down in the picture, z out of the lens).
    public func toLens(_ v: SIMD3<Double>) -> SIMD3<Double> {
        switch self {
        case .ceiling: SIMD3(v.x, -v.z, v.y)
        case .desk: SIMD3(v.x, v.z, -v.y)
        case .wall: v
        }
    }

    /// The inverse of `toLens`.
    public func fromLens(_ l: SIMD3<Double>) -> SIMD3<Double> {
        switch self {
        case .ceiling: SIMD3(l.x, l.z, -l.y)
        case .desk: SIMD3(l.x, -l.z, l.y)
        case .wall: l
        }
    }
}

/// What a tile shows of a fisheye camera.
public enum FisheyeViewMode: String, Sendable, CaseIterable {
    /// The circle as the camera sends it (digital zoom applies).
    case original
    /// One 360° strip (a wall mount: 180°).
    case panorama
    /// Two 180° strips stacked.
    case doublePanorama = "double_panorama"
    /// Four virtual 90° cameras.
    case quad
    /// One virtual camera the user aims and zooms.
    case ptz

    public var title: String {
        switch self {
        case .original: "Original"
        case .panorama: "Panorama"
        case .doublePanorama: "Double Panorama"
        case .quad: "Quad"
        case .ptz: "Virtual PTZ"
        }
    }

    /// Drawn through the dewarp pipeline.
    public var dewarps: Bool { self != .original }
}

/// Angles in degrees (the user's terms), converted where needed.
@inline(__always) func radians(_ degrees: Double) -> Double { degrees * .pi / 180 }
@inline(__always) func degrees(_ radians: Double) -> Double { radians * 180 / .pi }

/// The direction (ψ pan, τ tilt; degrees) in the viewer frame.
public func viewerDirection(pan: Double, tilt: Double) -> SIMD3<Double> {
    let p = radians(pan), t = radians(tilt)
    return SIMD3(sin(p) * cos(t), sin(t), cos(p) * cos(t))
}

/// Pan and tilt (degrees) of a viewer-frame direction; pan in -180...180.
public func panTilt(of v: SIMD3<Double>) -> (pan: Double, tilt: Double) {
    let n = v / max(simdLength(v), 1e-12)
    return (degrees(atan2(n.x, n.z)), degrees(asin(min(max(n.y, -1), 1))))
}

@inline(__always) func simdLength(_ v: SIMD3<Double>) -> Double { (v * v).sum().squareRoot() }

@inline(__always) func cross(_ a: SIMD3<Double>, _ b: SIMD3<Double>) -> SIMD3<Double> {
    SIMD3(a.y * b.z - a.z * b.y, a.z * b.x - a.x * b.z, a.x * b.y - a.y * b.x)
}

/// The lens's image circle in the picture: center as fractions of the width
/// and height, radius as a fraction of the width.
public struct FisheyeCircle: Sendable, Equatable {
    public var cx: Double
    public var cy: Double
    public var r: Double

    public init(cx: Double, cy: Double, r: Double) {
        self.cx = cx
        self.cy = cy
        self.r = r
    }

    /// The largest circle centered in a picture of this size (the fallback).
    public static func inscribed(width: Int, height: Int) -> FisheyeCircle {
        let w = Double(max(width, 1)), h = Double(max(height, 1))
        return FisheyeCircle(cx: 0.5, cy: 0.5, r: min(w, h) / 2 / w)
    }
}

/// The equidistant lens model, r = f·θ (docs/RENDERING.md).
public struct FisheyeLens: Sendable, Equatable {
    public var circle: FisheyeCircle
    /// The picture's width over its height (the radius is a fraction of the width).
    public var aspect: Double
    /// The whole field of view, degrees (180: the rim is the horizon).
    public var fieldOfView: Double

    public static let defaultFieldOfView = 180.0

    public init(circle: FisheyeCircle, aspect: Double = 1, fieldOfView: Double = FisheyeLens.defaultFieldOfView) {
        self.circle = circle
        self.aspect = aspect
        self.fieldOfView = fieldOfView
    }

    /// θmax, radians.
    public var thetaMax: Double { radians(fieldOfView / 2) }

    /// Where a lens-frame direction lands in the picture (fractions of the
    /// width and height), or nil outside the field of view.
    public func point(for direction: SIMD3<Double>) -> CGPoint? {
        let len = simdLength(direction)
        guard len > 0 else { return nil }
        let d = direction / len
        let theta = acos(min(max(d.z, -1), 1))
        guard theta <= thetaMax + 1e-9 else { return nil }
        let rho = theta / thetaMax
        let s = (d.x * d.x + d.y * d.y).squareRoot()
        guard s > 1e-12 else { return CGPoint(x: circle.cx, y: circle.cy) }
        return CGPoint(
            x: circle.cx + rho * circle.r * d.x / s,
            y: circle.cy + rho * circle.r * aspect * d.y / s)
    }

    /// The lens-frame unit direction a picture point (fractions) sees, or
    /// nil outside the circle.
    public func direction(at p: CGPoint) -> SIMD3<Double>? {
        let x = (Double(p.x) - circle.cx) / circle.r
        let y = (Double(p.y) - circle.cy) / aspect / circle.r
        let rho = (x * x + y * y).squareRoot()
        guard rho <= 1 + 1e-9 else { return nil }
        guard rho > 1e-12 else { return SIMD3(0, 0, 1) }
        let theta = rho * thetaMax
        return SIMD3(sin(theta) * x / rho, sin(theta) * y / rho, cos(theta))
    }
}

/// Where a tile's virtual camera looks (degrees): pan ψ, tilt τ and its
/// horizontal field of view. Panorama and Quad use only `pan`.
public struct FisheyeAim: Sendable, Equatable {
    public var pan: Double
    public var tilt: Double
    public var fov: Double

    public static let minFOV = 20.0
    public static let maxFOV = 120.0

    public init(pan: Double, tilt: Double, fov: Double) {
        self.pan = pan
        self.tilt = tilt
        self.fov = fov
    }

    /// Where a virtual camera starts for a mount.
    public static func `default`(for mount: FisheyeMount) -> FisheyeAim {
        switch mount {
        case .ceiling: FisheyeAim(pan: 0, tilt: 45, fov: 90)
        case .desk: FisheyeAim(pan: 0, tilt: -45, fov: 90)
        case .wall: FisheyeAim(pan: 0, tilt: 0, fov: 90)
        }
    }

    /// Kept within the mount's range: pan wraps to -180..<180 (a wall: within
    /// ±90°), tilt within the visible half, field of view within 20°...120°.
    public func clamped(for mount: FisheyeMount) -> FisheyeAim {
        var a = self
        a.fov = min(max(fov.isFinite ? fov : 90, Self.minFOV), Self.maxFOV)
        let p = pan.isFinite ? pan : 0
        let t = tilt.isFinite ? tilt : 0
        switch mount {
        case .ceiling:
            a.pan = Self.wrap(p)
            a.tilt = min(max(t, 0), 90)
        case .desk:
            a.pan = Self.wrap(p)
            a.tilt = min(max(t, -90), 0)
        case .wall:
            a.pan = min(max(p, -90), 90)
            a.tilt = min(max(t, -80), 80)
        }
        return a
    }

    static func wrap(_ degrees: Double) -> Double {
        let x = (degrees + 180).truncatingRemainder(dividingBy: 360)
        return (x < 0 ? x + 360 : x) - 180
    }
}

/// One view of a dewarped tile: its rectangle in the tile (0...1) and how
/// directions map onto it.
public struct FisheyeView: Sendable, Equatable {
    public enum Projection: Sendable, Equatable {
        /// A virtual camera (straight lines stay straight): (ψ, τ) of the
        /// view's center and its horizontal field of view, degrees.
        case rectilinear(pan: Double, tilt: Double, fov: Double)
        /// A panorama strip: pan from `pan0` over `panSpan`, tilt from
        /// `tilt0` over `tiltSpan` (degrees; top left to bottom right).
        case cylindrical(pan0: Double, panSpan: Double, tilt0: Double, tiltSpan: Double)
    }

    public var rect: CGRect
    public var projection: Projection

    public init(rect: CGRect, projection: Projection) {
        self.rect = rect
        self.projection = projection
    }

    /// Rectilinear only: tan of half the horizontal and vertical field of
    /// view for a rectangle of `size` pixels.
    func tangents(fov: Double, size: CGSize) -> (h: Double, v: Double) {
        let h = tan(radians(fov) / 2)
        let aspect = size.width > 0 ? Double(size.height / size.width) : 1
        return (h, h * aspect)
    }

    /// The viewer-frame direction the point `t` (0...1 within this view)
    /// looks along; `size` is the view's size in pixels. The reference for
    /// what the shader computes.
    public func direction(at t: CGPoint, size: CGSize) -> SIMD3<Double> {
        switch projection {
        case .rectilinear(let pan, let tilt, let fov):
            let (f, r, d) = Self.basis(pan: pan, tilt: tilt)
            let (th, tv) = tangents(fov: fov, size: size)
            return f + (2 * Double(t.x) - 1) * th * r + (2 * Double(t.y) - 1) * tv * d
        case .cylindrical(let pan0, let panSpan, let tilt0, let tiltSpan):
            return viewerDirection(pan: pan0 + Double(t.x) * panSpan, tilt: tilt0 + Double(t.y) * tiltSpan)
        }
    }

    /// Forward, right and down of a virtual camera aimed at (ψ, τ).
    public static func basis(pan: Double, tilt: Double)
        -> (forward: SIMD3<Double>, right: SIMD3<Double>, down: SIMD3<Double>)
    {
        let p = radians(pan)
        let f = viewerDirection(pan: pan, tilt: tilt)
        let r = SIMD3(cos(p), 0, -sin(p))
        return (f, r, cross(f, r))
    }

    /// The views of a tile `size` pixels big, in a mode, for a mount and aim.
    /// Original has none.
    public static func views(
        mode: FisheyeViewMode, mount: FisheyeMount, aim: FisheyeAim, size: CGSize
    ) -> [FisheyeView] {
        let w = max(Double(size.width), 1), h = max(Double(size.height), 1)
        let clamp = { (x: Double, lo: Double, hi: Double) in min(max(x, lo), hi) }
        let whole = CGRect(x: 0, y: 0, width: 1, height: 1)
        switch (mode, mount) {
        case (.original, _):
            return []
        case (.panorama, .wall), (.doublePanorama, .wall):
            let span = clamp(180 * h / w, 30, 180)
            return [FisheyeView(rect: whole, projection: .cylindrical(pan0: -90, panSpan: 180, tilt0: -span / 2, tiltSpan: span))]
        case (.panorama, _):
            let span = clamp(360 * h / w, 30, 90)
            let tilt0 = mount == .ceiling ? 0 : -span
            return [FisheyeView(rect: whole, projection: .cylindrical(pan0: aim.pan - 180, panSpan: 360, tilt0: tilt0, tiltSpan: span))]
        case (.doublePanorama, _):
            let span = clamp(180 * (h / 2) / w, 30, 90)
            let tilt0 = mount == .ceiling ? 0 : -span
            return [
                FisheyeView(
                    rect: CGRect(x: 0, y: 0, width: 1, height: 0.5),
                    projection: .cylindrical(pan0: aim.pan - 180, panSpan: 180, tilt0: tilt0, tiltSpan: span)),
                FisheyeView(
                    rect: CGRect(x: 0, y: 0.5, width: 1, height: 0.5),
                    projection: .cylindrical(pan0: aim.pan, panSpan: 180, tilt0: tilt0, tiltSpan: span)),
            ]
        case (.quad, _):
            let aims: [(Double, Double, Double)] =
                switch mount {
                case .ceiling: [0, 90, 180, 270].map { (aim.pan + $0, 45, 90) }
                case .desk: [0, 90, 180, 270].map { (aim.pan + $0, -45, 90) }
                case .wall: [-67.5, -22.5, 22.5, 67.5].map { ($0, 0, 60) }
                }
            return aims.enumerated().map { i, a in
                FisheyeView(
                    rect: CGRect(x: Double(i % 2) * 0.5, y: Double(i / 2) * 0.5, width: 0.5, height: 0.5),
                    projection: .rectilinear(pan: AimWrap.pan(a.0, mount), tilt: a.1, fov: a.2))
            }
        case (.ptz, _):
            let a = aim.clamped(for: mount)
            return [FisheyeView(rect: whole, projection: .rectilinear(pan: a.pan, tilt: a.tilt, fov: a.fov))]
        }
    }

    /// The view containing the tile point `p` (0...1) and the point within it.
    public static func locate(_ p: CGPoint, in views: [FisheyeView]) -> (view: FisheyeView, t: CGPoint)? {
        // Edges count (inclusive, as in the shader); the first view wins.
        for v in views where p.x >= v.rect.minX && p.x <= v.rect.maxX && p.y >= v.rect.minY && p.y <= v.rect.maxY {
            return (v, CGPoint(x: (p.x - v.rect.minX) / v.rect.width, y: (p.y - v.rect.minY) / v.rect.height))
        }
        return nil
    }
}

enum AimWrap {
    static func pan(_ p: Double, _ mount: FisheyeMount) -> Double {
        mount == .wall ? p : FisheyeAim.wrap(p)
    }
}

/// A dewarped tile's whole setup, and the reference mapping from a tile
/// pixel to the picture point the shader samples.
public struct FisheyeDewarp: Sendable, Equatable {
    public var lens: FisheyeLens
    public var mount: FisheyeMount
    public var views: [FisheyeView]
    /// The tile's size in pixels (the views' aspect).
    public var tileSize: CGSize

    public init(lens: FisheyeLens, mount: FisheyeMount, views: [FisheyeView], tileSize: CGSize) {
        self.lens = lens
        self.mount = mount
        self.views = views
        self.tileSize = tileSize
    }

    public init(lens: FisheyeLens, mount: FisheyeMount, mode: FisheyeViewMode, aim: FisheyeAim, tileSize: CGSize) {
        self.init(
            lens: lens, mount: mount, views: FisheyeView.views(mode: mode, mount: mount, aim: aim, size: tileSize),
            tileSize: tileSize)
    }

    /// The viewer-frame direction the tile point `p` (0...1) shows, or nil
    /// where no view is.
    public func direction(atTile p: CGPoint) -> SIMD3<Double>? {
        guard let (v, t) = FisheyeView.locate(p, in: views) else { return nil }
        let size = CGSize(width: tileSize.width * v.rect.width, height: tileSize.height * v.rect.height)
        return v.direction(at: t, size: size)
    }

    /// The picture point (fractions) the tile point `p` samples, or nil when
    /// it draws black (no view there, or outside the lens).
    public func picturePoint(atTile p: CGPoint) -> CGPoint? {
        direction(atTile: p).flatMap { lens.point(for: mount.toLens($0)) }
    }

    /// The constants the shader reads (docs/RENDERING.md, "Shader constants").
    public var constants: DewarpConstants {
        var c = DewarpConstants()
        let r = lens.circle
        c.lens = SIMD4(Float(r.cx), Float(r.cy), Float(r.r), Float(r.r * lens.aspect))
        c.params = SIMD4(Float(lens.thetaMax), Float(min(views.count, DewarpConstants.maxViews)), 0, 0)
        let f = { (v: SIMD3<Double>, w: Double) in SIMD4(Float(v.x), Float(v.y), Float(v.z), Float(w)) }
        for (i, v) in views.prefix(DewarpConstants.maxViews).enumerated() {
            var row = DewarpConstants.View()
            row.rect = SIMD4(Float(v.rect.minX), Float(v.rect.minY), Float(v.rect.maxX), Float(v.rect.maxY))
            switch v.projection {
            case .rectilinear(let pan, let tilt, let fov):
                let (fw, rt, dn) = FisheyeView.basis(pan: pan, tilt: tilt)
                let size = CGSize(width: tileSize.width * v.rect.width, height: tileSize.height * v.rect.height)
                let (th, tv) = v.tangents(fov: fov, size: size)
                row.axisX = f(mount.toLens(rt), 0)
                row.axisY = f(mount.toLens(dn), th)
                row.axisZ = f(mount.toLens(fw), tv)
                row.extra = .zero
            case .cylindrical(let pan0, let panSpan, let tilt0, let tiltSpan):
                row.axisX = f(mount.toLens(SIMD3(1, 0, 0)), 1)
                row.axisY = f(mount.toLens(SIMD3(0, 1, 0)), radians(pan0))
                row.axisZ = f(mount.toLens(SIMD3(0, 0, 1)), radians(panSpan))
                row.extra = SIMD4(Float(radians(tilt0)), Float(radians(tiltSpan)), 0, 0)
            }
            c[view: i] = row
        }
        return c
    }

    /// What the shader computes for the tile point `p`, from the constants
    /// alone (the tests compare it with `picturePoint`).
    public static func shade(_ c: DewarpConstants, tile p: CGPoint) -> CGPoint? {
        let count = Int(c.params.y)
        for i in 0..<min(count, DewarpConstants.maxViews) {
            let v = c[view: i]
            let x0 = Double(v.rect.x), y0 = Double(v.rect.y), x1 = Double(v.rect.z), y1 = Double(v.rect.w)
            let px = Double(p.x), py = Double(p.y)
            guard px >= x0, px <= x1, py >= y0, py <= y1 else { continue }
            let t = SIMD2((px - x0) / (x1 - x0), (py - y0) / (y1 - y0))
            let ax = SIMD3(Double(v.axisX.x), Double(v.axisX.y), Double(v.axisX.z))
            let ay = SIMD3(Double(v.axisY.x), Double(v.axisY.y), Double(v.axisY.z))
            let az = SIMD3(Double(v.axisZ.x), Double(v.axisZ.y), Double(v.axisZ.z))
            let dir: SIMD3<Double>
            if v.axisX.w < 0.5 {
                dir = az + (2 * t.x - 1) * Double(v.axisY.w) * ax + (2 * t.y - 1) * Double(v.axisZ.w) * ay
            } else {
                let psi = Double(v.axisY.w) + t.x * Double(v.axisZ.w)
                let tau = Double(v.extra.x) + t.y * Double(v.extra.y)
                dir = cos(tau) * sin(psi) * ax + sin(tau) * ay + cos(tau) * cos(psi) * az
            }
            let d = dir / simdLength(dir)
            let theta = acos(min(max(d.z, -1), 1))
            guard theta <= Double(c.params.x) + 1e-6 else { return nil }
            let s = (d.x * d.x + d.y * d.y).squareRoot()
            let rho = theta / Double(c.params.x)
            guard s > 1e-9 else { return CGPoint(x: Double(c.lens.x), y: Double(c.lens.y)) }
            return CGPoint(
                x: Double(c.lens.x) + rho * Double(c.lens.z) * d.x / s,
                y: Double(c.lens.y) + rho * Double(c.lens.w) * d.y / s)
        }
        return nil
    }
}

// MARK: Interaction

extension FisheyeAim {
    /// After a drag of `delta` points in a view `viewSize` points big: the
    /// picture follows the pointer (pan for every dewarped mode, tilt too in
    /// Virtual PTZ), by the angle the pointer moved across the view.
    public func dragged(
        by delta: CGVector, viewSize: CGSize, mode: FisheyeViewMode, mount: FisheyeMount
    ) -> FisheyeAim {
        guard viewSize.width > 0, viewSize.height > 0 else { return self }
        var a = self
        let w = Double(viewSize.width), h = Double(viewSize.height)
        switch mode {
        case .original:
            return self
        case .ptz:
            let vfov = degrees(2 * atan(tan(radians(fov) / 2) * h / w))
            a.pan -= Double(delta.dx) / w * fov
            a.tilt -= Double(delta.dy) / h * vfov
        case .panorama, .doublePanorama:
            // A strip spans 360° (or 180° each, two strips) across the view.
            let span = mode == .panorama ? 360.0 : 180.0
            a.pan -= Double(delta.dx) / w * span
        case .quad:
            a.pan -= Double(delta.dx) / (w / 2) * 90
        }
        return a.clamped(for: mount)
    }

    /// Zoomed by `factor` (more than 1 zooms in: a narrower field of view).
    public func zoomed(by factor: Double, mount: FisheyeMount) -> FisheyeAim {
        guard factor.isFinite, factor > 0 else { return self }
        var a = self
        a.fov = fov / factor
        return a.clamped(for: mount)
    }
}

/// Eases each key's virtual-camera field of view toward a target in
/// logarithmic steps on the stage's clock (the same time-based easing as
/// digital zoom: it finishes however few refreshes there are and ends
/// exactly on the target).
public struct FisheyeZoomEasing<Key: Hashable & Sendable>: Sendable {
    public var timeConstant: Double
    public private(set) var targets: [Key: Double] = [:]
    private var last: Double?

    public init(timeConstant: Double = 0.06) { self.timeConstant = timeConstant }

    public var isEasing: Bool { !targets.isEmpty }

    public func target(for key: Key) -> Double? { targets[key] }

    public mutating func set(fov: Double, for key: Key, now: Double) {
        if targets.isEmpty { last = now }
        targets[key] = min(max(fov, FisheyeAim.minFOV), FisheyeAim.maxFOV)
    }

    public mutating func cancel(_ key: Key) { targets[key] = nil }

    /// One step at `now`: the new field of view of every key being eased.
    public mutating func tick(now: Double, current: (Key) -> Double) -> [(key: Key, fov: Double)] {
        guard !targets.isEmpty else {
            last = nil
            return []
        }
        let dt = max(now - (last ?? now), 0)
        last = now
        var out: [(key: Key, fov: Double)] = []
        for (key, target) in targets {
            let fov = TileZoom.eased(from: current(key), to: target, dt: dt, timeConstant: timeConstant)
            out.append((key, fov))
            if fov == target { targets[key] = nil }
        }
        if targets.isEmpty { last = nil }
        return out
    }
}

// MARK: The tile's saved choice

/// A tile's fisheye view as saved with the layout: the leaf's optional
/// `fisheye` member, `{"view": "ptz", "pan": 12.5, "tilt": 45, "fov": 90}`
/// (no member: Original).
public struct FisheyeTileSetting: Sendable, Equatable {
    public var mode: FisheyeViewMode
    /// Nil until the user moves the view (the mount's default then applies).
    public var aim: FisheyeAim?

    public static let original = FisheyeTileSetting(mode: .original, aim: nil)

    public init(mode: FisheyeViewMode, aim: FisheyeAim? = nil) {
        self.mode = mode
        self.aim = aim
    }

    /// The aim in effect for a mount.
    public func aim(for mount: FisheyeMount) -> FisheyeAim {
        (aim ?? FisheyeAim.default(for: mount)).clamped(for: mount)
    }

    var json: JSONValue? {
        guard mode != .original || aim != nil else { return nil }
        var o: [String: JSONValue] = ["view": .string(mode.rawValue)]
        if let aim {
            o["pan"] = .number(aim.pan)
            o["tilt"] = .number(aim.tilt)
            o["fov"] = .number(aim.fov)
        }
        return .object(o)
    }

    init(json: JSONValue?) {
        guard case .object(let o)? = json else {
            self = .original
            return
        }
        func number(_ k: String) -> Double? {
            switch o[k] {
            case .integer(let i)?: Double(i)
            case .number(let d)?: d
            default: nil
            }
        }
        if case .string(let s)? = o["view"] { mode = FisheyeViewMode(rawValue: s) ?? .original } else { mode = .original }
        if let p = number("pan"), let t = number("tilt"), let f = number("fov") {
            aim = FisheyeAim(pan: p, tilt: t, fov: f)
        } else {
            aim = nil
        }
    }
}

extension LayoutNode {
    private static let fisheyeKey = "fisheye"

    /// The fisheye view of the tile at this leaf (Original for a split, no
    /// member, or a value this app does not know).
    public var fisheye: FisheyeTileSetting {
        guard case .leaf(_, let extras) = self else { return .original }
        return FisheyeTileSetting(json: extras.node[Self.fisheyeKey])
    }

    /// The tree with the leaf at `path` set to `setting` (Original without a
    /// moved view removes the member). Nil if `path` is not a leaf.
    public func settingFisheye(_ setting: FisheyeTileSetting, at path: NodePath) -> LayoutNode? {
        guard case .leaf(let camera, var extras) = self[path] else { return nil }
        extras.node[Self.fisheyeKey] = setting.json
        return replacing(at: path, with: .leaf(camera, extras: extras))
    }
}

// MARK: Finding the circle

/// Finds the image circle in a picture's luma plane (docs/RENDERING.md,
/// "Finding the circle").
public enum FisheyeCircleDetector {
    /// Rays cast from the center.
    public static let rays = 180
    /// Bright samples in a row that mark the rim.
    static let run = 4
    /// Above the border level, 8-bit codes.
    static let margin = 20

    /// The circle in a `width` x `height` luma plane read through `luma(x, y)`
    /// (8-bit), or nil when no clear circle is there.
    public static func detect(width: Int, height: Int, luma: (Int, Int) -> UInt8) -> FisheyeCircle? {
        guard width >= 16, height >= 16 else { return nil }
        let short = min(width, height)
        // Border level: the 10th percentile of the four corner squares.
        let m = max(4, short * 4 / 100)
        var corner: [UInt8] = []
        corner.reserveCapacity(4 * m * m)
        for (x0, y0) in [(0, 0), (width - m, 0), (0, height - m), (width - m, height - m)] {
            for y in y0..<(y0 + m) { for x in x0..<(x0 + m) { corner.append(luma(x, y)) } }
        }
        corner.sort()
        let threshold = Int(corner[corner.count / 10]) + margin
        // Rim points: inward along each ray from the edge.
        let cx = Double(width - 1) / 2, cy = Double(height - 1) / 2
        let stop = 0.1 * Double(short)
        var xs: [Double] = [], ys: [Double] = []
        xs.reserveCapacity(rays)
        ys.reserveCapacity(rays)
        for k in 0..<rays {
            let a = 2 * Double.pi * Double(k) / Double(rays)
            let dx = cos(a), dy = sin(a)
            let tx = dx > 1e-9 ? (Double(width - 1) - cx) / dx : dx < -1e-9 ? -cx / dx : .infinity
            let ty = dy > 1e-9 ? (Double(height - 1) - cy) / dy : dy < -1e-9 ? -cy / dy : .infinity
            var r = min(tx, ty).rounded(.down)
            var count = 0
            while r > stop {
                let x = min(max(Int((cx + r * dx).rounded()), 0), width - 1)
                let y = min(max(Int((cy + r * dy).rounded()), 0), height - 1)
                if Int(luma(x, y)) >= threshold {
                    count += 1
                    if count >= run {
                        let rim = r + Double(run - 1)
                        xs.append(cx + rim * dx)
                        ys.append(cy + rim * dy)
                        break
                    }
                } else {
                    count = 0
                }
                r -= 1
            }
        }
        guard var fit = fitCircle(xs, ys) else { return nil }
        var keptX = xs, keptY = ys
        var median = 0.0
        for _ in 0..<3 {
            let residuals = zip(keptX, keptY).map { abs(hypot($0 - fit.x, $1 - fit.y) - fit.r) }
            median = residuals.sorted()[residuals.count / 2]
            let limit = max(3 * 1.4826 * median, 1)
            let keep = residuals.indices.filter { residuals[$0] <= limit }
            keptX = keep.map { keptX[$0] }
            keptY = keep.map { keptY[$0] }
            guard keptX.count >= 3, let f = fitCircle(keptX, keptY) else { return nil }
            fit = f
        }
        let residuals = zip(keptX, keptY).map { abs(hypot($0 - fit.x, $1 - fit.y) - fit.r) }
        median = residuals.sorted()[residuals.count / 2]
        let w = Double(width), h = Double(height)
        guard keptX.count >= rays / 2,
            median <= 0.01 * w,
            fit.r >= 0.25 * Double(short), fit.r <= 0.75 * Double(short),
            fit.x >= 0.25 * w, fit.x <= 0.75 * w, fit.y >= 0.25 * h, fit.y <= 0.75 * h
        else { return nil }
        return FisheyeCircle(cx: (fit.x + 0.5) / w, cy: (fit.y + 0.5) / h, r: fit.r / w)
    }

    /// Algebraic least squares (x² + y² + Dx + Ey + F = 0).
    static func fitCircle(_ xs: [Double], _ ys: [Double]) -> (x: Double, y: Double, r: Double)? {
        guard xs.count >= 3 else { return nil }
        // Centered for conditioning.
        let n = Double(xs.count)
        let mx = xs.reduce(0, +) / n, my = ys.reduce(0, +) / n
        var suu = 0.0, svv = 0.0, suv = 0.0, suuu = 0.0, svvv = 0.0, suvv = 0.0, svuu = 0.0
        for (x, y) in zip(xs, ys) {
            let u = x - mx, v = y - my
            suu += u * u
            svv += v * v
            suv += u * v
            suuu += u * u * u
            svvv += v * v * v
            suvv += u * v * v
            svuu += v * u * u
        }
        let det = suu * svv - suv * suv
        guard abs(det) > 1e-9 else { return nil }
        let b1 = (suuu + suvv) / 2, b2 = (svvv + svuu) / 2
        let uc = (b1 * svv - b2 * suv) / det
        let vc = (b2 * suu - b1 * suv) / det
        let r2 = uc * uc + vc * vc + (suu + svv) / n
        guard r2 > 0 else { return nil }
        return (uc + mx, vc + my, r2.squareRoot())
    }
}

/// Finds a camera's circle from its first decoded pictures: one try per
/// picture at most every `interval` seconds; after `maxTries` failures the
/// inscribed circle, and no more tries (docs/RENDERING.md).
public struct FisheyeCircleTracker: Sendable {
    public static let interval = 0.5
    public static let maxTries = 8

    public private(set) var circle: FisheyeCircle?
    public private(set) var tries = 0
    /// The result is the fallback, not a measurement.
    public private(set) var fellBack = false
    private var lastTry: Double?

    public init() {}

    /// Whether a picture arriving at `now` should be measured.
    public func wantsTry(now: Double) -> Bool {
        guard circle == nil else { return false }
        guard let lastTry else { return true }
        return now - lastTry >= Self.interval
    }

    /// The outcome of a try at `now` on a `width` x `height` picture.
    public mutating func record(_ found: FisheyeCircle?, now: Double, width: Int, height: Int) {
        guard circle == nil else { return }
        lastTry = now
        tries += 1
        if let found {
            circle = found
        } else if tries >= Self.maxTries {
            circle = .inscribed(width: width, height: height)
            fellBack = true
        }
    }
}
