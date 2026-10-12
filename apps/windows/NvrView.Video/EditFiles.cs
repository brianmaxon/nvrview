// An edit list's downloaded files, open for drawing (docs/UX.md "Recordings",
// "Follow and Grid"): each source's MP4 decoded by Media Foundation
// (RecordedFile), and at any time t the plan's pictures as draws for the D3D11
// grid renderer, the one live view uses. Nothing is re-encoded; the screen and
// an export draw the same rectangles: the screen through Request (decoding off
// the render thread, the newest request winning) and Place on the window's
// render thread, an export or the self-test through Prepare and Render
// (decoding before drawing, frame by frame).

using NvrView.Shell;

namespace NvrView.Video;

/// <summary>Opens an edit's files off the UI thread.</summary>
/// <param name="gpuOf">The shared GPU device (read each time: another window may replace it).</param>
/// <param name="allowHardware">False forces CPU decoding.</param>
public sealed class EditFilesFactory(Func<GpuDevice> gpuOf, bool allowHardware = true) : IEditFilesFactory
{
    /// <inheritdoc/>
    public Task<IEditFiles> OpenAsync(EditPlan plan, CancellationToken cancel) =>
        Task.Run(() => (IEditFiles)EditFiles.Open(plan, gpuOf, allowHardware), cancel);
}

/// <summary>An edit's open files (see the file comment).</summary>
public sealed class EditFiles : IEditFiles
{
    /// <summary>How long before a cut the incoming camera starts decoding its first picture, so the cut shows it at once.</summary>
    public const long PrerollMs = 1500;

    private readonly Func<GpuDevice> _gpuOf;
    private readonly Dictionary<int, RecordedFile> _files;
    private readonly Dictionary<int, string> _failures;

    private EditFiles(EditPlan plan, Func<GpuDevice> gpuOf, Dictionary<int, RecordedFile> files, Dictionary<int, string> failures)
    {
        Plan = plan;
        _gpuOf = gpuOf;
        _files = files;
        _failures = failures;
    }

    /// <inheritdoc/>
    public EditPlan Plan { get; }

    /// <inheritdoc/>
    public IReadOnlyDictionary<int, string> Failures => _failures;

    /// <inheritdoc/>
    public bool AnyOpen => _files.Count > 0;

    /// <summary>The most draws <see cref="Place"/> writes.</summary>
    public int MaxDraws => Math.Max(Plan.Cells.Count, 1);

    /// <summary>The open file of a source, null when it could not be opened.</summary>
    public RecordedFile? File(int source) => _files.GetValueOrDefault(source);

    /// <summary>Opens every source's file (only those the plan shows).</summary>
    public static EditFiles Open(EditPlan plan, Func<GpuDevice> gpuOf, bool allowHardware = true)
    {
        HashSet<int> used = [.. plan.Segments.Select(s => s.Source), .. plan.Cells.Select(c => c.Source)];
        if (used.Count == 0)
        {
            used = [.. plan.Sources.Select(s => s.Source)];
        }
        var files = new Dictionary<int, RecordedFile>();
        var failures = new Dictionary<int, string>();
        foreach (EditSource s in plan.Sources.Where(s => used.Contains(s.Source)))
        {
            RecordedFile? file;
            string? failure;
            try
            {
                file = RecordedFile.Open(s.File, gpuOf, allowHardware, out failure);
            }
            catch (VideoException e)
            {
                file = null;
                failure = e.Message;
            }
            if (file is null)
            {
                failures[s.Source] = failure ?? "unreadable";
            }
            else
            {
                files[s.Source] = file;
            }
        }
        return new EditFiles(plan, gpuOf, files, failures);
    }

    /// <inheritdoc/>
    public string? FileOf(int source) => _files.GetValueOrDefault(source)?.Path;

    /// <inheritdoc/>
    public long? ShownPositionMs(int source) => _files.GetValueOrDefault(source)?.ShownPositionMs;

    // The file positions the pictures at t need, with the follow cut's next camera when its cut is near.
    private List<(int Source, long Ms)> Wanted(long t)
    {
        List<(int Source, long Ms)> wanted = [.. EditComposition.Layers(Plan, t, new Rect(0, 0, 0, 0)).Select(l => (l.Source, l.FilePositionMs))];
        if (EditTimeline.NextShot(Plan, t) is { } next && next.At - t <= PrerollMs && !wanted.Any(w => w.Source == next.Shot.Source))
        {
            wanted.Add((next.Shot.Source, next.Shot.FilePositionMs));
        }
        return wanted;
    }

    /// <inheritdoc/>
    public void Request(long t, bool keyframesOnly)
    {
        foreach ((int source, long ms) in Wanted(t))
        {
            _files.GetValueOrDefault(source)?.Want(ms, keyframesOnly);
        }
    }

    /// <summary>Decodes the pictures at <paramref name="t"/> before returning (export, self-test).</summary>
    public void Prepare(long t, bool keyframesOnly = false)
    {
        foreach (EditLayer l in EditComposition.Layers(Plan, t, new Rect(0, 0, 0, 0)))
        {
            _files.GetValueOrDefault(l.Source)?.Show(l.FilePositionMs, keyframesOnly);
        }
    }

    /// <summary>
    /// Writes the draws for the edit at <paramref name="t"/> into <paramref name="draws"/> (at least <see cref="MaxDraws"/>):
    /// each picture fitted (or filled) in its place in <paramref name="bounds"/> (pixels of the target, top-left origin). A
    /// missing file or a source without a picture yet draws nothing (black). Call it under the device lock, which also
    /// guards the pictures.
    /// </summary>
    public int Place(long t, Rect bounds, Span<TileDraw> draws)
    {
        int n = 0;
        foreach (EditLayer layer in EditComposition.Layers(Plan, t, bounds))
        {
            if (_files.GetValueOrDefault(layer.Source)?.Picture is not { HasPicture: true } picture || n >= draws.Length)
            {
                continue;
            }
            PictureFit fit = PictureFit.Of(picture.PictureSize.Width, picture.PictureSize.Height, layer.Rect, layer.Fill);
            draws[n++] = new TileDraw
            {
                Picture = picture,
                Tag = layer.Cell,
                DestX = (float)fit.Destination.X,
                DestY = (float)fit.Destination.Y,
                DestWidth = (float)fit.Destination.Width,
                DestHeight = (float)fit.Destination.Height,
                SrcX = (float)fit.Source.X,
                SrcY = (float)fit.Source.Y,
                SrcWidth = (float)fit.Source.Width,
                SrcHeight = (float)fit.Source.Height,
            };
        }
        return n;
    }

    /// <summary>
    /// Where each picture at <paramref name="t"/> is drawn in <paramref name="bounds"/> (letterboxed), by source and cell:
    /// what the self-test samples.
    /// </summary>
    public List<(int Source, int? Cell, Rect Rect)> PictureRects(long t, Rect bounds)
    {
        var rects = new List<(int, int?, Rect)>();
        foreach (EditLayer layer in EditComposition.Layers(Plan, t, bounds))
        {
            if (_files.GetValueOrDefault(layer.Source) is { } file)
            {
                rects.Add((layer.Source, layer.Cell, PictureFit.Of(file.Info.Width, file.Info.Height, layer.Rect, layer.Fill).Destination));
            }
        }
        return rects;
    }

    /// <summary>Decodes and draws the edit at <paramref name="t"/> into the renderer's offscreen frame (an export's frame; the self-test's samples), and reads it back.</summary>
    public RenderedImage Render(long t, GridRenderer renderer)
    {
        Prepare(t);
        var draws = new TileDraw[MaxDraws];
        var sequences = new ulong[MaxDraws];
        int n;
        using (_gpuOf().Lock())
        {
            n = Place(t, new Rect(0, 0, renderer.Width, renderer.Height), draws);
        }
        renderer.RenderDraws(draws.AsSpan(0, n), sequences);
        return renderer.ReadBack();
    }

    /// <inheritdoc/>
    public void Dispose()
    {
        foreach (RecordedFile f in _files.Values)
        {
            f.Dispose();
        }
    }
}
