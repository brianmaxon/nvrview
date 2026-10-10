import CoreGraphics
import Foundation

/// Decides when a Metal layer's drawable follows its view's size.
///
/// Changing `drawableSize` makes the layer allocate new drawables, and doing
/// it on every step of a resize animation (full screen, a window resize, a
/// divider drag) costs a reallocation per frame and can stall presentation.
/// Instead the view reports every new size here and keeps presenting the
/// drawable it has, which the layer scales to its bounds in the meantime;
/// this policy hands back a size to apply:
///
/// - at once, when the drawable has not changed for `minInterval` (a single
///   resize, or the first step of an animation);
/// - during a run of changes, at most once per `minInterval`;
/// - and as soon as the size has stayed the same for `settle`, so the
///   picture is sharp again right after the animation ends.
///
/// Pure value logic: the caller supplies the time (seconds, monotonic).
public struct DrawableSizePolicy: Equatable, Sendable {
    /// The shortest time between two drawable size changes during a resize.
    public var minInterval: TimeInterval
    /// How long the wanted size must stay put to count as settled.
    public var settle: TimeInterval

    /// The size the drawable has now (what was last applied).
    public private(set) var applied: CGSize?
    /// The size the view wants, if it differs from `applied`.
    public private(set) var pending: CGSize?
    private var appliedAt: TimeInterval = -.infinity
    private var pendingSince: TimeInterval = 0

    public init(minInterval: TimeInterval = 0.1, settle: TimeInterval = 0.05) {
        self.minInterval = minInterval
        self.settle = settle
    }

    /// The view's size in pixels changed (or was set again). Returns the size
    /// to apply now, if any.
    public mutating func want(_ size: CGSize, now: TimeInterval) -> CGSize? {
        guard size.width > 0, size.height > 0 else { return nil }
        if size == applied {
            pending = nil
            return nil
        }
        if size != pending {
            pending = size
            pendingSince = now
        }
        return now - appliedAt >= minInterval ? apply(now: now) : nil
    }

    /// Called every display refresh. Returns the size to apply now, if any.
    public mutating func tick(now: TimeInterval) -> CGSize? {
        guard pending != nil else { return nil }
        if now - appliedAt >= minInterval || now - pendingSince >= settle {
            return apply(now: now)
        }
        return nil
    }

    private mutating func apply(now: TimeInterval) -> CGSize? {
        guard let size = pending else { return nil }
        applied = size
        appliedAt = now
        pending = nil
        return size
    }
}
