// Tests of the Windows header's pure rules (NvrView.Tiles/Header.cs): nothing waits.

using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class HeaderTests
{
    public static IEnumerable<(string, Action)> All() =>
    [
        ("header: every app-wide action has one place, with a title", OneEach),
        ("header: the zones hold what the owner asked for", Zones),
        ("header: toggles read their on state", Toggles),
        ("header: app-wide titles are recognized, tile titles are not", AppWide),
        ("header: the recording spot is hidden until something records", Recording),
        ("settings: five sections in pane order", Sections),
        ("title bar: rectangles scale outward with the display scale", Scaling),
        ("title bar: passthrough regions are clipped and empty ones dropped", Passthrough),
        ("title bar: the height follows the system's caption", Height),
    ];

    private static void OneEach()
    {
        foreach (HeaderAction a in Enum.GetValues<HeaderAction>())
        {
            Check.Equal(1, HeaderActions.All.Count(i => i.Action == a), $"{a} appears once");
            Check.True(HeaderActions.Of(a).Title.Length > 0, $"{a} has a title");
        }
    }

    private static void Zones()
    {
        string Names(HeaderZone z) => string.Join(", ", HeaderActions.In(z).Select(i => i.Action));
        Check.Equal("FindDevices, EditLayout, FillTiles, FullScreen", Names(HeaderZone.Quick), "quick actions");
        Check.Equal("Settings", Names(HeaderZone.Right), "right");
        Check.Equal("NewLayout, NewLayoutFromTemplate, SaveAsLayout", Names(HeaderZone.LayoutPicker), "layout picker");
        Check.True(HeaderActions.In(HeaderZone.Overflow).Any(i => i.Action == HeaderAction.KeyboardShortcuts), "overflow holds the rest");
    }

    private static void Toggles()
    {
        HeaderItem edit = HeaderActions.Of(HeaderAction.EditLayout);
        Check.Equal("Edit layout", edit.TitleFor(false), "off");
        Check.Equal("Done editing", edit.TitleFor(true), "on");
        Check.Equal("Settings", HeaderActions.Of(HeaderAction.Settings).TitleFor(true), "no on title");
    }

    private static void AppWide()
    {
        foreach (string t in new[] { "Settings…", "Keyboard shortcuts", "Full screen", "Leave full screen", "Fill tiles", "Hide diagnostics",
                     "Show sidebar", "New window", "Indicator corner", "Edit layout", "Done editing", "Save as layout…", "New layout…",
                     "Check for updates…", "Update to 1.2.3…", "Switch to 1.2.3…", "Find devices" })
        {
            Check.True(HeaderActions.IsAppWideTitle(t), $"\"{t}\" is app-wide");
        }
        foreach (string t in new[] { "Focus", "Back to the grid", "Listen", "Stop listening", "Start conversation", "End conversation",
                     "Pop Out Call", "Calls open in", "Allow talk-back", "Unmute on ring", "Reset zoom", "Quality", "Fisheye view",
                     "Fisheye mount", "Dismiss alert", "Split side by side", "Remove tile", "Show camera", "Layout preset" })
        {
            Check.True(!HeaderActions.IsAppWideTitle(t), $"\"{t}\" is a tile action");
        }
    }

    private static void Recording()
    {
        Check.True(RecordingIndicator.Text(0) is null, "hidden at zero");
        Check.True(RecordingIndicator.Text(-1) is null, "hidden below zero");
        Check.Equal("● 3 recording", RecordingIndicator.Text(3)!, "text");
    }

    private static void Sections()
    {
        Check.Equal("Devices, Cameras, Alerts & calls, Updates, About", string.Join(", ", SettingsSections.All.Select(s => s.Title())), "titles");
        Check.True(SettingsSections.All.All(s => s.Glyph().Length == 1), "one glyph each");
    }

    private static void Scaling()
    {
        PixelRect p = TitleBarRegions.ToPixels(new WindowRect(10.2, 4, 40.5, 30), 1.5);
        Check.Equal(new PixelRect(15, 6, 62, 45), p, "scaled 150% and rounded outward");
        Check.Equal(new PixelRect(10, 4, 41, 30), TitleBarRegions.ToPixels(new WindowRect(10, 4, 40.5, 30), 1), "100%");
        Check.Equal(new PixelRect(10, 4, 41, 30), TitleBarRegions.ToPixels(new WindowRect(10, 4, 40.5, 30), double.NaN), "an unusable scale reads as 100%");
    }

    private static void Passthrough()
    {
        IReadOnlyList<PixelRect> r = TitleBarRegions.Passthrough(
            [new WindowRect(10, 4, 40, 40), new WindowRect(-20, 0, 10, 40), new WindowRect(790, 4, 40, 40), new WindowRect(0, 0, double.NaN, 5), new WindowRect(100, 60, 20, 20)],
            1, 800, 48);
        Check.Equal(2, r.Count, "the off-screen, unusable, and below-caption ones are dropped");
        Check.Equal(new PixelRect(10, 4, 40, 40), r[0], "inside");
        Check.Equal(new PixelRect(790, 4, 10, 40), r[1], "clipped at the window's right edge");
    }

    private static void Height()
    {
        Check.Equal(48.0, TitleBarRegions.HeaderHeight(0, 2), "no report: the tall default");
        Check.Equal(32.0, TitleBarRegions.HeaderHeight(48, 1.5), "48 px at 150% is 32 units");
        Check.Equal(69.0, TitleBarRegions.ToUnits(138, 2), "the caption buttons' inset in units");
    }
}
