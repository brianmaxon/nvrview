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
}
