import Foundation

// How evenly a playback tile's pictures were shown, measured against the
// pictures' own timestamps (docs/RENDERING.md, "Playback pacing"): the
// interval between two consecutive pictures is compared with the spacing of
// their timestamps; the difference is the jitter a viewer sees as judder.
// The same rules and test vectors as the Windows app's PacingStats.cs.

/// One stage's pacing over a measured stretch (milliseconds).
public struct PacingSummary: Equatable, Sendable, CustomStringConvertible {
    /// Pictures seen at this stage.
    public var pictures = 0
    /// Consecutive pairs compared (pairs across a discontinuity are left out).
    public var pairs = 0
    public var spacingP50 = 0.0
    public var intervalP50 = 0.0
    public var intervalP95 = 0.0
    public var intervalMax = 0.0
    /// |interval - timestamp spacing|.
    public var jitterP50 = 0.0
    public var jitterP95 = 0.0
    public var jitterMax = 0.0
    /// Pairs whose jitter is over `PacingStats.unevenMs`.
    public var uneven = 0

    public init() {}

    public var description: String {
        func f(_ v: Double) -> String { String(format: "%.1f", v) }
        return "\(pictures) pictures, spacing \(f(spacingP50)) ms, interval p50 \(f(intervalP50)) p95 \(f(intervalP95)) max \(f(intervalMax)) ms, "
            + "jitter p50 \(f(jitterP50)) p95 \(f(jitterP95)) max \(f(jitterMax)) ms, uneven \(uneven)"
    }
}

public enum PacingStats {
    /// Timestamps further apart than this (or going backwards) are a
    /// discontinuity (a seek, a gap in the recording), not a pacing error.
    public static let gapUs: Int64 = 1_000_000

    /// A pair whose jitter is above this is counted uneven: more than one and
    /// a half refreshes of a 60 Hz display.
    public static let unevenMs = 25.0

    /// The pacing of pictures handled at `atUs` (any monotonic clock, µs,
    /// ascending) whose timestamps are `ptsUs` (same order).
    public static func of(atUs: [Int64], ptsUs: [Int64]) -> PacingSummary {
        var s = PacingSummary()
        let n = min(atUs.count, ptsUs.count)
        s.pictures = n
        guard n >= 2 else { return s }
        var intervals: [Double] = [], spacings: [Double] = [], jitters: [Double] = []
        for i in 1..<n {
            let spacing = ptsUs[i] - ptsUs[i - 1]
            guard spacing > 0, spacing <= gapUs else { continue }
            let interval = Double(atUs[i] - atUs[i - 1]) / 1000
            let jitter = abs(interval - Double(spacing) / 1000)
            intervals.append(interval)
            spacings.append(Double(spacing) / 1000)
            jitters.append(jitter)
            if jitter > unevenMs { s.uneven += 1 }
        }
        intervals.sort()
        spacings.sort()
        jitters.sort()
        s.pairs = intervals.count
        s.spacingP50 = percentile(spacings, 50)
        s.intervalP50 = percentile(intervals, 50)
        s.intervalP95 = percentile(intervals, 95)
        s.intervalMax = intervals.last ?? 0
        s.jitterP50 = percentile(jitters, 50)
        s.jitterP95 = percentile(jitters, 95)
        s.jitterMax = jitters.last ?? 0
        return s
    }

    /// Pictures of `decodedPts` that never reached the screen: not in
    /// `shownPts` and within its range (one decoded after the last shown
    /// may still be shown).
    public static func skipped(decodedPts: [Int64], shownPts: [Int64]) -> Int {
        guard let first = shownPts.min(), let last = shownPts.max() else { return 0 }
        let shown = Set(shownPts)
        return decodedPts.filter { $0 > first && $0 < last && !shown.contains($0) }.count
    }

    /// The `p`th percentile of sorted values (nearest rank; 0 when empty).
    public static func percentile(_ sorted: [Double], _ p: Double) -> Double {
        guard !sorted.isEmpty else { return 0 }
        let rank = Int((p / 100 * Double(sorted.count)).rounded(.up))
        return sorted[min(max(rank - 1, 0), sorted.count - 1)]
    }
}
