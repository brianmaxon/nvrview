import CoreGraphics
import Foundation
import QuartzCore
import Synchronization

// Time in the viewer: everything that moves or expires on its own (the focus
// animation, zoom easing, chrome auto-hide, header fade-out, switch notes)
// reads one injectable clock and advances in a step function, `tick(now:)`.
// The app's display link and a slow window timer call the step with the
// system clock; tests and the app's self-test advance a manual clock and
// call it directly, so nothing waits on real time.

/// Seconds on a monotonic clock.
public protocol ViewerClock: AnyObject, Sendable {
    var now: Double { get }
}

/// The host's monotonic clock (`CACurrentMediaTime`, the display link's).
public final class SystemViewerClock: ViewerClock {
    public init() {}
    public var now: Double { CACurrentMediaTime() }
}

/// A clock that moves only when told to (tests, the self-test).
public final class ManualViewerClock: ViewerClock {
    private let value: Mutex<Double>

    public init(now: Double = 0) { value = Mutex(now) }

    public var now: Double { value.withLock { $0 } }

    public func advance(by seconds: Double) {
        value.withLock { $0 += max(seconds, 0) }
    }
}

// MARK: Focus animation

/// The focus animation's progress (0: the grid, 1: the focused tile fills
/// the stage), linear in time over `duration`; `RectAnimation` eases it.
public struct FocusAnimation: Sendable, Equatable {
    public static let defaultDuration = 0.28

    /// What a step finished.
    public enum Finished: Sendable, Equatable {
        /// Reached 1: the tile is focused (time for the sharper stream).
        case focused
        /// Reached 0: back to the grid.
        case unfocused
    }

    public var duration: Double
    public private(set) var progress: Double = 0
    private var run: (from: Double, to: Double, start: Double)?

    public init(duration: Double = Self.defaultDuration) { self.duration = duration }

    public var isRunning: Bool { run != nil }

    /// Starts toward `target` (0 or 1) from wherever it is now.
    public mutating func start(to target: Double, now: Double) {
        run = (progress, target, now)
    }

    /// Stops at once at 0 (no animation, nothing reported).
    public mutating func reset() {
        run = nil
        progress = 0
    }

    /// Advances to `now`; reports the end when it is reached.
    public mutating func tick(now: Double) -> Finished? {
        guard let r = run else { return nil }
        // Ends at exactly `duration` (a clock advanced by it lands a hair
        // short in floating point).
        let t = duration > 0 && now - r.start < duration - 1e-9 ? (now - r.start) / duration : 1
        progress = r.from + (r.to - r.from) * min(max(t, 0), 1)
        guard t >= 1 else { return nil }
        run = nil
        progress = r.to
        return r.to >= 1 ? .focused : .unfocused
    }

    public static func == (a: FocusAnimation, b: FocusAnimation) -> Bool {
        a.duration == b.duration && a.progress == b.progress && a.run?.from == b.run?.from
            && a.run?.to == b.run?.to && a.run?.start == b.run?.start
    }
}

// MARK: Zoom easing

/// Mouse-wheel zoom easing toward a target scale, per key (a camera), around
/// a unit point of the tile. Time-based: a step after a long gap
/// (a window behind others gets few display refreshes) moves as far as the
/// time that passed, so the easing always finishes, and it ends exactly on
/// the target. How the picture sits in its tile at 1x (`ZoomBase`) is asked
/// for at every step, so a picture whose size or shape changes mid-ease
/// (the switch between the sub-stream and the main stream) is followed.
public struct ZoomEasing<Key: Hashable & Sendable>: Sendable {
    public struct Target: Sendable, Equatable {
        public var scale: Double
        public var unit: CGPoint
        /// The tile it was asked in, if any (its shape is used).
        public var tile: NodePath?

        public init(scale: Double, unit: CGPoint, tile: NodePath? = nil) {
            self.scale = scale
            self.unit = unit
            self.tile = tile
        }
    }

    public var timeConstant: Double
    public private(set) var targets: [Key: Target] = [:]
    private var last: Double?

    public init(timeConstant: Double = 0.06) { self.timeConstant = timeConstant }

    public var isEasing: Bool { !targets.isEmpty }

    public func target(for key: Key) -> Target? { targets[key] }

    /// Eases `key` toward `target` from the next step on.
    public mutating func set(_ target: Target, for key: Key, now: Double) {
        if targets.isEmpty { last = now }
        targets[key] = target
    }

    /// Stops easing `key` where it is (a pinch or a pan took over).
    public mutating func cancel(_ key: Key) {
        targets[key] = nil
    }

    /// One step at `now`: the new zoom of every key being eased, from its
    /// zoom now (`current`) and how its picture sits in the tile now
    /// (`base`). A key that
    /// reaches its target stops easing.
    public mutating func tick(
        now: Double, current: (Key) -> TileZoom, base: (Key, Target) -> ZoomBase
    ) -> [(key: Key, zoom: TileZoom)] {
        guard !targets.isEmpty else {
            last = nil
            return []
        }
        let dt = max(now - (last ?? now), 0)
        last = now
        var out: [(key: Key, zoom: TileZoom)] = []
        for (key, t) in targets {
            let z = current(key)
            let f = base(key, t)
            let scale = TileZoom.eased(from: z.scale, to: t.scale, dt: dt, timeConstant: timeConstant)
            let next =
                scale <= 1.0001
                ? TileZoom.none
                : z.zoomed(to: scale, keeping: z.picturePoint(atUnit: t.unit, in: f), atUnit: t.unit, in: f)
            out.append((key, next))
            if scale == t.scale || (t.scale <= 1 && !next.isZoomed) { targets[key] = nil }
        }
        if targets.isEmpty { last = nil }
        return out
    }
}
