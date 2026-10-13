// Save picture for any tile drawn by a StageRenderer, a live tile's or a recording player's (docs/CAPTURE.md "Save
// picture", docs/UX.md "Save picture"): what region or view a save draws, the file written, and the confirmation.
// One path, so a saved picture is made one way wherever it comes from. The macOS app shares its save the same way
// (PictureRenderBuilder, PictureSaver, SavePicturePanel).

using NvrView.Tiles;
using NvrView.Video;
using Windows.Storage.Pickers;

namespace NvrView.App;

internal static class PictureSaving
{
    /// <summary>What to draw for <paramref name="kind"/> from a picture of <paramref name="pw"/> x <paramref name="ph"/>; null for nothing.</summary>
    internal static PicturePlan? Plan(Viewer viewer, StageTile t, PictureKind kind, int pw, int ph)
    {
        if (pw < 1 || ph < 1)
        {
            return null;
        }
        // A dewarped fisheye view is drawn at the source's resolution.
        if (kind != PictureKind.OriginalFisheye && t.Fisheye is { } fisheye && fisheye.Mode.Dewarps())
        {
            FisheyeCircle circle = viewer.Circles.Circle(t.Camera) ?? FisheyeCircle.Inscribed(pw, ph);
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
    /// Saves the picture of a tile: to the pictures folder, or with <paramref name="ask"/> where <paramref name="askWhere"/>
    /// says. The picture is taken now; <paramref name="finished"/> is called when the file is written (or failed).
    /// </summary>
    internal static async Task SaveAsync(
        Viewer viewer, StageTile t, PictureKind kind, bool ask, Func<string, PictureFormat, Task<string?>> askWhere,
        Action<string, string?, Action?> toast, Action<PictureSaveResult, PictureKind, PicturePlan?, int, int> finished,
        Action<TilePicture>? observer)
    {
        if (t.CameraOrNone is null || t.Video is not { } video)
        {
            toast("No picture to save", null, null);
            return;
        }
        ViewSettings settings = viewer.Settings;
        string folder = settings.PicturesFolder, camera = t.Name;
        PictureFormat format = settings.PictureFormat;
        bool shareSafe = settings.ShareSafePictureNames;
        DateTimeOffset at = DateTimeOffset.Now;
        string? chosen = null;
        if (ask)
        {
            string suggested = PictureNaming.FileName(PictureNaming.LocalMillis(at), camera, shareSafe, format, n => File.Exists(Path.Combine(folder, n))) ?? $"Picture.{format.Extension()}";
            chosen = await askWhere(suggested, format);
            if (chosen is null)
            {
                return;
            }
            format = PictureFormats.FromExtension(Path.GetExtension(chosen)) ?? format;
        }
        GpuDevice gpu = viewer.Host.Gpu;
        PicturePlan? used = null;
        int pictureWidth = 0, pictureHeight = 0;
        PictureSaveResult result;
        try
        {
            RenderedImage? image = await Task.Run(() => PictureRender.Render(gpu, video, (pw, ph) =>
            {
                pictureWidth = pw;
                pictureHeight = ph;
                used = Plan(viewer, t, kind, pw, ph);
                return used;
            }, observer));
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
        finished(result, kind, used, pictureWidth, pictureHeight);
    }

    /// <summary>The message after a save: "Picture saved" with Show in folder, or the failure naming the file only.</summary>
    internal static void Confirm(PictureSaveResult result, Action<string, string?, Action?> toast)
    {
        if (result.Saved is { } saved)
        {
            toast("Picture saved", "Show in folder", () => SystemActions.ShowInExplorer([saved.Path]));
        }
        else if (result.Failure == PictureFailure.Write && result.FileName is { } name)
        {
            toast($"Couldn't save {name}", null, null);
        }
        else
        {
            toast("Couldn't save the picture", null, null);
        }
    }

    /// <summary>The save picker with a format choice; the chosen path, null if canceled.</summary>
    internal static async Task<string?> PickAsync(Microsoft.UI.WindowId window, string suggested, PictureFormat format)
    {
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
            WinRT.Interop.InitializeWithWindow.Initialize(picker, Microsoft.UI.Win32Interop.GetWindowFromWindowId(window));
            Windows.Storage.StorageFile? file = await picker.PickSaveFileAsync();
            return file?.Path;
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or InvalidOperationException)
        {
            AppLog.Warn($"the save picker failed: {e.GetType().Name}");
            return null;
        }
    }
}
