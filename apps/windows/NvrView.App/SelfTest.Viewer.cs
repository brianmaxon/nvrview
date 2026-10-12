// Self-test area "viewer": focus, audio, zoom, fill, and pan on the grid.

using System.Diagnostics;
using System.Text;
using Microsoft.UI.Windowing;
using NvrView.Shell;
using NvrView.Tiles;
using NvrView.Video;
using Windows.Graphics;
using NvThumbKind = NvrView.Interop.NvThumbKind;
using Point = Windows.Foundation.Point;
using RecordedThumbnail = NvrView.Interop.RecordedThumbnail;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    private async Task ViewerSteps(Viewer v, StageView stage, StageRenderer r, StageTile first)
    {
        // parity: focus-main-stream
        // 4. Focus: animation, then the main stream after the aligned switch.
        int tierEventsBefore = v.TierEvents.Count;
        stage.FocusOn(first);
        bool main = await Until(() => first.Video?.ShownGeneration == 1 && first.ShownTier == "main", FocusAnimation.Seconds + 8);
        float stageW = (float)stage.ActualWidth, stageH = (float)stage.ActualHeight;
        Say($"focused tile 0: showing generation {first.Video?.ShownGeneration} ({first.ShownTier}), picture {first.PictureWidth}x{first.PictureHeight}, " +
            $"rect {first.X:0},{first.Y:0} {first.Width:0}x{first.Height:0} of {stageW:0}x{stageH:0}, " +
            $"tier switch {Ms(v.Timings.GetValueOrDefault("tier_switch"))}, delta {v.TierSwitchDeltaMs?.ToString() ?? "n/a"} ms, held {v.TierSwitchHeldMs?.ToString() ?? "n/a"} ms");
        if (!main)
        {
            _problems.Add("focus did not switch to the main stream");
        }
        if (Math.Abs(first.Width - stageW) > 2 && Math.Abs(first.Height - stageH) > 2)
        {
            _problems.Add("the focused tile does not fill the stage");
        }
        CheckTierEvents(v, first.Id, tierEventsBefore, 1);
        await CheckFrame("focused", [first]);

        // parity: audio-level-muted
        // 5. Listening.
        float muted = await MaxLevel(v, first, 0.5);
        Say($"muted level {muted:0.00}");
        if (muted <= 0)
        {
            _problems.Add("no audio level while muted");
        }
        long rendered = app.Audio?.FramesRendered ?? 0;
        // parity: audio-listen
        v.ToggleListening(first.Camera, additive: false);
        var heard = Stopwatch.StartNew();
        float listening = 0;
        while (heard.Elapsed.TotalSeconds < 6 && listening <= 0)
        {
            listening = await MaxLevel(v, first, 0.1);
        }
        Say($"listening: first level after {heard.Elapsed.TotalSeconds:0.00} s");
        // The core starts playout once its delay target is known; then the
        // WASAPI stream carries the tile (audio_stats: starts, no underruns).
        // parity: audio-output-renders
        bool started = app.Audio is not { Problem: null } || await Until(() => AudioStarted(first.AudioStats), 8);
        Say($"playout started after {heard.Elapsed.TotalSeconds:0.00} s: {started}");
        if (!started)
        {
            _problems.Add("the core never started playing the audible tile");
        }
        if (app.Audio is { Problem: null })
        {
            await Until(() => app.Audio.FramesRendered > rendered, 3);
        }
        long renderedNow = app.Audio?.FramesRendered ?? 0;
        string audio = app.Audio is null ? "off (--no-audio)" :
            app.Audio.Problem is { } why ? $"no output ({why})" : $"{app.Audio.SampleRate} Hz, {renderedNow - rendered} frames rendered";
        Say($"listening: audible {first.Audible}, level {listening:0.00}, audio output {audio}, lead {app.Audio?.LeadMs:0.0} ms");
        Say($"audio stats: {first.AudioStats ?? "none"}");
        if (!first.Audible)
        {
            _problems.Add("the tile did not become audible");
        }
        if (listening <= 0)
        {
            _problems.Add("no audio level while listening");
        }
        if (app.Audio is { Problem: null } && renderedNow <= rendered)
        {
            _problems.Add("the audio output rendered nothing");
        }
        v.SetListening(first.Camera, false);

        // parity: unfocus-sub-stream
        // 6. Leave focus.
        tierEventsBefore = v.TierEvents.Count;
        stage.Unfocus();
        bool sub = await Until(() => first.Video?.ShownGeneration == 2 && first.ShownTier == "sub" && r.Focus is null, FocusAnimation.Seconds + 8);
        Say($"after focus: showing generation {first.Video?.ShownGeneration} ({first.ShownTier})");
        if (!sub)
        {
            _problems.Add("leaving focus did not return to the sub stream");
        }
        CheckTierEvents(v, first.Id, tierEventsBefore, 2);

        // parity: zoom-digital
        // 7. Digital zoom.
        StageTile zoomed = Tiles[^1];
        StageView.ZoomAround(zoomed, 0.25, 0.25, 2);
        await CheckFrame("zoomed", [zoomed]);
        Say($"zoom {zoomed.Zoom:0.0} around {zoomed.CenterX:0.00},{zoomed.CenterY:0.00}");
        if (Math.Abs(zoomed.Zoom - 2) > 0.01)
        {
            _problems.Add("zoom was not applied");
        }
        StageView.ResetZoom(zoomed);
        await CheckZoomFillsTile();
        await CheckZoomChrome();
        await CheckFillTiles();
        await CheckFillPan();
        await CheckZoomPerTile();
        await CheckPanAndFrames();
    }

    // parity: zoom-fills-tile
    // A letterboxed tile zoomed past the point where its picture fills the
    // tile: the former bar shows picture; back at 1x it is black again.
    private async Task CheckZoomFillsTile()
    {
        StageRenderer r = app.Renderer!;
        StageTile? boxed = Tiles.FirstOrDefault(t => !(t.Fisheye is { } f && f.Mode.Dewarps()) && ZoomBar(t) is not null);
        if (boxed is null || ZoomBar(boxed) is not { } bar)
        {
            _problems.Add("no letterboxed tile to check the zoom on");
            return;
        }
        double? before = await BarLit(r, bar);
        ZoomBase b = boxed.ZoomBase;
        // Past the scale where the picture fills the tile's short side.
        double past = Math.Min(1.25 / Math.Min(b.Width, b.Height), TileZoom.MaxScale);
        StageView.ZoomAround(boxed, 0.5, 0.5, past);
        double? zoomed = await BarLit(r, bar);
        StageView.ResetZoom(boxed);
        double? after = await BarLit(r, bar);
        Say($"zoom fills the tile: tile {boxed.Id} at {past:0.00}x, bar lit {before:0.00} at 1x, {zoomed:0.00} zoomed, {after:0.00} back at 1x");
        Expect(before is < 0.02, "the letterbox bar was not black at 1x");
        Expect(zoomed is >= 0.5, "zoomed past filling the tile, the letterbox bar still shows no picture");
        Expect(after is < 0.02, "back at 1x, the letterbox bar is not black");
    }

    // parity: zoom-map
    // parity: zoom-reset-control
    // parity: zoom-touch-pinch
    // The zoom map and the header's reset control follow a tile's zoom, and a touch screen's pinch zooms
    // the digital zoom around the point between the fingers.
    private async Task CheckZoomChrome()
    {
        StageView stage = app.Stage!;
        StageTile? t = Tiles.FirstOrDefault(x => !(x.Fisheye is { } f && f.Mode.Dewarps()) && x.PictureWidth > 0 && x.ViewWidth > 0 && x.ZoomBase.Width > 0.99);
        TileChrome? chrome = t is null ? null : stage.ChromeFor(t);
        if (t is null || chrome is null)
        {
            _problems.Add("no ordinary tile to check the zoom map on");
            return;
        }
        StageView.ResetZoom(t);
        stage.Interact();
        stage.Tick();
        await Task.Delay(100);
        bool idleHidden = !chrome.MapShown && !chrome.ResetShown;
        // The pinch: spreading zooms in around the point between the fingers; closing zooms back out; junk changes nothing.
        double cx = t.ViewX + t.ViewWidth * 0.25, cy = t.ViewY + t.ViewHeight * 0.25;
        stage.PinchDigital(t, cx, cy, 2);
        bool pinchedIn = Math.Abs(t.Zoom - 2) < 0.01;
        double pointX = t.CenterX;
        stage.PinchDigital(t, cx, cy, double.NaN);
        bool junkIgnored = Math.Abs(t.Zoom - 2) < 0.01;
        for (int i = 0; i < 8; i++)
        {
            stage.PinchDigital(t, cx, cy, 2);
        }
        bool bounded = Math.Abs(t.Zoom - TileZoom.MaxScale) < 0.01;
        for (int i = 0; i < 8; i++)
        {
            stage.PinchDigital(t, cx, cy, 0.5);
        }
        bool pinchedOut = t.Zoom <= 1.0001f;
        Say($"zoom touch pinch: in {pinchedIn} (center x {pointX:0.00}), junk ignored {junkIgnored}, bounded at {TileZoom.MaxScale:0}x {bounded}, back out {pinchedOut}");
        Expect(pinchedIn && pointX < 0.5, $"a pinch spread did not zoom in around the fingers (zoom {t.Zoom:0.00}, center x {pointX:0.00})");
        Expect(junkIgnored, "a pinch step of NaN changed the zoom");
        Expect(bounded, $"pinching past the limit did not stop at the maximum zoom ({t.Zoom:0.00})");
        Expect(pinchedOut, $"pinching closed did not return to 1x ({t.Zoom:0.00})");

        // The map and the reset control, zoomed.
        StageView.ZoomAround(t, 0.25, 0.25, 2);
        stage.Interact();
        stage.Tick();
        bool shown = await Until(() => chrome.MapShown && chrome.ResetShown, 3);
        (UnitRect frame, UnitRect visible) = chrome.MapGeometry;
        TileZoom zoom = t.DigitalZoom;
        UnitRect d = zoom.Destination(t.ZoomBase);
        double left = t.ViewX + d.X * t.ViewWidth, bottom = t.ViewY + (d.Y + d.Height) * t.ViewHeight;
        bool corner = Math.Abs(frame.X - left - ZoomMap.Margin) < 1.5 && Math.Abs(bottom - ZoomMap.Margin - (frame.Y + frame.Height)) < 1.5;
        bool shape = frame.Height > 0 && Math.Abs(frame.Width / frame.Height - t.PictureWidth / (double)t.PictureHeight) < 0.1;
        UnitRect want = zoom.Visible(t.ZoomBase);
        bool region = Math.Abs(visible.X - want.X) < 1e-6 && Math.Abs(visible.Width - want.Width) < 1e-6 && visible.Width < 1;
        Say($"zoom map: shown {shown}, frame {frame.Width:0}x{frame.Height:0}, bottom left corner {corner}, picture shape {shape}, marks the visible region {region}");
        Expect(shown, "zoomed and active, the tile shows no zoom map or no reset control");
        Expect(corner && shape && region, $"the zoom map is misplaced or marks the wrong region: frame {frame}, visible {visible}, wanted corner at {left:0},{bottom:0}");

        // It fades with the chrome (a few idle seconds) and returns with the pointer.
        bool faded = await Until(() =>
        {
            stage.Tick();
            return !chrome.MapShown;
        }, ChromeIdle.HideAfterSeconds + 4);
        stage.Interact();
        stage.Tick();
        bool back = await Until(() => chrome.MapShown, 3);
        Say($"zoom map: fades with the chrome {faded}, returns with the pointer {back}, hidden at 1x before {idleHidden}");
        Expect(faded && back, "the zoom map did not fade with the chrome and return with the pointer");
        Expect(idleHidden, "at 1x the tile showed a zoom map or a reset control");

        // The header's reset control returns the tile to 1x and takes the map and itself away.
        chrome.PressReset();
        bool reset = t.Zoom <= 1.0001f && t.CenterX == 0.5f && t.CenterY == 0.5f;
        bool gone = await Until(() => !chrome.MapShown && !chrome.ResetShown, 3);
        Say($"zoom reset control: back to 1x {reset}, control and map gone {gone}");
        Expect(reset, $"the reset control did not return the tile to 1x ({t.Zoom:0.00})");
        Expect(gone, "after the reset the tile still shows the reset control or the zoom map");
    }

    // parity: fill-tiles
    // Fill Tiles crops the picture to fill its tile (the former letterbox bar shows picture) and
    // turning it off fits it again; a zoom still works from the crop.
    private async Task CheckFillTiles()
    {
        StageView stage = app.Stage!;
        StageRenderer r = app.Renderer!;
        StageTile? boxed = Tiles.FirstOrDefault(t => !(t.Fisheye is { } f && f.Mode.Dewarps()) && ZoomBar(t) is not null);
        if (boxed is null || ZoomBar(boxed) is not { } bar)
        {
            _problems.Add("no letterboxed tile to check Fill Tiles on");
            return;
        }
        double? fitted = await BarLit(r, bar);
        stage.SetFill(true);
        double? filled = await BarLit(r, bar);
        ZoomBase b = boxed.ZoomBase;
        bool crop = boxed.Fill && Math.Max(b.Width, b.Height) > 1.01 && Math.Min(b.Width, b.Height) > 0.999;
        bool tileArea = Near(boxed.X, boxed.ViewX) && Near(boxed.Width, boxed.ViewWidth) && Near(boxed.Y, boxed.ViewY) && Near(boxed.Height, boxed.ViewHeight);
        StageView.ZoomAround(boxed, 0.5, 0.5, 2);
        double? zoomedFilled = await BarLit(r, bar);
        bool reaches = boxed.DigitalZoom.Visible(boxed.ZoomBase).Width < 1;
        StageView.ResetZoom(boxed);
        stage.SetFill(false);
        double? fittedAgain = await BarLit(r, bar);
        Say($"fill tiles: bar lit {fitted:0.00} fitted, {filled:0.00} filled, {zoomedFilled:0.00} filled and zoomed, {fittedAgain:0.00} fitted again; base {b.Width:0.00}x{b.Height:0.00}, tile area {tileArea}");
        Expect(fitted is < 0.02, "the letterbox bar was not black before Fill Tiles");
        Expect(filled is >= 0.5, "with Fill Tiles the former letterbox bar still shows no picture");
        Expect(crop && tileArea, $"Fill Tiles did not crop the picture to the tile (base {b.Width:0.00}x{b.Height:0.00}, tile area {tileArea})");
        Expect(zoomedFilled is >= 0.5 && reaches, "zooming from the filled crop lost the picture");
        Expect(fittedAgain is < 0.02 && !boxed.Fill, "turning Fill Tiles off did not fit the picture again");
    }

    // Fill per tile, the filled crop's pan, the header swap, and the window's record (docs/UX.md "Fill
    // Tiles"): a letterboxed tile gets its own Fill tile (the others stay fitted); a drag on its picture
    // moves the crop and stops at the picture's edge; a drag on its shown header swaps it; the record
    // brings the crop and the single tile's choice back in a new window; Fill Tiles then sets every tile.
    private async Task CheckFillPan()
    {
        StageView stage = app.Stage!;
        ViewerWindow window = app.Windows.First(w => w.Stage == stage);
        StageTile? boxed = Tiles.FirstOrDefault(t => !(t.Fisheye is { } f && f.Mode.Dewarps()) && ZoomBar(t) is not null);
        StageTile? other = boxed is null ? null : Tiles.FirstOrDefault(t => t.CameraOrNone != boxed.CameraOrNone && !(t.Fisheye is { } f && f.Mode.Dewarps()));
        if (boxed?.CameraOrNone is not { } camera || other?.CameraOrNone is not { } otherCamera)
        {
            _problems.Add("no letterboxed tile and second camera for the Fill pan checks");
            return;
        }

        // parity: fill-per-tile
        var menu = stage.BuildTileMenu(boxed);
        var fillItem = menu?.Items.OfType<Microsoft.UI.Xaml.Controls.ToggleMenuFlyoutItem>().FirstOrDefault(i => i.Text == "Fill Tile");
        bool offAtFirst = fillItem is { IsChecked: false };
        stage.SetTileFill(boxed, true);
        bool published = await Until(() => boxed.Fill, 5);
        bool single = stage.Fills(boxed) && !stage.Fills(other) && stage.FillMixed && !other.WantsFill;
        bool checkedNow = stage.BuildTileMenu(boxed)?.Items.OfType<Microsoft.UI.Xaml.Controls.ToggleMenuFlyoutItem>().Any(i => i.Text == "Fill Tile" && i.IsChecked) == true;
        Say($"fill per tile: menu item {(fillItem is null ? "missing" : "present")}, this tile fills {stage.Fills(boxed)} (drawn {published}), another {stage.Fills(other)}, mixed {stage.FillMixed}, checked {checkedNow}");
        Expect(offAtFirst, "the tile menu has no unchecked Fill tile on a fitted tile");
        Expect(published && single, "Fill tile on one tile did not fill only it");
        Expect(checkedNow, "Fill tile is not checked on the tile that fills");

        // parity: fill-pan-drag
        ZoomBase b = boxed.ZoomBase;
        bool wide = b.Width > 1;
        var middle = new Point(boxed.ViewX + boxed.ViewWidth / 2, boxed.ViewY + boxed.ViewHeight / 2 + 20);
        string? kind = stage.DragKind(middle);
        double step = wide ? boxed.ViewWidth / 8 : boxed.ViewHeight / 8;
        stage.SimulateDrag(middle, wide ? new Point(middle.X + step, middle.Y) : new Point(middle.X, middle.Y + step));
        TileZoom want = TileZoom.None.Panned(wide ? 0.125 : 0, wide ? 0 : 0.125, b);
        TileZoom got = boxed.DigitalZoom;
        bool followed = Math.Abs(got.CenterX - want.CenterX) < 1e-4 && Math.Abs(got.CenterY - want.CenterY) < 1e-4 && got.IsPanned;
        stage.SimulateDrag(middle, wide ? new Point(middle.X + step * 40, middle.Y) : new Point(middle.X, middle.Y + step * 40));
        UnitRect edge = boxed.DigitalZoom.Visible(b);
        bool stopped = wide ? Math.Abs(edge.X) < 1e-4 : Math.Abs(edge.Y) < 1e-4;
        bool fittedSwaps = stage.DragKind(new Point(other.ViewX + other.ViewWidth / 2, other.ViewY + other.ViewHeight / 2 + 20)) == "swap";
        bool shared = stage.State.ViewKey(boxed.Path) is { } bk && stage.State.Views.Zoom(bk) == boxed.DigitalZoom;
        Say($"fill pan drag: kind {kind}, crop followed {followed} ({got.CenterX:0.0000},{got.CenterY:0.0000} for {want.CenterX:0.0000},{want.CenterY:0.0000}), stopped at the edge {stopped}, kept for the tile {shared}, a fitted tile swaps {fittedSwaps}");
        Expect(kind == "pan", "a drag on a filled, cropped picture would not pan it");
        Expect(followed, "dragging the filled picture did not move the crop with the drag");
        Expect(stopped, "panning the filled picture did not stop at its edge");
        Expect(shared, "the crop was not kept for the tile");
        Expect(fittedSwaps, "a drag on a fitted tile's picture no longer swaps it");
        // Back from the edge to a position of its own, for the restore check.
        stage.SimulateDrag(middle, wide ? new Point(middle.X - step * 2, middle.Y) : new Point(middle.X, middle.Y - step * 2));
        TileZoom crop = boxed.DigitalZoom;

        // parity: fill-pan-header-swap
        NodePath from = boxed.Path, to = other.Path;
        stage.Hover(boxed);
        var onHeader = new Point(boxed.FrameX + boxed.FrameWidth / 2, boxed.FrameY + 10);
        string? headerKind = stage.DragKind(onHeader);
        stage.SimulateDrag(onHeader, new Point(other.ViewX + other.ViewWidth / 2, other.ViewY + other.ViewHeight / 2));
        bool swapped = stage.State.Tree[from] is LayoutLeaf { Cam: { } a } && a == otherCamera && stage.State.Tree[to] is LayoutLeaf { Cam: { } c2 } && c2 == camera;
        StageTile? moved = stage.Tiles.FirstOrDefault(t => t.CameraOrNone == camera);
        bool kept = moved is not null && moved.DigitalZoom == crop;
        if (swapped && moved is not null)
        {
            await Until(() => moved.Visible && moved.Width > 1, 5);
            StageTile back = stage.Tiles.First(t => t.Path == from);
            await Until(() => back.Visible && back.ViewWidth > 1, 5);
            stage.Hover(moved);
            stage.SimulateDrag(new Point(moved.FrameX + moved.FrameWidth / 2, moved.FrameY + 10), new Point(back.ViewX + back.ViewWidth / 2, back.ViewY + back.ViewHeight / 2));
        }
        bool swappedBack = stage.State.Tree[from] is LayoutLeaf { Cam: { } a2 } && a2 == camera;
        Say($"fill pan header swap: kind {headerKind}, swapped {swapped}, crop kept with the camera {kept}, swapped back {swappedBack}");
        Expect(headerKind == "swap", "a drag on the header of a panning tile would not swap it");
        Expect(swapped, "dragging a tile's header onto another did not swap them");
        Expect(kept, "the crop did not follow the camera through the swap");
        Expect(swappedBack, "dragging the header back did not swap the tiles back");

        // The tiles were made anew by the swaps.
        boxed = stage.Tiles.First(t => t.CameraOrNone == camera);
        other = stage.Tiles.First(t => t.CameraOrNone == otherCamera);
        StageTile b2 = boxed, o2 = other;
        await Until(() => b2.Visible && b2.Fill && o2.Visible, 5);

        // parity: fill-pan-restore
        // The record keeps the crop and the Fill choice under the tile's identity; a window opened from it
        // shows them in that tile.
        SavedWindow record = WindowStore.FromJson(System.Text.Json.Nodes.JsonNode.Parse(WindowStore.ToJson([window.ToSaved()])))[0];
        TileViews<TileViewKey> saved = record.PictureViews;
        TileViewKey? key = stage.State.ViewKey(boxed.Path), otherKey = stage.State.ViewKey(other.Path);
        bool readsBack = key is { } k1 && otherKey is { } k2 && saved.Zoom(k1) == crop && saved.Fills(k1) && !saved.Fills(k2)
            && record.Views?.Entries.Any(e => e.Key.Tile == k1.Tile && e.Key.Layout == stage.State.ViewsKey) == true;
        ViewerWindow second = app.OpenWindow(window.State.Content, record);
        bool same = false;
        try
        {
            NodePath boxedPath = boxed.Path, otherPath = other.Path;
            StageTile? there = null;
            bool drawn = await Until(() => (there = second.Stage.Tiles.FirstOrDefault(t => t.Path == boxedPath && t.CameraOrNone == camera)) is { Fill: true, PictureWidth: > 0 }, 10);
            StageTile? fitted = second.Stage.Tiles.FirstOrDefault(t => t.Path == otherPath);
            same = drawn && there!.DigitalZoom == crop && second.Stage.Fills(there) && fitted is not null && !second.Stage.Fills(fitted) && !fitted.WantsFill;
        }
        finally
        {
            second.Window.Close();
            await Until(() => app.Windows.Count == 1, 5);
        }
        Say($"fill pan restore: record reads back per tile {readsBack}, a window opened from it has the tile's crop and Fill {same}");
        Expect(readsBack, "the window's saved record lost the tile's crop or Fill choice");
        Expect(same, "a window opened from the record did not get the tile's crop and Fill choice back");

        // parity: fill-window-switch
        // The window's switch sets every tile; the crop stays with the tile; Reset position recenters it.
        stage.SetFill(true);
        bool allFill = stage.Fills(boxed) && stage.Fills(other) && !stage.FillMixed;
        stage.SetFill(false);
        bool allFit = !stage.Fills(boxed) && !stage.Fills(other) && !stage.FillMixed;
        stage.SetTileFill(boxed, true);
        bool offered = stage.BuildTileMenu(boxed)?.Items.OfType<Microsoft.UI.Xaml.Controls.MenuFlyoutItem>().Any(i => i.Text == "Reset Position") == true;
        StageView.ResetZoom(boxed);
        bool reset = boxed.DigitalZoom == TileZoom.None && key is { } k3 && stage.State.Views.Zoom(k3) == TileZoom.None;
        stage.SetTileFill(boxed, false);
        Say($"fill per tile: Fill Tiles fills every tile {allFill}, then fits every tile {allFit}; Reset position offered {offered}, recenters {reset}");
        Expect(allFill && allFit, "Fill Tiles did not set every tile of the window");
        Expect(offered && reset, "Reset position was not offered or did not recenter the filled crop");
    }

    // Zoom per tile (docs/UX.md "Digital zoom"): two tiles of one camera are independent views. Show camera
    // puts the camera in a second tile at the default view; each tile zooms on its own, focus keeps a tile's
    // zoom, and resetting one leaves the other. Then a swap moves each view with its camera while the tiles
    // keep their identities, and the layout is put back as it was.
    // A press, a long drag and a release on a zoomed picture through the pointer path the handlers take (the
    // button state each move carries, a mouse drag also starting a manipulation, the hover header appearing
    // under the pointer and capture going away while the button is down): the picture follows the pointer 1:1
    // the whole way, and the tile's frame (outline and header) is the tile's area at 1x, zoomed, and focused.
    // WinUI cannot be fed real pointer events from inside the app without moving the user's pointer, so the
    // handlers' own entry points stand in for them.
    private async Task CheckPanAndFrames()
    {
        StageView stage = app.Stage!;
        if (Tiles.FirstOrDefault(x => !(x.Fisheye is { } f && f.Mode.Dewarps()) && ZoomBar(x) is not null) is not { } t
            || stage.ChromeFor(t) is not { } chrome)
        {
            _problems.Add("no letterboxed tile for the pan and frame checks");
            return;
        }
        bool FrameIsTile() => chrome.Placed is var c && Near(c.X, t.ViewX) && Near(c.Y, t.ViewY) && Near(c.Width, t.ViewWidth) && Near(c.Height, t.ViewHeight);

        // parity: tile-frame-zoom
        bool at1x = await Until(FrameIsTile, 3);
        var picture = (t.X, t.Y, t.Width, t.Height);
        bool letterboxed = picture.Width < t.ViewWidth - 2 || picture.Height < t.ViewHeight - 2;
        StageView.ZoomAround(t, 0.5, 0.5, 4);
        bool drawnZoomed = await Until(() => t.Zoom > 3.9f, 3);
        bool zoomedFrame = drawnZoomed && await Until(FrameIsTile, 3);
        Say($"tile frame zoom: letterboxed {letterboxed}, frame is the tile at 1x {at1x} and at 4x {zoomedFrame} ({chrome.Placed.Width:0}x{chrome.Placed.Height:0} for a tile {t.ViewWidth:0}x{t.ViewHeight:0})");
        Expect(at1x && zoomedFrame, "the tile's frame is not the tile's area (it stays at the fitted picture)");

        // parity: pan-continuous
        TileZoom from = t.DigitalZoom;
        ZoomBase b = t.ZoomBase;
        var start = new Point(t.ViewX + t.ViewWidth / 2, t.ViewY + t.ViewHeight / 2 + 20);
        bool began = stage.DragKind(start) == "pan" && stage.PointerDown(start);
        const int moves = 60;
        double sx = 0.5, sy = 0.25;
        for (int i = 1; i <= moves; i++)
        {
            stage.PointerMove(new Point(start.X + i * sx, start.Y + i * sy), buttonDown: true);
            if (i == 8)
            {
                // A mouse drag past the system's threshold starts a manipulation too.
                stage.ManipulationBegan(touch: false);
            }
            if (i == 20)
            {
                // The hover header appears under the pointer, and capture goes away while the button is down.
                stage.Hover(t);
                stage.PointerCaptureLostWith(buttonDown: true);
            }
        }
        var end = new Point(start.X + moves * sx, start.Y + moves * sy);
        stage.PointerUp(end);
        TileZoom got = t.DigitalZoom;
        // The whole picture's length at this zoom (it overflows the tile), so a change of center is a distance
        // the picture moved (the drawn rectangle itself is clipped to the tile).
        double lengthX = from.Scale * b.Width * t.ViewWidth, lengthY = from.Scale * b.Height * t.ViewHeight;
        double movedX = (from.CenterX - got.CenterX) * lengthX, movedY = (from.CenterY - got.CenterY) * lengthY;
        bool oneToOne = Math.Abs(movedX - moves * sx) < 0.5 && Math.Abs(movedY - moves * sy) < 0.5;
        // After the release, moves without the button do nothing.
        stage.PointerMove(new Point(end.X + 40, end.Y + 40), buttonDown: false);
        bool ended = t.DigitalZoom == got;
        Say($"pan continuous: began {began}, {moves} moves of {moves * sx:0.0},{moves * sy:0.0} in all moved the picture {movedX:0.00},{movedY:0.00}, ended at release {ended}");
        Expect(began && oneToOne, "a long drag did not pan the picture 1:1 with the pointer to its end");
        Expect(ended, "the pan went on after the release");

        // parity: tile-frame-focus
        stage.FocusOn(t);
        double w = stage.ActualWidth, h = stage.ActualHeight;
        bool focusFrame = await Until(() => chrome.Placed is var c && Near(c.X, 0) && Near(c.Y, 0) && Near(c.Width, w) && Near(c.Height, h), FocusAnimation.Seconds + 3);
        bool pictureSmaller = t.Width < w - 2 || t.Height < h - 2 || t.Zoom > 1.0001f;
        stage.Unfocus();
        bool unfocused = await Until(() => app.Renderer!.Focus is null, FocusAnimation.Seconds + 2);
        bool backFrame = unfocused && await Until(FrameIsTile, 3);
        StageView.ResetZoom(t);
        Say($"tile frame focus: frame is the whole stage {focusFrame} ({chrome.Placed.Width:0}x{chrome.Placed.Height:0} of {w:0}x{h:0}, picture {t.Width:0}x{t.Height:0}, zoomed or smaller {pictureSmaller}), back to the tile {backFrame}");
        Expect(focusFrame, "a focused tile's frame hugs the picture instead of the stage");
        Expect(backFrame, "the tile's frame did not come back to its cell after focus");
    }

    private async Task CheckZoomPerTile()
    {
        StageView stage = app.Stage!;
        StageTile? first = Tiles.FirstOrDefault(t => !(t.Fisheye is { } f && f.Mode.Dewarps()));
        StageTile? next = first is null ? null : Tiles.FirstOrDefault(t => t != first && t.CameraOrNone != first.CameraOrNone && !(t.Fisheye is { } f && f.Mode.Dewarps()));
        if (first?.CameraOrNone is not { } camera || next?.CameraOrNone is not { } otherCamera)
        {
            _problems.Add("no two plain tiles for the zoom per tile checks");
            return;
        }
        NodePath path = first.Path, second = next.Path;
        StageTile At(NodePath p) => stage.Tiles.First(t => t.Path == p);
        static bool Scale(StageTile t, double s) => Math.Abs(t.DigitalZoom.Scale - s) < 0.01;

        // parity: zoom-per-tile
        stage.Assign(camera, second);
        bool fresh = At(second).CameraOrNone == camera && At(second).DigitalZoom == TileZoom.None;
        StageView.ZoomAround(At(path), 0.5, 0.5, 2);
        StageView.ZoomAround(At(second), 0.5, 0.5, 3);
        bool independent = Scale(At(path), 2) && Scale(At(second), 3) && stage.State.ViewKey(path) is { } ka && stage.State.ViewKey(second) is { } kb
            && ka != kb && Math.Abs(stage.State.Views.Zoom(ka).Scale - 2) < 0.01 && Math.Abs(stage.State.Views.Zoom(kb).Scale - 3) < 0.01;
        stage.FocusOn(At(second));
        bool focusKeeps = stage.IsFocused && Scale(At(second), 3);
        stage.Unfocus();
        bool unfocused = await Until(() => app.Renderer!.Focus is null, FocusAnimation.Seconds + 2);
        bool unfocusKeeps = unfocused && Scale(At(second), 3) && Scale(At(path), 2);
        StageView.ResetZoom(At(second));
        bool resetOne = At(second).DigitalZoom == TileZoom.None && Scale(At(path), 2);
        Say($"zoom per tile: one camera in two tiles at {At(path).DigitalZoom.Scale:0.00}x and 3x, Show camera starts at 1x {fresh}, independent {independent}, focus keeps {focusKeeps}, unfocus keeps {unfocusKeeps}, reset one tile only {resetOne}");
        Expect(fresh, "Show camera did not start the tile at the default view");
        Expect(independent, "two tiles of one camera did not zoom independently");
        Expect(focusKeeps && unfocusKeeps, "focus lost a tile's own zoom");
        Expect(resetOne, "resetting one tile's zoom changed another tile of the same camera");

        // parity: zoom-follows-swap
        stage.Assign(otherCamera, second);
        bool showReset = At(second).CameraOrNone == otherCamera && At(second).DigitalZoom == TileZoom.None;
        StageView.ZoomAround(At(second), 0.5, 0.5, 3);
        string? idA = stage.State.Tree[path]?.TileId(), idB = stage.State.Tree[second]?.TileId();
        stage.Swap(path, second);
        bool moved = At(second).CameraOrNone == camera && Scale(At(second), 2) && At(path).CameraOrNone == otherCamera && Scale(At(path), 3);
        bool idsStay = idA is not null && stage.State.Tree[path]?.TileId() == idA && stage.State.Tree[second]?.TileId() == idB;
        stage.Swap(second, path);
        bool back = At(path).CameraOrNone == camera && Scale(At(path), 2) && At(second).CameraOrNone == otherCamera && Scale(At(second), 3);
        StageView.ResetZoom(At(path));
        StageView.ResetZoom(At(second));
        bool clean = At(path).DigitalZoom == TileZoom.None && At(second).DigitalZoom == TileZoom.None
            && stage.State.ViewKey(path) is { } kp && stage.State.Views.Zoom(kp) == TileZoom.None;
        Say($"zoom follows swap: Show camera resets {showReset}, views moved with the cameras {moved}, tiles kept their identities {idsStay}, swapped back {back}, reset {clean}");
        Expect(showReset, "Show camera did not reset the tile's view");
        Expect(moved, "a swap did not move each tile's zoom with its camera");
        Expect(idsStay, "a swap moved the tiles' identities");
        Expect(back && clean, "swapping back did not bring the zooms back");
    }

    // One of a tile's bars at 1x (DIPs, a unit in from its edges), or null if the picture fills it.
    private static Rect? ZoomBar(StageTile t)
    {
        if (t.PictureWidth <= 0 || t.ViewWidth <= 0 || t.ViewHeight <= 0)
        {
            return null;
        }
        if (t.Y - t.ViewY >= 4)
        {
            return new Rect(t.ViewX + 2, t.ViewY + 1, t.ViewWidth - 4, t.Y - t.ViewY - 2);
        }
        if (t.X - t.ViewX >= 4)
        {
            return new Rect(t.ViewX + 1, t.ViewY + 2, t.X - t.ViewX - 2, t.ViewHeight - 4);
        }
        return null;
    }

    // The share of a rectangle's pixels (DIPs; sampled) that are not black in a frame begun after
    // the caller's change: the next frame may already be in flight with the old zoom (the render
    // thread reads it at the start of a frame), so that one is skipped and the one after is read.
    private async Task<double?> BarLit(StageRenderer r, Rect bar)
    {
        StageCapture c = null!;
        for (int i = 0; i < 2; i++)
        {
            Task<StageCapture> task = r.CaptureAsync();
            if (await Task.WhenAny(task, Task.Delay(5000)) != task)
            {
                _problems.Add("no frame was drawn for the zoom check");
                return null;
            }
            c = await task;
        }
        int lit = 0, n = 0;
        double step = Math.Max(Math.Min(bar.Width, bar.Height) / 8, 1);
        for (double y = bar.Y; y < bar.Y + bar.Height; y += step)
        {
            for (double x = bar.X; x < bar.X + bar.Width; x += step)
            {
                int px = (int)(x * c.ScaleX), py = (int)(y * c.ScaleY);
                if (px < 0 || py < 0 || px >= c.Image.Width || py >= c.Image.Height)
                {
                    continue;
                }
                n++;
                (int cr, int cg, int cb) = c.Image.Rgb(px, py);
                if (cr + cg + cb > 24)
                {
                    lit++;
                }
            }
        }
        return n > 0 ? lit / (double)n : null;
    }

    // A frame read back: the card's picture is the last tile drawn and is not flat.
    private async Task CheckPictureFrame(string what, StageTile picture, StageRenderer r)
    {
        Task<StageCapture> task = r.CaptureAsync();
        if (await Task.WhenAny(task, Task.Delay(5000)) != task)
        {
            _problems.Add($"no {what} frame was drawn");
            return;
        }
        StageCapture c = await task;
        (StageTile last, float x, float y, float cw, float ch) = c.Rects.Length > 0 ? c.Rects[^1] : default;
        double spread = Spread(c.Image, x, y, cw, ch);
        Say($"{what} frame {c.Image.Width}x{c.Image.Height}: drawn last {last == picture}, {cw:0}x{ch:0} px at {x:0},{y:0}, spread {spread:0.0}");
        Expect(last == picture, $"the {what} picture is not drawn last (on top)");
        Expect(spread >= 8, $"the {what} picture is flat (spread {spread:0.0})");
    }
}
