// What export does on Windows (docs/UX.md "Recordings", "Export"): the Media
// Foundation writers of NvrView.Video behind the shell's IExportWriters, the
// save and folder pickers behind IExportDialogs, and the frames a follow cut
// or grid is drawn from (EditFrames: the Recordings window opens W3's
// EditFrameRenderer over a second set of the plan's files, so the window's
// pictures never move). The sound follows the player's rules
// (AudioPiece.Of): the shown camera's in a follow cut, the chosen cell's in a
// grid while it has video, copied from the files as AAC.

using NvrView.Shell;
using NvrView.Tiles;
using NvrView.Video;
using Windows.Storage.Pickers;

namespace NvrView.App;

/// <summary>A follow cut or grid drawn offscreen, frame by frame (what the follow and grid views provide).</summary>
internal interface IEditFrames : IDisposable
{
    /// <summary>The output size, already fitted to 1920 by 1080 (<see cref="ExportGeometry"/>).</summary>
    int Width { get; }

    int Height { get; }

    /// <summary>Draws the edit at a plan time (device-local milliseconds) into BGRA pixels, top row first; false when it cannot.</summary>
    bool Render(long timeMs, byte[] bgra);
}

/// <summary>The window's export writers.</summary>
internal sealed class ExportWriters(CoreHost host) : IExportWriters
{
    /// <summary>Opens the frames of an edit plan (the core's <c>nv_edit_compute</c> answer); null when the edit cannot be drawn.</summary>
    public Func<string, IEditFrames?>? EditFrames { get; set; }

    /// <summary>The grid cell whose sound a grid export carries (the one the player plays; the first when unset).</summary>
    public Func<int>? GridSoundCell { get; set; }

    /// <inheritdoc/>
    public bool CanExportEdit => EditFrames is not null;

    /// <summary>Opens W3's offscreen edit renderer as an export's frames, at the output size; null when the files cannot be opened.</summary>
    public static IEditFrames? OpenEditFrames(string planJson, Func<GpuDevice> gpuOf)
    {
        if (EditPlan.Parse(planJson) is not { } plan)
        {
            return null;
        }
        (int width, int height) = ExportGeometry.EditSize(plan);
        try
        {
            var renderer = new EditFrameRenderer(plan, gpuOf, width, height);
            if (renderer.Failures.Count == plan.Sources.Count)
            {
                AppLog.Limited("export-edit", 5, $"export: no file of the edit opened ({string.Join("; ", renderer.Failures.Values)})");
                renderer.Dispose();
                return null;
            }
            return new RendererFrames(renderer);
        }
        catch (Exception e) when (e is VideoException or InvalidOperationException)
        {
            AppLog.Limited("export-edit", 5, $"export: the edit could not be drawn: {e.Message}");
            return null;
        }
    }

    // W3's renderer as the export's frames.
    private sealed class RendererFrames(EditFrameRenderer renderer) : IEditFrames
    {
        public int Width => renderer.Width;

        public int Height => renderer.Height;

        public bool Render(long timeMs, byte[] bgra) => renderer.Render(timeMs, bgra);

        public void Dispose() => renderer.Dispose();
    }

    /// <summary>The sound pieces of an edit plan, placed in its files (none when the plan cannot be read).</summary>
    public static List<MfSoundPiece> Sound(string planJson, int gridCell)
    {
        if (EditPlan.Parse(planJson) is not { } plan)
        {
            return [];
        }
        var pieces = new List<MfSoundPiece>();
        foreach (AudioPiece p in AudioPiece.Of(plan, gridCell))
        {
            if (plan.SourceOf(p.Source) is { } source)
            {
                pieces.Add(new MfSoundPiece(source.File, p.FileStartMs, p.FileEndMs, p.AtMs));
            }
        }
        return pieces;
    }

    private static ExportOutcome Outcome(MfExportResult r, string path) => r.Status switch
    {
        MfExportStatus.Done => new ExportOutcome(ExportResult.Done, path, null, r.Width, r.Height, r.Frames),
        MfExportStatus.Canceled => ExportOutcome.Canceled(path),
        _ => ExportOutcome.Failed(path, r.Reason ?? "The export failed"),
    };

    /// <inheritdoc/>
    public ExportOutcome Trim(TrimOutput output, bool exact, string partialPath, ExportControl control) =>
        Outcome(
            MfExport.Cut(output.File, output.SourceInMs, output.SourceOutMs, exact || !output.CopyPossible, partialPath,
                () => control.IsCanceled, f => control.Fraction = f),
            partialPath);

    /// <inheritdoc/>
    public ExportOutcome StillFromFile(string file, long sourceMs, bool exact, PictureFormat format, string path)
    {
        RenderedImage? image = MfExport.Picture(file, sourceMs, exact);
        return image is null ? ExportOutcome.Failed(path, "A picture could not be read from the file") : Write(image, format, path);
    }

    /// <inheritdoc/>
    public ExportOutcome StillFromTile(ulong tile, PictureFormat format, string path)
    {
        if (host.VideoOf(tile) is not { } video)
        {
            return ExportOutcome.Failed(path, "That camera shows no picture yet");
        }
        RenderedImage? image;
        try
        {
            image = PictureRender.Render(host.Gpu, video, (w, h) => new PicturePlan(0, 0, w, h, w, h));
        }
        catch (Exception e) when (e is VideoException or InvalidOperationException)
        {
            image = null;
        }
        return image is null ? ExportOutcome.Failed(path, "The picture could not be drawn") : Write(image, format, path);
    }

    private static ExportOutcome Write(RenderedImage image, PictureFormat format, string path)
    {
        byte[]? data = PictureSaver.EncodeAsync(image, format).GetAwaiter().GetResult();
        if (data is null)
        {
            return ExportOutcome.Failed(path, "The picture could not be encoded");
        }
        PictureSaveResult saved = PictureSaver.WriteTo(data, image.Width, image.Height, path);
        return saved.Saved is { } s
            ? new ExportOutcome(ExportResult.Done, path, null, s.Width, s.Height, 0, s.Bytes)
            : ExportOutcome.Failed(path, "A picture could not be written");
    }

    /// <inheritdoc/>
    public ExportOutcome ExportEdit(string planJson, ExportCodec codec, string partialPath, ExportControl control, bool withSound = true)
    {
        if (EditFrames?.Invoke(planJson) is not { } frames || EditExportPlan.Parse(planJson) is not { } plan)
        {
            return ExportOutcome.Failed(partialPath, "The follow cut or grid could not be drawn");
        }
        using (frames)
        {
            int count = ExportGeometry.FrameCount(plan.StartMs, plan.EndMs);
            List<MfSoundPiece> sound = withSound ? Sound(planJson, plan.Kind == "grid" ? GridSoundCell?.Invoke() ?? 0 : 0) : [];
            return Outcome(
                MfExport.RenderEdit(partialPath, frames.Width, frames.Height, ExportGeometry.FramesPerSecond, count, codec == ExportCodec.Hevc,
                    ExportGeometry.FramesPerSecond * 2,
                    (i, pixels) => frames.Render(ExportGeometry.FrameTime(plan.StartMs, plan.EndMs, i), pixels),
                    () => control.IsCanceled, f => control.Fraction = f, sound),
                partialPath);
        }
    }
}

/// <summary>The save and folder pickers of an export, over the Recordings window.</summary>
internal sealed class ExportDialogs(Func<nint> window) : IExportDialogs
{
    /// <inheritdoc/>
    public async Task<string?> SaveFile(string suggestedName, string? folder, string extension)
    {
        try
        {
            var picker = new FileSavePicker
            {
                SuggestedStartLocation = extension is "mp4" ? PickerLocationId.VideosLibrary : PickerLocationId.PicturesLibrary,
                SuggestedFileName = Path.GetFileNameWithoutExtension(suggestedName),
                SettingsIdentifier = "nvrview-export",
            };
            if (extension == "mp4")
            {
                picker.FileTypeChoices.Add("MP4 video", new List<string> { ".mp4" });
            }
            else
            {
                picker.FileTypeChoices.Add(extension == "jpg" ? "JPEG" : "PNG", new List<string> { extension == "jpg" ? ".jpg" : ".png" });
            }
            WinRT.Interop.InitializeWithWindow.Initialize(picker, window());
            Windows.Storage.StorageFile? file = await picker.PickSaveFileAsync();
            return file?.Path;
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or InvalidOperationException)
        {
            AppLog.Warn($"the save picker failed: {e.GetType().Name}");
            return null;
        }
    }

    /// <inheritdoc/>
    public async Task<string?> ChooseFolder(string message, string? folder)
    {
        try
        {
            var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.VideosLibrary, CommitButtonText = "Save Here", SettingsIdentifier = "nvrview-export" };
            picker.FileTypeFilter.Add("*");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, window());
            Windows.Storage.StorageFolder? chosen = await picker.PickSingleFolderAsync();
            return chosen?.Path;
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or InvalidOperationException)
        {
            AppLog.Warn($"the folder picker failed: {e.GetType().Name}");
            return null;
        }
    }
}
