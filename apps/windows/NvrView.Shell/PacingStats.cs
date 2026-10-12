// How evenly a playback tile's pictures were handled, measured against the
// pictures' own timestamps (docs/RENDERING.md, "Playback pacing"): for each
// stage (released to the app, shown on screen) the interval between two
// consecutive pictures is compared with the spacing of their timestamps.
// The difference is the jitter a viewer sees as judder. The same rules and
// test vectors as the macOS app's PacingStats.swift. Pure, tested on Linux.

namespace NvrView.Shell;

/// <summary>One stage's pacing over a measured stretch (milliseconds).</summary>
/// <param name="Pictures">Pictures seen at this stage.</param>
/// <param name="Pairs">Consecutive pairs compared (pairs across a discontinuity are left out).</param>
/// <param name="SpacingP50">Median spacing of the pictures' timestamps.</param>
/// <param name="IntervalP50">Median interval between the pictures at this stage.</param>
/// <param name="IntervalP95">95th percentile of the intervals.</param>
/// <param name="IntervalMax">Longest interval.</param>
/// <param name="JitterP50">Median of |interval - timestamp spacing|.</param>
/// <param name="JitterP95">95th percentile of the jitter.</param>
/// <param name="JitterMax">Largest jitter.</param>
/// <param name="Uneven">Pairs whose jitter is over <see cref="PacingStats.UnevenMs"/>.</param>
public readonly record struct PacingSummary(
    int Pictures, int Pairs, double SpacingP50, double IntervalP50, double IntervalP95, double IntervalMax,
    double JitterP50, double JitterP95, double JitterMax, int Uneven)
{
    /// <summary>A short line for logs and the self-test.</summary>
    public override string ToString() =>
        FormattableString.Invariant($"{Pictures} pictures, spacing {SpacingP50:0.0} ms, interval p50 {IntervalP50:0.0} p95 {IntervalP95:0.0} max {IntervalMax:0.0} ms, ") +
        FormattableString.Invariant($"jitter p50 {JitterP50:0.0} p95 {JitterP95:0.0} max {JitterMax:0.0} ms, uneven {Uneven}");
}

/// <summary>Computes <see cref="PacingSummary"/> values (see the file comment).</summary>
public static class PacingStats
{
    /// <summary>
    /// Timestamps further apart than this (or going backwards) are a
    /// discontinuity (a seek, a gap in the recording), not a pacing error.
    /// </summary>
    public const long GapUs = 1_000_000;

    /// <summary>
    /// A pair whose jitter is above this is counted uneven: more than one and
    /// a half refreshes of a 60 Hz display, so more than the refresh grid
    /// alone can explain.
    /// </summary>
    public const double UnevenMs = 25;

    /// <summary>
    /// The pacing of pictures handled at <paramref name="atUs"/> (any
    /// monotonic clock, microseconds, ascending) whose timestamps are
    /// <paramref name="ptsUs"/> (same order).
    /// </summary>
    public static PacingSummary Of(ReadOnlySpan<long> atUs, ReadOnlySpan<long> ptsUs)
    {
        int n = Math.Min(atUs.Length, ptsUs.Length);
        if (n < 2)
        {
            return new PacingSummary(n, 0, 0, 0, 0, 0, 0, 0, 0, 0);
        }
        var intervals = new double[n - 1];
        var spacings = new double[n - 1];
        var jitters = new double[n - 1];
        int pairs = 0, uneven = 0;
        for (int i = 1; i < n; i++)
        {
            long spacing = ptsUs[i] - ptsUs[i - 1];
            if (spacing <= 0 || spacing > GapUs)
            {
                continue;
            }
            double interval = (atUs[i] - atUs[i - 1]) / 1000.0;
            double jitter = Math.Abs(interval - spacing / 1000.0);
            intervals[pairs] = interval;
            spacings[pairs] = spacing / 1000.0;
            jitters[pairs] = jitter;
            pairs++;
            if (jitter > UnevenMs)
            {
                uneven++;
            }
        }
        Span<double> iv = intervals.AsSpan(0, pairs), sp = spacings.AsSpan(0, pairs), ji = jitters.AsSpan(0, pairs);
        iv.Sort();
        sp.Sort();
        ji.Sort();
        return new PacingSummary(
            n, pairs, Percentile(sp, 50), Percentile(iv, 50), Percentile(iv, 95), pairs == 0 ? 0 : iv[^1],
            Percentile(ji, 50), Percentile(ji, 95), pairs == 0 ? 0 : ji[^1], uneven);
    }

    /// <summary>The <paramref name="p"/>th percentile of sorted values (nearest rank; 0 when empty).</summary>
    public static double Percentile(ReadOnlySpan<double> sorted, double p)
    {
        if (sorted.IsEmpty)
        {
            return 0;
        }
        int rank = (int)Math.Ceiling(p / 100.0 * sorted.Length);
        return sorted[Math.Clamp(rank - 1, 0, sorted.Length - 1)];
    }

    /// <summary>
    /// Pictures of <paramref name="decodedPts"/> that never reached the
    /// screen: not in <paramref name="shownPts"/> and within its range (a
    /// picture decoded after the last one shown may still be shown).
    /// </summary>
    public static int Skipped(ReadOnlySpan<long> decodedPts, ReadOnlySpan<long> shownPts)
    {
        if (shownPts.IsEmpty)
        {
            return 0;
        }
        var shown = new HashSet<long>();
        long first = long.MaxValue, last = long.MinValue;
        foreach (long p in shownPts)
        {
            shown.Add(p);
            first = Math.Min(first, p);
            last = Math.Max(last, p);
        }
        int skipped = 0;
        foreach (long p in decodedPts)
        {
            if (p > first && p < last && !shown.Contains(p))
            {
                skipped++;
            }
        }
        return skipped;
    }
}
