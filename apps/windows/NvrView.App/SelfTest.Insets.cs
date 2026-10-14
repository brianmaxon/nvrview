// Self-test area "calls", the alert insets (docs/UX.md "Alert insets"): the swap with the stage, a smoke alarm's inset that stays
// until dismissed, the return inset (a whole layout in miniature, or one camera), the stable order and the "+N more" chip, the
// inset grid, and the Entrance option. The rules are NvrView.Tiles AlertInsets (unit tests); these steps check them wired to a
// window, its leases, its renderer and the keys, on the manual clock the call steps use (no real-time waits). The cameras that
// only exist to fill insets are channels the mock does not have: their insets draw no picture and open no tile.

using NvrView.Shell;
using NvrView.Tiles;
using Windows.System;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    private static CoreEvent PersonAt(DeviceEntry device, int channel, bool start) => new()
    {
        Type = "detection",
        Device = device.Id,
        Channel = channel,
        Kind = "people",
        Source = DetectionSource.Device,
        State = start ? "start" : "stop",
    };

    // Takes every inset and the swap away (a smoke alarm's inset dismisses its alert too).
    private static void ClearInsets(StageView stage)
    {
        for (int i = 0; i < 64 && !stage.Board.IsEmpty; i++)
        {
            CameraRef next = stage.Board.Swap?.Shown ?? stage.Board.Items.First().Camera;
            stage.DismissInset(next);
        }
    }

    private async Task AlertInsetSteps(Viewer v, StageView stage, DeviceEntry device, CameraRef doorbell, ManualViewerClock clock)
    {
        StageRenderer r = stage.Renderer;
        ViewerWindow window = app.Windows.First(w => w.Stage == stage);
        StageTile[] tiles = Tiles;
        StageTile? focusTile = tiles.FirstOrDefault(t => t.Camera == new CameraRef(device.Key, 1));
        StageTile? hallTile = tiles.FirstOrDefault(t => t.Camera == new CameraRef(device.Key, 2));
        if (focusTile is null || hallTile is null || tiles.Length < 3)
        {
            _problems.Add("the alert inset steps need a window showing several of the mock's cameras");
            return;
        }
        CameraRef other = focusTile.Camera;
        CameraRef hall = hallTile.Camera;
        // The board's times are the viewer clock's: the steps before this one used other clocks, so it starts over.
        stage.ResetInsets();
        ClearInsets(stage);

        // parity: alert-inset-swap
        // With another camera focused, a doorbell's call is an inset. A click on its picture swaps: the doorbell fills the stage
        // (the main stream, drawn over the whole stage), the focused camera goes into the return inset, and clicking that or
        // Tab swaps back; Esc then ends the call and leaves the focus alone.
        stage.FocusOn(focusTile);
        v.Apply(Alarm(device, doorbell.Channel, "visitor", true));
        v.Apply(Alarm(device, doorbell.Channel, "visitor", false));
        stage.CallCards.UpdateLayout();
        Call? call = v.Calls.CallFor(doorbell);
        bool inset = stage.Board.Shown.Any(i => i.Camera == doorbell && i.Kind.IsCall) && call is not null;
        Expect(inset, "a press with another camera focused did not open the doorbell's inset");
        if (!inset || call is null)
        {
            stage.Unfocus();
            ClearInsets(stage);
            return;
        }
        clock.Advance(1);
        double clicked = clock.Now;
        CallCard? card = stage.CallCards.Cards.FirstOrDefault(c => c.Camera == doorbell && c.Mode == CallCardMode.Inset);
        bool clickedPicture = false;
        if (card is { Placed: true } && stage.CallCards.Column is not null)
        {
            // The card's own press and release on its picture, without moving: a click.
            card.DragPress(new Windows.Foundation.Point(card.Frame.X + 60, card.Frame.Y + 120), onHeader: false);
            card.DragRelease();
            clickedPicture = stage.Board.Swap is not null;
        }
        if (!clickedPicture)
        {
            stage.SwapInset(doorbell);
        }
        bool swapped = stage.Board.Swap?.Shown == doorbell && stage.Board.Items.FirstOrDefault() is { IsReturn: true } ret && ret.Camera == other;
        bool main = v.Planner.WantedTier(doorbell) == StreamTier.Main;
        bool drawn = await Until(() => stage.CallCards.TakeoverTile is { } t && r.Covered && r.Overlays.Any(o => o.Tile == t && o.Rect.Width > 100), 10);
        Say($"alert insets: click on the picture swapped {clickedPicture}, the doorbell is on the stage {swapped}, wants {v.Planner.WantedTier(doorbell)}, drawn over the whole stage {drawn}, header \"{stage.TakeoverText}\"");
        Expect(clickedPicture, "a click on the inset's picture did not swap it with the stage");
        Expect(swapped, "swapping did not put the doorbell on the stage and the focused camera into the return inset");
        Expect((call.LastActivity) >= clicked, "clicking the call's inset did not count as call activity");
        Expect(main, "the swapped-in doorbell did not ask for the main stream");
        Expect(drawn, "the swapped-in camera is not drawn over the stage while the layout is covered");
        Expect(stage.ShowsCamera(doorbell) && !stage.ShowsCamera(other) && stage.TakeoverText.Contains("swap back", StringComparison.Ordinal), "after the swap the stage does not show the doorbell with its header");
        stage.HandleKey(VirtualKey.Tab, ctrl: false, repeat: false);
        bool back = stage.Board.Swap is null && stage.Board.ItemFor(doorbell) is not null;
        bool undrawn = await Until(() => !r.Covered && stage.CallCards.TakeoverTile is null, 5);
        Say($"alert insets: Tab swapped back {back}, the layout is drawn again {undrawn}");
        Expect(back && undrawn, "Tab did not swap back");
        stage.HandleKey(VirtualKey.Escape, ctrl: false, repeat: false);
        bool ended = stage.Board.IsEmpty && v.Calls.CallFor(doorbell) is null;
        Say($"alert insets: Esc dismissed the inset and ended the call {ended}, still focused {stage.IsFocused}");
        Expect(ended, "Esc did not dismiss the inset and end the call");
        Expect(stage.IsFocused, "Esc on an inset also left focus");

        // parity: alert-inset-smoke-stays
        // A smoke alarm on a camera the focused window does not show opens a red inset that stays however long it is left (a
        // lost stop included); Esc dismisses it and its alert.
        v.Apply(Detect(hallTile, "smoke_alarm", "local_audio", true));
        v.Apply(Detect(hallTile, "smoke_alarm", "local_audio", false));
        stage.CallCards.UpdateLayout();
        InsetItem? smoke = stage.Board.ItemFor(hall);
        bool critical = smoke is { Active: false } && smoke.Kind.IsCritical;
        clock.Advance(InsetBoard.LostStopAfter + InsetBoard.AutoCloseDelay + 60);
        stage.Tick();
        bool stays = stage.Board.ItemFor(hall) is not null;
        stage.HandleKey(VirtualKey.Escape, ctrl: false, repeat: false);
        bool dismissed = stage.Board.IsEmpty && !v.Alerts.IsActive(hall);
        Say($"alert insets: smoke inset critical {critical}, still open after 4 minutes {stays}, Esc dismissed it and its alert {dismissed}, still focused {stage.IsFocused}");
        Expect(critical, "a smoke alarm did not open a critical inset");
        Expect(stays, "the smoke alarm's inset did not stay");
        Expect(dismissed, "Esc did not dismiss the smoke inset and its alert");
        stage.Unfocus();
        bool unfocused = await Until(() => r.Focus is null, FocusAnimation.Seconds + 2);
        Expect(unfocused, "the focus did not end after the inset steps' focus");
        // Dismissed in another window's header or by Enter on a tile: the inset goes with the alert.
        stage.FocusOn(focusTile);
        v.Apply(Detect(hallTile, "smoke_alarm", "local_audio", true));
        bool opened = stage.Board.ItemFor(hall) is not null;
        v.DismissAlert(hall);
        stage.CallCards.Sync();
        bool wentWithAlert = opened && stage.Board.ItemFor(hall) is null;
        v.Apply(Detect(hallTile, "smoke_alarm", "local_audio", false));
        stage.Unfocus();
        await Until(() => r.Focus is null, FocusAnimation.Seconds + 2);
        Say($"alert insets: an alert dismissed elsewhere takes its inset with it {wentWithAlert}");
        Expect(wentWithAlert, "dismissing the alert did not take the smoke inset with it");
        ClearInsets(stage);

        await AlertInsetReturn(v, stage, device, tiles, clock);
        await AlertInsetOrder(v, stage, device, clock);
        await AlertInsetGrid(v, stage, window, device, clock);
        AlertInsetEntrance(v, stage, device, focusTile);
    }

    // After a swap from a layout the return inset draws the whole layout in miniature (the layout's own tiles scaled into the
    // inset); a click or Tab restores the layout as it was; after a swap from one focused tile it shows that one camera.
    private async Task AlertInsetReturn(Viewer v, StageView stage, DeviceEntry device, StageTile[] tiles, ManualViewerClock clock)
    {
        StageRenderer r = stage.Renderer;
        var side = new CameraRef(device.Key, 11);
        stage.Unfocus();
        await Until(() => r.Focus is null, FocusAnimation.Seconds + 2);
        StageTile first = tiles[0];
        int leaves = stage.State.Tree.Leaves.Count(l => l.Camera is not null);
        if (leaves < 3)
        {
            _problems.Add("the inset return steps need a layout of several cameras");
            return;
        }
        // A zoomed tile and a selection must survive the round trip.
        stage.PinchDigital(first, first.X + first.Width / 2, first.Y + first.Height / 2, 2);
        float zoomBefore = first.Zoom;
        stage.State.Selected = tiles[1].Path;
        NodePath? selectedBefore = stage.State.Selected;
        IReadOnlyList<CameraRef?> camerasBefore = stage.State.Tree.CameraList;
        stage.Board.Started(InsetKind.Detection("people"), side, false, clock.Now, v.WallClock());
        stage.CallCards.Sync();
        stage.CallCards.UpdateLayout();

        // parity: alert-inset-return-layout
        // A click on the inset (its own press and release): the stage shows its camera and the return inset draws all the layout's
        // tiles, scaled into the inset's rectangle, from the layout's own shared tiles.
        CallCard? card = stage.CallCards.Cards.FirstOrDefault(c => c.Camera == side);
        if (card is { Placed: true } && stage.CallCards.Column is not null)
        {
            card.DragPress(new Windows.Foundation.Point(card.Frame.X + 60, card.Frame.Y + 60), onHeader: false);
            card.DragRelease();
        }
        else
        {
            stage.SwapInset(side);
        }
        bool swapped = stage.Board.Swap?.Shown == side;
        InsetItem? returnItem = stage.Board.Items.FirstOrDefault(i => i.IsReturn);
        bool isLayout = returnItem is { ShowsLayout: true } && stage.Board.Swap?.DisplacedLayout == true;
        bool drawn = await Until(() => stage.CallCards.MiniatureTiles.Count == leaves && r.Overlays.Length == 1 + leaves, 10);
        IReadOnlyList<StageTile> minis = stage.CallCards.MiniatureTiles;
        var expected = tiles.Select(t => t.Shared).Where(t => t is not null).ToHashSet();
        bool samePictures = minis.Select(t => t.Shared).Where(t => t is not null).ToHashSet().SetEquals(expected);
        Windows.Foundation.Rect[] cells = [.. r.Overlays.Where(o => minis.Contains(o.Tile)).Select(o => new Windows.Foundation.Rect(o.Rect.X, o.Rect.Y, o.Rect.Width, o.Rect.Height))];
        double left = cells.Length > 0 ? cells.Min(c => c.Left) : 0, top = cells.Length > 0 ? cells.Min(c => c.Top) : 0;
        double right = cells.Length > 0 ? cells.Max(c => c.Right) : 0, bottom = cells.Length > 0 ? cells.Max(c => c.Bottom) : 0;
        double stageAspect = stage.ActualWidth / Math.Max(stage.ActualHeight, 1);
        bool sameShape = right > left && bottom > top && Math.Abs((right - left) / (bottom - top) - stageAspect) / stageAspect < 0.06;
        Say($"alert insets: return layout: swapped {swapped}, return inset shows the layout {isLayout}, {minis.Count} tiles drawn in miniature of {leaves} ({r.Overlays.Length} pictures over the grid {drawn}), " +
            $"the layout's own pictures {samePictures}, the stage's shape {sameShape}");
        Expect(swapped, "a click on the inset did not swap it onto the stage");
        Expect(isLayout, "the return inset does not show the layout the stage had");
        Expect(drawn, "the layout in miniature is not drawn: one picture for the stage and one for each of the layout's tiles");
        Expect(samePictures, "the miniature does not draw the layout's own shared tiles");
        Expect(sameShape, "the miniature layout does not have the stage's shape");

        // parity: alert-inset-return-tab
        stage.HandleKey(VirtualKey.Tab, ctrl: false, repeat: false);
        bool restored = stage.Board.Swap is null && stage.State.Tree.CameraList.SequenceEqual(camerasBefore)
            && stage.State.Selected == selectedBefore && Math.Abs(first.Zoom - zoomBefore) < 0.001;
        bool minisGone = await Until(() => stage.CallCards.MiniatureTiles.Count == 0 && !r.Covered, 5);
        Say($"alert insets: return Tab: layout restored with the same cameras, zoom and selection {restored}, miniature gone {minisGone}");
        Expect(restored, "Tab did not restore the same layout (cameras, zoom, selection)");
        Expect(minisGone, "after Tab the grid still draws the miniature or the covered layout");

        // parity: alert-inset-return-single
        // A single focused tile keeps the one-camera return inset.
        stage.FocusOn(tiles[2]);
        stage.SwapInset(side);
        InsetItem? single = stage.Board.Items.FirstOrDefault(i => i.IsReturn);
        bool oneCamera = single is { ShowsLayout: false } && single.Camera == tiles[2].Camera;
        bool twoDrawn = await Until(() => r.Overlays.Length == 2, 10);
        Say($"alert insets: return single: shows the focused camera {oneCamera}, {r.Overlays.Length} pictures over the grid");
        Expect(oneCamera, "a swap from a focused tile did not keep a one-camera return inset");
        Expect(twoDrawn, "the single-camera return did not draw the stage's camera and the inset's picture");
        stage.HandleKey(VirtualKey.Tab, ctrl: false, repeat: false);
        Expect(stage.Board.Swap is null, "Tab did not swap back from the single-camera return");
        stage.Unfocus();
        await Until(() => r.Focus is null, FocusAnimation.Seconds + 2);
        ClearInsets(stage);
        first.DigitalZoom = new TileZoom(1, 0.5, 0.5);
        stage.State.Selected = null;
    }

    // Four cameras with person events interleaved on the manual clock (the Entrance option is what opens them): three insets in,
    // the fourth waits until the lowest has been up 10 s, then the order holds; the chip brings a waiting camera in.
    private async Task AlertInsetOrder(Viewer v, StageView stage, DeviceEntry device, ManualViewerClock clock)
    {
        CameraRef[] cameras = [.. Enumerable.Range(10, 4).Select(c => new CameraRef(device.Key, c))];
        // Without the Entrance option a person opens nothing.
        v.Apply(PersonAt(device, 10, true));
        stage.CallCards.Sync();
        bool nothing = stage.Board.IsEmpty;
        v.Apply(PersonAt(device, 10, false));
        Expect(nothing, "a person on a camera that is not an entrance opened an inset");
        foreach (CameraRef c in cameras)
        {
            v.Settings.SetEntrance(c, true);
        }

        // parity: alert-inset-order
        double start = clock.Now;
        var changes = new List<double>();
        string last = "";
        for (int second = 0; second < 24; second++)
        {
            v.Apply(PersonAt(device, 10 + second % 4, true));
            clock.Advance(1);
            stage.Tick();
            string shown = string.Join(",", stage.Board.Shown.Select(i => i.Camera.ToString()));
            if (shown != last)
            {
                changes.Add(clock.Now - start);
                last = shown;
            }
        }
        bool stable = changes.Count == 4 && changes.Take(3).All(c => c <= 3) && changes[^1] >= InsetBoard.MinimumShownTime;
        int[] counts = [.. cameras.Select(c => stage.Board.ItemFor(c)?.Count ?? 0)];
        bool refreshed = counts.All(c => c >= 6);
        Say($"alert insets: order: changes at [{string.Join(", ", changes.Select(c => (int)c))}] s, repeats refreshed in place {refreshed}");
        Expect(stable, $"insets churned: the shown set changed at [{string.Join(", ", changes)}] s (expected 3 arrivals, then one swap after 10 s)");
        Expect(refreshed, "repeated events did not refresh their insets in place");
        stage.CallCards.UpdateLayout();
        bool chip = await Until(() =>
        {
            stage.CallCards.UpdateLayout();
            return stage.CallCards.ChipText.StartsWith('+');
        }, 5);
        Say($"alert insets: waiting {stage.Board.Waiting.Count}, chip \"{stage.CallCards.ChipText}\"");
        Expect(stage.Board.Waiting.Count == 1 && chip, $"the chip does not show the waiting camera: \"{stage.CallCards.ChipText}\"");
        // The chip lists the waiting camera; choosing it brings it in.
        if (stage.Board.Waiting.FirstOrDefault() is { } waiting)
        {
            stage.BringInInset(waiting.Camera);
            bool brought = stage.Board.Shown[0].Camera == waiting.Camera && stage.Board.Shown.Count == 3 && stage.Board.Waiting.Count == 1;
            Expect(brought, "choosing a waiting camera did not bring it in");
        }
        foreach (CameraRef c in cameras)
        {
            v.Apply(PersonAt(device, c.Channel, false));
        }
        ClearInsets(stage);
    }

    // The Settings grid (rows x columns) sets how many insets show; a window can override it, saved with the window.
    private async Task AlertInsetGrid(Viewer v, StageView stage, ViewerWindow window, DeviceEntry device, ManualViewerClock clock)
    {
        CameraRef[] cameras = [.. Enumerable.Range(10, 7).Select(c => new CameraRef(device.Key, c))];
        foreach (CameraRef c in cameras)
        {
            v.Settings.SetEntrance(c, true);
        }

        // parity: alert-inset-grid
        v.Settings.InsetGrid = new InsetGridSetting(2, 3);
        foreach (CameraRef c in cameras)
        {
            v.Apply(PersonAt(device, c.Channel, true));
        }
        stage.CallCards.UpdateLayout();
        int shown = stage.Board.Shown.Count, more = stage.Board.More;
        await Until(() =>
        {
            stage.CallCards.UpdateLayout();
            return stage.CallCards.ChipText.StartsWith('+');
        }, 5);
        string chipText = stage.CallCards.ChipText;
        double width = stage.CallCards.ActualWidth;
        int fits = InsetGeometry.FittingColumns(width, 3);
        int columns = stage.CallCards.Cards.Where(c => c.Placed).Select(c => (int)Math.Round(c.Frame.X)).Distinct().Count();
        Say($"alert insets: grid 2x3: {shown} shown, {more} more, chip \"{chipText}\", {columns} columns of {fits} fitting in {width:0} DIPs");
        Expect(shown == 6 && more == 1, $"a 2 x 3 inset grid shows {shown} with {more} more, expected 6 and 1");
        Expect(chipText.StartsWith('+') && (stage.CallCards.Truncated || chipText == "+1 more"), $"the 7th inset is not in \"+N more\": \"{chipText}\"");
        if (fits >= 3 && !stage.CallCards.Truncated)
        {
            Expect(columns == 3, $"the insets are in {columns} columns, expected 3");
        }

        // This window's own grid wins and is saved with the window; the Settings default is untouched.
        stage.SetInsetGrid(1, 2);
        bool overridden = stage.Board.Shown.Count == 2 && stage.Board.More == 5
            && window.ToSaved().InsetGrid == new InsetGridSetting(1, 2) && v.Settings.InsetGrid == new InsetGridSetting(2, 3);
        SavedWindow record = WindowStore.FromJson(System.Text.Json.Nodes.JsonNode.Parse(WindowStore.ToJson([window.ToSaved()])))[0];
        bool readsBack = record.InsetGrid == new InsetGridSetting(1, 2);
        Say($"alert insets: grid override 1x2: {stage.Board.Shown.Count} shown, saved {window.ToSaved().InsetGrid is not null}, reads back {readsBack}, Settings kept {v.Settings.InsetGrid.Capacity == 6}");
        Expect(overridden, "this window's inset grid did not override the Settings default");
        Expect(readsBack, "the window's own inset grid did not save and read back");
        stage.UseDefaultInsetGrid();
        bool back = stage.Board.Shown.Count == 6 && window.ToSaved().InsetGrid is null;
        Expect(back, "Use the Settings Default did not bring the Settings grid back");
        // The Settings default changes for windows without their own.
        v.Settings.InsetGrid = InsetGridSetting.Standard;
        Expect(stage.Board.Shown.Count == 3, "changing the Settings grid did not reach the window");
        foreach (CameraRef c in cameras)
        {
            v.Apply(PersonAt(device, c.Channel, false));
            v.Settings.SetEntrance(c, false);
        }
        ClearInsets(stage);
        await Task.CompletedTask;
    }

    // The Entrance option per camera: in the tile's menu and Settings (one setting), saved with the app's view settings.
    private void AlertInsetEntrance(Viewer v, StageView stage, DeviceEntry device, StageTile tile)
    {
        // parity: alert-inset-entrance
        CameraRef camera = tile.Camera;
        bool before = v.Settings.Entrance(camera);
        var menuBefore = stage.BuildTileMenu(tile)!.Items.OfType<Microsoft.UI.Xaml.Controls.ToggleMenuFlyoutItem>().FirstOrDefault(i => i.Text == "Entrance");
        v.Settings.SetEntrance(camera, true);
        var menuAfter = stage.BuildTileMenu(tile)!.Items.OfType<Microsoft.UI.Xaml.Controls.ToggleMenuFlyoutItem>().FirstOrDefault(i => i.Text == "Entrance");
        bool menu = menuBefore is { IsEnabled: true, IsChecked: false } && menuAfter is { IsEnabled: true, IsChecked: true };
        string file = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-view-settings-{Guid.NewGuid():N}.json");
        bool saved;
        try
        {
            var writer = new ViewSettings(file);
            writer.SetEntrance(camera, true);
            writer.InsetGrid = new InsetGridSetting(2, 3);
            var reread = new ViewSettings(file);
            saved = reread.Entrance(camera) && !reread.Entrance(new CameraRef(camera.Device, camera.Channel + 1)) && reread.InsetGrid == new InsetGridSetting(2, 3);
        }
        finally
        {
            File.Delete(file);
        }
        v.Settings.SetEntrance(camera, before);
        Say($"alert insets: Entrance in the tile menu follows the setting {menu}, entrance and grid read back from view-settings.json {saved}");
        Expect(menu, "the tile menu's Entrance item is missing, disabled, or does not follow the setting");
        Expect(saved, "the Entrance option and the inset grid did not save and read back");
    }
}
