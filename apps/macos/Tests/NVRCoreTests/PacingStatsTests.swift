import NVRCore
import Testing

/// PacingStats: the same vectors as the Windows PacingTests. Synthetic timestamps only.
@Suite struct PacingStatsTests {
    @Test func evenPicturesHaveNoJitter() {
        let s = PacingStats.of(
            atUs: [5_000, 105_000, 205_000, 305_000, 405_000], ptsUs: [0, 100_000, 200_000, 300_000, 400_000])
        #expect(s.pictures == 5 && s.pairs == 4)
        #expect(s.spacingP50 == 100 && s.intervalP50 == 100)
        #expect(s.jitterMax == 0 && s.uneven == 0)
    }

    @Test func aBunchedPairShowsAsJitter() {
        // The third picture is 60 ms late, so its interval is 160 ms and the next 40 ms.
        let s = PacingStats.of(
            atUs: [0, 100_000, 260_000, 300_000, 400_000], ptsUs: [0, 100_000, 200_000, 300_000, 400_000])
        #expect(s.jitterMax == 60 && s.intervalMax == 160)
        #expect(s.uneven == 2)
        #expect(s.jitterP95 == 60 && s.jitterP50 == 0)
    }

    @Test func aSeekOrGapIsNotAPacingError() {
        let s = PacingStats.of(
            atUs: [0, 100_000, 200_000, 300_000, 400_000, 500_000],
            ptsUs: [0, 100_000, 5_000_000, 5_100_000, 4_000_000, 4_100_000])
        #expect(s.pairs == 3 && s.jitterMax == 0)
    }

    @Test func fewerThanTwoPictures() {
        #expect(PacingStats.of(atUs: [], ptsUs: []).pairs == 0)
        #expect(PacingStats.of(atUs: [1], ptsUs: [1]).pictures == 1)
    }

    @Test func skippedPictures() {
        // 200 was skipped; 0 and 500 lie outside the shown range.
        #expect(PacingStats.skipped(decodedPts: [0, 100, 200, 300, 400, 500], shownPts: [100, 300, 400]) == 1)
        #expect(PacingStats.skipped(decodedPts: [0, 100], shownPts: []) == 0)
    }

    @Test func nearestRankPercentiles() {
        let v: [Double] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        #expect(PacingStats.percentile(v, 50) == 5)
        #expect(PacingStats.percentile(v, 95) == 10)
        #expect(PacingStats.percentile(v, 0) == 1)
        #expect(PacingStats.percentile([], 50) == 0)
    }

    @Test func pairsOverAThreshold() {
        let pts: [Int64] = [0, 100_000, 200_000, 300_000, 400_000]
        let at: [Int64] = [0, 100_000, 260_000, 300_000, 400_000]
        #expect(PacingStats.over(atUs: at, ptsUs: pts, thresholdMs: 50) == 2)
        #expect(PacingStats.over(atUs: at, ptsUs: pts, thresholdMs: 60) == 0)
    }

    @Test func theSimulatedDisplayDrawsTheNewestPictureAtEachRefresh() {
        let even = PacingRefresh.show(decodedAtUs: [0, 10_000, 20_000, 40_000], ptsUs: [0, 1, 2, 3], refreshUs: 16_667)
        #expect(even.atUs == [0, 16_667, 33_334, 50_001])
        #expect(even.ptsUs == [0, 1, 2, 3])
        #expect(even.skipped == 0)
        // Two pictures decoded between two refreshes: the older is never drawn.
        let bunched = PacingRefresh.show(decodedAtUs: [0, 5_000, 12_000, 20_000], ptsUs: [0, 1, 2, 3], refreshUs: 16_667)
        #expect(bunched.atUs == [0, 16_667, 33_334])
        #expect(bunched.ptsUs == [0, 2, 3])
        #expect(bunched.skipped == 1)
        // The refresh grid's phase.
        #expect(PacingRefresh.show(decodedAtUs: [1_000], ptsUs: [7], refreshUs: 16_667, phaseUs: 5_000).atUs == [5_000])
        #expect(PacingRefresh.show(decodedAtUs: [], ptsUs: [], refreshUs: 16_667).atUs.isEmpty)
    }

    // The pipeline on simulated time: pictures of a 10 fps stream that a
    // device sends in pairs (the second picture's time plus 5 ms), released
    // by the pacer at their own time or on arrival (the old bug), decoded
    // 3 ms after release one after another, shown by a 60 Hz display.
    static let n = 60
    static let pts: [Int64] = (0..<n).map { Int64($0) * 100_000 }
    static let pairArrivals: [Int64] = (0..<n).map { Int64($0 | 1) * 100_000 + 5_000 }

    static func decode(_ released: [Int64]) -> [Int64] {
        var d: [Int64] = []
        for (i, r) in released.enumerated() { d.append(Swift.max(r, i > 0 ? d[i - 1] : Int64.min) + 3_000) }
        return d
    }

    static func verdict(_ decoded: [Int64], _ pts: [Int64]) -> [String] {
        let s = PacingRefresh.show(decodedAtUs: decoded, ptsUs: pts, refreshUs: PacingRefresh.refresh60HzUs)
        return PacingVerdict.problems(
            shownAtUs: s.atUs, shownPtsUs: s.ptsUs, released: pts.count, skipped: s.skipped,
            refreshMs: Double(PacingRefresh.refresh60HzUs) / 1000, minPairs: 40)
    }

    static func has(_ problems: [String], _ key: String) -> Bool { problems.contains { $0.hasPrefix(key + ":") } }

    @Test func pacedPicturesPassOnTheSimulatedDisplay() {
        // The pacer's delay: the earliest picture relative to its time (5 ms) plus how much later the others came (100 ms).
        let released = zip(Self.pairArrivals, Self.pts).map { Swift.max($0, $1 + 105_000) }
        let problems = Self.verdict(Self.decode(released), Self.pts)
        #expect(problems.isEmpty, "\(problems)")
    }

    @Test func decodedOnArrivalWithoutThePacerFailsClearly() {
        // The old bug: about half the pictures are never shown.
        let problems = Self.verdict(Self.decode(Self.pairArrivals), Self.pts)
        #expect(Self.has(problems, "shown") && Self.has(problems, "skipped"), "\(problems)")
    }

    @Test func burstsFromAWrongUTCOffsetFailClearly() {
        // Every picture is overdue: each second of media is released as its download arrives, decoded back to back.
        let released = (0..<Self.n).map { Int64($0 / 10) * 1_000_000 }
        let problems = Self.verdict(Self.decode(released), Self.pts)
        #expect(
            Self.has(problems, "max") && Self.has(problems, "late") && Self.has(problems, "shown") && Self.has(problems, "skipped"),
            "\(problems)")
    }

    @Test func onePictureAFewRefreshesLatePasses() {
        var released = Self.pts.map { $0 + 105_000 }
        released[30] += 50_000
        let s = PacingRefresh.show(decodedAtUs: Self.decode(released), ptsUs: Self.pts, refreshUs: PacingRefresh.refresh60HzUs)
        let summary = PacingStats.of(atUs: s.atUs, ptsUs: s.ptsUs)
        // Over the former fixed 34 ms bound, within three refreshes; the pairs before and after it are late.
        #expect(summary.jitterMax > 34 && summary.jitterMax < 51, "max \(summary.jitterMax)")
        let lateMs = (PacingVerdict.lateRefreshes + PacingVerdict.noiseRefreshes) * Double(PacingRefresh.refresh60HzUs) / 1000
        #expect(PacingStats.over(atUs: s.atUs, ptsUs: s.ptsUs, thresholdMs: lateMs) == 2)
        let problems = PacingVerdict.problems(
            shownAtUs: s.atUs, shownPtsUs: s.ptsUs, released: Self.n, skipped: s.skipped,
            refreshMs: Double(PacingRefresh.refresh60HzUs) / 1000, minPairs: 40)
        #expect(problems.isEmpty, "\(problems)")
    }

    @Test func halfThePicturesShownOrTooFewPairsFails() {
        let pts = (0..<(Self.n / 2)).map { Int64($0) * 200_000 }
        let at = pts.map { $0 + 105_000 }
        let half = PacingVerdict.problems(shownAtUs: at, shownPtsUs: pts, released: Self.n, skipped: 0, refreshMs: 16.667, minPairs: 20)
        #expect(Self.has(half, "shown") && !Self.has(half, "max"), "\(half)")
        let few = PacingVerdict.problems(
            shownAtUs: Array(at.prefix(5)), shownPtsUs: Array(pts.prefix(5)), released: 5, skipped: 0, refreshMs: 16.667, minPairs: 20)
        #expect(Self.has(few, "pairs"), "\(few)")
    }
}
