// Save picture (docs/CAPTURE.md "Save picture", docs/UX.md "Save picture"): the tile's current decoded
// picture, at the full size of the stream it shows, written as PNG or JPEG to the pictures folder. The
// picture is the tile's own (no extra decode, no stream switch): drawn offscreen by the renderer's own
// shaders (NvrView.Video PictureRender) on a worker thread, encoded with WIC, written, and only then
// confirmed with a toast. The macOS app's StageView+Capture.swift does the same.

using NvrView.Tiles;
using NvrView.Video;
using Windows.Storage.Pickers;

namespace NvrView.App;

/// <summary>Which picture a save makes.</summary>
internal enum PictureKind
{
    /// <summary>What the tile shows, at the source's size: a fisheye tile's dewarped view; otherwise the whole picture, whatever the zoom.</summary>
    View,

    /// <summary>The visible region of a zoomed (or cropped) tile.</summary>
    ZoomedArea,

    /// <summary>A fisheye camera's circle as received, not dewarped.</summary>
    OriginalFisheye,
}

/// <summary>A finished save, with what it was made from (self-test).</summary>
internal sealed record PictureSaveRecord(PictureSaveResult Result, PictureKind Kind, PicturePlan? Plan, int PictureWidth, int PictureHeight);

internal sealed partial class StageView
{
    /// <summary>How long the message with a button stays (a toast without one: <see cref="ToastSeconds"/>).</summary>
    public const double ActionToastSeconds = 6;

    /// <summary>Saves finished, whether or not they worked (self-test).</summary>
    public int PictureSaves { get; private set; }

    /// <summary>The last finished save (self-test).</summary>
    internal PictureSaveRecord? LastPictureSave { get; private set; }

    /// <summary>
    /// Stands in for the save picker of Save Picture As (self-test): gets the suggested file name and the format
    /// from Settings, answers the chosen path, or null for Cancel.
    /// </summary>
    internal Func<string, PictureFormat, Task<string?>>? ChoosePictureFile { get; set; }

    /// <summary>Sees the picture a save draws, under the device lock, at the moment it is drawn (self-test).</summary>
    internal Action<TilePicture>? PictureObserver { get; set; }

    /// <summary>What to draw for <paramref name="kind"/> from a picture of <paramref name="pw"/> x <paramref name="ph"/>; null for nothing.</summary>
    internal PicturePlan? PlanPicture(StageTile t, PictureKind kind, int pw, int ph)
    {
        if (pw < 1 || ph < 1)
        {
            return null;
        }
        // A dewarped fisheye view is drawn at the source's resolution.
        if (kind != PictureKind.OriginalFisheye && t.Fisheye is { } fisheye && fisheye.Mode.Dewarps())
        {
            FisheyeCircle circle = _viewer.Circles.Circle(t.Camera) ?? FisheyeCircle.Inscribed(pw, ph);
            var lens = new FisheyeLens(circle, pw / (double)ph);
            DewarpConstants rows = FisheyeDewarp.Of(lens, fisheye.Mount, fisheye.Mode, fisheye.Aim, pw, ph).Constants();
            return new PicturePlan(0, 0, pw, ph, pw, ph, true, rows);
        }
        PixelRegion region = PictureRegion.Whole(pw, ph);
        if (kind == PictureKind.ZoomedArea)
        {
            region = PictureRegion.Visible(pw, ph, t.ViewWidth, t.ViewHeight, t.Fill, t.DigitalZoom) ?? region;
        }
        return new PicturePlan(region.X, region.Y, region.Width, region.Height, region.Width, region.Height);
    }

    /// <summary>
    /// Saves the picture of a tile: to the pictures folder, or with <paramref name="ask"/> where the save picker
    /// says. The picture is taken now; the confirmation comes when the file is written.
    /// </summary>
    internal async Task SavePictureAsync(StageTile t, PictureKind kind, bool ask)
    {
        if (t.CameraOrNone is null || t.Video is not { } video)
        {
            ShowToast("No picture to save");
            return;
        }
        ViewSettings settings = _viewer.Settings;
        string folder = settings.PicturesFolder, camera = t.Name;
        PictureFormat format = settings.PictureFormat;
        bool shareSafe = settings.ShareSafePictureNames;
        DateTimeOffset at = DateTimeOffset.Now;
        string? chosen = null;
        if (ask)
        {
            string suggested = PictureNaming.FileName(PictureNaming.LocalMillis(at), camera, shareSafe, format, n => File.Exists(Path.Combine(folder, n))) ?? $"Picture.{format.Extension()}";
            chosen = await AskWhere(suggested, format);
            if (chosen is null)
            {
                return;
            }
            format = PictureFormats.FromExtension(Path.GetExtension(chosen)) ?? format;
        }
        GpuDevice gpu = _viewer.Host.Gpu;
        PicturePlan? used = null;
        int pictureWidth = 0, pictureHeight = 0;
        PictureSaveResult result;
        try
        {
            RenderedImage? image = await Task.Run(() => PictureRender.Render(gpu, video, (pw, ph) =>
            {
                pictureWidth = pw;
                pictureHeight = ph;
                used = PlanPicture(t, kind, pw, ph);
                return used;
            }, PictureObserver));
            byte[]? data = image is null ? null : await PictureSaver.EncodeAsync(image, format);
            result = image is null || data is null
                ? new PictureSaveResult(null, PictureFailure.Encode)
                : await Task.Run(() => chosen is null
                    ? PictureSaver.WriteNext(data, image.Width, image.Height, folder, camera, at, format, shareSafe)
                    : PictureSaver.WriteTo(data, image.Width, image.Height, chosen));
        }
        catch (Exception e) when (e is VideoException or InvalidOperationException)
        {
            AppLog.Warn($"drawing a picture to save failed: {e.GetType().Name}");
            result = new PictureSaveResult(null, PictureFailure.Encode);
        }
        FinishSave(result, kind, used, pictureWidth, pictureHeight);
    }

    private void FinishSave(PictureSaveResult result, PictureKind kind, PicturePlan? plan, int pw, int ph)
    {
        LastPictureSave = new PictureSaveRecord(result, kind, plan, pw, ph);
        PictureSaves++;
        if (result.Saved is { } saved)
        {
            ShowToast("Picture saved", "Show in folder", () => SystemActions.ShowInExplorer([saved.Path]));
        }
        else if (result.Failure == PictureFailure.Write && result.FileName is { } name)
        {
            ShowToast($"Couldn't save {name}");
        }
        else
        {
            ShowToast("Couldn't save the picture");
        }
    }

    /// <summary>Save Picture As…: the save picker with a format choice; the chosen path, null if canceled.</summary>
    private async Task<string?> AskWhere(string suggested, PictureFormat format)
    {
        if (ChoosePictureFile is { } hook)
        {
            return await hook(suggested, format);
        }
        try
        {
            var picker = new FileSavePicker
            {
                SuggestedStartLocation = PickerLocationId.PicturesLibrary,
                SuggestedFileName = Path.GetFileNameWithoutExtension(suggested),
            };
            var png = new List<string> { ".png" };
            var jpeg = new List<string> { ".jpg", ".jpeg" };
            if (format == PictureFormat.Png)
            {
                picker.FileTypeChoices.Add("PNG", png);
                picker.FileTypeChoices.Add("JPEG", jpeg);
            }
            else
            {
                picker.FileTypeChoices.Add("JPEG", jpeg);
                picker.FileTypeChoices.Add("PNG", png);
            }
            WinRT.Interop.InitializeWithWindow.Initialize(picker, Microsoft.UI.Win32Interop.GetWindowFromWindowId(_appWindow.Id));
            Windows.Storage.StorageFile? file = await picker.PickSaveFileAsync();
            return file?.Path;
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or InvalidOperationException)
        {
            AppLog.Warn($"the save picker failed: {e.GetType().Name}");
            return null;
        }
    }

    /// <summary>Opens the pictures folder in Explorer (made first if missing).</summary>
    internal void OpenPicturesFolder()
    {
        string folder = _viewer.Settings.PicturesFolder;
        try
        {
            Directory.CreateDirectory(folder);
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo { FileName = folder, UseShellExecute = true })?.Dispose();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            AppLog.Warn($"the pictures folder could not be opened: {e.GetType().Name}");
        }
    }
}
