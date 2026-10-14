// The Windows header (docs/UX.md "Windows header"): what its zones hold, which
// words an app-wide action goes by (so the tile menu can be checked for
// leftovers), the reserved recording indicator, the Settings sections, and the
// arithmetic that turns the header's controls into the pixel rectangles the
// system needs for its title bar. Pure; no window or XAML types.

namespace NvrView.Tiles;

/// <summary>An action of the whole app (not of one tile), reachable from the header.</summary>
public enum HeaderAction
{
    FindDevices,
    EditLayout,
    FillTiles,
    FullScreen,
    Settings,
    NewLayout,
    NewLayoutFromTemplate,
    SaveAsLayout,
    NewWindow,
    ToggleSidebar,
    Recordings,
    StopAllRecordings,
    OpenRecordingsFolder,
    ShowFrameRates,
    Diagnostics,
    IndicatorCorner,
    KeyboardShortcuts,
    CheckForUpdates,
}

/// <summary>Where the header offers an action.</summary>
public enum HeaderZone
{
    /// <summary>The layout picker's menu (left).</summary>
    LayoutPicker,

    /// <summary>A button in the middle.</summary>
    Quick,

    /// <summary>A button on the right.</summary>
    Right,

    /// <summary>The "…" menu on the right.</summary>
    Overflow,
}

/// <summary>One action's place in the header.</summary>
public sealed record HeaderItem(HeaderAction Action, HeaderZone Zone, string Title, string? OnTitle = null, string? Keys = null)
{
    /// <summary>The words for the current state: <see cref="OnTitle"/> while a toggle is on.</summary>
    public string TitleFor(bool on) => on && OnTitle is not null ? OnTitle : Title;
}

/// <summary>The header's catalog: every app-wide action, once, in the order the header shows it.</summary>
public static class HeaderActions
{
    public static readonly IReadOnlyList<HeaderItem> All =
    [
        new(HeaderAction.NewLayout, HeaderZone.LayoutPicker, "New layout…"),
        new(HeaderAction.NewLayoutFromTemplate, HeaderZone.LayoutPicker, "New layout from template"),
        new(HeaderAction.SaveAsLayout, HeaderZone.LayoutPicker, "Save as layout…"),
        new(HeaderAction.FindDevices, HeaderZone.Quick, "Find devices"),
        new(HeaderAction.EditLayout, HeaderZone.Quick, "Edit layout", "Done editing", "Ctrl+E"),
        new(HeaderAction.FillTiles, HeaderZone.Quick, "Fill tiles", "Fit tiles"),
        new(HeaderAction.FullScreen, HeaderZone.Quick, "Full screen", "Leave full screen", "F"),
        new(HeaderAction.Settings, HeaderZone.Right, "Settings", null, "Ctrl+,"),
        new(HeaderAction.NewWindow, HeaderZone.Overflow, "New window", null, "Ctrl+N"),
        new(HeaderAction.Recordings, HeaderZone.Overflow, "Recordings", null, "Ctrl+Alt+R"),
        new(HeaderAction.StopAllRecordings, HeaderZone.Overflow, "Stop all recordings"),
        new(HeaderAction.OpenRecordingsFolder, HeaderZone.Overflow, "Open recordings folder"),
        new(HeaderAction.ToggleSidebar, HeaderZone.Overflow, "Show sidebar", "Hide sidebar", "S"),
        new(HeaderAction.IndicatorCorner, HeaderZone.Overflow, "Indicator corner"),
        new(HeaderAction.ShowFrameRates, HeaderZone.Overflow, "Show frame rates", "Hide frame rates"),
        new(HeaderAction.Diagnostics, HeaderZone.Overflow, "Show diagnostics", "Hide diagnostics", "Ctrl+D"),
        new(HeaderAction.KeyboardShortcuts, HeaderZone.Overflow, "Keyboard shortcuts", null, "F1"),
        new(HeaderAction.CheckForUpdates, HeaderZone.Overflow, "Check for updates…"),
    ];

    public static HeaderItem Of(HeaderAction action) => All.First(i => i.Action == action);

    public static IEnumerable<HeaderItem> In(HeaderZone zone) => All.Where(i => i.Zone == zone);

    /// <summary>
    /// Whether a menu entry's text is one of the app-wide actions (in any state, or an update entry).
    /// The tile menu must hold none: those live in the header.
    /// </summary>
    public static bool IsAppWideTitle(string text)
    {
        string plain = text.TrimEnd('…', '.', ' ');
        foreach (HeaderItem item in All)
        {
            foreach (string title in new[] { item.Title, item.OnTitle ?? item.Title })
            {
                if (string.Equals(plain, title.TrimEnd('…', '.', ' '), StringComparison.OrdinalIgnoreCase))
                {
                    return true;
                }
            }
        }
        return text.StartsWith("Update to ", StringComparison.Ordinal) || text.StartsWith("Switch to ", StringComparison.Ordinal);
    }
}

/// <summary>The "● N recording" spot of the header: hidden until something records.</summary>
public static class RecordingIndicator
{
    /// <summary>The indicator's words; null (hidden) when nothing records.</summary>
    public static string? Text(int recording) => recording > 0 ? $"● {recording} recording" : null;

    /// <summary>
    /// Where a tile's "● REC" pill sits (docs/UX.md "Recording"): the corner opposite the corner
    /// indicators, except that the top right (where the header's buttons are) becomes the top left.
    /// </summary>
    public static ChromeCorner PillCorner(ChromeCorner indicators)
    {
        ChromeCorner opposite = indicators.Opposite();
        return opposite == ChromeCorner.TopRight ? ChromeCorner.TopLeft : opposite;
    }
}

/// <summary>The sections of the Settings window, in the order of its pane.</summary>
public enum SettingsSection
{
    Devices,
    Cameras,
    AlertsAndCalls,
    Capture,
    Updates,
    About,
}

public static class SettingsSections
{
    public static IReadOnlyList<SettingsSection> All { get; } = Enum.GetValues<SettingsSection>();

    public static string Title(this SettingsSection s) => s switch
    {
        SettingsSection.Devices => "Devices",
        SettingsSection.Cameras => "Cameras",
        SettingsSection.AlertsAndCalls => "Alerts & calls",
        SettingsSection.Capture => "Capture",
        SettingsSection.Updates => "Updates",
        _ => "About",
    };

    /// <summary>A Segoe Fluent Icons glyph for the pane.</summary>
    public static string Glyph(this SettingsSection s) => s switch
    {
        SettingsSection.Devices => "",
        SettingsSection.Cameras => "",
        SettingsSection.AlertsAndCalls => "",
        SettingsSection.Capture => "\uE722",
        SettingsSection.Updates => "",
        _ => "",
    };
}

/// <summary>A rectangle in whole physical pixels.</summary>
public readonly record struct PixelRect(int X, int Y, int Width, int Height);

/// <summary>Turns the header's layout (device-independent units) into what the title bar APIs take (pixels).</summary>
public static class TitleBarRegions
{
    /// <summary>The standard tall caption height in device-independent units.</summary>
    public const double TallHeight = 48;

    /// <summary>A rectangle in units to pixels at <paramref name="scale"/>, rounded outward so it covers the control.</summary>
    public static PixelRect ToPixels(WindowRect r, double scale)
    {
        scale = Sanitize(scale);
        int left = (int)Math.Floor(r.MinX * scale);
        int top = (int)Math.Floor(r.MinY * scale);
        int right = (int)Math.Ceiling(r.MaxX * scale);
        int bottom = (int)Math.Ceiling(r.MaxY * scale);
        return new PixelRect(left, top, right - left, bottom - top);
    }

    /// <summary>
    /// The regions of the header that must receive the pointer (buttons, the picker) instead of dragging the
    /// window, in pixels and clipped to the window's width and the caption's height; empty ones are dropped.
    /// </summary>
    public static IReadOnlyList<PixelRect> Passthrough(IEnumerable<WindowRect> interactive, double scale, int windowWidth, int captionHeight)
    {
        var result = new List<PixelRect>();
        foreach (WindowRect r in interactive)
        {
            if (!double.IsFinite(r.X) || !double.IsFinite(r.Y) || !double.IsFinite(r.Width) || !double.IsFinite(r.Height))
            {
                continue;
            }
            PixelRect p = ToPixels(r, scale);
            int left = Math.Max(p.X, 0);
            int top = Math.Max(p.Y, 0);
            int right = Math.Min(p.X + p.Width, windowWidth);
            int bottom = Math.Min(p.Y + p.Height, captionHeight);
            if (right > left && bottom > top)
            {
                result.Add(new PixelRect(left, top, right - left, bottom - top));
            }
        }
        return result;
    }

    /// <summary>A length in pixels in device-independent units (the caption buttons' inset, the caption height).</summary>
    public static double ToUnits(int pixels, double scale) => pixels / Sanitize(scale);

    /// <summary>The header's height in units: the system's caption height when it reports one, else the tall default.</summary>
    public static double HeaderHeight(int captionPixels, double scale) =>
        captionPixels > 0 ? ToUnits(captionPixels, scale) : TallHeight;

    private static double Sanitize(double scale) => double.IsFinite(scale) && scale > 0 ? scale : 1;
}
