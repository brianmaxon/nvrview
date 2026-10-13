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
    internal PicturePlan? PlanPicture(StageTile t, PictureKind kind, int pw, int ph) => PictureSaving.Plan(_viewer, t, kind, pw, ph);

    /// <summary>
    /// Saves the picture of a tile: to the pictures folder, or with <paramref name="ask"/> where the save picker
    /// says. The picture is taken now; the confirmation comes when the file is written.
    /// </summary>
    internal Task SavePictureAsync(StageTile t, PictureKind kind, bool ask) =>
        PictureSaving.SaveAsync(_viewer, t, kind, ask, AskWhere, ShowToast, FinishSave, PictureObserver);

    private void FinishSave(PictureSaveResult result, PictureKind kind, PicturePlan? plan, int pw, int ph)
    {
        LastPictureSave = new PictureSaveRecord(result, kind, plan, pw, ph);
        PictureSaves++;
        PictureSaving.Confirm(result, ShowToast);
    }

    /// <summary>Save Picture As…: the save picker with a format choice; the chosen path, null if canceled.</summary>
    private async Task<string?> AskWhere(string suggested, PictureFormat format)
    {
        if (ChoosePictureFile is { } hook)
        {
            return await hook(suggested, format);
        }
        return await PictureSaving.PickAsync(_appWindow.Id, suggested, format);
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
