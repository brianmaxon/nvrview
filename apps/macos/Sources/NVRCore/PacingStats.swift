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

    /// Consecutive pairs (as `of` compares them) whose jitter is over `thresholdMs`.
    public static func over(atUs: [Int64], ptsUs: [Int64], thresholdMs: Double) -> Int {
        let n = min(atUs.count, ptsUs.count)
        guard n >= 2 else { return 0 }
        var over = 0
        for i in 1..<n {
            let spacing = ptsUs[i] - ptsUs[i - 1]
            guard spacing > 0, spacing <= gapUs else { continue }
            let jitter = abs(Double(atUs[i] - atUs[i - 1]) / 1000 - Double(spacing) / 1000)
            if jitter > thresholdMs { over += 1 }
        }
        return over
    }
}

/// What an ideal display showed: when each shown picture was first drawn
/// (refresh times, µs), their timestamps, and how many pictures were
/// replaced by a newer one before any refresh drew them.
public struct RefreshShown: Equatable, Sendable {
    public var atUs: [Int64] = []
    public var ptsUs: [Int64] = []
    public var skipped = 0

    public init(atUs: [Int64] = [], ptsUs: [Int64] = [], skipped: Int = 0) {
        self.atUs = atUs
        self.ptsUs = ptsUs
        self.skipped = skipped
    }
}

/// The display stage on simulated time: at every refresh the newest decoded
/// picture is drawn, as both renderers do, so the self-test's verdict does
/// not depend on how many refreshes the real display (a window behind
/// others, a busy machine) happened to miss. The same rules and test vectors
/// as the Windows app's PacingRefresh.
public enum PacingRefresh {
    /// A 60 Hz display's refresh period (µs).
    public static let refresh60HzUs: Int64 = 16_667

    /// The pictures decoded at `decodedAtUs` (ascending) with timestamps
    /// `ptsUs`, shown on a display that refreshes at `phaseUs + k × refreshUs`.
    /// A picture decoded exactly at a refresh is drawn by it.
    public static func show(decodedAtUs: [Int64], ptsUs: [Int64], refreshUs: Int64 = refresh60HzUs, phaseUs: Int64 = 0) -> RefreshShown {
        let n = min(decodedAtUs.count, ptsUs.count)
        guard n > 0, refreshUs > 0 else { return RefreshShown() }
        var out = RefreshShown()
        // The first refresh at or after the first decode.
        var tick = phaseUs + Int64((Double(decodedAtUs[0] - phaseUs) / Double(refreshUs)).rounded(.up)) * refreshUs
        var next = 0, newest = -1
        var newestShown = true
        while next < n || !newestShown {
            while next < n, decodedAtUs[next] <= tick {
                if !newestShown { out.skipped += 1 }
                newest = next
                next += 1
                newestShown = false
            }
            if !newestShown {
                out.atUs.append(tick)
                out.ptsUs.append(ptsUs[newest])
                newestShown = true
            }
            tick += refreshUs
        }
        return out
    }
}

/// The bounds the self-tests' pacing steps apply to the shown pictures
/// (docs/RENDERING.md, "Playback pacing"). A display draws pictures on its
/// refresh grid, so a pair's jitter of one refresh is the grid alone. The
/// bounds leave room for that and for a rare late picture, and still fail
/// clearly on the regressions that matter: pictures decoded in bunches
/// (about half of them never shown, or shown in consecutive refreshes far
/// from their spacing). The same bounds as the Windows app's PacingVerdict.
public enum PacingVerdict {
    /// Jitter (ms) the shown pairs may have at the 95th percentile: one 60 Hz refresh plus a few ms.
    public static let jitterP95Ms = 20.0
    /// Largest jitter allowed for any shown pair, in refresh periods (plus `noiseRefreshes`).
    public static let maxRefreshes = 3.0
    /// A pair whose jitter is over this many refresh periods (plus `noiseRefreshes`) is late.
    public static let lateRefreshes = 2.0
    /// Room for timing noise on top of whole refreshes: a jitter of exactly
    /// two refreshes measures a little over or under it.
    public static let noiseRefreshes = 0.5
    /// Late pairs allowed (one late picture makes two: the interval before it and the one after).
    public static let lateAllowed = 4
    /// Share of the released pictures that must be shown.
    public static let shownRatio = 0.9
    /// Pictures that may be decoded and never shown.
    public static let skippedAllowed = 2

    /// What is wrong with the shown pictures (empty when they pass). Each
    /// problem starts with its key: "pairs", "p95", "max", "late", "shown",
    /// or "skipped".
    public static func problems(
        shownAtUs: [Int64], shownPtsUs: [Int64], released: Int, skipped: Int, refreshMs: Double, minPairs: Int
    ) -> [String] {
        var problems: [String] = []
        let s = PacingStats.of(atUs: shownAtUs, ptsUs: shownPtsUs)
        let max = (maxRefreshes + noiseRefreshes) * refreshMs, lateMs = (lateRefreshes + noiseRefreshes) * refreshMs
        let late = PacingStats.over(atUs: shownAtUs, ptsUs: shownPtsUs, thresholdMs: lateMs)
        let ratio = released > 0 ? Double(s.pictures) / Double(released) : 0
        func f(_ v: Double) -> String { String(format: "%.1f", v) }
        if s.pairs < minPairs { problems.append("pairs: only \(s.pairs) picture pairs were shown (at least \(minPairs))") }
        if s.jitterP95 > jitterP95Ms { problems.append("p95: jitter p95 \(f(s.jitterP95)) ms (at most \(Int(jitterP95Ms)) ms)") }
        if s.jitterMax > max {
            problems.append("max: jitter max \(f(s.jitterMax)) ms (at most \(Int(maxRefreshes)) refreshes, \(f(max)) ms)")
        }
        if late > lateAllowed {
            problems.append("late: \(late) pairs more than \(Int(lateRefreshes)) refreshes (\(f(lateMs)) ms) off their spacing (at most \(lateAllowed))")
        }
        if ratio < shownRatio {
            problems.append("shown: \(s.pictures) of \(released) released pictures were shown (at least \(Int(shownRatio * 100))%)")
        }
        if skipped > skippedAllowed {
            problems.append("skipped: \(skipped) decoded pictures were never shown (at most \(skippedAllowed))")
        }
        return problems
    }
}
