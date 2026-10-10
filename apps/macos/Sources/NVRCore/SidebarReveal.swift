import Foundation

/// When the collapsed sidebar slides over the stage and when it leaves again
/// (docs/UX.md "Sidebar"). Pure state with the time passed in, so tests drive
/// it with manual ticks; the window arms one timer for `deadline` and calls
/// `tick` when it fires (nothing runs while the pointer is elsewhere).
public struct SidebarReveal: Equatable, Sendable {
    /// Things that keep the overlay up although the pointer is outside it.
    public enum Hold: Hashable, Sendable {
        /// A drag from the overlay (or any press) is in progress.
        case drag
        /// A context menu is open.
        case menu
        /// A text field in the overlay is being edited.
        case editing
    }

    /// How long the pointer rests at the edge before the overlay slides in.
    public var dwell: TimeInterval = 0.15
    /// How long after the pointer leaves the overlay slides away.
    public var hideDelay: TimeInterval = 0.5
    /// How soon to look again while a hold keeps the overlay up.
    public var recheck: TimeInterval = 0.25

    /// Whether the overlay is visible (or sliding in).
    public private(set) var isShown = false
    /// False while the sidebar is docked: nothing is revealed then.
    public private(set) var isEnabled = true
    private var dwellDeadline: TimeInterval?
    private var hideDeadline: TimeInterval?

    public init() {}

    /// When `tick` next has something to do; nil when idle.
    public var deadline: TimeInterval? {
        [dwellDeadline, hideDeadline].compactMap(\.self).min()
    }

    /// The sidebar was docked (`false`) or collapsed (`true`). Docking hides
    /// the overlay at once.
    public mutating func setEnabled(_ enabled: Bool) {
        isEnabled = enabled
        if !enabled {
            isShown = false
            dwellDeadline = nil
            hideDeadline = nil
        }
    }

    /// The pointer is now at the stage's left edge strip and/or over the
    /// overlay. Call on every enter and exit.
    public mutating func pointer(inEdge: Bool, inOverlay: Bool, now: TimeInterval) {
        guard isEnabled else { return }
        if isShown {
            if inEdge || inOverlay {
                hideDeadline = nil
            } else if hideDeadline == nil {
                hideDeadline = now + hideDelay
            }
        } else if inEdge {
            if dwellDeadline == nil { dwellDeadline = now + dwell }
        } else {
            dwellDeadline = nil
        }
    }

    /// Runs what is due at `now`; true if `isShown` changed.
    @discardableResult
    public mutating func tick(now: TimeInterval, holds: Set<Hold> = []) -> Bool {
        guard isEnabled else { return false }
        if let d = dwellDeadline, now >= d {
            dwellDeadline = nil
            if !isShown {
                isShown = true
                return true
            }
        }
        if isShown, let d = hideDeadline, now >= d {
            if holds.isEmpty {
                hideDeadline = nil
                isShown = false
                return true
            }
            hideDeadline = now + recheck
        }
        return false
    }
}
