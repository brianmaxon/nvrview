import CoreGraphics
import Foundation

// The viewer's input decisions, pure so they are tested headless
// (docs/UX.md "Keyboard", "Full screen chrome", "Digital zoom"): which tile
// an arrow key moves to, which key does what where, when the chrome hides,
// and the digital zoom's arithmetic.

// MARK: Spatial navigation

/// An arrow key's direction, in layout coordinates (origin top left).
public enum MoveDirection: Sendable, CaseIterable {
    case left, right, up, down
}

extension LayoutFrames {
    /// The tile an arrow key moves to from `path`, by position: only tiles
    /// whose center lies beyond this tile's edge in `direction` count. Of
    /// those, the ones that overlap this tile across the direction (its
    /// neighbors on that side) come first, nearest edge first, then the one
    /// across from this tile's center (the line through the center crosses
    /// it, or passes nearest); failing any, the nearest center,
    /// counting the offset across the direction twice. Nil at the edge of
    /// the layout.
    public func neighbor(of path: NodePath, _ direction: MoveDirection) -> NodePath? {
        guard let from = leaf(path)?.rect else { return nil }
        let epsilon: CGFloat = 0.5
        // Ranked by (overlaps, edge gap or score, off-center); smaller is better.
        var best: (overlaps: Bool, primary: CGFloat, secondary: CGFloat, path: NodePath)?
        for l in leaves where l.path != path {
            let r = l.rect
            let beyond: Bool, gap: CGFloat, along: CGFloat, across: CGFloat, overlap: CGFloat, offCenter: CGFloat
            switch direction {
            case .left, .right:
                beyond = direction == .left ? r.midX < from.minX + epsilon : r.midX > from.maxX - epsilon
                gap = direction == .left ? from.minX - r.maxX : r.minX - from.maxX
                along = abs(r.midX - from.midX)
                across = abs(r.midY - from.midY)
                overlap = min(r.maxY, from.maxY) - max(r.minY, from.minY)
                offCenter = max(r.minY - from.midY, from.midY - r.maxY, 0)
            case .up, .down:
                beyond = direction == .up ? r.midY < from.minY + epsilon : r.midY > from.maxY - epsilon
                gap = direction == .up ? from.minY - r.maxY : r.minY - from.maxY
                along = abs(r.midY - from.midY)
                across = abs(r.midX - from.midX)
                overlap = min(r.maxX, from.maxX) - max(r.minX, from.minX)
                offCenter = max(r.minX - from.midX, from.midX - r.maxX, 0)
            }
            guard beyond else { continue }
            let overlaps = overlap > epsilon
            let candidate =
                overlaps
                ? (overlaps: true, primary: max(gap, 0).rounded(), secondary: offCenter, path: l.path)
                : (overlaps: false, primary: along + 2 * across, secondary: 0, path: l.path)
            if let b = best {
                if b.overlaps && !candidate.overlaps { continue }
                if b.overlaps == candidate.overlaps {
                    if b.primary < candidate.primary { continue }
                    if b.primary == candidate.primary && b.secondary <= candidate.secondary { continue }
                }
            }
            best = candidate
        }
        return best?.path
    }
}

// MARK: Keyboard

/// What a key does in a viewer window.
public enum ViewerShortcut: Sendable, Equatable {
    case toggleFullScreen
    case toggleSidebar
    case move(MoveDirection)
    case toggleFocus
    case resetZoom
    /// M: listen to the selected tile, or mute it if it is the one playing.
    case toggleListening
    /// Return or Enter: dismisses the selected tile's alert if it has one,
    /// else toggles focus (as before).
    case confirm
    /// P: save the selected tile's picture (docs/UX.md "Capture and camera keys").
    case savePicture
    /// Shift+P: Save Picture As….
    case savePictureAs
    /// R: start or stop recording the selected tile's camera.
    case toggleRecording
    /// T: start or end a conversation with the selected tile's camera.
    case toggleConversation
}

/// Where the keyboard focus is in the window, as far as shortcuts care.
public enum KeyContext: Sendable, Equatable {
    /// A text field or text view is being edited: no shortcut fires.
    case textEditing
    /// A list or control in the sidebar: it keeps its own arrows, Space and
    /// Return; the letter and digit shortcuts still act on the window.
    case sidebar
    /// The stage, or nothing in particular.
    case stage
}

/// The modifier keys that matter to single-key shortcuts.
public struct KeyModifiers: OptionSet, Sendable {
    public let rawValue: Int
    public init(rawValue: Int) { self.rawValue = rawValue }
    public static let shift = KeyModifiers(rawValue: 1)
    public static let control = KeyModifiers(rawValue: 2)
    public static let option = KeyModifiers(rawValue: 4)
    public static let command = KeyModifiers(rawValue: 8)
}

/// What Tab and Esc do while the stage has alert insets (docs/UX.md
/// "Alert insets"): Tab swaps the top inset with the stage (or back), Esc
/// dismisses it. Without insets the keys go on as before.
public enum InsetKey: Sendable, Equatable {
    case swap
    case dismiss
}

public enum ViewerKeys {
    /// macOS virtual key codes used here.
    public static let leftArrow: UInt16 = 123
    public static let rightArrow: UInt16 = 124
    public static let downArrow: UInt16 = 125
    public static let upArrow: UInt16 = 126
    public static let space: UInt16 = 49
    public static let returnKey: UInt16 = 36
    public static let enter: UInt16 = 76
    public static let tab: UInt16 = 48
    public static let escape: UInt16 = 53

    /// Tab or Esc for the insets: only on the stage (never while text is
    /// edited or a sidebar list has the keyboard), without modifiers, not on
    /// key repeat.
    public static func insetKey(keyCode: UInt16, modifiers: KeyModifiers, isRepeat: Bool, context: KeyContext)
        -> InsetKey?
    {
        guard context == .stage, modifiers.isEmpty, !isRepeat else { return nil }
        switch keyCode {
        case tab: return .swap
        case escape: return .dismiss
        default: return nil
        }
    }

    /// The shortcut for a key press, or nil to let the key go on to
    /// whatever has the keyboard focus (and then the menus). Letters work
    /// in either case (Shift or Caps Lock); Command, Control or Option make
    /// it a menu key equivalent instead. Toggles ignore key repeat; arrows
    /// repeat.
    public static func shortcut(
        keyCode: UInt16, characters: String?, modifiers: KeyModifiers, isRepeat: Bool, context: KeyContext
    ) -> ViewerShortcut? {
        guard context != .textEditing, modifiers.isDisjoint(with: [.command, .control, .option]) else { return nil }
        let stage = context == .stage
        switch keyCode {
        case leftArrow: return stage ? .move(.left) : nil
        case rightArrow: return stage ? .move(.right) : nil
        case upArrow: return stage ? .move(.up) : nil
        case downArrow: return stage ? .move(.down) : nil
        case space: return stage && !isRepeat ? .toggleFocus : nil
        case returnKey, enter: return stage && !isRepeat ? .confirm : nil
        default: break
        }
        guard !isRepeat else { return nil }
        switch characters?.lowercased() {
        case "f": return .toggleFullScreen
        case "s": return .toggleSidebar
        case "0": return .resetZoom
        case "m": return .toggleListening
        // Shift picks Save Picture As; Caps Lock (no Shift) does not.
        case "p": return modifiers.contains(.shift) ? .savePictureAs : .savePicture
        case "r": return .toggleRecording
        case "t": return .toggleConversation
        default: return nil
        }
    }
}

// MARK: Chrome auto-hide

/// When the overlay chrome (headers, selection outline, zoom map, and in
/// full screen the pointer) hides: `delay` seconds after the last
/// interaction, back at the next one. Time is any monotonic clock.
public struct ChromeIdle: Sendable, Equatable {
    public var delay: Double
    public private(set) var visible = true
    public private(set) var lastInteraction: Double

    public init(delay: Double = 3, now: Double) {
        self.delay = delay
        lastInteraction = now
    }

    /// A pointer move, click, key or scroll. True if the chrome was hidden
    /// and must come back.
    public mutating func interact(now: Double) -> Bool {
        lastInteraction = now
        guard !visible else { return false }
        visible = true
        return true
    }

    /// Called now and then. True if the chrome must hide now.
    public mutating func tick(now: Double) -> Bool {
        guard visible, now - lastInteraction >= delay else { return false }
        visible = false
        return true
    }
}

// MARK: Digital zoom

/// How a picture sits in its tile at 1x, for the digital zoom: the picture's
/// size as a multiple of the tile's on each axis. Fitted (letterboxed) one
/// axis is 1 and the other at most 1; filled (cropped) one axis is 1 and the
/// other at least 1. `whole` is a picture exactly the tile's shape.
public struct ZoomBase: Sendable, Equatable {
    public var width: CGFloat
    public var height: CGFloat

    public static let whole = ZoomBase(width: 1, height: 1)

    public init(width: CGFloat, height: CGFloat) {
        self.width = width
        self.height = height
    }

    /// A `picture` (any unit; only its shape counts) fitted, or filled, in a
    /// `tile`.
    public init(picture: CGSize, tile: CGSize, fill: Bool) {
        guard picture.width > 0, picture.height > 0, tile.width > 0, tile.height > 0 else {
            self = .whole
            return
        }
        let sx = tile.width / picture.width, sy = tile.height / picture.height
        let k = fill ? max(sx, sy) : min(sx, sy)
        // The axis that touches the tile's edges is exactly 1.
        width = sx == k ? 1 : picture.width * k / tile.width
        height = sy == k ? 1 : picture.height * k / tile.height
    }
}

/// A tile's digital zoom (docs/UX.md "Digital zoom"): `scale` times the
/// picture's 1x size (1 shows the tile as it is unzoomed, fitted or filled),
/// and the `center`, the picture point at the tile's center as a fraction of
/// the full picture (0...1 on both axes, origin top left). Independent of
/// the picture's resolution, so the sub-stream and the main stream show the
/// same region.
///
/// The rule, per axis, with `b` the picture's 1x size as a fraction of the
/// tile (`ZoomBase`), `s` the scale and `c` the center: the picture is drawn
/// `L = s * b` tiles long, starting at `u0` (in tile units, 0...1):
///
/// - `L <= 1` (still smaller than the tile): centered, `u0 = (1 - L) / 2`;
///   the rest of the axis is bars.
/// - `L > 1`: `u0 = 0.5 - c * L`, clamped to `1 - L ... 0` so the picture
///   always covers the tile on that axis (panning stops at its edges).
///
/// The destination is the picture's span `u0 ... u0 + L` intersected with
/// the tile (`0 ... 1`), and the source the matching part of the picture,
/// `(d - u0) / L`. So zooming in grows the picture over the bars until it
/// fills each axis, then crops; at 1x the tile looks as it does unzoomed.
/// Unit points are positions in the tile (0...1, origin top left).
///
/// At 1x a filled picture (Fill Tiles) is longer than the tile on one axis,
/// so the same rule places its crop: `c` is the crop's center, dragging moves
/// it up to the picture's edges (`canPan`), and an unpanned crop is the
/// centered one (docs/UX.md "Fill Tiles"). On an axis the picture does not
/// overflow, `c` is always 0.5.
public struct TileZoom: Sendable, Equatable {
    public static let maxScale = 8.0
    public static let none = TileZoom(scale: 1, center: CGPoint(x: 0.5, y: 0.5))

    public var scale: Double
    public var center: CGPoint

    public init(scale: Double, center: CGPoint) {
        self.scale = scale
        self.center = center
    }

    public var isZoomed: Bool { scale > 1.0001 }

    /// The center is the picture's middle (an unmoved crop).
    public var isCentered: Bool { abs(center.x - 0.5) < 1e-6 && abs(center.y - 0.5) < 1e-6 }

    /// 1x and centered: nothing to remember.
    public var isDefault: Bool { !isZoomed && isCentered }

    /// Moved at 1x: a filled picture's crop dragged off center.
    public var isPanned: Bool { !isZoomed && !isCentered }

    /// Whether a drag moves the picture: it is longer than the tile on
    /// some axis (zoomed in, or filled at 1x and of another shape).
    public func canPan(in base: ZoomBase = .whole) -> Bool {
        guard base.width > 0, base.height > 0 else { return false }
        let s = CGFloat(min(max(scale, 1), Self.maxScale))
        return s * base.width > 1.0001 || s * base.height > 1.0001
    }

    /// A value read from a file: the scale to 1...maxScale, the center to
    /// the picture (0...1); what is not a number becomes the default.
    public var sanitized: TileZoom {
        let s = scale.isFinite ? min(max(scale, 1), Self.maxScale) : 1
        func unit(_ v: CGFloat) -> CGFloat { v.isFinite ? min(max(v, 0), 1) : 0.5 }
        return TileZoom(scale: s <= 1.0001 ? 1 : s, center: CGPoint(x: unit(center.x), y: unit(center.y)))
    }

    /// One axis: where the picture starts (tile units) and how long it is
    /// (tiles), for scale `s`, 1x size `b` and center `c`.
    private static func span(scale s: CGFloat, base b: CGFloat, center c: CGFloat) -> (
        start: CGFloat, length: CGFloat
    ) {
        let length = s * b
        guard length > 1 else { return ((1 - length) / 2, length) }
        return (min(max(0.5 - c * length, 1 - length), 0), length)
    }

    /// One axis's center clamped: 0.5 where the picture does not overflow.
    private static func clampedCenter(scale s: CGFloat, base b: CGFloat, center c: CGFloat) -> CGFloat {
        let (start, length) = span(scale: s, base: b, center: c)
        return length > 1 ? (0.5 - start) / length : 0.5
    }

    private func spans(_ base: ZoomBase) -> (
        x: (start: CGFloat, length: CGFloat), y: (start: CGFloat, length: CGFloat)
    ) {
        let s = CGFloat(min(max(scale, 1), Self.maxScale))
        return (
            Self.span(scale: s, base: base.width, center: center.x),
            Self.span(scale: s, base: base.height, center: center.y)
        )
    }

    /// Where the picture is drawn, as a fraction of the tile: the zoomed
    /// picture intersected with the tile.
    public func destination(in base: ZoomBase = .whole) -> CGRect {
        let (x, y) = spans(base)
        let x0 = max(x.start, 0), x1 = min(x.start + x.length, 1)
        let y0 = max(y.start, 0), y1 = min(y.start + y.length, 1)
        return CGRect(x: x0, y: y0, width: max(x1 - x0, 0), height: max(y1 - y0, 0))
    }

    /// The region of the picture shown (fraction of the full picture): the
    /// part under `destination`.
    public func visible(in base: ZoomBase = .whole) -> CGRect {
        let (x, y) = spans(base)
        guard x.length > 0, y.length > 0 else { return CGRect(x: 0, y: 0, width: 1, height: 1) }
        let d = destination(in: base)
        return CGRect(
            x: (d.minX - x.start) / x.length, y: (d.minY - y.start) / y.length,
            width: d.width / x.length, height: d.height / y.length)
    }

    /// The scale clamped to 1...maxScale and the center to where the
    /// picture covers the tile (centered on an axis it does not fill). At 1x
    /// a filled picture keeps its crop's center on the axis it overflows.
    public func clamped(in base: ZoomBase = .whole) -> TileZoom {
        var s = min(max(scale.isFinite ? scale : 1, 1), Self.maxScale)
        if s <= 1.0001 { s = 1 }
        guard base.width > 0, base.height > 0, center.x.isFinite, center.y.isFinite else { return .none }
        let k = CGFloat(s)
        return TileZoom(
            scale: s,
            center: CGPoint(
                x: Self.clampedCenter(scale: k, base: base.width, center: center.x),
                y: Self.clampedCenter(scale: k, base: base.height, center: center.y)))
    }

    /// The picture point (fraction of the full picture) under tile unit
    /// point `u`; outside 0...1 over a bar.
    public func picturePoint(atUnit u: CGPoint, in base: ZoomBase = .whole) -> CGPoint {
        let (x, y) = spans(base)
        guard x.length > 0, y.length > 0 else { return u }
        return CGPoint(x: (u.x - x.start) / x.length, y: (u.y - y.start) / y.length)
    }

    /// Zoomed to `newScale`, keeping picture point `q` under tile unit point
    /// `u` (zooming around the pointer), then clamped: on an axis the
    /// picture does not fill yet it stays centered instead. Back at 1x a
    /// filled picture's crop stays where the zoom left it.
    public func zoomed(to newScale: Double, keeping q: CGPoint, atUnit u: CGPoint, in base: ZoomBase = .whole)
        -> TileZoom
    {
        var s = min(max(newScale, 1), Self.maxScale)
        if s <= 1.0001 { s = 1 }
        guard base.width > 0, base.height > 0 else { return .none }
        let lx = CGFloat(s) * base.width, ly = CGFloat(s) * base.height
        let c = CGPoint(x: q.x - (u.x - 0.5) / lx, y: q.y - (u.y - 0.5) / ly)
        return TileZoom(scale: s, center: c).clamped(in: base)
    }

    /// Zoomed by `factor` around tile unit point `u`.
    public func zoomed(by factor: Double, atUnit u: CGPoint, in base: ZoomBase = .whole) -> TileZoom {
        zoomed(to: scale * factor, keeping: picturePoint(atUnit: u, in: base), atUnit: u, in: base)
    }

    /// Panned so the picture follows a drag of `delta` tile units (a drag to
    /// the right moves the picture right), clamped. Also at 1x, where a
    /// filled picture's crop moves.
    public func panned(byUnit delta: CGVector, in base: ZoomBase = .whole) -> TileZoom {
        guard canPan(in: base) else { return self }
        let z = clamped(in: base)
        let s = CGFloat(z.scale)
        let c = CGPoint(x: z.center.x - delta.dx / (s * base.width), y: z.center.y - delta.dy / (s * base.height))
        return TileZoom(scale: z.scale, center: c).clamped(in: base)
    }

    /// The scale reached after one step of `dt` seconds of easing from
    /// `current` toward `target` (in log space, so zooming in and out feel
    /// alike), snapping when close. Mouse-wheel notches set a target; this
    /// moves toward it at display rate.
    public static func eased(from current: Double, to target: Double, dt: Double, timeConstant: Double = 0.06)
        -> Double
    {
        guard current > 0, target > 0 else { return target }
        let k = 1 - exp(-max(dt, 0) / timeConstant)
        let next = exp(log(current) + (log(target) - log(current)) * k)
        return abs(log(next / target)) < 0.002 ? target : next
    }
}

/// Where one tile's picture is drawn and what part of it is sampled, in
/// pixels of the drawable: at 1x the layout's rectangle fitted (or filled);
/// zoomed, the picture grows over the bars until it fills the tile, then
/// crops (`TileZoom`).
public enum TilePlacement {
    /// - Parameters:
    ///   - rect: the tile in points.
    ///   - scaleX, scaleY: drawable pixels per point (they differ while the
    ///     drawable lags a resize and is stretched to the view).
    ///   - picture: the picture's layout size in pixels (zero: no picture).
    ///   - offset: added to the source (a buffer at its full coded size).
    public static func place(
        rect: CGRect, scaleX: CGFloat, scaleY: CGFloat, picture: CGSize, offset: CGPoint = .zero, fill: Bool,
        zoom: TileZoom = .none
    ) -> (destination: CGRect, source: CGRect?) {
        let cell = rect.pixels(scaleX: scaleX, scaleY: scaleY)
        guard picture.width > 0, picture.height > 0 else { return (cell, nil) }
        // Placed where the picture appears (even scale), then mapped into
        // the possibly stretched drawable, so the picture keeps its shape
        // while the drawable catches up with a resize.
        let stretch = scaleY / scaleX
        let shown = abs(stretch - 1) < 0.001 ? cell : rect.pixels(scale: scaleX)
        var destination: CGRect
        let source: CGRect
        if zoom.isZoomed || (fill && !zoom.isCentered) {
            let base = ZoomBase(picture: picture, tile: shown.size, fill: fill)
            let d = zoom.destination(in: base), v = zoom.visible(in: base)
            destination = CGRect(
                x: shown.minX + d.minX * shown.width, y: shown.minY + d.minY * shown.height,
                width: d.width * shown.width, height: d.height * shown.height)
            source = CGRect(
                x: v.minX * picture.width, y: v.minY * picture.height,
                width: v.width * picture.width, height: v.height * picture.height)
        } else {
            let fit = PictureFit.fit(picture: picture, in: shown, fill: fill)
            destination = fit.destination
            source = fit.source
        }
        if shown != cell {
            destination = CGRect(
                x: destination.minX, y: destination.minY * stretch,
                width: destination.width, height: destination.height * stretch)
        }
        return (destination, source.offsetBy(dx: offset.x, dy: offset.y))
    }

    /// Where the picture is drawn in the tile `rect`, in points, with
    /// `zoom` (the zoom map sits in it).
    public static func shownRect(rect: CGRect, picture: CGSize, fill: Bool, zoom: TileZoom = .none) -> CGRect {
        guard picture.width > 0, picture.height > 0 else { return rect }
        guard zoom.isZoomed || (fill && !zoom.isCentered) else {
            // The fit rounds to whole units; in points that is close enough.
            return PictureFit.fit(picture: picture, in: rect, fill: fill).destination
        }
        let d = zoom.destination(in: ZoomBase(picture: picture, tile: rect.size, fill: fill))
        return CGRect(
            x: rect.minX + d.minX * rect.width, y: rect.minY + d.minY * rect.height,
            width: d.width * rect.width, height: d.height * rect.height)
    }

    /// The unit point of `point` in `rect` (a tile), clamped to it.
    public static func unit(_ point: CGPoint, in rect: CGRect) -> CGPoint {
        guard rect.width > 0, rect.height > 0 else { return CGPoint(x: 0.5, y: 0.5) }
        return CGPoint(
            x: min(max((point.x - rect.minX) / rect.width, 0), 1),
            y: min(max((point.y - rect.minY) / rect.height, 0), 1))
    }
}
