namespace NvrView.Shell.Tests;

/// <summary>PacingStats: the same vectors as the macOS PacingStatsTests. Synthetic timestamps only.</summary>
internal static class PacingTests
{
    public static void Run()
    {
        T.Run("pacing: pictures shown exactly at their spacing have no jitter", () =>
        {
            long[] pts = [0, 100_000, 200_000, 300_000, 400_000];
            long[] at = [5_000, 105_000, 205_000, 305_000, 405_000];
            PacingSummary s = PacingStats.Of(at, pts);
            T.Equal(5, s.Pictures);
            T.Equal(4, s.Pairs);
            T.Equal(100.0, s.SpacingP50);
            T.Equal(100.0, s.IntervalP50);
            T.Equal(0.0, s.JitterMax);
            T.Equal(0, s.Uneven);
        });

        T.Run("pacing: a bunched pair shows as jitter and an uneven pair", () =>
        {
            // The third picture is 60 ms late, so its interval is 160 ms and the next 40 ms.
            long[] pts = [0, 100_000, 200_000, 300_000, 400_000];
            long[] at = [0, 100_000, 260_000, 300_000, 400_000];
            PacingSummary s = PacingStats.Of(at, pts);
            T.Equal(60.0, s.JitterMax);
            T.Equal(160.0, s.IntervalMax);
            T.Equal(2, s.Uneven);
            T.Equal(60.0, s.JitterP95);
            T.Equal(0.0, s.JitterP50);
        });

        T.Run("pacing: a seek or a gap is not a pacing error", () =>
        {
            long[] pts = [0, 100_000, 5_000_000, 5_100_000, 4_000_000, 4_100_000];
            long[] at = [0, 100_000, 200_000, 300_000, 400_000, 500_000];
            PacingSummary s = PacingStats.Of(at, pts);
            T.Equal(3, s.Pairs);
            T.Equal(0.0, s.JitterMax);
        });

        T.Run("pacing: fewer than two pictures give an empty summary", () =>
        {
            T.Equal(0, PacingStats.Of([], []).Pairs);
            T.Equal(1, PacingStats.Of([1], [1]).Pictures);
        });

        T.Run("pacing: nearest-rank percentiles", () =>
        {
            double[] v = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
            T.Equal(5.0, PacingStats.Percentile(v, 50));
            T.Equal(10.0, PacingStats.Percentile(v, 95));
            T.Equal(1.0, PacingStats.Percentile(v, 0));
            T.Equal(0.0, PacingStats.Percentile([], 50));
        });

        T.Run("pacing: skipped pictures are those decoded inside the shown range and never shown", () =>
        {
            long[] decoded = [0, 100, 200, 300, 400, 500];
            long[] shown = [100, 300, 400];
            // 200 was skipped; 0 and 500 lie outside the shown range.
            T.Equal(1, PacingStats.Skipped(decoded, shown));
            T.Equal(0, PacingStats.Skipped(decoded, []));
        });
    }
}
