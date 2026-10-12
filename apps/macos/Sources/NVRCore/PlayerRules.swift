import CoreGraphics
import Foundation

// The pure rules of the Recordings window's player (docs/UX.md
// "Recordings"): the speeds and what each one means, the transport's
// arithmetic, how up to four cameras share the player, how the window's
// panes give way in a narrow window, and the incident row's thumbnail
// strip. Views read these; unit tests pin them.

/// The playback speeds (FFI-RECORDINGS.md section 6, decision 3) and what
/// each one means for the picture and the sound.
public enum PlaybackSpeed {
    /// Every speed the core accepts, slowest first.
    public static let all: [Double] = [0.25, 0.5, 1, 2, 4, 8, 16]

    /// "0.25×", "1×", "16×".
    public static func label(_ rate: Double) -> String {
        let text = rate == rate.rounded() ? String(Int(rate)) : String(format: "%g", rate)
        return text + "×"
    }

    /// From 2× up, devices send keyframes only and no audio (protocol.md,
    /// "Rates, keyframes, and how a play ends").
    public static func keyframesOnly(_ rate: Double) -> Bool { rate >= 2 }

    /// Recorded sound plays at 1× only.
    public static func hasSound(_ rate: Double) -> Bool { rate == 1 }

    /// What the transport says next to the speed, or nil at 1×.
    public static func hint(_ rate: Double) -> String? {
        if keyframesOnly(rate) { return "Keyframes only, no sound" }
        if rate < 1 { return "No sound below 1×" }
        return nil
    }

    /// The next speed up or down, or nil at the end of the list.
    public static func faster(than rate: Double) -> Double? { all.first { $0 > rate } }
    public static func slower(than rate: Double) -> Double? { all.last { $0 < rate } }
}

/// The transport's view of one playback: where it is, in which range, and
/// what the buttons do.
public struct TransportState: Equatable, Sendable {
    /// The playback's range, device-local milliseconds.
    public var startMs: Int64
    public var endMs: Int64
    public var positionMs: Int64
    /// Nil before the playback reports one.
    public var state: PlaybackState?
    public var rate: Double

    /// How far the skip buttons move.
    public static let skipMs: Int64 = 10_000

    public init(startMs: Int64, endMs: Int64, positionMs: Int64, state: PlaybackState?, rate: Double) {
        self.startMs = startMs
        self.endMs = endMs
        self.positionMs = positionMs
        self.state = state
        self.rate = rate
    }

    /// Whether the clock runs (or waits for a camera to catch up).
    public var isPlaying: Bool { state == .playing || state == .buffering }

    /// The play button's action shows "play" (not "pause").
    public var showsPlay: Bool { !isPlaying }

    /// `positionMs` moved by `delta`, kept within the range.
    public func skipped(by delta: Int64) -> Int64 { clamp(positionMs + delta) }

    public func clamp(_ ms: Int64) -> Int64 { min(max(ms, startMs), endMs) }

    /// The position as a fraction of `[from, to]` (0 before, 1 after).
    public static func fraction(_ ms: Int64, from: Int64, to: Int64) -> Double {
        guard to > from else { return 0 }
        return min(max(Double(ms - from) / Double(to - from), 0), 1)
    }

    /// The time at `fraction` of `[from, to]`, whole milliseconds.
    public static func time(atFraction f: Double, from: Int64, to: Int64) -> Int64 {
        from + Int64((min(max(f, 0), 1) * Double(to - from)).rounded())
    }

    /// "07:42:05" for a device-local millisecond time.
    public static func clock(_ ms: Int64) -> String { DeviceTime.time(ms.floorDiv(1000)) }
}

extension Int64 {
    /// Division rounding toward minus infinity (times before 1970 stay whole seconds).
    public func floorDiv(_ d: Int64) -> Int64 { (self - ((self % d) + d) % d) / d }
}

/// How the player shows its cameras.
public enum PlayerGridMode: Equatable, Sendable {
    /// All cameras the same size: one, side by side, or two by two.
    case grid
    /// One camera large, the others small beside it.
    case emphasis(Int)
    /// One camera alone.
    case single(Int)
}

/// Where each of the player's cameras goes (at most four, decision of the
/// review mode in PLAYBACK.md).
public enum PlayerGrid {
    /// The most cameras a playback shows.
    public static let maxCameras = 4

    /// Each camera's cell in `bounds` (top-left origin), nil for one that is
    /// not shown (a single view shows only its camera). `gap` separates cells.
    public static func cells(count: Int, mode: PlayerGridMode, in bounds: CGRect, gap: CGFloat = 2) -> [CGRect?] {
        guard count > 0 else { return [] }
        let n = min(count, maxCameras)
        var out = [CGRect?](repeating: nil, count: count)
        switch normalized(mode, count: n) {
        case .single(let i):
            out[i] = bounds
        case .emphasis(let i):
            let side = (bounds.width - gap) / 4
            let big = CGRect(x: bounds.minX, y: bounds.minY, width: bounds.width - side - gap, height: bounds.height)
            out[i] = big
            let others = (0..<n).filter { $0 != i }
            // Small cells keep a picture's shape: at most a third of the height.
            let h = min((bounds.height - gap * CGFloat(max(others.count - 1, 0))) / CGFloat(max(others.count, 1)), bounds.height / 3)
            for (k, j) in others.enumerated() {
                out[j] = CGRect(x: big.maxX + gap, y: bounds.minY + CGFloat(k) * (h + gap), width: side, height: h)
            }
        case .grid:
            let (cols, rows) = n == 1 ? (1, 1) : n == 2 ? (2, 1) : (2, 2)
            let w = (bounds.width - gap * CGFloat(cols - 1)) / CGFloat(cols)
            let h = (bounds.height - gap * CGFloat(rows - 1)) / CGFloat(rows)
            for i in 0..<n {
                let (c, r) = (i % cols, i / cols)
                out[i] = CGRect(
                    x: bounds.minX + CGFloat(c) * (w + gap), y: bounds.minY + CGFloat(r) * (h + gap), width: w, height: h)
            }
        }
        return out
    }

    /// A mode that fits `count` cameras (an index past the end falls back
    /// to the grid; one camera is always the grid).
    public static func normalized(_ mode: PlayerGridMode, count: Int) -> PlayerGridMode {
        switch mode {
        case .grid: return .grid
        case .emphasis(let i), .single(let i):
            guard count > 1, i >= 0, i < count else { return .grid }
            return mode
        }
    }

    /// A click on camera `i`: makes it big, or back to the grid when it
    /// already is; a single view stays as it is.
    public static func click(_ mode: PlayerGridMode, on i: Int, count: Int) -> PlayerGridMode {
        switch normalized(mode, count: count) {
        case .grid: return count > 1 ? .emphasis(i) : .grid
        case .emphasis(let j): return j == i ? .grid : .emphasis(i)
        case .single: return mode
        }
    }

    /// A double click on camera `i` (or the one-camera button): that camera
    /// alone, or back to the grid from it.
    public static func doubleClick(_ mode: PlayerGridMode, on i: Int, count: Int) -> PlayerGridMode {
        guard count > 1 else { return .grid }
        if case .single(let j) = normalized(mode, count: count), j == i { return .grid }
        return .single(i)
    }

    /// Which camera's sound the player plays: the large or single one, else
    /// the first.
    public static func audibleIndex(_ mode: PlayerGridMode, count: Int) -> Int? {
        guard count > 0 else { return nil }
        switch normalized(mode, count: count) {
        case .grid: return 0
        case .emphasis(let i), .single(let i): return i
        }
    }

    /// The cameras of an incident the player shows: the first four, in the
    /// incident's order.
    public static func cameras<T>(_ all: [T]) -> [T] { Array(all.prefix(maxCameras)) }
}

/// How the Recordings window's panes share its width and the center its
/// height (docs/UX.md "Recordings"). The side panes have fixed sensible
/// widths; in a window too narrow for all three the inspector gives way
/// first, then the sidebar, so the player never gets squeezed or overlapped.
public enum RecordingsPanes {
    public static let sidebarWidth: CGFloat = 280
    public static let inspectorWidth: CGFloat = 290
    /// The least the player column may have.
    public static let centerMin: CGFloat = 440
    /// The window's least content size.
    public static let windowMin = CGSize(width: 720, height: 560)
    /// The transport bar under the player.
    public static let transportHeight: CGFloat = 92
    /// One timeline lane, and the hour ruler above them.
    public static let laneHeight: CGFloat = 26
    public static let rulerHeight: CGFloat = 18
    /// The least height of the player.
    public static let playerMin: CGFloat = 200

    /// Which side panes are shown in a window `width` points wide, given
    /// what the user asked for.
    public static func shown(width: CGFloat, sidebar: Bool, inspector: Bool) -> (sidebar: Bool, inspector: Bool) {
        var s = sidebar, i = inspector
        if s, i, width < sidebarWidth + inspectorWidth + centerMin { i = false }
        if s, width < sidebarWidth + centerMin { s = false }
        if i, !s, width < inspectorWidth + centerMin { i = false }
        return (s, i)
    }

    /// The timeline lanes' height in a center column `height` points tall:
    /// every lane if they fit in a third of it, else a third (they scroll),
    /// never less than two lanes.
    public static func lanesHeight(height: CGFloat, lanes: Int) -> CGFloat {
        let want = rulerHeight + CGFloat(max(lanes, 1)) * laneHeight + 8
        let cap = max((height - transportHeight) / 3, rulerHeight + 2 * laneHeight + 8)
        return min(want, cap)
    }
}

/// Where "Split Here" splits an incident: at the playhead's second, when it
/// lies strictly inside the incident (a split at its first or last second
/// would leave a part without events).
public enum IncidentSplit {
    public static func second(start: Int64, end: Int64, playheadMs: Int64) -> Int64? {
        let t = playheadMs.floorDiv(1000)
        return start < t && t < end ? t : nil
    }
}

/// The incident row's thumbnail strip: a fixed width whatever the number of
/// cameras, so every row's text starts at the same place.
public enum IncidentRowStrip {
    public static let thumbnail = CGSize(width: 64, height: 36)
    public static let spacing: CGFloat = 4
    /// Thumbnails shown; more cameras show as "+N".
    public static let maxShown = 2
    public static var width: CGFloat { CGFloat(maxShown) * thumbnail.width + CGFloat(maxShown - 1) * spacing }

    /// How many thumbnails a row of `count` cameras shows, and how many
    /// more it counts.
    public static func slots(_ count: Int) -> (shown: Int, more: Int) {
        let shown = min(max(count, 0), maxShown)
        return (shown, max(count - shown, 0))
    }
}
