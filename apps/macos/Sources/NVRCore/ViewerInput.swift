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
    /// Return or Enter: dismisses the selected tile's alert if it has one,
    /// else toggles focus (as before).
    case confirm
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

public enum ViewerKeys {
    /// macOS virtual key codes used here.
    public static let leftArrow: UInt16 = 123
    public static let rightArrow: UInt16 = 124
    public static let downArrow: UInt16 = 125
    public static let upArrow: UInt16 = 126
    public static let space: UInt16 = 49
    public static let returnKey: UInt16 = 36
    public static let enter: UInt16 = 76

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

/// A tile's digital zoom, in picture coordinates: `scale` (1 shows the whole
/// fitted picture) and the `center` of the visible region as a fraction of
/// the full picture (0...1 on both axes, origin top left). Independent of
/// the picture's resolution, so the sub-stream and the main stream show the
/// same region.
///
/// `frame` arguments are the part of the picture shown at 1x, as a fraction
/// of the full picture: the whole picture when letterboxed, the centered
/// crop when filling the tile. Unit points are positions inside the shown
/// picture's rectangle on screen (0...1, origin top left).
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

    public static let wholeFrame = CGRect(x: 0, y: 0, width: 1, height: 1)

    /// The region shown, as a fraction of the full picture, inside `frame`.
    public func visible(in frame: CGRect = wholeFrame) -> CGRect {
        let s = CGFloat(min(max(scale, 1), Self.maxScale))
        let w = frame.width / s, h = frame.height / s
        let x = min(max(center.x - w / 2, frame.minX), frame.maxX - w)
        let y = min(max(center.y - h / 2, frame.minY), frame.maxY - h)
        return CGRect(x: x, y: y, width: w, height: h)
    }

    /// The scale clamped to 1...maxScale and the region kept inside `frame`.
    public func clamped(in frame: CGRect = wholeFrame) -> TileZoom {
        let s = min(max(scale, 1), Self.maxScale)
        guard s > 1.0001 else { return .none }
        let v = TileZoom(scale: s, center: center).visible(in: frame)
        return TileZoom(scale: s, center: CGPoint(x: v.midX, y: v.midY))
    }

    /// The picture point (fraction of the full picture) under unit point `u`.
    public func picturePoint(atUnit u: CGPoint, in frame: CGRect = wholeFrame) -> CGPoint {
        let v = visible(in: frame)
        return CGPoint(x: v.minX + u.x * v.width, y: v.minY + u.y * v.height)
    }

    /// Zoomed to `newScale`, keeping picture point `q` under unit point `u`
    /// (zooming around the pointer), then clamped.
    public func zoomed(to newScale: Double, keeping q: CGPoint, atUnit u: CGPoint, in frame: CGRect = wholeFrame)
        -> TileZoom
    {
        let s = min(max(newScale, 1), Self.maxScale)
        guard s > 1.0001 else { return .none }
        let w = frame.width / CGFloat(s), h = frame.height / CGFloat(s)
        let origin = CGPoint(x: q.x - u.x * w, y: q.y - u.y * h)
        return TileZoom(scale: s, center: CGPoint(x: origin.x + w / 2, y: origin.y + h / 2)).clamped(in: frame)
    }

    /// Zoomed by `factor` around unit point `u`.
    public func zoomed(by factor: Double, atUnit u: CGPoint, in frame: CGRect = wholeFrame) -> TileZoom {
        zoomed(to: scale * factor, keeping: picturePoint(atUnit: u, in: frame), atUnit: u, in: frame)
    }

    /// Panned so the picture follows a drag of `delta` units of the shown
    /// rectangle (a drag to the right moves the picture right), clamped.
    public func panned(byUnit delta: CGVector, in frame: CGRect = wholeFrame) -> TileZoom {
        guard isZoomed else { return self }
        let v = visible(in: frame)
        let c = CGPoint(x: v.midX - delta.dx * v.width, y: v.midY - delta.dy * v.height)
        return TileZoom(scale: scale, center: c).clamped(in: frame)
    }

    /// The source rectangle to sample, in pixels of a `picture`-sized
    /// picture, for the 1x source `fit` (also in pixels): `fit` itself at 1x,
    /// else the visible region of it.
    public func source(fit: CGRect, picture: CGSize) -> CGRect {
        guard isZoomed, picture.width > 0, picture.height > 0 else { return fit }
        let v = visible(in: Self.normalized(fit, picture: picture))
        return CGRect(
            x: v.minX * picture.width, y: v.minY * picture.height,
            width: v.width * picture.width, height: v.height * picture.height)
    }

    /// `rect` (pixels) as a fraction of `picture`.
    public static func normalized(_ rect: CGRect, picture: CGSize) -> CGRect {
        guard picture.width > 0, picture.height > 0 else { return wholeFrame }
        return CGRect(
            x: rect.minX / picture.width, y: rect.minY / picture.height,
            width: rect.width / picture.width, height: rect.height / picture.height)
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
/// pixels of the drawable: the layout's rectangle fitted (or filled), then
/// the zoom applied to the source only. The destination never changes with
/// the zoom.
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
        // Fitted where the picture appears (even scale), then mapped into
        // the possibly stretched drawable, so the picture keeps its shape
        // while the drawable catches up with a resize.
        let stretch = scaleY / scaleX
        let shown = abs(stretch - 1) < 0.001 ? cell : rect.pixels(scale: scaleX)
        let fit = PictureFit.fit(picture: picture, in: shown, fill: fill)
        let destination =
            shown == cell
            ? fit.destination
            : CGRect(
                x: fit.destination.minX, y: fit.destination.minY * stretch,
                width: fit.destination.width, height: fit.destination.height * stretch)
        let source = zoom.source(fit: fit.source, picture: picture).offsetBy(dx: offset.x, dy: offset.y)
        return (destination, source)
    }

    /// The shown picture's rectangle in points (for pointer positions).
    public static func shownRect(rect: CGRect, picture: CGSize, fill: Bool) -> CGRect {
        guard picture.width > 0, picture.height > 0 else { return rect }
        // The fit rounds to whole units; in points that is close enough.
        return PictureFit.fit(picture: picture, in: rect, fill: fill).destination
    }

    /// The unit point of `point` in `shown`, clamped to the rectangle.
    public static func unit(_ point: CGPoint, in shown: CGRect) -> CGPoint {
        guard shown.width > 0, shown.height > 0 else { return CGPoint(x: 0.5, y: 0.5) }
        return CGPoint(
            x: min(max((point.x - shown.minX) / shown.width, 0), 1),
            y: min(max((point.y - shown.minY) / shown.height, 0), 1))
    }
}
