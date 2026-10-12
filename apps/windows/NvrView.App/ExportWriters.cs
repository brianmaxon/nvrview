// What export does on Windows (docs/UX.md "Recordings", "Export"): the Media
// Foundation writers of NvrView.Video behind the shell's IExportWriters, the
// save and folder pickers behind IExportDialogs, and the frames a follow cut
// or grid is drawn from (supplied by the follow and grid views once they
// exist; until then those two exports are not offered).

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

    /// <inheritdoc/>
    public bool CanExportEdit => EditFrames is not null;

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
    public ExportOutcome ExportEdit(string planJson, ExportCodec codec, string partialPath, ExportControl control)
    {
        if (EditFrames?.Invoke(planJson) is not { } frames || EditExportPlan.Parse(planJson) is not { } plan)
        {
            return ExportOutcome.Failed(partialPath, "The follow cut or grid could not be drawn");
        }
        using (frames)
        {
            int count = ExportGeometry.FrameCount(plan.StartMs, plan.EndMs);
            return Outcome(
                MfExport.RenderEdit(partialPath, frames.Width, frames.Height, ExportGeometry.FramesPerSecond, count, codec == ExportCodec.Hevc,
                    ExportGeometry.FramesPerSecond * 2,
                    (i, pixels) => frames.Render(ExportGeometry.FrameTime(plan.StartMs, plan.EndMs, i), pixels),
                    () => control.IsCanceled, f => control.Fraction = f),
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
