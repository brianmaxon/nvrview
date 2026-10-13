using NvrView.Tiles;

namespace NvrView.Shell.Tests;

/// <summary>
/// The recording player's picture settings (docs/UX.md "Recordings"): they belong to the player's tile, so a
/// seek, a pause, a speed change or a change of layout keeps them, another camera starts fresh, and nothing the
/// player downloads or exports reads them. The macOS app has the same tests (PlayerTileViewsTests.swift).
/// </summary>
internal static class PlayerTileViewsTests
{
    private static readonly RecordingCamera A = new("dev", 0);
    private static readonly RecordingCamera B = new("dev", 1);
    private static readonly RecordingCamera C = new("dev", 2);
    private static readonly ZoomBase Base = ZoomBase.Whole;

    public static void Run()
    {
        T.Run("player views: zoom keeps the point under the pointer", () =>
        {
            var v = new PlayerTileViews();
            (double X, double Y) before = v.Zoom(A).PicturePoint(0.25, 0.75, Base);
            v.ZoomAround(A, 0.25, 0.75, 2, Base);
            T.Equal(2.0, v.Zoom(A).Scale);
            (double X, double Y) after = v.Zoom(A).PicturePoint(0.25, 0.75, Base);
            T.Check(Math.Abs(after.X - before.X) < 1e-9 && Math.Abs(after.Y - before.Y) < 1e-9);
            T.Check(v.IsZoomed(A));
            T.Check(!v.IsZoomed(B));
        });

        T.Run("player views: the math is the live tiles' own", () =>
        {
            var v = new PlayerTileViews();
            v.ZoomAround(A, 0.9, 0.1, 3, Base);
            v.Pan(A, -0.1, 0.05, Base);
            TileZoom live = TileZoom.None.ZoomedBy(3, 0.9, 0.1, Base).Panned(-0.1, 0.05, Base);
            T.Equal(live, v.Zoom(A));
        });

        T.Run("player views: pan stops at the edges and needs a zoom", () =>
        {
            var v = new PlayerTileViews();
            v.Pan(A, 0.3, 0.3, Base);
            T.Equal(TileZoom.None, v.Zoom(A), "a drag at 1x moves nothing");
            v.ZoomAround(A, 0.5, 0.5, 2, Base);
            v.Pan(A, 5, 5, Base);
            UnitRect visible = v.Zoom(A).Visible(Base);
            T.Check(visible.X >= -1e-9 && visible.Y >= -1e-9 && visible.X + visible.Width <= 1 + 1e-9 && visible.Y + visible.Height <= 1 + 1e-9);
        });

        T.Run("player views: each camera has its own view", () =>
        {
            var v = new PlayerTileViews();
            v.ZoomAround(A, 0.2, 0.2, 4, Base);
            v.SetFill(B, true);
            T.Equal(TileZoom.None, v.Zoom(B));
            T.Check(v.Fills(B) && !v.Fills(A));
            T.Equal(4.0, v.Zoom(A).Scale);
        });

        T.Run("player views: a seek or a layout change keeps the view", () =>
        {
            // The view is state of the player's tile: nothing about the playback (position, rate, the grid or
            // single mode) is an input. Retaining the cameras that keep playing changes nothing.
            var v = new PlayerTileViews();
            v.ZoomAround(A, 0.3, 0.6, 2.5, Base);
            v.SetFisheyeMode(B, FisheyeViewMode.Ptz);
            TileZoom zoom = v.Zoom(A);
            FisheyeTileSetting fisheye = v.Fisheye(B);
            v.Retain([A, B]);
            v.Retain([B, A]);
            v.Retain([A, B, C]);
            T.Equal(zoom, v.Zoom(A));
            T.Equal(fisheye, v.Fisheye(B));
        });

        T.Run("player views: a camera that leaves starts fresh when it comes back", () =>
        {
            var v = new PlayerTileViews();
            v.ZoomAround(A, 0.5, 0.5, 2, Base);
            v.SetFill(A, true);
            v.SetFisheyeMode(A, FisheyeViewMode.Panorama);
            v.ZoomAround(B, 0.5, 0.5, 2, Base);
            v.Retain([B]);
            T.Check(v.Zoom(A) == TileZoom.None && !v.Fills(A) && v.Fisheye(A) == FisheyeTileSetting.Original);
            T.Equal(2.0, v.Zoom(B).Scale, "the camera that stayed keeps its view");
            v.Retain([]);
            T.Check(v.IsEmpty);
        });

        T.Run("player views: reset brings back 1x and the crop", () =>
        {
            var v = new PlayerTileViews();
            ZoomBase wide = ZoomBase.Of(16, 9, 1, 1, fill: true);
            v.SetFill(A, true);
            v.Pan(A, -0.1, 0, wide);
            T.Check(v.IsPanned(A) && !v.IsZoomed(A));
            v.Reset(A);
            T.Check(v.Zoom(A).IsDefault && !v.IsPanned(A));
            v.ZoomAround(A, 0.1, 0.9, 3, Base);
            v.Reset(A);
            T.Equal(TileZoom.None, v.Zoom(A));
        });

        T.Run("player views: fill is per camera", () =>
        {
            var v = new PlayerTileViews();
            v.SetFill(A, true);
            T.Check(v.Fills(A) && !v.Fills(B));
            v.SetFill(A, false);
            T.Check(!v.Fills(A) && v.IsEmpty);
        });

        T.Run("player views: a fisheye view turns and zooms like a live tile", () =>
        {
            var v = new PlayerTileViews();
            v.TurnFisheye(A, 40, 0, 400, 300, FisheyeMount.Ceiling);
            T.Equal(FisheyeTileSetting.Original, v.Fisheye(A), "Original has nothing to turn");
            v.SetFisheyeMode(A, FisheyeViewMode.Ptz);
            v.TurnFisheye(A, 40, -20, 400, 300, FisheyeMount.Ceiling);
            FisheyeAim want = FisheyeAim.Default(FisheyeMount.Ceiling).Dragged(40, -20, 400, 300, FisheyeViewMode.Ptz, FisheyeMount.Ceiling);
            T.Equal(want, v.Fisheye(A).Aim);
            T.Check(v.IsZoomed(A), "a moved virtual camera has Reset Zoom");
            v.ZoomFisheye(A, 2, FisheyeMount.Ceiling);
            T.Equal(want.Zoomed(2, FisheyeMount.Ceiling).Fov, v.Fisheye(A).Aim!.Value.Fov);
            // The wheel does nothing in the panorama.
            v.SetFisheyeMode(B, FisheyeViewMode.Panorama);
            v.ZoomFisheye(B, 2, FisheyeMount.Ceiling);
            T.Check(v.Fisheye(B).Aim is null);
            v.Reset(A);
            T.Equal(new FisheyeTileSetting(FisheyeViewMode.Ptz, null), v.Fisheye(A), "back to the default view, same mode");
            T.Check(!v.IsZoomed(A));
            // Digital zoom and the dewarped view do not mix.
            v.SetZoom(A, new TileZoom(2, 0.5, 0.5));
            T.Check(!v.IsZoomed(A), "the dewarped view has no digital zoom");
            v.SetFisheyeMode(A, FisheyeViewMode.Original);
            T.Check(v.IsZoomed(A));
        });

        T.Run("player views: exports never read the view", () =>
        {
            // The requests the player's downloads and exports build take no view state and carry no region.
            var v = new PlayerTileViews();
            var options = new ExportOptions(ShareSafe: true);
            string range = ExportRequest.Range(1_000, 9_000, [A, B], null, options);
            string stills = ExportRequest.Stills(7, [1UL, 2UL], PictureFormat.Png, options);
            v.ZoomAround(A, 0.1, 0.1, 4, Base);
            v.SetFisheyeMode(A, FisheyeViewMode.Quad);
            v.SetFill(B, true);
            T.Equal(range, ExportRequest.Range(1_000, 9_000, [A, B], null, options));
            T.Equal(stills, ExportRequest.Stills(7, [1UL, 2UL], PictureFormat.Png, options));
            foreach (string json in new[] { range, stills })
            {
                foreach (string word in new[] { "zoom", "crop", "region", "scale", "fisheye" })
                {
                    T.Check(!json.Contains(word, StringComparison.Ordinal), word);
                }
            }
        });
    }
}
