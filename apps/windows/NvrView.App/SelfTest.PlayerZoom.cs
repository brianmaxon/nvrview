// Self-test area "recordings": the recording player's picture features (docs/UX.md "Recordings"), on the real core's
// playback of the mock (the fake playback makes no pictures): digital zoom and pan with the wheel, a pinch and a drag,
// kept across seeks, speed changes and a change of layout; the part of the picture shown is the zoom math's; Save Zoomed
// Area and Save Picture write the region and the whole frame; 0 resets. The macOS step of the same ID is
// SelfTest+PlayerZoom.swift.

using NvrView.Interop;
using NvrView.Shell;
using NvrView.Tiles;
using Point = Windows.Foundation.Point;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    private async Task PlaybackZoomStep(Viewer v, RecordingsWindow w, RecordingsPlayer p)
    {
        // parity: playback-zoom
        StageTile? t = w.PlayerTile(0);
        bool ready = t is not null && await Until(() => t.PictureWidth > 0 && t.ViewWidth > 0 && w.PlayerChrome(t) is not null && ReadYuv(v, t) is not null, 15);
        if (!ready || t is null || w.PlayerChrome(t) is not { } chrome)
        {
            _problems.Add("player zoom: the playback's tile showed no picture to zoom");
            return;
        }
        var camera = new RecordingCamera(t.Camera.Device, t.Camera.Channel, t.Camera.Lens);
        int opens = p.Opens;
        string folder = FreshPictureFolder(v);
        try
        {
            StageView.ResetZoom(t);
            ZoomBase b = t.ZoomBase;
            var at = new Point(t.ViewX + t.ViewWidth * 0.7, t.ViewY + t.ViewHeight * 0.3);
            double ux = (at.X - t.ViewX) / t.ViewWidth, uy = (at.Y - t.ViewY) / t.ViewHeight;

            // A wheel notch zooms around the pointer; a pinch zooms at once; the picture point under the pointer stays put on
            // every axis the picture fills (the others stay centered until it does: docs/UX.md "Digital zoom").
            Expect(w.PlayerWheelAt(at, 360), "player zoom: a wheel over the tile was not used");
            double wheeled = t.Zoom, notch = Math.Pow(1.0015, 360);
            Expect(Math.Abs(wheeled - notch) < 0.01, $"player zoom: the wheel made {wheeled:0.000}x, expected {notch:0.000}x");
            TileZoom beforePinch = t.DigitalZoom;
            (double X, double Y) pointBefore = beforePinch.PicturePoint(ux, uy, b);
            Expect(w.PlayerPinchAt(at, 1.6), "player zoom: a pinch over the tile was not used");
            (double X, double Y) pointAfter = t.DigitalZoom.PicturePoint(ux, uy, b);
            Say($"player zoom: wheel {wheeled:0.00}x, pinch {t.Zoom:0.00}x, the point under the pointer ({pointBefore.X:0.000}, {pointBefore.Y:0.000}) -> ({pointAfter.X:0.000}, {pointAfter.Y:0.000})");
            Expect(Math.Abs(t.Zoom - wheeled * 1.6) < 0.01, $"player zoom: a pinch made {t.Zoom:0.000}x");
            if (wheeled * b.Width > 1.0001)
            {
                Expect(Math.Abs(pointAfter.X - pointBefore.X) < 1e-6, "player zoom: the point under the pointer moved across");
            }
            if (wheeled * b.Height > 1.0001)
            {
                Expect(Math.Abs(pointAfter.Y - pointBefore.Y) < 1e-6, "player zoom: the point under the pointer moved down");
            }

            // A drag moves the zoomed picture (a hand over it); the zoom map and the reset control follow.
            TileZoom zoomed = t.DigitalZoom;
            Expect(w.PlayerDragKind(at) == "pan", $"player zoom: a drag on a zoomed tile would be {w.PlayerDragKind(at) ?? "nothing"}");
            w.SimulatePlayerDrag(at, new Point(at.X - 30, at.Y + 12));
            TileZoom panned = t.DigitalZoom;
            Expect(panned.Scale == zoomed.Scale && (panned.CenterX != zoomed.CenterX || panned.CenterY != zoomed.CenterY), "player zoom: a drag did not move the picture");
            bool chromeUp = await Until(() => chrome.MapShown && chrome.ResetShown, 5);
            Expect(chromeUp, "player zoom: a zoomed tile shows no zoom map or no reset control");
            Expect(w.PlayerHeaderTile(t) is { Playback: true, Zoomed: true }, "player zoom: the header model is not the player's");

            // Seeks, speed changes and a change of layout keep the zoom and do not reopen the playback.
            TileZoom kept = t.DigitalZoom;
            p.Seek(Math.Min(p.PositionMs + 3_000, p.EndMs));
            p.SetRate(2);
            p.SetRate(1);
            double widthBefore = t.ViewWidth;
            WindowRect saved = w.CurrentFrame;
            double scale = w.Window.Content.XamlRoot?.RasterizationScale ?? 1;
            // Narrower (but not below the window's least width), or wider when it is already near it.
            int target = (int)Math.Max(saved.Width * 0.8, Math.Ceiling(RecordingsPanes.WindowMinWidth * scale));
            if (Math.Abs(target - saved.Width) < 40 * scale)
            {
                target = (int)(saved.Width * 1.25);
            }
            w.ResizeTo(target, (int)saved.Height);
            bool relaid = await Until(() => Math.Abs(t.ViewWidth - widthBefore) > 10, 5);
            Say($"player zoom: after a seek, speed changes and a resize ({widthBefore:0} -> {t.ViewWidth:0} wide, scale {scale:0.0}) the zoom is {t.Zoom:0.00}x");
            Expect(relaid, "player zoom: the player area did not change size");
            Expect(t.DigitalZoom == kept && w.Browser.Player.Views.Zoom(camera) == kept, $"player zoom: the view changed to {t.DigitalZoom} from {kept}");
            Expect(p.Opens == opens, "player zoom: the playback was reopened");

            // The region saved is the zoom math's, and the file is that part of the decoded picture.
            await Until(() => t.PictureWidth > 0 && t.ViewWidth > 0 && ReadYuv(v, t) is not null, 5);
            string nameEnd = " " + PictureNaming.Sanitize(t.Name);
            Yuv? drawn = null;
            w.PictureObserver = pic => drawn = YuvOf(pic);
            int pw = t.PictureWidth, ph = t.PictureHeight;
            UnitRect visible = t.DigitalZoom.Visible(ZoomBase.Of(pw, ph, t.ViewWidth, t.ViewHeight, t.Fill));
            var want = new PixelRegion((int)Math.Round(visible.X * pw), (int)Math.Round(visible.Y * ph), (int)Math.Round(visible.Width * pw), (int)Math.Round(visible.Height * ph));
            await w.SavePlayerPictureAsync(t, PictureKind.ZoomedArea, ask: false);
            PictureSaveRecord? rec = w.LastPictureSave;
            await CheckSavedFile(w.ToastText, w.ToastActionTitle, rec, "player zoomed area", "png", nameEnd, (want.Width, want.Height), 3, drawn, want);
            Expect(want.Width < pw && want.Height < ph, $"player zoomed area: the region {want.Width}x{want.Height} is not smaller than the picture {pw}x{ph}");
            Expect(rec?.Plan is { Dewarp: false } z && Math.Abs(z.SrcX - want.X) <= 1 && Math.Abs(z.SrcY - want.Y) <= 1 && Math.Abs(z.SrcWidth - want.Width) <= 1,
                "player zoomed area: the saved region is not the one the zoom shows");
            // Save Picture (the P key) saves the whole frame, whatever the zoom.
            drawn = null;
            int saves = w.PictureSaves;
            Expect(w.HandlePlayerKey(Windows.System.VirtualKey.P, false, false, false), "player zoom: the P key was not used");
            Expect(await Until(() => w.PictureSaves > saves, 20), "player zoom: the P key saved no picture");
            await CheckSavedFile(w.ToastText, w.ToastActionTitle, w.LastPictureSave, "player picture", "png", nameEnd, (pw, ph), 3, drawn);

            // 0 resets the view; the zoom map and the reset control go.
            Expect(w.HandlePlayerKey(Windows.System.VirtualKey.Number0, false, false, false), "player zoom: the 0 key was not used");
            bool reset = t.Zoom <= 1.0001f && t.CenterX == 0.5f && t.CenterY == 0.5f;
            bool gone = await Until(() => !chrome.MapShown && !chrome.ResetShown, 5);
            Say($"player zoom: 0 back to 1x {reset}, control and map gone {gone}");
            Expect(reset && gone, "player zoom: 0 did not return the tile to 1x");

            // Fill Tile crops the picture to the tile; the header model offers Reset Position for a moved crop.
            w.SetPlayerFill(t, true);
            Expect(w.PlayerFills(t), "player fill: Fill Tile did not fill");
            await Until(() => t.Fill || t.ZoomBase.Width <= 0, 3);
            if (StageView.PicturePans(t))
            {
                Expect(w.PlayerDragKind(new Point(t.ViewX + t.ViewWidth / 2, t.ViewY + t.ViewHeight / 2)) == "pan", "player fill: a drag on a cropped picture would not move it");
                w.SimulatePlayerDrag(new Point(t.ViewX + t.ViewWidth / 2, t.ViewY + t.ViewHeight / 2), new Point(t.ViewX + t.ViewWidth / 2 + 25, t.ViewY + t.ViewHeight / 2 + 25));
                Expect(w.PlayerHeaderTile(t) is { Panned: true } && TileHeaderLayout.Menu(w.PlayerHeaderTile(t)).Any(e => e.Item == TileHeaderItem.ResetPosition),
                    "player fill: a moved crop does not offer Reset Position");
                w.PerformPlayerItem(t, TileHeaderItem.ResetPosition, null);
                Expect(t.DigitalZoom.IsDefault, "player fill: Reset Position left the crop moved");
            }
            else
            {
                Say("player fill: the picture has the tile's shape; no crop to move");
            }
            w.SetPlayerFill(t, false);
            Expect(!w.PlayerFills(t), "player fill: Fill Tile again did not fit");

            // The header model: picture actions only.
            List<TileHeaderItem> menu = [.. TileHeaderLayout.Menu(w.PlayerHeaderTile(t)).Select(e => e.Item)];
            Expect(menu.All(TileHeaderLayout.PlaybackItems.Contains) && menu.Contains(TileHeaderItem.SavePicture) && menu.Contains(TileHeaderItem.FillTile),
                $"player header: the menu holds {string.Join(", ", menu)}");
            w.ResizeTo((int)saved.Width, (int)saved.Height);
        }
        finally
        {
            w.PictureObserver = null;
            w.ChoosePictureFile = null;
            RemoveFolder(folder);
            v.Settings.PicturesFolder = ViewSettings.DefaultPicturesFolder;
        }
    }
}
