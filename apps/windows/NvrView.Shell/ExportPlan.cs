// Export, the pure part (docs/UX.md "Recordings", "Export"; the core's side is
// FFI-RECORDINGS.md section 7a): the in and out points the user sets, the
// requests sent to the core, the plans that come back (a trim, still
// images), and the rules for an exported follow cut or grid (output size,
// frame times, whose sound is written when). The same rules, and the same
// test vectors, as the macOS app's ExportPlan.swift. The writers (NvrView.Video)
// only carry these out.

using System.Globalization;
using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>Which end of the span a press on the scrubber grabbed.</summary>
public enum PointHandle
{
    In,
    Out,
}

/// <summary>
/// The in and out points on the scrubber: device-local milliseconds, either
/// or both unset. An unset point means the start or the end of the range the
/// scrubber spans.
/// </summary>
public readonly record struct InOutPoints(long? InMs = null, long? OutMs = null)
{
    /// <summary>The shortest span an export may cover.</summary>
    public const long MinimumMs = 100;

    /// <summary>Neither point is set.</summary>
    public bool IsEmpty => InMs is null && OutMs is null;

    private static long Clamp(long ms, long lo, long hi) => Math.Min(Math.Max(ms, lo), hi);

    /// <summary>
    /// Sets the in point (kept inside the range). An in point at or after the
    /// out point clears the out point, so the points never cross.
    /// </summary>
    public InOutPoints SetIn(long ms, long lo, long hi)
    {
        long at = Clamp(ms, lo, hi);
        return new InOutPoints(at, OutMs is long o && o < at + MinimumMs ? null : OutMs);
    }

    /// <summary>Sets the out point; an out point at or before the in point clears the in point.</summary>
    public InOutPoints SetOut(long ms, long lo, long hi)
    {
        long at = Clamp(ms, lo, hi);
        return new InOutPoints(InMs is long i && i > at - MinimumMs ? null : InMs, at);
    }

    /// <summary>
    /// The points that lie inside the range, for a range that changed
    /// (another incident, another view): a point outside it is dropped.
    /// </summary>
    public InOutPoints Kept(long lo, long hi)
    {
        long? i = InMs is long a && a >= lo && a <= hi ? a : null;
        long? o = OutMs is long b && b >= lo && b <= hi ? b : null;
        if (i is long x && o is long y && y < x + MinimumMs)
        {
            o = null;
        }
        return new InOutPoints(i, o);
    }

    /// <summary>
    /// The span to export in the range: the points, with the range's ends for
    /// an unset one; null when it would be shorter than <see cref="MinimumMs"/>.
    /// </summary>
    public (long Start, long End)? Window(long lo, long hi)
    {
        long start = Clamp(InMs ?? lo, lo, hi);
        long end = Clamp(OutMs ?? hi, lo, hi);
        return end - start >= MinimumMs ? (start, end) : null;
    }

    /// <summary>
    /// Which point a press at <paramref name="fraction"/> (0 to 1 across the
    /// range) grabs: the nearer one within the tolerance, null when none is set near it.
    /// </summary>
    public PointHandle? HandleAt(double fraction, long lo, long hi, double tolerance)
    {
        double span = hi - lo;
        if (span <= 0)
        {
            return null;
        }
        double? Distance(long? ms) => ms is long m ? Math.Abs((m - lo) / span - fraction) : null;
        double? di = Distance(InMs), dout = Distance(OutMs);
        if (di is double a && dout is double b)
        {
            return Math.Min(a, b) <= tolerance ? (a <= b ? PointHandle.In : PointHandle.Out) : null;
        }
        if (di is double a1)
        {
            return a1 <= tolerance ? PointHandle.In : null;
        }
        if (dout is double b1)
        {
            return b1 <= tolerance ? PointHandle.Out : null;
        }
        return null;
    }

    /// <summary>Moves a grabbed point to <paramref name="ms"/>, never past the other one.</summary>
    public InOutPoints Move(PointHandle handle, long ms, long lo, long hi)
    {
        if (handle == PointHandle.In)
        {
            long limit = (OutMs ?? hi) - MinimumMs;
            return this with { InMs = Math.Min(Math.Max(ms, lo), Math.Max(limit, lo)) };
        }
        long floor = (InMs ?? lo) + MinimumMs;
        return this with { OutMs = Math.Max(Math.Min(ms, hi), Math.Min(floor, hi)) };
    }

    /// <summary>"07:42:05.250 to 07:42:11.000", with "start" or "end" for an unset point.</summary>
    public string Text(Func<long, string> clock) => $"{(InMs is long i ? clock(i) : "start")} to {(OutMs is long o ? clock(o) : "end")}";
}

/// <summary>Everything an export request can carry besides its subject.</summary>
/// <param name="Dest">An absolute directory the user chose; null for the core's default.</param>
/// <param name="ShareSafe">No metadata of the app's own and neutral camera names ("Camera A").</param>
public sealed record ExportOptions(string? Dest = null, bool ShareSafe = false)
{
    /// <summary>Adds <c>dest</c> and <c>share_safe</c> (each only when set) to a request.</summary>
    public void AddTo(JsonObject request)
    {
        if (Dest is not null)
        {
            request["dest"] = Dest;
        }
        if (ShareSafe)
        {
            request["share_safe"] = true;
        }
    }
}

/// <summary>The export requests of FFI-RECORDINGS.md section 7a, as JSON text.</summary>
public static class ExportRequest
{
    private static JsonNode CameraNode(RecordingCamera c) => JsonNode.Parse(c.Json)!;

    /// <summary><c>nv_edit_compute</c> of kind <c>trim</c>; <paramref name="cameras"/> null for every camera of the download.</summary>
    public static string Trim(string download, IEnumerable<RecordingCamera>? cameras, long inMs, long outMs, bool exact, ExportOptions options)
    {
        var o = new JsonObject { ["kind"] = "trim", ["download"] = download };
        if (cameras is not null)
        {
            o["cameras"] = new JsonArray([.. cameras.Select(CameraNode)]);
        }
        o["in_ms"] = inMs;
        o["out_ms"] = outMs;
        o["cut"] = exact ? "exact" : "keyframe";
        options.AddTo(o);
        return o.ToJsonString();
    }

    /// <summary><c>nv_edit_compute</c> of kind <c>follow</c> or (with a layout, "2up" or "4up") <c>grid</c>, cut to a window.</summary>
    public static string Edit(string? gridLayout, string download, (long Start, long End)? window, ExportOptions options)
    {
        var o = new JsonObject { ["kind"] = gridLayout is null ? "follow" : "grid", ["download"] = download };
        if (gridLayout is not null)
        {
            o["layout"] = gridLayout;
            o["canvas"] = new JsonObject { ["width"] = 1920, ["height"] = 1080 };
        }
        if (window is { } w)
        {
            o["in_ms"] = w.Start;
            o["out_ms"] = w.End;
        }
        options.AddTo(o);
        return o.ToJsonString();
    }

    /// <summary><c>nv_download_range</c>: whole seconds, the start rounded down and the end up.</summary>
    public static string Range(long startMs, long endMs, IEnumerable<RecordingCamera> cameras, string? name, ExportOptions options)
    {
        long start = DeviceTime.FloorDiv(startMs, 1000), end = -DeviceTime.FloorDiv(-endMs, 1000);
        var list = new JsonArray([.. cameras.Select(c => (JsonNode)new JsonObject { ["camera"] = CameraNode(c), ["tier"] = "main" })]);
        var o = new JsonObject { ["start"] = start, ["end"] = end, ["cameras"] = list };
        if (!string.IsNullOrWhiteSpace(name))
        {
            o["name"] = name;
        }
        options.AddTo(o);
        return o.ToJsonString();
    }

    /// <summary><c>nv_still_plan</c> for the pictures a playback's tiles show now.</summary>
    public static string Stills(ulong playback, IEnumerable<ulong>? tiles, PictureFormat format, ExportOptions options)
    {
        var o = new JsonObject { ["playback"] = playback };
        if (tiles is not null)
        {
            o["tiles"] = new JsonArray([.. tiles.Select(t => (JsonNode)JsonValue.Create(t))]);
        }
        o["format"] = format.Key();
        options.AddTo(o);
        return o.ToJsonString();
    }

    /// <summary><c>nv_still_plan</c> for one camera's picture at a time in a downloaded file.</summary>
    public static string Still(string download, RecordingCamera camera, long atMs, bool exact, PictureFormat format, ExportOptions options)
    {
        var o = new JsonObject
        {
            ["download"] = download,
            ["camera"] = CameraNode(camera),
            ["at_ms"] = atMs,
            ["exact"] = exact,
            ["format"] = format.Key(),
        };
        options.AddTo(o);
        return o.ToJsonString();
    }
}

/// <summary>A trim plan (<c>nv_edit_compute</c> of kind <c>trim</c>).</summary>
public sealed record TrimPlan(string Download, long StartMs, long EndMs, IReadOnlyList<TrimOutput> Outputs, bool ShareSafe)
{
    /// <summary>Reads a plan; null when it is not a trim, has no outputs, or an output ends before it starts.</summary>
    public static TrimPlan? Parse(string json)
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json);
            JsonElement root = doc.RootElement;
            if (DownloadProgress.Str(root, "kind") != "trim" || DownloadProgress.Str(root, "download") is not { } download
                || DownloadProgress.Long(root, "start_ms") is not long start || DownloadProgress.Long(root, "end_ms") is not long end
                || !root.TryGetProperty("outputs", out JsonElement list) || list.ValueKind != JsonValueKind.Array)
            {
                return null;
            }
            var outputs = new List<TrimOutput>();
            foreach (JsonElement e in list.EnumerateArray())
            {
                if (DownloadProgress.Long(e, "source") is not long source || !e.TryGetProperty("camera", out JsonElement ce)
                    || RecordingCamera.From(ce) is not { } camera || DownloadProgress.Str(e, "file") is not { } file
                    || DownloadProgress.Long(e, "source_in_ms") is not long sin || DownloadProgress.Long(e, "source_out_ms") is not long sout
                    || sout <= sin)
                {
                    return null;
                }
                outputs.Add(new TrimOutput((int)source, camera, DownloadProgress.Str(e, "name") ?? "", file, sin, sout,
                    DownloadProgress.Bool(e, "copy_possible"), DownloadProgress.Str(e, "suggested_file") ?? ""));
            }
            return outputs.Count == 0 ? null : new TrimPlan(download, start, end, outputs, DownloadProgress.Bool(root, "share_safe"));
        }
        catch (JsonException)
        {
            return null;
        }
    }
}

/// <summary>One camera's file of a trim.</summary>
/// <param name="File">The downloaded MP4.</param>
/// <param name="SourceInMs">Where the output starts in the file, in milliseconds from its first picture.</param>
/// <param name="SourceOutMs">Where the output ends in the file.</param>
/// <param name="CopyPossible">Whether the samples can be copied without re-encoding.</param>
public sealed record TrimOutput(
    int Source, RecordingCamera Camera, string Name, string File, long SourceInMs, long SourceOutMs, bool CopyPossible, string SuggestedFile);

/// <summary>The answer of <c>nv_still_plan</c>.</summary>
public sealed record StillPlan(IReadOnlyList<StillItem> Stills)
{
    /// <summary>Reads a plan; null when it has no stills.</summary>
    public static StillPlan? Parse(string json)
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json);
            if (!doc.RootElement.TryGetProperty("stills", out JsonElement list) || list.ValueKind != JsonValueKind.Array)
            {
                return null;
            }
            var stills = new List<StillItem>();
            foreach (JsonElement e in list.EnumerateArray())
            {
                if (!e.TryGetProperty("camera", out JsonElement ce) || RecordingCamera.From(ce) is not { } camera)
                {
                    return null;
                }
                stills.Add(new StillItem(
                    DownloadProgress.Long(e, "tile") is long t ? (ulong)t : null, camera, DownloadProgress.Str(e, "name") ?? "",
                    DownloadProgress.Long(e, "generation"), DownloadProgress.Long(e, "position_ms"), DownloadProgress.Str(e, "file"),
                    DownloadProgress.Long(e, "source_in_ms"), DownloadProgress.Str(e, "suggested_file") ?? ""));
            }
            return stills.Count == 0 ? null : new StillPlan(stills);
        }
        catch (JsonException)
        {
            return null;
        }
    }
}

/// <summary>One picture to save.</summary>
/// <param name="Tile">From a playback: the tile whose picture to save.</param>
/// <param name="Generation">From a playback: the picture's generation.</param>
/// <param name="PositionMs">From a playback: the picture's recorded time.</param>
/// <param name="File">From a file: the MP4.</param>
/// <param name="SourceInMs">From a file: the position in it to decode at.</param>
public sealed record StillItem(
    ulong? Tile, RecordingCamera Camera, string Name, long? Generation, long? PositionMs, string? File, long? SourceInMs, string SuggestedFile);

/// <summary>A follow cut or grid export's plan, as far as the export needs it (the core's <c>nv_edit_compute</c> answer).</summary>
public sealed record EditExportPlan(string Kind, string Download, long StartMs, long EndMs, string SuggestedFile)
{
    /// <summary>Reads the members every follow or grid plan has; null when one is missing.</summary>
    public static EditExportPlan? Parse(string json)
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json);
            JsonElement r = doc.RootElement;
            return DownloadProgress.Str(r, "kind") is { } kind and ("follow" or "grid") && DownloadProgress.Str(r, "download") is { } download
                && DownloadProgress.Long(r, "start_ms") is long start && DownloadProgress.Long(r, "end_ms") is long end && end > start
                ? new EditExportPlan(kind, download, start, end, DownloadProgress.Str(r, "suggested_file") ?? "")
                : null;
        }
        catch (JsonException)
        {
            return null;
        }
    }
}

/// <summary>A piece of one file's sound written into an exported follow cut or grid.</summary>
/// <param name="Source">The plan's source (file).</param>
/// <param name="FileStartMs">Where in the file the piece starts.</param>
/// <param name="FileEndMs">Where it ends.</param>
/// <param name="AtMs">Where it starts in the output, from its first frame.</param>
public sealed record AudioPiece(int Source, long FileStartMs, long FileEndMs, long AtMs)
{
    /// <summary>
    /// The sound of an exported edit, by the rules of the player (<see cref="EditTimeline.AudibleSource"/>): the follow cut
    /// carries the shown camera's sound (it cuts with the picture); a grid carries one cell's (the first unless another is
    /// chosen) while it has video. Adjacent pieces of one file join.
    /// </summary>
    public static List<AudioPiece> Of(EditPlan plan, int gridCell = 0)
    {
        var raw = new List<AudioPiece>();
        if (plan.Kind == EditKind.Follow)
        {
            foreach (EditSegment s in plan.Segments.Where(s => s.EndMs > s.StartMs))
            {
                raw.Add(new AudioPiece(s.Source, s.SourceInMs, s.SourceInMs + (s.EndMs - s.StartMs), s.StartMs - plan.StartMs));
            }
        }
        else
        {
            if (gridCell < 0 || gridCell >= plan.Cells.Count || plan.SourceOf(plan.Cells[gridCell].Source) is not { } source)
            {
                return [];
            }
            foreach ((long start, long end) in plan.Cells[gridCell].Present)
            {
                long a = Math.Max(start, plan.StartMs), b = Math.Min(end, plan.EndMs);
                if (b > a)
                {
                    raw.Add(new AudioPiece(source.Source, EditTimeline.FilePosition(source, a), EditTimeline.FilePosition(source, b), a - plan.StartMs));
                }
            }
            raw.Sort((x, y) => x.AtMs.CompareTo(y.AtMs));
        }
        return Join(raw);
    }

    /// <summary>Joins adjacent pieces of one file that follow each other in file and time.</summary>
    public static List<AudioPiece> Join(IEnumerable<AudioPiece> pieces)
    {
        var joined = new List<AudioPiece>();
        foreach (AudioPiece p in pieces)
        {
            if (joined.Count > 0 && joined[^1] is { } last && last.Source == p.Source
                && last.AtMs + (last.FileEndMs - last.FileStartMs) == p.AtMs && last.FileEndMs == p.FileStartMs)
            {
                joined[^1] = last with { FileEndMs = p.FileEndMs };
            }
            else
            {
                joined.Add(p);
            }
        }
        return joined;
    }
}

/// <summary>How an exported follow cut or grid is made.</summary>
public static class ExportGeometry
{
    /// <summary>Frames per second of an exported follow cut or grid.</summary>
    public const int FramesPerSecond = 30;

    /// <summary>The largest output.</summary>
    public const int MaxWidth = 1920, MaxHeight = 1080;

    private static int Even(double v) => Math.Max(2, (int)Math.Floor(v) / 2 * 2);

    /// <summary>The size scaled down (never up) to fit 1920 by 1080, with even sides.</summary>
    public static (int Width, int Height) Fitted(int width, int height)
    {
        if (width <= 0 || height <= 0)
        {
            return (1280, 720);
        }
        double scale = Math.Min(1, Math.Min((double)MaxWidth / width, (double)MaxHeight / height));
        return (Even(width * scale), Even(height * scale));
    }

    /// <summary>The follow cut's output: the largest picture among the sources it uses (smaller ones are letterboxed in it), fitted.</summary>
    public static (int Width, int Height) FollowSize(IEnumerable<(int Width, int Height)> usedSources)
    {
        int w = 0, h = 0;
        foreach ((int sw, int sh) in usedSources)
        {
            w = Math.Max(w, sw);
            h = Math.Max(h, sh);
        }
        return Fitted(w, h);
    }

    /// <summary>An edit's output: a grid's canvas, or the follow cut's largest source among those it shows, fitted.</summary>
    public static (int Width, int Height) EditSize(EditPlan plan)
    {
        if (plan.Kind == EditKind.Grid)
        {
            EditCanvas canvas = plan.Canvas ?? new EditCanvas(MaxWidth, MaxHeight);
            return Fitted(canvas.Width, canvas.Height);
        }
        HashSet<int> used = [.. plan.Segments.Select(s => s.Source)];
        return FollowSize(plan.Sources.Where(s => used.Contains(s.Source)).Select(s => (s.Width, s.Height)));
    }

    /// <summary>How many frames a plan from <paramref name="startMs"/> to <paramref name="endMs"/> makes.</summary>
    public static int FrameCount(long startMs, long endMs, int fps = FramesPerSecond)
    {
        long ms = Math.Max(endMs - startMs, 0);
        return (int)Math.Max(1, (ms * fps + 999) / 1000);
    }

    /// <summary>The plan time (device-local milliseconds) of frame <paramref name="i"/>; the last frame never lies past the end.</summary>
    public static long FrameTime(long startMs, long endMs, int i, int fps = FramesPerSecond) => Math.Min(startMs + (i * 1000L / fps), endMs);
}

/// <summary>What an export reports when it ends.</summary>
public enum ExportResult
{
    Done,
    Canceled,
    Failed,
}

/// <summary>One finished (or failed or canceled) export of one file.</summary>
/// <param name="Reason">Why it failed (shown to the user).</param>
public sealed record ExportOutcome(ExportResult Result, string Path, string? Reason = null, int Width = 0, int Height = 0, int Frames = 0, long Bytes = 0)
{
    public static ExportOutcome Failed(string path, string reason) => new(ExportResult.Failed, path, reason);

    public static ExportOutcome Canceled(string path) => new(ExportResult.Canceled, path);
}

/// <summary>Cancel and progress of a running export, shared between the writer's thread and the screen.</summary>
public sealed class ExportControl
{
    private volatile bool _canceled;
    private long _fractionBits;

    /// <summary>Asks the export to stop at its next sample.</summary>
    public void Cancel() => _canceled = true;

    public bool IsCanceled => _canceled;

    /// <summary>0 to 1.</summary>
    public double Fraction
    {
        get => BitConverter.Int64BitsToDouble(Interlocked.Read(ref _fractionBits));
        set => Interlocked.Exchange(ref _fractionBits, BitConverter.DoubleToInt64Bits(Math.Min(Math.Max(value, 0), 1)));
    }
}

/// <summary>Where an export is written and how it is finished.</summary>
public static class ExportFile
{
    /// <summary>
    /// A hidden-by-name file next to the destination; the finished export
    /// replaces the destination only when it is whole, so a canceled or failed
    /// export leaves nothing (and a file the user agreed to replace stays until then).
    /// </summary>
    public static string Partial(string path) =>
        Path.Combine(Path.GetDirectoryName(Path.GetFullPath(path))!, $".nvrview-export-{Guid.NewGuid():N}{Path.GetExtension(path)}");

    /// <summary>Makes the finished partial file share-safe if asked, moves it over <paramref name="path"/>, and answers its size.</summary>
    public static long Commit(string partial, string path, bool shareSafe)
    {
        if (shareSafe)
        {
            Mp4ShareSafe.Apply(partial);
        }
        File.Move(partial, path, overwrite: true);
        return new FileInfo(path).Length;
    }

    /// <summary>Removes a partial file; nothing happens when it is not there.</summary>
    public static void Discard(string partial)
    {
        try
        {
            File.Delete(partial);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
        }
    }

    /// <summary>
    /// <paramref name="path"/>, or with " (2)", " (3)", ... before the
    /// extension when the file exists or its name is in <paramref name="taken"/>.
    /// </summary>
    public static string Free(string path, Func<string, bool>? exists = null, ISet<string>? taken = null)
    {
        exists ??= File.Exists;
        string dir = Path.GetDirectoryName(path) ?? "", stem = Path.GetFileNameWithoutExtension(path), ext = Path.GetExtension(path);
        string candidate = path;
        for (int n = 2; exists(candidate) || taken?.Contains(Path.GetFileName(candidate)) == true; n++)
        {
            candidate = Path.Combine(dir, string.Create(CultureInfo.InvariantCulture, $"{stem} ({n}){ext}"));
        }
        return candidate;
    }
}
