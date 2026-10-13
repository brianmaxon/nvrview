// A pacing trace of one playback tile (docs/RENDERING.md, "Playback
// pacing"): when each picture's Decode reached the app (the engine released
// it), when the decoder answered it (nv_tile_decoded), and when the render
// thread first drew it into a frame. Off unless something attaches one: the
// self-test's playback-pacing step, or NVRVIEW_PACING_TRACE=1 for every
// playback tile, which logs a summary at most every ten seconds per tile.
//
// Writers are the tile's action thread (Arrived, Decoded) and render
// threads (Presented); a lock keeps the three rings consistent. The rings
// are allocated once; recording allocates nothing.

using System.Diagnostics;
using NvrView.Shell;

namespace NvrView.Video;

/// <summary>What a <see cref="PacingTrace"/> measured.</summary>
/// <param name="Released">When Decodes reached the app, against the pictures' timestamps.</param>
/// <param name="Shown">When pictures were first drawn, against their timestamps.</param>
/// <param name="DecodeP50Ms">Median time from a Decode reaching the app to its answer.</param>
/// <param name="DecodeMaxMs">Longest such time.</param>
/// <param name="Decoded">Pictures answered decoded.</param>
/// <param name="Skipped">Pictures decoded but never drawn (overwritten before a frame took them).</param>
/// <param name="Simulated">The decoded pictures as an ideal 60 Hz display shows them (<see cref="PacingRefresh"/>).</param>
/// <param name="SimulatedSkipped">Pictures that display never drew (replaced before its next refresh).</param>
/// <param name="Problems">What <see cref="PacingVerdict"/> finds wrong with <paramref name="Simulated"/> (empty when it passes).</param>
public sealed record PacingReport(PacingSummary Released, PacingSummary Shown, double DecodeP50Ms, double DecodeMaxMs, int Decoded, int Skipped,
    PacingSummary Simulated, int SimulatedSkipped, IReadOnlyList<string> Problems)
{
    /// <summary>Share of the released pictures the real display drew.</summary>
    public double ShownRatio => Released.Pictures > 0 ? (double)Shown.Pictures / Released.Pictures : 0;

    /// <inheritdoc/>
    public override string ToString() =>
        FormattableString.Invariant($"released: {Released}; shown: {Shown}; decode p50 {DecodeP50Ms:0.0} max {DecodeMaxMs:0.0} ms; decoded {Decoded}, skipped {Skipped}; ") +
        FormattableString.Invariant($"simulated 60 Hz: {Simulated}, skipped {SimulatedSkipped}");
}

/// <summary>Records one tile's picture timings (see the file comment).</summary>
public sealed class PacingTrace
{
    /// <summary>Entries kept per stage (about two minutes at 30 pictures a second).</summary>
    public const int Capacity = 4096;

    /// <summary>Whether NVRVIEW_PACING_TRACE=1 asks for a trace on every playback tile.</summary>
    public static bool FromEnvironment { get; } = Environment.GetEnvironmentVariable("NVRVIEW_PACING_TRACE") == "1";

    private readonly Lock _lock = new();
    private readonly Ring _arrived = new();
    private readonly Ring _decoded = new();
    private readonly Ring _shown = new();
    private readonly Action<string>? _log;
    private readonly long _logEvery;
    private long _lastLog;

    /// <param name="log">When set, a summary is passed to it at most every <paramref name="logSeconds"/> (then the trace starts over).</param>
    /// <param name="logSeconds">Seconds between summaries.</param>
    public PacingTrace(Action<string>? log = null, double logSeconds = 10)
    {
        _log = log;
        _logEvery = (long)(logSeconds * Stopwatch.Frequency);
        _lastLog = Stopwatch.GetTimestamp();
    }

    /// <summary>A Decode for the picture <paramref name="ptsUs"/> reached the app.</summary>
    public void Arrived(long ptsUs)
    {
        long now = Stopwatch.GetTimestamp();
        lock (_lock)
        {
            _arrived.Add(now, ptsUs);
        }
    }

    /// <summary>The picture <paramref name="ptsUs"/> was answered decoded.</summary>
    public void Decoded(long ptsUs)
    {
        long now = Stopwatch.GetTimestamp();
        lock (_lock)
        {
            _decoded.Add(now, ptsUs);
        }
        if (_log is { } log && now - Volatile.Read(ref _lastLog) >= _logEvery)
        {
            Volatile.Write(ref _lastLog, now);
            log(Report().ToString());
            Reset();
        }
    }

    /// <summary>A frame started at <paramref name="ticks"/> (Stopwatch) drew the picture <paramref name="ptsUs"/> for the first time.</summary>
    public void Presented(long ptsUs, long ticks)
    {
        lock (_lock)
        {
            _shown.Add(ticks, ptsUs);
        }
    }

    /// <summary>Pictures that arrived, were decoded, and were shown since the last reset (up to <see cref="Capacity"/> each).</summary>
    public (int Arrived, int Decoded, int Shown) Counts
    {
        get
        {
            lock (_lock)
            {
                return (_arrived.Count, _decoded.Count, _shown.Count);
            }
        }
    }

    /// <summary>Forgets everything recorded so far.</summary>
    public void Reset()
    {
        lock (_lock)
        {
            _arrived.Clear();
            _decoded.Clear();
            _shown.Clear();
        }
    }

    /// <summary>
    /// The pacing measured since the last reset (allocates; not for a hot
    /// path). The verdict needs <paramref name="minPairs"/> shown pairs.
    /// </summary>
    public PacingReport Report(int minPairs = 0)
    {
        long[] aT, aP, dT, dP, sT, sP;
        lock (_lock)
        {
            (aT, aP) = _arrived.Copy();
            (dT, dP) = _decoded.Copy();
            (sT, sP) = _shown.Copy();
        }
        // Decode time: each answered picture against its own arrival.
        var arrivedAt = new Dictionary<long, long>(aP.Length);
        for (int i = 0; i < aP.Length; i++)
        {
            arrivedAt[aP[i]] = aT[i];
        }
        var decodeMs = new List<double>(dP.Length);
        for (int i = 0; i < dP.Length; i++)
        {
            if (arrivedAt.TryGetValue(dP[i], out long at))
            {
                decodeMs.Add((dT[i] - at) * 1000.0 / Stopwatch.Frequency);
            }
        }
        decodeMs.Sort();
        double[] decode = [.. decodeMs];
        // The display stage on simulated time: what the real display drew depends on how many refreshes a window
        // behind others or a busy machine got; the verdict does not.
        RefreshShown sim = PacingRefresh.Show(Micros(dT), dP);
        List<string> problems = PacingVerdict.Problems(sim.AtUs, sim.PtsUs, aP.Length, sim.Skipped, PacingRefresh.Refresh60HzUs / 1000.0, minPairs);
        return new PacingReport(
            PacingStats.Of(Micros(aT), aP),
            PacingStats.Of(Micros(sT), sP),
            PacingStats.Percentile(decode, 50),
            decode.Length == 0 ? 0 : decode[^1],
            dP.Length,
            PacingStats.Skipped(dP, sP),
            PacingStats.Of(sim.AtUs, sim.PtsUs),
            sim.Skipped,
            problems);
    }

    private static long[] Micros(long[] ticks)
    {
        var us = new long[ticks.Length];
        for (int i = 0; i < ticks.Length; i++)
        {
            us[i] = (long)(ticks[i] * 1_000_000.0 / Stopwatch.Frequency);
        }
        return us;
    }

    // A ring of (Stopwatch ticks, pts) pairs, oldest first when copied.
    private sealed class Ring
    {
        private readonly long[] _ticks = new long[Capacity];
        private readonly long[] _pts = new long[Capacity];
        private int _next;
        private int _count;

        public void Add(long ticks, long pts)
        {
            _ticks[_next] = ticks;
            _pts[_next] = pts;
            _next = (_next + 1) % Capacity;
            _count = Math.Min(_count + 1, Capacity);
        }

        public int Count => _count;

        public void Clear() => _next = _count = 0;

        public (long[] Ticks, long[] Pts) Copy()
        {
            var t = new long[_count];
            var p = new long[_count];
            int start = (_next - _count + Capacity) % Capacity;
            for (int i = 0; i < _count; i++)
            {
                t[i] = _ticks[(start + i) % Capacity];
                p[i] = _pts[(start + i) % Capacity];
            }
            return (t, p);
        }
    }
}
