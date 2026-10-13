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

        T.Run("pacing: pairs over a threshold", () =>
        {
            long[] pts = [0, 100_000, 200_000, 300_000, 400_000];
            long[] at = [0, 100_000, 260_000, 300_000, 400_000];
            T.Equal(2, PacingStats.Over(at, pts, 50));
            T.Equal(0, PacingStats.Over(at, pts, 60));
        });

        T.Run("pacing: the simulated display draws the newest picture at each refresh", () =>
        {
            RefreshShown even = PacingRefresh.Show([0, 10_000, 20_000, 40_000], [0, 1, 2, 3], 16_667);
            T.Check(even.AtUs.SequenceEqual([0L, 16_667, 33_334, 50_001]), string.Join(",", even.AtUs));
            T.Check(even.PtsUs.SequenceEqual([0L, 1, 2, 3]), string.Join(",", even.PtsUs));
            T.Equal(0, even.Skipped);
            // Two pictures decoded between two refreshes: the older is never drawn.
            RefreshShown bunched = PacingRefresh.Show([0, 5_000, 12_000, 20_000], [0, 1, 2, 3], 16_667);
            T.Check(bunched.AtUs.SequenceEqual([0L, 16_667, 33_334]), string.Join(",", bunched.AtUs));
            T.Check(bunched.PtsUs.SequenceEqual([0L, 2, 3]), string.Join(",", bunched.PtsUs));
            T.Equal(1, bunched.Skipped);
            // The refresh grid's phase.
            RefreshShown phased = PacingRefresh.Show([1_000], [7], 16_667, 5_000);
            T.Check(phased.AtUs.SequenceEqual([5_000L]), string.Join(",", phased.AtUs));
            T.Equal(0, PacingRefresh.Show([], [], 16_667).AtUs.Length);
        });

        // The pipeline on simulated time: pictures of a 10 fps stream that a device sends in pairs (the second
        // picture's time plus 5 ms), released by the pacer at their own time or on arrival (the old bug), decoded
        // 3 ms after release one after another, shown by a 60 Hz display.
        const int N = 60;
        long[] Pts() => [.. Enumerable.Range(0, N).Select(k => k * 100_000L)];
        long[] PairArrivals() => [.. Enumerable.Range(0, N).Select(k => (k | 1) * 100_000L + 5_000)];
        long[] Decode(long[] released)
        {
            var d = new long[released.Length];
            for (int i = 0; i < d.Length; i++)
            {
                d[i] = Math.Max(released[i], i > 0 ? d[i - 1] : long.MinValue) + 3_000;
            }
            return d;
        }
        List<string> Verdict(long[] decoded, long[] pts)
        {
            RefreshShown s = PacingRefresh.Show(decoded, pts, PacingRefresh.Refresh60HzUs);
            return PacingVerdict.Problems(s.AtUs, s.PtsUs, pts.Length, s.Skipped, PacingRefresh.Refresh60HzUs / 1000.0, 40);
        }
        bool Has(List<string> problems, string key) => problems.Any(p => p.StartsWith(key + ":", StringComparison.Ordinal));

        T.Run("pacing verdict: paced pictures pass on the simulated display", () =>
        {
            long[] pts = Pts(), arrivals = PairArrivals();
            // The pacer's delay: the earliest picture relative to its time (5 ms) plus how much later the others came (100 ms).
            long[] released = [.. arrivals.Select((a, i) => Math.Max(a, pts[i] + 105_000))];
            List<string> problems = Verdict(Decode(released), pts);
            T.Check(problems.Count == 0, string.Join("; ", problems));
        });

        T.Run("pacing verdict: decoded on arrival without the pacer (the old bug) fails clearly", () =>
        {
            long[] pts = Pts();
            List<string> problems = Verdict(Decode(PairArrivals()), pts);
            T.Check(Has(problems, "shown") && Has(problems, "skipped"), string.Join("; ", problems));
        });

        T.Run("pacing verdict: released in bursts as each download arrives (a wrong UTC offset) fails clearly", () =>
        {
            long[] pts = Pts();
            // Every picture is overdue: each second of media is released as its download arrives, decoded back to back.
            long[] released = [.. Enumerable.Range(0, N).Select(k => k / 10 * 1_000_000L)];
            List<string> problems = Verdict(Decode(released), pts);
            T.Check(Has(problems, "max") && Has(problems, "late") && Has(problems, "shown") && Has(problems, "skipped"), string.Join("; ", problems));
        });

        T.Run("pacing verdict: one picture a few refreshes late passes (the display's grid, not a regression)", () =>
        {
            long[] pts = Pts();
            long[] released = [.. pts.Select(p => p + 105_000)];
            released[30] += 50_000;
            RefreshShown s = PacingRefresh.Show(Decode(released), pts, PacingRefresh.Refresh60HzUs);
            PacingSummary summary = PacingStats.Of(s.AtUs, s.PtsUs);
            // Over the former fixed 34 ms bound, within three refreshes; the pairs before and after it are late.
            T.Check(summary.JitterMax > 34 && summary.JitterMax < 51, $"max {summary.JitterMax}");
            T.Equal(2, PacingStats.Over(s.AtUs, s.PtsUs, (PacingVerdict.LateRefreshes + PacingVerdict.NoiseRefreshes) * PacingRefresh.Refresh60HzUs / 1000.0));
            List<string> problems = PacingVerdict.Problems(s.AtUs, s.PtsUs, N, s.Skipped, PacingRefresh.Refresh60HzUs / 1000.0, 40);
            T.Check(problems.Count == 0, string.Join("; ", problems));
        });

        T.Run("pacing verdict: half the pictures shown, or too few pairs, fails", () =>
        {
            long[] pts = [.. Enumerable.Range(0, N / 2).Select(k => k * 200_000L)];
            long[] at = [.. pts.Select(p => p + 105_000)];
            List<string> half = PacingVerdict.Problems(at, pts, N, 0, 16.667, 20);
            T.Check(Has(half, "shown") && !Has(half, "max"), string.Join("; ", half));
            List<string> few = PacingVerdict.Problems(at[..5], pts[..5], 5, 0, 16.667, 20);
            T.Check(Has(few, "pairs"), string.Join("; ", few));
        });
    }
}
