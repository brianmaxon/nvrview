// The pure rules of the Recordings window's player (docs/UX.md
// "Recordings"): the speeds and what each one means, the transport's
// arithmetic, how up to four cameras share the player, how the window's panes
// give way in a narrow window, and the incident row's thumbnail strip. The
// same rules, with the same test vectors, as the macOS app's PlayerRules.swift.

using System.Globalization;
using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>The playback speeds (FFI-RECORDINGS.md section 6, decision 3) and what each one means for the picture and the sound.</summary>
public static class PlaybackSpeed
{
    /// <summary>Every speed the core accepts, slowest first.</summary>
    public static IReadOnlyList<double> All { get; } = [0.25, 0.5, 1, 2, 4, 8, 16];

    /// <summary>"0.25×", "1×", "16×".</summary>
    public static string Label(double rate) =>
        (rate == Math.Round(rate) ? ((int)rate).ToString(CultureInfo.InvariantCulture) : rate.ToString("0.##", CultureInfo.InvariantCulture)) + "×";

    /// <summary>From 2× up, devices send keyframes only and no audio (protocol.md, "Rates, keyframes, and how a play ends").</summary>
    public static bool KeyframesOnly(double rate) => rate >= 2;

    /// <summary>Recorded sound plays at 1× only.</summary>
    public static bool HasSound(double rate) => rate == 1;

    /// <summary>What the transport says next to the speed, or null at 1×.</summary>
    public static string? Hint(double rate) =>
        KeyframesOnly(rate) ? "Keyframes only, no sound" : rate < 1 ? "No sound below 1×" : null;

    /// <summary>The next speed up, or null at the end of the list.</summary>
    public static double? Faster(double rate) => All.Where(r => r > rate).Select(r => (double?)r).FirstOrDefault();

    /// <summary>The next speed down, or null at the end of the list.</summary>
    public static double? Slower(double rate) => All.Where(r => r < rate).Select(r => (double?)r).LastOrDefault();
}

/// <summary>The transport's view of one playback: where it is, in which range, and what the buttons do.</summary>
/// <param name="StartMs">The playback's range start, device-local milliseconds.</param>
/// <param name="EndMs">The range end.</param>
/// <param name="PositionMs">Where the playhead is.</param>
/// <param name="State">Null before the playback reports one.</param>
/// <param name="Rate">The speed.</param>
public readonly record struct TransportState(long StartMs, long EndMs, long PositionMs, NvPlaybackState? State, double Rate)
{
    /// <summary>How far the skip buttons move.</summary>
    public const long SkipMs = 10_000;

    /// <summary>Whether the clock runs (or waits for a camera to catch up).</summary>
    public bool IsPlaying => State is NvPlaybackState.Playing or NvPlaybackState.Buffering;

    /// <summary>The play button's action shows "play" (not "pause").</summary>
    public bool ShowsPlay => !IsPlaying;

    /// <summary>The position moved by <paramref name="delta"/>, kept within the range.</summary>
    public long Skipped(long delta) => Clamp(PositionMs + delta);

    /// <summary>Keeps a time within the range.</summary>
    public long Clamp(long ms) => Math.Min(Math.Max(ms, StartMs), EndMs);

    /// <summary>The position as a fraction of [from, to] (0 before, 1 after).</summary>
    public static double Fraction(long ms, long from, long to) =>
        to > from ? Math.Min(Math.Max((double)(ms - from) / (to - from), 0), 1) : 0;

    /// <summary>The time at <paramref name="fraction"/> of [from, to], whole milliseconds.</summary>
    public static long TimeAtFraction(double fraction, long from, long to) =>
        from + (long)Math.Round(Math.Min(Math.Max(fraction, 0), 1) * (to - from), MidpointRounding.AwayFromZero);

    /// <summary>"07:42:05" for a device-local millisecond time.</summary>
    public static string Clock(long ms) => DeviceTime.Time(DeviceTime.FloorDiv(ms, 1000));
}

/// <summary>How the player shows its cameras.</summary>
public abstract record PlayerGridMode
{
    /// <summary>All cameras the same size: one, side by side, or two by two.</summary>
    public sealed record Grid : PlayerGridMode;

    /// <summary>One camera large, the others small beside it.</summary>
    public sealed record Emphasis(int Index) : PlayerGridMode;

    /// <summary>One camera alone.</summary>
    public sealed record Single(int Index) : PlayerGridMode;

    /// <summary>The grid.</summary>
    public static PlayerGridMode Tiled { get; } = new Grid();
}

/// <summary>
/// What the player says when the core refuses to open a playback, by status (<c>nv_playback_open</c>,
/// FFI-RECORDINGS.md section 6). Every camera of an enabled device plays, so a refused camera means its device's
/// recordings are not searched.
/// </summary>
public static class PlaybackOpenMessage
{
    /// <summary>The message for <paramref name="status"/>.</summary>
    public static string Text(NvStatus status) => status switch
    {
        NvStatus.Limit => "Too many cameras or playbacks to play this",
        NvStatus.Unsupported => "This device cannot play its recordings",
        NvStatus.InvalidArgument => "A camera's device is not searched for recordings",
        NvStatus.NotConfigured => "Recordings are not set up yet",
        NvStatus.UnknownId => "That incident changed; select it again",
        NvStatus.Closed => "The app is closing",
        _ => $"The recording could not be played (status {(int)status})",
    };
}

/// <summary>Where each of the player's cameras goes (at most four).</summary>
public static class PlayerGrid
{
    /// <summary>The most cameras a playback shows.</summary>
    public const int MaxCameras = 4;

    /// <summary>
    /// Each camera's cell in <paramref name="bounds"/> (top-left origin), null for
    /// one that is not shown (a single view shows only its camera).
    /// <paramref name="gap"/> separates cells.
    /// </summary>
    public static Rect?[] Cells(int count, PlayerGridMode mode, Rect bounds, double gap = 2)
    {
        if (count <= 0)
        {
            return [];
        }
        int n = Math.Min(count, MaxCameras);
        var cells = new Rect?[count];
        switch (Normalized(mode, n))
        {
            case PlayerGridMode.Single s:
                cells[s.Index] = bounds;
                break;
            case PlayerGridMode.Emphasis e:
            {
                double side = (bounds.Width - gap) / 4;
                var big = new Rect(bounds.X, bounds.Y, bounds.Width - side - gap, bounds.Height);
                cells[e.Index] = big;
                int[] others = [.. Enumerable.Range(0, n).Where(i => i != e.Index)];
                // Small cells keep a picture's shape: at most a third of the height.
                double h = Math.Min((bounds.Height - gap * Math.Max(others.Length - 1, 0)) / Math.Max(others.Length, 1), bounds.Height / 3);
                for (int k = 0; k < others.Length; k++)
                {
                    cells[others[k]] = new Rect(big.Right + gap, bounds.Y + k * (h + gap), side, h);
                }
                break;
            }
            default:
            {
                (int cols, int rows) = n == 1 ? (1, 1) : n == 2 ? (2, 1) : (2, 2);
                double w = (bounds.Width - gap * (cols - 1)) / cols;
                double h = (bounds.Height - gap * (rows - 1)) / rows;
                for (int i = 0; i < n; i++)
                {
                    cells[i] = new Rect(bounds.X + (i % cols) * (w + gap), bounds.Y + (i / cols) * (h + gap), w, h);
                }
                break;
            }
        }
        return cells;
    }

    /// <summary>A mode that fits <paramref name="count"/> cameras (an index past the end falls back to the grid; one camera is always the grid).</summary>
    public static PlayerGridMode Normalized(PlayerGridMode mode, int count)
    {
        int? index = mode switch
        {
            PlayerGridMode.Emphasis e => e.Index,
            PlayerGridMode.Single s => s.Index,
            _ => null,
        };
        return index is { } i && count > 1 && i >= 0 && i < count ? mode : PlayerGridMode.Tiled;
    }

    /// <summary>A click on camera <paramref name="i"/>: makes it big, or back to the grid when it already is; a single view stays as it is.</summary>
    public static PlayerGridMode Click(PlayerGridMode mode, int i, int count) => Normalized(mode, count) switch
    {
        PlayerGridMode.Emphasis e => e.Index == i ? PlayerGridMode.Tiled : new PlayerGridMode.Emphasis(i),
        PlayerGridMode.Single => mode,
        _ => count > 1 ? new PlayerGridMode.Emphasis(i) : PlayerGridMode.Tiled,
    };

    /// <summary>A double click on camera <paramref name="i"/> (or the one-camera button): that camera alone, or back to the grid from it.</summary>
    public static PlayerGridMode DoubleClick(PlayerGridMode mode, int i, int count)
    {
        if (count <= 1)
        {
            return PlayerGridMode.Tiled;
        }
        return Normalized(mode, count) is PlayerGridMode.Single s && s.Index == i ? PlayerGridMode.Tiled : new PlayerGridMode.Single(i);
    }

    /// <summary>Which camera's sound the player plays: the large or single one, else the first.</summary>
    public static int? AudibleIndex(PlayerGridMode mode, int count) =>
        count <= 0 ? null : Normalized(mode, count) switch
        {
            PlayerGridMode.Emphasis e => e.Index,
            PlayerGridMode.Single s => s.Index,
            _ => 0,
        };

    /// <summary>The cameras of an incident the player shows: the first four, in the incident's order.</summary>
    public static List<T> Cameras<T>(IEnumerable<T> all) => [.. all.Take(MaxCameras)];
}

/// <summary>
/// The part of the day the timeline lanes show (docs/UX.md "Recordings"): the zoom (1 is the whole day in the lanes'
/// width, up to 48x) and the fraction of the day at the left edge. Zooming keeps the time under the pointer where it
/// is; the visible part never leaves the day. The same rule, with the same test vectors, as the macOS app's
/// PlayerRules.swift.
/// </summary>
public readonly record struct TimelineViewport
{
    /// <summary>The whole day.</summary>
    public const double MinZoom = 1;

    /// <summary>The closest zoom.</summary>
    public const double MaxZoom = 48;

    /// <summary>A mouse wheel notch zooms by this much.</summary>
    public const double Notch = 1.25;

    /// <summary>A horizontal notch (Shift and the wheel) pans this much of the visible part.</summary>
    public const double LineStep = 0.1;

    /// <summary>The whole day.</summary>
    public TimelineViewport()
        : this(MinZoom, 0)
    {
    }

    /// <summary>A viewport, clamped: the zoom to 1...48 and the start to 0...(1 - 1/zoom).</summary>
    public TimelineViewport(double zoom, double start = 0)
    {
        double z = double.IsFinite(zoom) ? Math.Clamp(zoom, MinZoom, MaxZoom) : MinZoom;
        Zoom = z;
        Start = double.IsFinite(start) ? Math.Clamp(start, 0, 1 - 1 / z) : 0;
    }

    /// <summary>1...48.</summary>
    public double Zoom { get; }

    /// <summary>The fraction of the day at the left edge.</summary>
    public double Start { get; }

    /// <summary>How much of the day is visible.</summary>
    public double Visible => 1 / Zoom;

    /// <summary>The fraction of the day at pointer <paramref name="p"/> (0 at the left edge, 1 at the right).</summary>
    public double FractionAtPointer(double p) => Start + p / Zoom;

    /// <summary>Zoomed by <paramref name="factor"/> around the pointer <paramref name="p"/> (its time stays under it).</summary>
    public TimelineViewport ZoomedBy(double factor, double p) => ZoomedTo(Zoom * factor, p);

    /// <summary>Zoomed to <paramref name="z"/> around the pointer <paramref name="p"/>.</summary>
    public TimelineViewport ZoomedTo(double z, double p)
    {
        double t = FractionAtPointer(p);
        double next = Math.Clamp(z, MinZoom, MaxZoom);
        return new TimelineViewport(next, t - p / next);
    }

    /// <summary>Panned by <paramref name="amount"/> visible widths (positive: later in the day).</summary>
    public TimelineViewport PannedBy(double amount) => new(Zoom, Start + amount / Zoom);

    /// <summary>Mouse wheel notches (positive: away from the user) as a zoom factor.</summary>
    public static double WheelFactor(double notches) => Math.Pow(Notch, notches);
}

/// <summary>
/// How the Recordings window's panes share its width and the center its
/// height (docs/UX.md "Recordings"). The side panes have fixed sensible
/// widths; in a window too narrow for all three the inspector gives way
/// first, then the sidebar, so the player never gets squeezed or overlapped.
/// </summary>
public static class RecordingsPanes
{
    public const double SidebarWidth = 280;
    public const double InspectorWidth = 290;

    /// <summary>The least the player column may have.</summary>
    public const double CenterMin = 440;

    /// <summary>The window's least content width.</summary>
    public const double WindowMinWidth = 720;

    /// <summary>The window's least content height.</summary>
    public const double WindowMinHeight = 560;

    /// <summary>The transport bar under the player.</summary>
    public const double TransportHeight = 92;

    public const double LaneHeight = 26;
    public const double RulerHeight = 18;

    /// <summary>The loudness strip under each lane: its default height (the user drags it within <see cref="LoudnessMin"/> to <see cref="LoudnessMax"/>; kept across launches; the macOS app uses the same numbers).</summary>
    public const double LoudnessHeight = 28;

    public const double LoudnessMin = 12;
    public const double LoudnessMax = 96;

    /// <summary>A lane with its loudness strip.</summary>
    public const double RowHeight = LaneHeight + LoudnessHeight;

    /// <summary>A dragged or stored strip height, kept in range (the default for nothing or garbage).</summary>
    public static double ClampLoudness(double? h) =>
        h is { } v && double.IsFinite(v) ? Math.Clamp(v, LoudnessMin, LoudnessMax) : LoudnessHeight;

    /// <summary>A lane with a strip <paramref name="loudness"/> tall.</summary>
    public static double RowHeightFor(double loudness) => LaneHeight + loudness;

    /// <summary>The loudness strips' legend under the lanes.</summary>
    public const double LegendHeight = 14;

    /// <summary>The least height of the player.</summary>
    public const double PlayerMin = 200;

    /// <summary>Which side panes are shown in a window <paramref name="width"/> wide, given what the user asked for.</summary>
    public static (bool Sidebar, bool Inspector) Shown(double width, bool sidebar, bool inspector)
    {
        bool s = sidebar, i = inspector;
        if (s && i && width < SidebarWidth + InspectorWidth + CenterMin)
        {
            i = false;
        }
        if (s && width < SidebarWidth + CenterMin)
        {
            s = false;
        }
        if (i && !s && width < InspectorWidth + CenterMin)
        {
            i = false;
        }
        return (s, i);
    }

    /// <summary>
    /// The timeline lanes' height in a center column <paramref name="height"/> tall:
    /// every lane if they fit in a third of it, else a third (they scroll),
    /// never less than two lanes.
    /// </summary>
    public static double LanesHeight(double height, int lanes, double loudness = LoudnessHeight)
    {
        double row = RowHeightFor(loudness);
        double want = RulerHeight + Math.Max(lanes, 1) * row + LegendHeight + 8;
        double cap = Math.Max((height - TransportHeight) / 3, RulerHeight + 2 * row + LegendHeight + 8);
        return Math.Min(want, cap);
    }
}

/// <summary>
/// Where "Split Here" splits an incident: at the playhead's second, when it
/// lies strictly inside the incident (a split at its first or last second
/// would leave a part without events).
/// </summary>
public static class IncidentSplit
{
    /// <summary>The second to split at, or null.</summary>
    public static long? Second(long start, long end, long playheadMs)
    {
        long t = DeviceTime.FloorDiv(playheadMs, 1000);
        return start < t && t < end ? t : null;
    }
}

/// <summary>The incident row's thumbnail strip: a fixed width whatever the number of cameras, so every row's text starts at the same place.</summary>
public static class IncidentRowStrip
{
    public const double ThumbnailWidth = 64;
    public const double ThumbnailHeight = 36;
    public const double Spacing = 4;

    /// <summary>Thumbnails shown; more cameras show as "+N".</summary>
    public const int MaxShown = 2;

    /// <summary>The strip's width.</summary>
    public static double Width => MaxShown * ThumbnailWidth + (MaxShown - 1) * Spacing;

    /// <summary>How many thumbnails a row of <paramref name="count"/> cameras shows, and how many more it counts.</summary>
    public static (int Shown, int More) Slots(int count)
    {
        int shown = Math.Min(Math.Max(count, 0), MaxShown);
        return (shown, Math.Max(count - shown, 0));
    }
}
