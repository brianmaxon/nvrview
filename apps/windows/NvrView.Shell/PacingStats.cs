// How evenly a playback tile's pictures were handled, measured against the
// pictures' own timestamps (docs/RENDERING.md, "Playback pacing"): for each
// stage (released to the app, shown on screen) the interval between two
// consecutive pictures is compared with the spacing of their timestamps.
// The difference is the jitter a viewer sees as judder. The same rules and
// test vectors as the macOS app's PacingStats.swift. Pure, tested on Linux.
//
// PacingRefresh is the display stage on simulated time: given when each
// picture was decoded, it shows the newest one at every refresh of an ideal
// display, so the self-test's verdict does not depend on how many refreshes
// the real display (a window behind others, a busy machine) happened to
// miss. PacingVerdict holds the bounds both self-tests apply.

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

    /// <summary>
    /// Consecutive pairs (as <see cref="Of"/> compares them) whose jitter is
    /// over <paramref name="thresholdMs"/>.
    /// </summary>
    public static int Over(ReadOnlySpan<long> atUs, ReadOnlySpan<long> ptsUs, double thresholdMs)
    {
        int n = Math.Min(atUs.Length, ptsUs.Length), over = 0;
        for (int i = 1; i < n; i++)
        {
            long spacing = ptsUs[i] - ptsUs[i - 1];
            if (spacing <= 0 || spacing > GapUs)
            {
                continue;
            }
            double jitter = Math.Abs((atUs[i] - atUs[i - 1]) / 1000.0 - spacing / 1000.0);
            if (jitter > thresholdMs)
            {
                over++;
            }
        }
        return over;
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

/// <summary>What an ideal display showed: when each shown picture was first drawn, and how many were replaced unseen.</summary>
/// <param name="AtUs">Refresh times (µs) at which a picture was drawn for the first time.</param>
/// <param name="PtsUs">The timestamps of those pictures (same order).</param>
/// <param name="Skipped">Pictures decoded and replaced by a newer one before any refresh drew them.</param>
public readonly record struct RefreshShown(long[] AtUs, long[] PtsUs, int Skipped);

/// <summary>
/// The display stage on simulated time (see the file comment): at every
/// refresh the newest decoded picture is drawn, as both renderers do.
/// </summary>
public static class PacingRefresh
{
    /// <summary>A 60 Hz display's refresh period (µs).</summary>
    public const long Refresh60HzUs = 16_667;

    /// <summary>
    /// The pictures decoded at <paramref name="decodedAtUs"/> (ascending)
    /// with timestamps <paramref name="ptsUs"/>, shown on a display that
    /// refreshes at <paramref name="phaseUs"/> + k × <paramref name="refreshUs"/>.
    /// A picture decoded exactly at a refresh is drawn by it.
    /// </summary>
    public static RefreshShown Show(ReadOnlySpan<long> decodedAtUs, ReadOnlySpan<long> ptsUs, long refreshUs = Refresh60HzUs, long phaseUs = 0)
    {
        int n = Math.Min(decodedAtUs.Length, ptsUs.Length);
        if (n == 0 || refreshUs <= 0)
        {
            return new RefreshShown([], [], 0);
        }
        var at = new List<long>(n);
        var pts = new List<long>(n);
        // The first refresh at or after the first decode.
        long tick = phaseUs + (long)Math.Ceiling((decodedAtUs[0] - phaseUs) / (double)refreshUs) * refreshUs;
        int next = 0, newest = -1, skipped = 0;
        bool newestShown = true;
        while (next < n || !newestShown)
        {
            while (next < n && decodedAtUs[next] <= tick)
            {
                if (!newestShown)
                {
                    skipped++;
                }
                newest = next++;
                newestShown = false;
            }
            if (!newestShown)
            {
                at.Add(tick);
                pts.Add(ptsUs[newest]);
                newestShown = true;
            }
            tick += refreshUs;
        }
        return new RefreshShown([.. at], [.. pts], skipped);
    }
}

/// <summary>
/// The bounds the self-tests' pacing steps apply to the shown pictures
/// (docs/RENDERING.md, "Playback pacing"). A display draws pictures on its
/// refresh grid, so a pair's jitter of one refresh is the grid alone. The
/// bounds leave room for that and for a rare late picture, and still fail
/// clearly on the regressions that matter: pictures decoded in bunches
/// (about half of them never shown, or shown in consecutive refreshes far
/// from their spacing).
/// </summary>
public static class PacingVerdict
{
    /// <summary>Jitter (ms) the shown pairs may have at the 95th percentile: one 60 Hz refresh plus a few ms.</summary>
    public const double JitterP95Ms = 20;

    /// <summary>Largest jitter allowed for any shown pair, in refresh periods (plus <see cref="NoiseRefreshes"/>).</summary>
    public const double MaxRefreshes = 3;

    /// <summary>A pair whose jitter is over this many refresh periods (plus <see cref="NoiseRefreshes"/>) is late.</summary>
    public const double LateRefreshes = 2;

    /// <summary>
    /// Room for timing noise on top of whole refreshes: a jitter of exactly
    /// two refreshes measures a little over or under it.
    /// </summary>
    public const double NoiseRefreshes = 0.5;

    /// <summary>Late pairs allowed (one late picture makes two: the interval before it and the one after).</summary>
    public const int LateAllowed = 4;

    /// <summary>Share of the released pictures that must be shown.</summary>
    public const double ShownRatio = 0.9;

    /// <summary>Pictures that may be decoded and never shown.</summary>
    public const int SkippedAllowed = 2;

    /// <summary>
    /// What is wrong with the shown pictures (empty when they pass). Each
    /// problem starts with its key: "pairs", "p95", "max", "late", "shown",
    /// or "skipped".
    /// </summary>
    /// <param name="shownAtUs">When each picture was first shown (µs, ascending).</param>
    /// <param name="shownPtsUs">Their timestamps.</param>
    /// <param name="released">Pictures released to the app over the same stretch.</param>
    /// <param name="skipped">Pictures decoded and never shown.</param>
    /// <param name="refreshMs">The display's refresh period (ms).</param>
    /// <param name="minPairs">Shown pairs needed for a verdict.</param>
    public static List<string> Problems(ReadOnlySpan<long> shownAtUs, ReadOnlySpan<long> shownPtsUs, int released, int skipped, double refreshMs, int minPairs)
    {
        var problems = new List<string>();
        PacingSummary s = PacingStats.Of(shownAtUs, shownPtsUs);
        double max = (MaxRefreshes + NoiseRefreshes) * refreshMs, lateMs = (LateRefreshes + NoiseRefreshes) * refreshMs;
        int late = PacingStats.Over(shownAtUs, shownPtsUs, lateMs);
        double ratio = released > 0 ? (double)s.Pictures / released : 0;
        if (s.Pairs < minPairs)
        {
            problems.Add(FormattableString.Invariant($"pairs: only {s.Pairs} picture pairs were shown (at least {minPairs})"));
        }
        if (s.JitterP95 > JitterP95Ms)
        {
            problems.Add(FormattableString.Invariant($"p95: jitter p95 {s.JitterP95:0.0} ms (at most {JitterP95Ms:0} ms)"));
        }
        if (s.JitterMax > max)
        {
            problems.Add(FormattableString.Invariant($"max: jitter max {s.JitterMax:0.0} ms (at most {MaxRefreshes:0} refreshes, {max:0.0} ms)"));
        }
        if (late > LateAllowed)
        {
            problems.Add(FormattableString.Invariant($"late: {late} pairs more than {LateRefreshes:0} refreshes ({lateMs:0.0} ms) off their spacing (at most {LateAllowed})"));
        }
        if (ratio < ShownRatio)
        {
            problems.Add(FormattableString.Invariant($"shown: {s.Pictures} of {released} released pictures were shown (at least {ShownRatio * 100:0}%)"));
        }
        if (skipped > SkippedAllowed)
        {
            problems.Add(FormattableString.Invariant($"skipped: {skipped} decoded pictures were never shown (at most {SkippedAllowed})"));
        }
        return problems;
    }
}
