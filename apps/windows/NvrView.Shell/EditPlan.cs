// The follow cut and the 2-up and 4-up grids as the app plays them
// (docs/UX.md "Recordings", "Follow and Grid"): the core's edit list
// (FFI-RECORDINGS.md section 7, nv_edit_compute) parsed, and the pure rules
// that turn it into what is on screen at a time: which source, at which file
// position, in which rectangle; where the cuts are; whose sound plays; and a
// clock the transport drives. The renderer and export both read these, so the
// picture on screen and in a file is the same edit. The same rules, with the
// same test vectors, as the macOS app's EditPlan.swift.

using System.Text.Json;
using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>One camera's finished file in an edit list.</summary>
/// <param name="Source">The plan's number for it.</param>
/// <param name="Camera">The camera.</param>
/// <param name="Name">The core's name for it.</param>
/// <param name="File">The MP4's path.</param>
/// <param name="FileStartMs">The recorded time of the file's first picture.</param>
/// <param name="OffsetMs">The user's correction for a clock that disagrees.</param>
/// <param name="Codec">"h264", "h265".</param>
/// <param name="Width">Picture width.</param>
/// <param name="Height">Picture height.</param>
public sealed record EditSource(
    int Source, RecordingCamera Camera, string Name, string File, long FileStartMs, long OffsetMs, string Codec, int Width, int Height);

/// <summary>A shot of the follow cut: <see cref="Source"/> from <see cref="StartMs"/> to <see cref="EndMs"/>, starting <see cref="SourceInMs"/> into its file.</summary>
public sealed record EditSegment(long StartMs, long EndMs, int Source, long SourceInMs);

/// <summary>A rectangle of the canvas in pixels, top-left origin.</summary>
public sealed record EditRect(int X, int Y, int Width, int Height);

/// <summary>The grid's canvas size.</summary>
public sealed record EditCanvas(int Width, int Height);

/// <summary>A grid cell: <see cref="Source"/> in <see cref="Rect"/> for the whole range, black outside <see cref="Present"/>.</summary>
/// <param name="Rect">Where it goes on the canvas.</param>
/// <param name="Source">The plan's source number.</param>
/// <param name="Fit">"fit" (letterboxed) or "fill" (cropped).</param>
/// <param name="Present">[start, end] pairs where the source has video.</param>
public sealed record EditCell(EditRect Rect, int Source, string Fit, IReadOnlyList<(long Start, long End)> Present);

/// <summary>A label the plan asks for.</summary>
public sealed record EditLabel(int Source, string? Name, bool Time);

/// <summary>Which edit.</summary>
public enum EditKind
{
    /// <summary>One picture that cuts between cameras.</summary>
    Follow,

    /// <summary>Cameras side by side on a canvas.</summary>
    Grid,
}

/// <summary>An edit list from <c>nv_edit_compute</c>: a follow cut (<see cref="Segments"/>) or a grid (<see cref="Cells"/>). Times are device-local milliseconds.</summary>
public sealed class EditPlan
{
    /// <summary>Follow or grid.</summary>
    public required EditKind Kind { get; init; }

    /// <summary>The download's ID.</summary>
    public string Download { get; init; } = "";

    /// <summary>The range start.</summary>
    public required long StartMs { get; init; }

    /// <summary>The range end.</summary>
    public required long EndMs { get; init; }

    /// <summary>The name the core suggests for an export.</summary>
    public string SuggestedFile { get; init; } = "";

    /// <summary>The edit could be written without re-encoding.</summary>
    public bool CopyPossible { get; init; }

    /// <summary>The files.</summary>
    public required IReadOnlyList<EditSource> Sources { get; init; }

    /// <summary>Follow only.</summary>
    public IReadOnlyList<EditSegment> Segments { get; init; } = [];

    /// <summary>The labels asked for.</summary>
    public IReadOnlyList<EditLabel> Labels { get; init; } = [];

    /// <summary>Grid only: "2up", "4up".</summary>
    public string? Layout { get; init; }

    /// <summary>Grid only.</summary>
    public EditCanvas? Canvas { get; init; }

    /// <summary>Grid only.</summary>
    public IReadOnlyList<EditCell> Cells { get; init; } = [];

    /// <summary>The source with this number.</summary>
    public EditSource? SourceOf(int n) => Sources.FirstOrDefault(s => s.Source == n);

    /// <summary>Whether the plan shows this camera.</summary>
    public bool Involves(RecordingCamera camera) => Sources.Any(s => s.Camera == camera);

    /// <summary>The plan from nv_edit_compute's answer; null when it is not one, or names a source it does not list.</summary>
    public static EditPlan? Parse(string json)
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json);
            JsonElement root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object || Str(root, "kind") is not { } kindText)
            {
                return null;
            }
            EditKind? kind = kindText switch { "follow" => EditKind.Follow, "grid" => EditKind.Grid, _ => null };
            if (kind is null || Long(root, "start_ms") is not { } start || Long(root, "end_ms") is not { } end || end < start ||
                !root.TryGetProperty("sources", out JsonElement sourceList) || sourceList.ValueKind != JsonValueKind.Array)
            {
                return null;
            }
            var sources = new List<EditSource>();
            foreach (JsonElement s in sourceList.EnumerateArray())
            {
                if (Long(s, "source") is not { } n || !s.TryGetProperty("camera", out JsonElement ce) || RecordingCamera.From(ce) is not { } camera ||
                    Str(s, "file") is not { } file || Long(s, "file_start_ms") is not { } fileStart)
                {
                    return null;
                }
                sources.Add(new EditSource(
                    (int)n, camera, Str(s, "name") ?? "", file, fileStart, Long(s, "offset_ms") ?? 0, Str(s, "codec") ?? "",
                    (int)(Long(s, "width") ?? 0), (int)(Long(s, "height") ?? 0)));
            }
            HashSet<int> known = [.. sources.Select(s => s.Source)];
            var segments = new List<EditSegment>();
            if (root.TryGetProperty("segments", out JsonElement segmentList) && segmentList.ValueKind == JsonValueKind.Array)
            {
                foreach (JsonElement s in segmentList.EnumerateArray())
                {
                    if (Long(s, "start_ms") is not { } a || Long(s, "end_ms") is not { } b || Long(s, "source") is not { } n || !known.Contains((int)n))
                    {
                        return null;
                    }
                    segments.Add(new EditSegment(a, b, (int)n, Long(s, "source_in_ms") ?? 0));
                }
            }
            var cells = new List<EditCell>();
            if (root.TryGetProperty("cells", out JsonElement cellList) && cellList.ValueKind == JsonValueKind.Array)
            {
                foreach (JsonElement c in cellList.EnumerateArray())
                {
                    if (!c.TryGetProperty("rect", out JsonElement r) || Long(r, "x") is not { } x || Long(r, "y") is not { } y ||
                        Long(r, "width") is not { } w || Long(r, "height") is not { } h || Long(c, "source") is not { } n || !known.Contains((int)n))
                    {
                        return null;
                    }
                    var present = new List<(long, long)>();
                    if (c.TryGetProperty("present", out JsonElement spans) && spans.ValueKind == JsonValueKind.Array)
                    {
                        foreach (JsonElement span in spans.EnumerateArray())
                        {
                            if (span.ValueKind != JsonValueKind.Array || span.GetArrayLength() != 2 ||
                                !span[0].TryGetInt64(out long p) || !span[1].TryGetInt64(out long q))
                            {
                                return null;
                            }
                            present.Add((p, q));
                        }
                    }
                    cells.Add(new EditCell(new EditRect((int)x, (int)y, (int)w, (int)h), (int)n, Str(c, "fit") ?? "fit", present));
                }
            }
            var labels = new List<EditLabel>();
            if (root.TryGetProperty("labels", out JsonElement labelList) && labelList.ValueKind == JsonValueKind.Array)
            {
                foreach (JsonElement l in labelList.EnumerateArray())
                {
                    if (Long(l, "source") is { } n)
                    {
                        labels.Add(new EditLabel((int)n, Str(l, "name"), l.TryGetProperty("time", out JsonElement t) && t.ValueKind == JsonValueKind.True));
                    }
                }
            }
            EditCanvas? canvas = root.TryGetProperty("canvas", out JsonElement ce2) && Long(ce2, "width") is { } cw && Long(ce2, "height") is { } ch
                ? new EditCanvas((int)cw, (int)ch) : null;
            return new EditPlan
            {
                Kind = kind.Value,
                Download = Str(root, "download") ?? "",
                StartMs = start,
                EndMs = end,
                SuggestedFile = Str(root, "suggested_file") ?? "",
                CopyPossible = root.TryGetProperty("copy_possible", out JsonElement cp) && cp.ValueKind == JsonValueKind.True,
                Sources = sources,
                Segments = segments,
                Labels = labels,
                Layout = Str(root, "layout"),
                Canvas = canvas,
                Cells = cells,
            };
        }
        catch (Exception e) when (e is JsonException or InvalidOperationException or FormatException)
        {
            return null;
        }
    }

    private static string? Str(JsonElement e, string name) =>
        e.ValueKind == JsonValueKind.Object && e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    private static long? Long(JsonElement e, string name) =>
        e.ValueKind == JsonValueKind.Object && e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.Number && v.TryGetInt64(out long n)
            ? n : null;
}

/// <summary>The follow cut's shot at a time.</summary>
/// <param name="Segment">The segment's index.</param>
/// <param name="Source">The plan's source number.</param>
/// <param name="FilePositionMs">Where in the source's file (milliseconds from its first picture).</param>
public readonly record struct EditShot(int Segment, int Source, long FilePositionMs);

/// <summary>What a plan shows at a time.</summary>
public static class EditTimeline
{
    /// <summary>
    /// The follow cut's shot at <paramref name="t"/>: the segment with start &lt;= t &lt; end (the plan's last instant shows the last
    /// segment's last picture); null in a gap or outside the plan (the picture is black).
    /// </summary>
    public static EditShot? Shot(EditPlan plan, long t)
    {
        if (plan.Kind != EditKind.Follow)
        {
            return null;
        }
        for (int i = 0; i < plan.Segments.Count; i++)
        {
            EditSegment s = plan.Segments[i];
            bool last = i == plan.Segments.Count - 1;
            if (s.StartMs <= t && (t < s.EndMs || (last && t == s.EndMs)))
            {
                return new EditShot(i, s.Source, s.SourceInMs + (t - s.StartMs));
            }
        }
        return null;
    }

    /// <summary>Where the follow cut changes the picture: the start of every segment after the first whose source differs from the one before it, or that follows a gap.</summary>
    public static List<long> Cuts(EditPlan plan)
    {
        var cuts = new List<long>();
        if (plan.Kind != EditKind.Follow)
        {
            return cuts;
        }
        for (int i = 1; i < plan.Segments.Count; i++)
        {
            EditSegment prev = plan.Segments[i - 1], s = plan.Segments[i];
            if (s.Source != prev.Source || s.StartMs != prev.EndMs)
            {
                cuts.Add(s.StartMs);
            }
        }
        return cuts;
    }

    /// <summary>A source's file position at time <paramref name="t"/> (its own clock with the user's offset applied).</summary>
    public static long FilePosition(EditSource source, long t) => t - source.FileStartMs - source.OffsetMs;

    /// <summary>Whether a grid cell has video at <paramref name="t"/> (its spans, each including its start and excluding its end, except at the plan's end).</summary>
    public static bool Present(EditCell cell, long t, long planEnd) =>
        cell.Present.Any(span => span.Start <= t && (t < span.End || (t == span.End && t == planEnd)));

    /// <summary>The grid cell's file position at <paramref name="t"/>, null where the cell is black.</summary>
    public static long? CellPosition(EditPlan plan, int cell, long t)
    {
        if (cell < 0 || cell >= plan.Cells.Count || plan.SourceOf(plan.Cells[cell].Source) is not { } source ||
            !Present(plan.Cells[cell], t, plan.EndMs))
        {
            return null;
        }
        return FilePosition(source, t);
    }

    /// <summary>
    /// The source whose sound plays at <paramref name="t"/>: in the follow cut, the shown camera's (the sound cuts with the picture);
    /// in a grid, the chosen cell's (the first unless the user picked another) while it has video. Nothing when muted or at a speed
    /// without sound.
    /// </summary>
    public static int? AudibleSource(EditPlan plan, long t, double rate, bool muted, int gridCell = 0)
    {
        if (muted || !PlaybackSpeed.HasSound(rate))
        {
            return null;
        }
        if (plan.Kind == EditKind.Follow)
        {
            return Shot(plan, t)?.Source;
        }
        return CellPosition(plan, gridCell, t) is null ? null : plan.Cells[gridCell].Source;
    }

    /// <summary>
    /// The source a follow cut shows next after <paramref name="t"/>, with the time it starts and where in its file, so the renderer can
    /// have its picture ready at the cut. Null after the last cut.
    /// </summary>
    public static (long At, EditShot Shot)? NextShot(EditPlan plan, long t)
    {
        if (plan.Kind != EditKind.Follow)
        {
            return null;
        }
        if (Shot(plan, t) is not { } current)
        {
            int first = plan.Segments.ToList().FindIndex(s => s.StartMs > t);
            if (first < 0)
            {
                return null;
            }
            EditSegment f = plan.Segments[first];
            return (f.StartMs, new EditShot(first, f.Source, f.SourceInMs));
        }
        for (int i = current.Segment + 1; i < plan.Segments.Count; i++)
        {
            EditSegment s = plan.Segments[i];
            if (s.Source != current.Source)
            {
                return (s.StartMs, new EditShot(i, s.Source, s.SourceInMs));
            }
        }
        return null;
    }
}

/// <summary>One picture of an edit placed in a view or an output frame.</summary>
/// <param name="Source">The plan's source number.</param>
/// <param name="FilePositionMs">Where in its file.</param>
/// <param name="Rect">Where it goes (top-left origin, in the units of the bounds given).</param>
/// <param name="Fill">Cropped to fill <paramref name="Rect"/> rather than letterboxed in it.</param>
/// <param name="Cell">For a grid, the cell's index.</param>
public readonly record struct EditLayer(int Source, long FilePositionMs, Rect Rect, bool Fill, int? Cell = null);

/// <summary>Where a picture goes in a rectangle: the destination, and the part of the picture drawn.</summary>
/// <param name="Destination">In the rectangle's units.</param>
/// <param name="Source">In the picture's pixels.</param>
public readonly record struct PictureFit(Rect Destination, Rect Source)
{
    /// <summary>Fits (letterboxed) or fills (cropped, centered) a picture in a cell, in whole units.</summary>
    public static PictureFit Of(double pictureWidth, double pictureHeight, Rect cell, bool fill)
    {
        var full = new Rect(0, 0, pictureWidth, pictureHeight);
        if (pictureWidth <= 0 || pictureHeight <= 0 || cell.Width <= 0 || cell.Height <= 0)
        {
            return new PictureFit(cell, full);
        }
        if (!fill)
        {
            return new PictureFit(LayoutGeometry.Fit(pictureWidth, pictureHeight, cell), full);
        }
        double pa = pictureWidth / pictureHeight, ca = cell.Width / cell.Height;
        if (pa > ca)
        {
            double w = Math.Round(pictureHeight * ca);
            return new PictureFit(cell, new Rect(Math.Floor((pictureWidth - w) / 2), 0, w, pictureHeight));
        }
        double h = Math.Round(pictureWidth / ca);
        return new PictureFit(cell, new Rect(0, Math.Floor((pictureHeight - h) / 2), pictureWidth, h));
    }
}

/// <summary>
/// How an edit fills a frame. The follow cut's picture fills the bounds (letterboxed by the renderer); a grid's canvas is letterboxed
/// into the bounds and its cells scaled with it, so the screen shows what an export of the same canvas would.
/// </summary>
public static class EditComposition
{
    /// <summary>The canvas's place in <paramref name="bounds"/> (the whole bounds for a follow cut).</summary>
    public static Rect Frame(EditPlan plan, Rect bounds) =>
        plan.Kind == EditKind.Grid && plan.Canvas is { Width: > 0, Height: > 0 } c
            ? PictureFit.Of(c.Width, c.Height, bounds, fill: false).Destination
            : bounds;

    /// <summary>Every cell's rectangle in <paramref name="bounds"/>, shown or not (a grid; empty for a follow cut).</summary>
    public static List<Rect> CellRects(EditPlan plan, Rect bounds)
    {
        if (plan.Kind != EditKind.Grid || plan.Canvas is not { Width: > 0, Height: > 0 } c)
        {
            return [];
        }
        Rect f = Frame(plan, bounds);
        double sx = f.Width / c.Width, sy = f.Height / c.Height;
        return [.. plan.Cells.Select(cell => new Rect(f.X + cell.Rect.X * sx, f.Y + cell.Rect.Y * sy, cell.Rect.Width * sx, cell.Rect.Height * sy))];
    }

    /// <summary>The pictures shown at <paramref name="t"/>: the follow cut's one shot, or each grid cell with video.</summary>
    public static List<EditLayer> Layers(EditPlan plan, long t, Rect bounds)
    {
        var layers = new List<EditLayer>();
        if (plan.Kind == EditKind.Follow)
        {
            if (EditTimeline.Shot(plan, t) is { } shot)
            {
                layers.Add(new EditLayer(shot.Source, shot.FilePositionMs, bounds, false));
            }
            return layers;
        }
        List<Rect> rects = CellRects(plan, bounds);
        for (int i = 0; i < plan.Cells.Count; i++)
        {
            if (EditTimeline.CellPosition(plan, i, t) is { } p)
            {
                layers.Add(new EditLayer(plan.Cells[i].Source, p, rects[i], plan.Cells[i].Fit == "fill", i));
            }
        }
        return layers;
    }
}

/// <summary>
/// The edit's own clock: the transport (play, pause, speed, seek) over the plan's range, read against a host time the caller supplies
/// (seconds), so tests and the self-test drive it with a simulated clock. It stops at the end.
/// </summary>
public sealed class EditClock
{
    private long _anchorMs;
    private double _anchorHost;

    /// <summary>A clock over a range, at <paramref name="positionMs"/> (the start when null).</summary>
    public EditClock(long startMs, long endMs, long? positionMs = null)
    {
        StartMs = startMs;
        EndMs = Math.Max(endMs, startMs);
        _anchorMs = Math.Min(Math.Max(positionMs ?? startMs, StartMs), EndMs);
    }

    /// <summary>The range start.</summary>
    public long StartMs { get; }

    /// <summary>The range end.</summary>
    public long EndMs { get; }

    /// <summary>The speed.</summary>
    public double Rate { get; private set; } = 1;

    /// <summary>Whether the clock runs.</summary>
    public bool Playing { get; private set; }

    /// <summary>The position at host time <paramref name="now"/>.</summary>
    public long Position(double now)
    {
        if (!Playing)
        {
            return _anchorMs;
        }
        long moved = (long)Math.Floor((now - _anchorHost) * 1000 * Rate);
        return Math.Min(Math.Max(_anchorMs + Math.Max(moved, 0), StartMs), EndMs);
    }

    /// <summary>Whether the clock reached the end.</summary>
    public bool Ended(double now) => Position(now) >= EndMs;

    /// <summary>The transport's state at <paramref name="now"/>.</summary>
    public NvPlaybackState State(double now) =>
        Ended(now) ? NvPlaybackState.Ended : Playing ? NvPlaybackState.Playing : NvPlaybackState.Paused;

    /// <summary>Plays from where it is (from the start again at the end).</summary>
    public void Play(double now)
    {
        _anchorMs = Ended(now) ? StartMs : Position(now);
        _anchorHost = now;
        Playing = true;
    }

    /// <summary>Pauses where it is.</summary>
    public void Pause(double now)
    {
        _anchorMs = Position(now);
        _anchorHost = now;
        Playing = false;
    }

    /// <summary>Moves to a time (kept within the range).</summary>
    public void Seek(long ms, double now)
    {
        _anchorMs = Math.Min(Math.Max(ms, StartMs), EndMs);
        _anchorHost = now;
    }

    /// <summary>Changes the speed from where it is.</summary>
    public void SetRate(double rate, double now)
    {
        if (rate <= 0)
        {
            return;
        }
        _anchorMs = Position(now);
        _anchorHost = now;
        Rate = rate;
    }

    /// <summary>The transport's view at <paramref name="now"/>.</summary>
    public TransportState Transport(double now) => new(StartMs, EndMs, Position(now), State(now), Rate);
}
