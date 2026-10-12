// Self-test steps of Save Picture (docs/CAPTURE.md "Save picture"; the same IDs as the Mac app's). Each
// saves through the tile's own entry points (the P key, the header's items), reads the file back with WIC,
// and checks its name, size, dimensions and pixels: a plain tile and a zoomed area against a CPU conversion
// of the decoded picture the file was made from (the same matrix, the sampler's bilinear chroma, so the
// D3D11 path and the encoder are what is checked), a fisheye view against the synthetic scene's colors.

using System.Text.RegularExpressions;
using NvrView.Tiles;
using NvrView.Video;
using Windows.System;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    private static readonly Regex PictureNameShape = new(@"^\d{4}-\d{2}-\d{2} \d{2}\.\d{2}\.\d{2}\.\d{3} .+$");

    // The decoded picture as planes, with the matrix the renderer used.
    private sealed record Yuv(int Width, int Height, byte[] Luma, byte[] Chroma, float[] Rows);

    // A throwaway pictures folder for the viewer to save into.
    private static string FreshPictureFolder(Viewer v)
    {
        string folder = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-pictures-{Guid.NewGuid():N}");
        v.Settings.PicturesFolder = folder;
        v.Settings.PictureFormat = PictureFormat.Png;
        v.Settings.ShareSafePictureNames = false;
        return folder;
    }

    private static void RemoveFolder(string folder)
    {
        try
        {
            Directory.Delete(folder, recursive: true);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            // A leftover temp folder is harmless.
        }
    }

    // Runs an action (a key or a menu item), waits for the save it starts to finish, and gives the record (null if none came).
    private async Task<PictureSaveRecord?> Saving(StageView stage, Action action)
    {
        int before = stage.PictureSaves;
        action();
        return await Until(() => stage.PictureSaves > before, 20) ? stage.LastPictureSave : null;
    }

    // Reads the tile's picture as it is now (device lock held inside; no await here).
    private static Yuv? ReadYuv(Viewer v, StageTile t)
    {
        using DeviceLock held = v.Host.Gpu.Lock();
        return t.Video?.ShownPicture is { } p ? YuvOf(p) : null;
    }

    // The picture's planes (call under the device lock).
    private static Yuv? YuvOf(TilePicture p) =>
        p.ReadYuv(out int w, out int h, out byte[] luma, out byte[] chroma) ? new Yuv(w, h, luma, chroma, p.ColorRows()) : null;

    // A pixel of the picture converted to RGB as the shader does: luma at the pixel, chroma interpolated between its texels.
    private static (int R, int G, int B) CpuRgb(Yuv y, int x, int row)
    {
        int cw = (y.Width + 1) / 2, ch = (y.Height + 1) / 2;
        float luma = y.Luma[row * y.Width + x] / 255f;
        float fx = (x + 0.5f) * cw / y.Width - 0.5f, fy = (row + 0.5f) * ch / y.Height - 0.5f;
        int x0 = (int)MathF.Floor(fx), y0 = (int)MathF.Floor(fy);
        float tx = fx - x0, ty = fy - y0;
        (float, float) Texel(int cx, int cy)
        {
            int o = (Math.Clamp(cy, 0, ch - 1) * cw + Math.Clamp(cx, 0, cw - 1)) * 2;
            return (y.Chroma[o] / 255f, y.Chroma[o + 1] / 255f);
        }
        (float, float) a = Texel(x0, y0), b = Texel(x0 + 1, y0), c = Texel(x0, y0 + 1), d = Texel(x0 + 1, y0 + 1);
        float Mix(float p, float q, float r, float s) => (p * (1 - tx) + q * tx) * (1 - ty) + (r * (1 - tx) + s * tx) * ty;
        float cb = Mix(a.Item1, b.Item1, c.Item1, d.Item1), cr = Mix(a.Item2, b.Item2, c.Item2, d.Item2);
        int Channel(int r) => (int)MathF.Round(Math.Clamp(y.Rows[r * 4] * luma + y.Rows[r * 4 + 1] * cb + y.Rows[r * 4 + 2] * cr + y.Rows[r * 4 + 3], 0, 1) * 255);
        return (Channel(0), Channel(1), Channel(2));
    }

    // How many of a grid of sample points of the saved image match the picture at region (its top left in picture pixels) within tolerance codes per channel.
    private static (int Checked, int Matched) ComparePicture(RenderedImage image, Yuv y, PixelRegion region, int tolerance)
    {
        int tried = 0, right = 0;
        for (int j = 1; j <= 6; j++)
        {
            for (int i = 1; i <= 6; i++)
            {
                int x = image.Width * i / 7, row = image.Height * j / 7;
                if (region.X + x >= y.Width || region.Y + row >= y.Height)
                {
                    continue;
                }
                (int wr, int wg, int wb) = CpuRgb(y, region.X + x, region.Y + row);
                (int gr, int gg, int gb) = image.Rgb(x, row);
                tried++;
                if (Math.Abs(gr - wr) <= tolerance && Math.Abs(gg - wg) <= tolerance && Math.Abs(gb - wb) <= tolerance)
                {
                    right++;
                }
            }
        }
        return (tried, right);
    }

    // For a JPEG: how many sample points of the saved image match the same encoder's JPEG of a CPU conversion of the picture
    // (null when that could not be made). The codec's own losses move with the picture's content, so comparing encoded with
    // encoded cancels them and checks the drawing alone.
    private static async Task<(int Checked, int Matched)?> CompareEncoded(RenderedImage image, Yuv y, PixelRegion region, int tolerance)
    {
        var bgra = new byte[image.Width * image.Height * 4];
        for (int row = 0; row < image.Height; row++)
        {
            for (int x = 0; x < image.Width; x++)
            {
                int i = (row * image.Width + x) * 4;
                if (region.X + x < y.Width && region.Y + row < y.Height)
                {
                    (int r, int g, int b) = CpuRgb(y, region.X + x, region.Y + row);
                    bgra[i] = (byte)b;
                    bgra[i + 1] = (byte)g;
                    bgra[i + 2] = (byte)r;
                }
                bgra[i + 3] = 255;
            }
        }
        byte[]? jpeg = await PictureSaver.EncodeAsync(new RenderedImage(image.Width, image.Height, bgra), PictureFormat.Jpeg);
        if (jpeg is null || await DecodeJpeg(jpeg) is not { } reference || reference.Width != image.Width || reference.Height != image.Height)
        {
            return null;
        }
        int tried = 0, right = 0;
        for (int j = 1; j <= 6; j++)
        {
            for (int i = 1; i <= 6; i++)
            {
                int x = image.Width * i / 7, row = image.Height * j / 7;
                if (region.X + x >= y.Width || region.Y + row >= y.Height)
                {
                    continue;
                }
                int o = (row * reference.Width + x) * 4;
                (int gr, int gg, int gb) = image.Rgb(x, row);
                tried++;
                if (Math.Abs(gr - reference.Pixels[o + 2]) <= tolerance && Math.Abs(gg - reference.Pixels[o + 1]) <= tolerance
                    && Math.Abs(gb - reference.Pixels[o]) <= tolerance)
                {
                    right++;
                }
            }
        }
        return (tried, right);
    }

    // Checks one finished save: a file with the naming rule's shape and extension, the "Picture saved" toast with its button, the
    // dimensions (size, within a pixel, when given), and, when the picture is a copy of the decoded one (yuv given), its pixels
    // (a JPEG against the same encoder's JPEG of the picture, CompareEncoded).
    private async Task<RenderedImage?> CheckSaved(StageView stage, PictureSaveRecord? rec, string what, string ext, string nameEnds, (int W, int H)? size, int tolerance, Yuv? yuv, PixelRegion? region = null, int minPercent = 95)
    {
        if (rec is null)
        {
            _problems.Add($"{what}: no picture was saved");
            return null;
        }
        if (rec.Result.Saved is not { } saved)
        {
            _problems.Add($"{what}: the save failed ({rec.Result.Failure} {rec.Result.FileName})");
            return null;
        }
        string name = Path.GetFileName(saved.Path);
        long fileSize = File.Exists(saved.Path) ? new FileInfo(saved.Path).Length : -1;
        RenderedImage? image = await PictureSaver.DecodeAsync(saved.Path);
        if (image is null)
        {
            _problems.Add($"{what}: {name} is not an image file");
            return null;
        }
        bool named = PictureNameShape.IsMatch(name) && name.EndsWith($"{nameEnds}.{ext}", StringComparison.Ordinal);
        bool sized = size is not { } s || (Math.Abs(image.Width - s.W) <= 1 && Math.Abs(image.Height - s.H) <= 1);
        string? toast = stage.ToastText;
        Say($"{what}: {name}, {image.Width}x{image.Height}, {fileSize} bytes, toast {toast ?? "none"}");
        Expect(named, $"{what}: the file name {name} does not follow the rule");
        Expect(fileSize == saved.Bytes && fileSize > 100, $"{what}: the file has {fileSize} bytes, wrote {saved.Bytes}");
        Expect(sized, $"{what}: {image.Width}x{image.Height}, expected {size?.W}x{size?.H}");
        Expect(image.Width == saved.Width && image.Height == saved.Height, $"{what}: the file is {image.Width}x{image.Height}, not the rendered {saved.Width}x{saved.Height}");
        Expect(toast == "Picture saved" && stage.ToastActionTitle == "Show in folder", $"{what}: the confirmation toast said {toast ?? "nothing"}");
        if (yuv is not null)
        {
            // A JPEG is compared with the same encoder's JPEG of the drawn picture: its losses (far off on a sharp color
            // edge, so they vary with the frame saved) cancel out.
            PixelRegion r = region ?? PictureRegion.Whole(yuv.Width, yuv.Height);
            (int tried, int right) = ext == "jpg" ? await CompareEncoded(image, yuv, r, tolerance) ?? (0, 0) : ComparePicture(image, yuv, r, tolerance);
            Say($"{what}: {right} of {tried} sampled pixels match the decoded picture");
            Expect(tried >= 20 && right * 100 >= tried * minPercent, $"{what}: only {right} of {tried} pixels match the decoded picture");
        }
        return image;
    }

    // P saves the tile's full picture as PNG; a share-safe JPEG names the camera Camera A; Save Picture As uses the chosen file; a
    // folder that cannot be written fails naming the file only; Settings > Capture holds the choices; Save Zoomed Area saves the
    // visible region of a zoomed tile at the source's resolution. Each file is read back and its pixels compared with the decoded picture.
    private async Task SavePictures(Viewer v, StageView stage)
    {
        StageTile? t = Tiles.FirstOrDefault(x => x.Video is not null && !v.IsFisheye(x.Camera) && stage.HeaderTileOf(x) is { Picture: true });
        if (t is null)
        {
            _problems.Add("no plain tile to save a picture from");
            return;
        }
        string folder = FreshPictureFolder(v);
        try
        {
            bool ready = await Until(() => t.PictureWidth > 0 && t.ViewWidth > 0 && ReadYuv(v, t) is not null, 15);
            Expect(ready, "the tile to save a picture from showed no picture");
            string nameEnd = " " + PictureNaming.Sanitize(t.Name);

            // parity: capture-picture-tile
            await CaptureSettingsPage(v);
            folder = FreshPictureFolder(v);
            stage.Select(t.Path);
            (int W, int H) full = (t.PictureWidth, t.PictureHeight);
            // The picture each save draws, read under the same lock at the moment it is drawn (the stream keeps moving).
            Yuv? drawn = null;
            stage.PictureObserver = p => drawn = YuvOf(p);
            PictureSaveRecord? rec = await Saving(stage, () => stage.HandleKey(VirtualKey.P, false, false));
            await CheckSaved(stage, rec, "picture", "png", nameEnd, full, 3, drawn);
            Expect(rec?.Plan is { Dewarp: false } plan && plan.SrcWidth == full.W && plan.SrcHeight == full.H, "P did not save the whole picture");

            // Share-safe names and JPEG, from Settings.
            v.Settings.ShareSafePictureNames = true;
            v.Settings.PictureFormat = PictureFormat.Jpeg;
            rec = await Saving(stage, () => stage.PerformHeaderItem(t, TileHeaderItem.SavePicture, null));
            await CheckSaved(stage, rec, "picture (share-safe JPEG)", "jpg", " Camera A", full, 8, drawn);
            v.Settings.ShareSafePictureNames = false;
            v.Settings.PictureFormat = PictureFormat.Png;

            // Save Picture As…: exactly the file the picker answers; the format follows its extension.
            string chosen = Path.Combine(folder, "chosen name.jpg");
            string? suggested = null;
            stage.ChoosePictureFile = (name, _) =>
            {
                suggested = name;
                return Task.FromResult<string?>(chosen);
            };
            rec = await Saving(stage, () => stage.PerformHeaderItem(t, TileHeaderItem.SavePictureAs, null));
            RenderedImage? asked = rec?.Result.Saved is { } sa && sa.Path == chosen ? await PictureSaver.DecodeAsync(chosen) : null;
            byte[] head = File.Exists(chosen) ? File.ReadAllBytes(chosen).Take(3).ToArray() : [];
            Say($"picture as: suggested {suggested ?? "nothing"}, wrote {(File.Exists(chosen) ? "the chosen file" : "nothing")}, {asked?.Width}x{asked?.Height}");
            Expect(asked is not null && head.SequenceEqual(new byte[] { 0xFF, 0xD8, 0xFF }), "Save Picture As did not write a JPEG to the chosen file");
            Expect(suggested is not null && PictureNameShape.IsMatch(suggested) && suggested.EndsWith(nameEnd + ".png", StringComparison.Ordinal), "Save Picture As did not suggest the shared name");
            // Canceled: nothing saved.
            int saves = stage.PictureSaves;
            stage.ChoosePictureFile = (_, _) => Task.FromResult<string?>(null);
            stage.PerformHeaderItem(t, TileHeaderItem.SavePictureAs, null);
            await Task.Delay(300); // proves that nothing happens
            Expect(stage.PictureSaves == saves, "canceling the picker still saved a picture");
            stage.ChoosePictureFile = null;

            // A folder that cannot be made: the message names the file, never the path.
            string blocker = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-blocker-{Guid.NewGuid():N}");
            File.WriteAllText(blocker, "not a folder");
            try
            {
                v.Settings.PicturesFolder = blocker;
                rec = await Saving(stage, () => stage.PerformHeaderItem(t, TileHeaderItem.SavePicture, null));
                string? failure = stage.ToastText;
                Say($"picture to a blocked folder: {rec?.Result.Failure}, toast {failure ?? "none"}");
                Expect(rec?.Result is { Failure: PictureFailure.Write, FileName: not null } && failure is not null && failure.StartsWith("Couldn't save ", StringComparison.Ordinal)
                    && !failure.Contains(blocker, StringComparison.OrdinalIgnoreCase) && !failure.Contains(Path.GetTempPath(), StringComparison.OrdinalIgnoreCase),
                    $"a blocked folder did not fail naming the file only (\"{failure}\")");
            }
            finally
            {
                File.Delete(blocker);
                v.Settings.PicturesFolder = folder;
            }

            // parity: capture-picture-zoomed
            t.DigitalZoom = new TileZoom(2, 0.5, 0.5);
            bool zoomed = await Until(() => stage.HeaderTileOf(t) is { Zoomed: true }, 5);
            PixelRegion? want = PictureRegion.Visible(t.PictureWidth, t.PictureHeight, t.ViewWidth, t.ViewHeight, t.Fill, t.DigitalZoom);
            bool offered = stage.HeaderTileOf(t) is { } model && TileHeaderLayout.Menu(model).Any(e => e.Item == TileHeaderItem.SaveZoomedArea);
            Expect(zoomed && offered && want is not null, "a zoomed tile does not offer Save Zoomed Area");
            if (want is { } region)
            {
                rec = await Saving(stage, () => stage.PerformHeaderItem(t, TileHeaderItem.SaveZoomedArea, null));
                await CheckSaved(stage, rec, "zoomed area", "png", nameEnd, (region.Width, region.Height), 3, drawn, region);
                Expect(region.Width < t.PictureWidth && region.Height < t.PictureHeight, $"the zoomed region {region.Width}x{region.Height} is not smaller than the picture {t.PictureWidth}x{t.PictureHeight}");
                Expect(rec?.Plan is { Dewarp: false } z && z.SrcX == region.X && z.SrcY == region.Y && z.SrcWidth == region.Width, "the zoomed area was not the region the tile shows");
            }
            StageView.ResetZoom(t);
        }
        finally
        {
            stage.ChoosePictureFile = null;
            stage.PictureObserver = null;
            RemoveFolder(folder);
            v.Settings.PicturesFolder = ViewSettings.DefaultPicturesFolder;
        }
    }

    // Settings > Capture: the page shows the saved choices and changes them, and they come back from view-settings.json.
    private async Task CaptureSettingsPage(Viewer v)
    {
        var hooks = new SettingsPagesHooks();
        SettingsWindow window = SettingsWindow.Open(v, development: true, SettingsSection.Capture, hooks: hooks, background: true);
        await Until(() => window.IsLoaded, 5);
        string folder = v.Settings.PicturesFolder;
        var shown = hooks.Capture();
        Expect(shown.Folder == folder && shown.Format == PictureFormat.Png && !shown.ShareSafe && shown.CanReset, $"the Capture page does not show the saved choices ({shown})");
        hooks.ChooseFormat(PictureFormat.Jpeg);
        hooks.SetShareSafe(true);
        Expect(v.Settings.PictureFormat == PictureFormat.Jpeg && v.Settings.ShareSafePictureNames, "the Capture page did not change the format or the share-safe names");
        hooks.ChooseFormat(PictureFormat.Png);
        hooks.SetShareSafe(false);
        hooks.ResetFolder();
        var reset = hooks.Capture();
        Expect(v.Settings.PicturesFolder == ViewSettings.DefaultPicturesFolder && reset.Folder == ViewSettings.DefaultPicturesFolder && !reset.CanReset, "Reset did not bring back the default pictures folder");
        window.Close();
        // The choices are kept between launches.
        string file = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-view-{Guid.NewGuid():N}.json");
        try
        {
            string custom = Path.Combine(Path.GetTempPath(), "nvrview-selftest-custom");
            var written = new ViewSettings(file) { PicturesFolder = custom, PictureFormat = PictureFormat.Jpeg, ShareSafePictureNames = true };
            var read = new ViewSettings(file);
            bool kept = read.PicturesFolder == custom && read.PictureFormat == PictureFormat.Jpeg && read.ShareSafePictureNames;
            written.PicturesFolder = ViewSettings.DefaultPicturesFolder;
            written.PictureFormat = PictureFormat.Png;
            written.ShareSafePictureNames = false;
            var back = new ViewSettings(file);
            Say($"capture settings: kept {kept}, defaults back {back.PicturesFolder == ViewSettings.DefaultPicturesFolder && back.PictureFormat == PictureFormat.Png && !back.ShareSafePictureNames}");
            Expect(kept, "the pictures folder, format or share-safe names were not read back from view-settings.json");
            Expect(back.PicturesFolder == ViewSettings.DefaultPicturesFolder && back.PictureFormat == PictureFormat.Png && !back.ShareSafePictureNames, "the default capture settings were not read back");
        }
        finally
        {
            File.Delete(file);
        }
    }

    // A fisheye tile's saved view is the dewarped picture at the source's resolution (its pixels show the synthetic scene's colors
    // where the shared mapping says), and Save Original Fisheye Picture is the circle as received.
    private async Task FisheyePictures(Viewer v, StageView stage, StageRenderer r, StageTile f, FisheyeCircle circle)
    {
        string folder = FreshPictureFolder(v);
        try
        {
            stage.SetFisheyeMode(f, FisheyeViewMode.Panorama);
            long drawn = r.DewarpDraws;
            await Until(() => r.DewarpDraws > drawn + 3 && f.PictureWidth > 0, 10);
            stage.Select(f.Path);
            string nameEnd = " " + PictureNaming.Sanitize(f.Name);
            TileHeaderTile? model = stage.HeaderTileOf(f);
            Expect(model is not null && TileHeaderLayout.Menu(model).Any(e => e.Item == TileHeaderItem.SaveOriginalFisheye), "a dewarped fisheye tile does not offer Save Original Fisheye Picture");

            // parity: capture-picture-fisheye
            PictureSaveRecord? rec = await Saving(stage, () => stage.HandleKey(VirtualKey.P, false, false));
            RenderedImage? view = await CheckSaved(stage, rec, "fisheye view", "png", nameEnd, null, 3, null);
            Expect(rec?.Plan is { Dewarp: true }, "fisheye view: the saved picture was not dewarped");
            if (view is not null && rec is not null && f.Fisheye is { } fe)
            {
                Expect(view.Width == rec.PictureWidth && view.Height == rec.PictureHeight, $"fisheye view: {view.Width}x{view.Height}, not the source's size {rec.PictureWidth}x{rec.PictureHeight}");
                var lens = new FisheyeLens(circle, rec.PictureWidth / (double)Math.Max(rec.PictureHeight, 1));
                FisheyeDewarp d = FisheyeDewarp.Of(lens, fe.Mount, fe.Mode, fe.Aim, view.Width, view.Height);
                (int tried, int right) = FisheyeImageMatches(view, d);
                Say($"fisheye view: {right} of {tried} saved pixels match the fixture");
                Expect(tried >= 20 && right * 10 >= tried * 9, $"fisheye view: {right} of {tried} saved pixels match the synthetic scene");
            }
            Yuv? yuv = null;
            stage.PictureObserver = p => yuv = YuvOf(p);
            rec = await Saving(stage, () => stage.PerformHeaderItem(f, TileHeaderItem.SaveOriginalFisheye, null));
            RenderedImage? original = await CheckSaved(stage, rec, "original fisheye", "png", nameEnd, view is null ? null : (view.Width, view.Height), 3, yuv);
            Expect(rec?.Plan is { Dewarp: false }, "original fisheye: the saved picture was dewarped");
            Say($"original fisheye: {original?.Width}x{original?.Height}");
        }
        finally
        {
            stage.PictureObserver = null;
            RemoveFolder(folder);
            v.Settings.PicturesFolder = ViewSettings.DefaultPicturesFolder;
        }
    }

    // How many of the dewarped image's sample points show the synthetic scene's color for the direction they look along.
    private static (int Tried, int Right) FisheyeImageMatches(RenderedImage image, FisheyeDewarp d)
    {
        int tried = 0, right = 0;
        for (int j = 0; j < 32; j++)
        {
            for (int i = 0; i < 32; i++)
            {
                var p = new UnitPoint((i + 0.5) / 32, (j + 0.5) / 32);
                if (d.Direction(p) is not { } dir)
                {
                    continue;
                }
                (double pan, double tilt) = Fisheye.PanTilt(dir);
                double psi = ((pan % 360) + 360) % 360;
                static double Off(double a) => Math.Abs(a - Math.Round(a / 15) * 15);
                if (tilt < 5 || tilt > 80 || Off(psi) < 3 || Off(tilt) < 3 || Math.Abs(tilt - 20) < 8)
                {
                    continue;
                }
                int sx = Math.Min((int)(p.X * image.Width), image.Width - 1), sy = Math.Min((int)(p.Y * image.Height), image.Height - 1);
                (int cr, int cg, int cb) = image.Rgb(sx, sy);
                int got = cr > cb + 40 && cg > cb + 40 && cg > cr * 0.6 ? 3
                    : cr > cg + 25 && cr > cb + 25 ? 0
                    : cg > cr + 25 && cg > cb + 25 ? 1
                    : cb > cr + 25 && cb > cg + 25 ? 2
                    : -1;
                tried++;
                if (got == (int)(psi / 90) % 4)
                {
                    right++;
                }
            }
        }
        return (tried, right);
    }
}
