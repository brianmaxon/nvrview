// Self-test area "header": the tile header model and the Windows header, menus, and keys.

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
    // The tile header (docs/UX.md "Tile header"): laid out by the shared model on the real chrome at several widths, the "..."
    // and right-click menus built from it, and what a not-yet-built action says.
    private async Task HeaderModel(Viewer v, StageView stage)
    {
        StageTile? tile = Tiles.FirstOrDefault(t => stage.HeaderTileOf(t) is { Picture: true } && stage.ChromeFor(t) is not null);
        if (tile is null || stage.ChromeFor(tile) is not { } chrome || stage.HeaderTileOf(tile) is not { } model)
        {
            _problems.Add("no tile with a picture to check the header on");
            return;
        }
        var clock = new ManualViewerClock(new SystemViewerClock().Now);
        v.UseClock(clock);
        try
        {
            IReadOnlyList<TileHeaderItem> buttons = TileHeaderLayout.Buttons(model);
            TileHeaderMetrics m = TileHeaderMetrics.Standard;
            string placedWhy = "", whyNot = "";
            // Lays the header out at a total width: the buttons shown, and whether they sit inside the tile, apart, and right of the title.
            (IReadOnlyList<TileHeaderItem> Shown, TileHeaderLayout? Layout, bool Placed) At(double width)
            {
                chrome.ForceWidth(width);
                var frames = chrome.ShownButtons.Select(i => chrome.ButtonFrame(i)).Where(f => f is not null).Select(f => f!.Value).OrderBy(f => f.X).ToList();
                bool apart = frames.Zip(frames.Skip(1)).All(p => p.First.X + p.First.Width <= p.Second.X + 0.5);
                bool inside = frames.All(f => f.X >= 0 && f.X + f.Width <= width + 0.5);
                bool clear = frames.Count == 0 || chrome.TitleFrame.X + chrome.TitleFrame.Width <= frames[0].X + 0.5;
                placedWhy = $"{width:0}: apart {apart} inside {inside} clear {clear}; title {chrome.TitleFrame.X:0}+{chrome.TitleFrame.Width:0}; buttons {string.Join(" ", frames.Select(f => $"{f.X:0}+{f.Width:0}"))}";
                if (apart && inside && clear)
                {
                    placedWhy = "";
                }
                else
                {
                    whyNot += placedWhy + " | ";
                }
                return (chrome.ShownButtons, chrome.HeaderLayout, apart && inside && clear);
            }
            // parity: header-priority-narrow
            chrome.ForceWidth(m.Narrow + 100);
            double reserve = chrome.Reserve;
            var narrow = At(110 + reserve);
            // One DIP short of every button: some go into "...".
            double width = reserve + 2 * m.Padding + m.TitleMin + m.Icon * buttons.Count - 1;
            int fit = (int)((width - reserve - 2 * m.Padding - m.TitleMin - m.Icon) / m.Icon);
            var middle = At(width);
            var nudged = At(width + 5);
            var wide = At(1000);
            bool narrowOK = narrow.Shown.SequenceEqual([TileHeaderItem.More]) && narrow.Layout?.Visible.SequenceEqual([TileHeaderItem.Title, TileHeaderItem.More]) == true && narrow.Placed;
            bool middleOK = middle.Shown.SequenceEqual([.. buttons.Take(fit), TileHeaderItem.More]) && middle.Placed
                && middle.Layout?.Visible.Where(i => i != TileHeaderItem.Title).SequenceEqual(middle.Shown) == true;
            bool wideOK = wide.Shown.SequenceEqual(buttons) && wide.Layout?.ShowsMore == false && wide.Placed;
            bool steady = nudged.Shown.SequenceEqual(middle.Shown);
            // The channels event's capabilities reach the model: a doorbell with talk-back, and a second lens that moves.
            using var doc = System.Text.Json.JsonDocument.Parse(
                "{\"channel\":6,\"talk\":true,\"doorbell\":true,\"ptz\":{\"pan_tilt\":false,\"presets\":false,\"zoom_focus\":false},"
                + "\"lens_talk\":[true,false],\"lens_ptz\":[{\"pan_tilt\":false},{\"pan_tilt\":true,\"presets\":true,\"zoom_focus\":true}]}");
            ChannelInfo? caps = ChannelInfo.From(doc.RootElement);
            bool capsOK = caps is { Doorbell: true } && caps.TalkOf(0) && !caps.TalkOf(1) && !caps.PtzOf(0).PanTilt && caps.PtzOf(1) is { PanTilt: true, Presets: true, ZoomFocus: true };
            Say($"header: placed {narrow.Placed}/{middle.Placed}/{wide.Placed}, narrow layout [{string.Join(",", narrow.Layout?.Visible ?? [])}], reserve {reserve:0}, {whyNot}; 110 DIP {string.Join(",", narrow.Shown)}, {width:0} DIP {string.Join(",", middle.Shown)}, 5 DIP wider unchanged {steady}, 1000 DIP {string.Join(",", wide.Shown)}; capabilities parsed {capsOK}");
            Expect(narrowOK, "below 120 DIP the header did not show only the title and the more button");
            Expect(middleOK, $"one DIP short of every button, the header did not show the model's first {fit} buttons and the more button");
            Expect(wideOK, "a wide header did not show every button without the more button");
            Expect(steady, "a 5 DIP width change reflowed the header");
            Expect(capsOK, "the channels event's talk, doorbell and PTZ capabilities were not read");

            // parity: header-overflow-menu
            At(width);
            List<string> more = [.. stage.MoreMenuTexts(tile)];
            List<string> wantMore = chrome.HeaderLayout is { } l ? [.. l.MoreMenu.Select(e => e.Label)] : ["?"];
            List<string> context = Texts(stage.BuildTileMenu(tile)!.Items);
            List<string> wantContext = [.. TileHeaderLayout.Menu(model).Select(e => e.Label)];
            bool menusOK = more.SequenceEqual(wantMore) && context.Take(wantContext.Count).SequenceEqual(wantContext)
                && context.Contains("Split Side by Side") && more.Contains("Entrance") && !more.Contains("Listen");
            Say($"header menus: more [{string.Join(" / ", more)}], right-click starts with the model's {wantContext.Count} items {menusOK}");
            Expect(menusOK, $"the more or right-click menu is not the header model's list ({string.Join(" / ", more)} vs {string.Join(" / ", wantMore)})");
            // Every header action is built: nothing is left to say "coming soon".
        }
        finally
        {
            chrome.ForceWidth(null);
            v.UseClock(new SystemViewerClock());
        }
        if (Runs("capture"))
        {
            await CaptureRecordSteps(v, stage, tile);
        }
    }

    // 11c. The header, the trimmed tile menu, and the Settings window (docs/UX.md "Windows header", "Settings window").
    private async Task HeaderAndSettings(Viewer v)
    {
        ViewerWindow w = app.Windows[0];
        StageView stage = w.Stage;
        HeaderBar header = w.Header;
        IViewerClock realClock = w.RevealClock;

        // parity: header-present
        // The header is the title bar: it draws into it, keeps room for the caption buttons, hands the system its
        // controls, and shows the quick actions, Settings, and the reserved recording spot.
        var buttons = header.Buttons();
        Say($"header: {string.Join(" / ", buttons.Select(b => b.Name))}; layout \"{header.LayoutLabel}\"; recording spot reserved {header.HasRecordingSpot}, shown {header.RecordingSpotVisible}");
        Expect(w.ExtendsIntoTitleBar, "the window does not extend its content into the title bar");
        HeaderAction[] expected = [HeaderAction.FindDevices, HeaderAction.EditLayout, HeaderAction.FillTiles, HeaderAction.FullScreen, HeaderAction.Settings];
        Expect(expected.All(a => buttons.Any(b => b.Action == a && b.Enabled && b.Visible && b.Name.Length > 0)), "the header lacks a quick action or Settings");
        Expect(header.LayoutLabel == w.ContentTitle && w.DockedHeaderShown, "the header does not show the current layout");
        Expect(header.HasRecordingSpot && !header.RecordingSpotVisible, "the recording spot is not reserved, or shows before anything records");
        Expect(Math.Abs(header.Root.ActualHeight - header.Root.Height) < 1 && header.Root.ActualHeight >= 32, "the header does not have the title bar's height");
        bool regions = await Until(() => w.PassthroughRegions >= buttons.Count, 5);
        Say($"header: {w.PassthroughRegions} interactive regions handed to the system");
        Expect(regions, "the header's controls were not given to the system as interactive regions");

        // parity: header-actions
        // The actions do what the menu items did.
        header.Press(HeaderAction.EditLayout);
        bool editing = stage.State.Editing;
        string editName = header.Buttons().First(b => b.Action == HeaderAction.EditLayout).Name;
        header.Press(HeaderAction.EditLayout);
        Expect(editing && editName == "Done editing" && !stage.State.Editing, "Edit layout in the header did not toggle editing");
        header.Press(HeaderAction.FillTiles);
        bool filled = stage.State.Fill;
        header.Press(HeaderAction.FillTiles);
        Expect(filled && !stage.State.Fill, "Fill tiles in the header did not toggle");
        stage.FullScreenOverride = true;
        header.Update();
        string fullName = header.Buttons().First(b => b.Action == HeaderAction.FullScreen).Name;
        stage.FullScreenOverride = null;
        header.Update();
        Expect(fullName == "Leave full screen" && header.Buttons().First(b => b.Action == HeaderAction.FullScreen).Name == "Full screen", "the Full screen button does not say what it does");
        List<string> overflow = Texts(header.BuildOverflow().Items);
        Say($"header: overflow {string.Join(" / ", overflow)}");
        string[] rest = ["New window", "Recordings", w.SidebarDocked ? "Hide sidebar" : "Show sidebar", "Indicator corner", "Show diagnostics", "Keyboard shortcuts"];
        Expect(rest.All(overflow.Contains) && overflow.Any(t => t == "Check for updates…" || t.StartsWith("Update to ", StringComparison.Ordinal) || t.StartsWith("Switch to ", StringComparison.Ordinal)),
            "the overflow menu lacks an app-wide item that left the tile menu");
        // The same arrangement saved as a layout, so no camera loses its lease while the window switches.
        WindowContent before = w.State.Content;
        string? layoutId = v.CreateLayout("Header test", w.State.Tree);
        List<string> picker = Texts(header.BuildLayoutMenu().Items);
        Say($"header: layout picker {string.Join(" / ", picker)}");
        Expect(layoutId is not null && picker.Contains("All Cameras") && picker.Contains("Header test") && picker.Contains("New layout…")
            && picker.Contains("New layout from template") && picker.Contains("Save as layout…"), "the layout picker lacks a layout or a layout action");
        // parity: header-layout-picker
        // Choosing a layout in the picker shows it in the window, and the header follows.
        if (layoutId is not null)
        {
            w.Show(new WindowContent.Layout(layoutId));
            Expect(header.LayoutLabel == "Header test" && Texts(header.BuildLayoutMenu().Items).Contains("Header test")
                && !Texts(header.BuildLayoutMenu().Items).Contains("Save as layout…"), "the header did not follow the layout shown");
            w.Show(before);
            v.DeleteLayout(layoutId);
        }
        Expect(header.LayoutLabel == w.ContentTitle, "the header did not return to the arrangement shown");

        // parity: header-fullscreen-reveal
        // In full screen the header is hidden and slides in from the top edge on the sidebar's timing (manual clock).
        var clock = new ManualViewerClock(5000);
        w.RevealClock = clock;
        w.SimulateHeaderFullScreen(true);
        Expect(!w.HeaderVisible && !w.DockedHeaderShown && !w.HeaderOverlayInteractive, "the header is not hidden in full screen");
        w.HeaderPointerAt(inEdge: true, inHeader: false);
        clock.Advance(0.1);
        w.TickHeader();
        Expect(!w.HeaderVisible, "a pass over the top edge revealed the header");
        clock.Advance(0.06);
        w.TickHeader();
        Expect(w.HeaderVisible && w.HeaderOverlayShown && w.HeaderOverlayInteractive, "resting at the top edge did not reveal the header");
        w.HeaderPointerAt(inEdge: false, inHeader: true);
        clock.Advance(5);
        w.TickHeader();
        Expect(w.HeaderVisible, "the header slid away while the pointer was on it");
        w.HeaderPointerAt(inEdge: false, inHeader: false);
        clock.Advance(0.49);
        w.TickHeader();
        bool stillThere = w.HeaderVisible;
        clock.Advance(0.02);
        w.TickHeader();
        Say($"header: full screen reveal: hidden {!w.HeaderOverlayShown}, held until half a second after the pointer left {stillThere}");
        Expect(stillThere && !w.HeaderVisible && !w.HeaderOverlayInteractive, "the header did not slide away half a second after the pointer left");
        w.SimulateHeaderFullScreen(null);
        w.RevealClock = realClock;
        Expect(w.HeaderVisible && w.DockedHeaderShown, "leaving full screen did not dock the header again");

        // parity: tile-menu-no-app-wide
        // The right-click menu holds the tile's own actions and none of the app-wide ones.
        StageTile tile = Tiles[0];
        Microsoft.UI.Xaml.Controls.MenuFlyout? tileMenu = stage.BuildTileMenu(tile);
        List<string> tileItems = tileMenu is null ? [] : Texts(tileMenu.Items);
        Say($"tile menu: {string.Join(" / ", tileItems)}");
        string[] own = ["Focus", "Listen", "Start Conversation", "Allow talk-back", "Quality"];
        Expect(own.All(tileItems.Contains), "the tile menu lost one of the tile's own actions");
        Expect(!tileItems.Any(HeaderActions.IsAppWideTitle), $"the tile menu still holds an app-wide item: {string.Join(", ", tileItems.Where(HeaderActions.IsAppWideTitle))}");
        stage.SetEditing(true);
        List<string> editItems = Texts(stage.BuildTileMenu(tile)!.Items);
        stage.SetEditing(false);
        Expect(editItems.Contains("Split Side by Side") && editItems.Contains("Show Camera") && !editItems.Any(HeaderActions.IsAppWideTitle),
            "the tile menu while editing lacks a layout action or holds an app-wide item");

        if (Runs("settings"))
        {
            await SettingsWindowSteps(v, w);
        }
    }
}
