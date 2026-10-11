// Where a call is shown and where its own window opens (docs/UX.md "Doorbell calls"): the rules
// of the macOS app's CallPresentation, WindowRect, CallWindowPlacement and CallWindowQuality
// (NVRCore DoorbellCall.swift), with the same test vectors. Pure: the app measures the screens.

namespace NvrView.Tiles;

/// <summary>Where a call is shown: in the doorbell's inset in every viewer window, or in a call window of its own.</summary>
public enum CallPresentation
{
    Inset,
    Window,
}

public static class CallPresentations
{
    /// <summary>
    /// Decided when the call opens (a ring or Start Conversation): the doorbell's own choice if it
    /// has one, else the app's "Open calls in their own window". A press that joins an open call
    /// keeps its presentation.
    /// </summary>
    public static CallPresentation Resolve(bool global, CallPresentation? @override) =>
        @override ?? (global ? CallPresentation.Window : CallPresentation.Inset);
}

/// <summary>A rectangle in screen units with a top-left origin and y growing downward (Windows' convention).</summary>
public readonly record struct WindowRect(double X, double Y, double Width, double Height)
{
    public double MinX => X;

    public double MinY => Y;

    public double MaxX => X + Width;

    public double MaxY => Y + Height;

    public double MidX => X + Width / 2;

    /// <summary>Edges included.</summary>
    public bool Contains(double px, double py) => px >= MinX && px <= MaxX && py >= MinY && py <= MaxY;

    /// <summary>The same rectangle with y measured from the other edge of a space <paramref name="total"/> tall.</summary>
    public WindowRect Flipped(double total) => new(X, total - Y - Height, Width, Height);
}

/// <summary>Where a call window opens, over the screens' visible frames (the area without taskbar).</summary>
public static class CallWindowPlacement
{
    /// <summary>The "Open call windows" setting.</summary>
    public enum Mode
    {
        /// <summary>Where the user last left a call window (the default).</summary>
        Remembered,

        /// <summary>The top right of the screen holding the active viewer window.</summary>
        ActiveScreen,
    }

    public static string Title(this Mode mode) => mode == Mode.Remembered ? "Where I left it" : "Top right of the active screen";

    public const double DefaultWidth = 480;
    public const double DefaultHeight = 410;

    /// <summary>From the visible frame's edges, for the top right placement.</summary>
    public const double Margin = 12;

    /// <summary>Each further call window open at once moves this far.</summary>
    public const double Cascade = 24;

    /// <summary>Whether a remembered frame can still be used: some screen's visible frame holds the middle of its top edge (the title bar can be reached).</summary>
    public static bool IsOnScreen(WindowRect frame, IReadOnlyList<WindowRect> screens) =>
        screens.Any(s => s.Contains(frame.MidX, frame.MinY));

    /// <summary>
    /// The frame for a call window. <paramref name="remembered"/>: the last frame the user left a
    /// call window at (its size is kept in both modes); <paramref name="active"/>: the visible
    /// frame of the screen holding the key viewer window (null: the first screen);
    /// <paramref name="index"/>: how many call windows are open already.
    /// "Where I left it" with a remembered frame still on a screen: that frame; each further
    /// window moves 24 right and down, kept inside that screen. Otherwise (the other mode, no
    /// frame yet, or its screen is gone): the top right of the active screen, 12 in, each further
    /// window 24 left and down, the size limited to the screen.
    /// </summary>
    public static WindowRect Frame(Mode mode, WindowRect? remembered, IReadOnlyList<WindowRect> screens, WindowRect? active, int index = 0)
    {
        double step = Cascade * Math.Max(index, 0);
        if (mode == Mode.Remembered && remembered is { } r)
        {
            foreach (WindowRect s in screens)
            {
                if (s.Contains(r.MidX, r.MinY))
                {
                    return step > 0 ? Clamp(new WindowRect(r.X + step, r.Y + step, r.Width, r.Height), s) : r;
                }
            }
        }
        double width = remembered?.Width ?? DefaultWidth;
        double height = remembered?.Height ?? DefaultHeight;
        WindowRect? chosen = active ?? (screens.Count > 0 ? screens[0] : null);
        if (chosen is not { } screen)
        {
            return new WindowRect(step, step, width, height);
        }
        double w = Math.Min(width, Math.Max(screen.Width - 2 * Margin, 0));
        double h = Math.Min(height, Math.Max(screen.Height - 2 * Margin, 0));
        return Clamp(new WindowRect(screen.MaxX - Margin - w - step, screen.MinY + Margin + step, w, h), screen);
    }

    /// <summary>Moved (not resized) to lie inside <paramref name="screen"/> where it can.</summary>
    internal static WindowRect Clamp(WindowRect r, WindowRect screen) => new(
        Math.Max(screen.MinX, Math.Min(r.X, screen.MaxX - r.Width)),
        Math.Max(screen.MinY, Math.Min(r.Y, screen.MaxY - r.Height)),
        r.Width, r.Height);
}

/// <summary>
/// The call window's stream: Auto, the main stream while the picture is drawn wider than the
/// sub-stream's pictures, the sub-stream otherwise.
/// </summary>
public static class CallWindowQuality
{
    /// <summary>The sub-stream's width in pixels assumed until one has been seen.</summary>
    public const double AssumedSubWidth = 640;

    /// <summary>Back to the sub-stream only below this share of its width, so a window resized around the threshold does not switch back and forth.</summary>
    public const double DownShare = 0.8;

    /// <summary>Whether the window asks for the main stream, drawing its picture <paramref name="drawnWidth"/> pixels wide; <paramref name="subWidth"/> is the sub-stream's picture width when known, <paramref name="wasMain"/> what it asked for last.</summary>
    public static bool WantsMain(double drawnWidth, double? subWidth, bool wasMain)
    {
        double sub = subWidth is > 0 ? subWidth.Value : AssumedSubWidth;
        return wasMain ? drawnWidth >= sub * DownShare : drawnWidth > sub;
    }
}
