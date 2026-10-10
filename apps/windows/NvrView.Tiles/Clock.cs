// Time for everything that moves or expires on its own (indicator hold,
// full screen auto-hide, alert blinking): one injectable clock, so tests and
// the app's self-test advance a manual clock instead of waiting.

using System.Diagnostics;

namespace NvrView.Tiles;

/// <summary>Seconds on a steady clock.</summary>
public interface IViewerClock
{
    double Now { get; }
}

/// <summary>The host's monotonic clock.</summary>
public sealed class SystemViewerClock : IViewerClock
{
    public double Now => Stopwatch.GetTimestamp() / (double)Stopwatch.Frequency;
}

/// <summary>A clock that moves only when told to (tests, the self-test).</summary>
public sealed class ManualViewerClock(double now = 0) : IViewerClock
{
    public double Now { get; private set; } = now;

    public void Advance(double seconds) => Now += Math.Max(seconds, 0);
}
