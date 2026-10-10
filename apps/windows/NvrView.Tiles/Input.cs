// Keys the tile features act on, and the full screen chrome's idle timer
// (docs/UX.md "Keyboard", "Full screen chrome"). Pure, so both are tested
// without a window.

namespace NvrView.Tiles;

/// <summary>A key, as far as the tile features care.</summary>
public enum TileKey
{
    Other,
    Enter,
    Space,
    M,
    L,
}

/// <summary>What a key does to the selected tile.</summary>
public enum TileAction
{
    None,
    Dismiss,
    ToggleFocus,
    ToggleListening,
}

public static class TileKeyMap
{
    /// <summary>
    /// Enter dismisses the selected tile's alert if it has one, and otherwise toggles focus, as
    /// Space does; M (or Ctrl+L) listens to the tile or mutes it. Other modifiers make a key
    /// something else (a menu shortcut), so it is not mapped; text being edited takes every key.
    /// </summary>
    public static TileAction Map(TileKey key, bool ctrl, bool alt, bool shift, bool hasAlert, bool editingText = false)
    {
        if (editingText || alt)
        {
            return TileAction.None;
        }
        return key switch
        {
            TileKey.Enter when !ctrl && !shift => hasAlert ? TileAction.Dismiss : TileAction.ToggleFocus,
            TileKey.Space when !ctrl && !shift => TileAction.ToggleFocus,
            TileKey.M when !ctrl => TileAction.ToggleListening,
            TileKey.L when ctrl => TileAction.ToggleListening,
            _ => TileAction.None,
        };
    }
}

/// <summary>
/// The hover chrome's idle timer: headers (and, in full screen, the pointer)
/// hide <see cref="HideAfterSeconds"/> after the last interaction and return on the next.
/// Alert headers and corner indicators are not part of it.
/// </summary>
public sealed class ChromeIdle(IViewerClock clock)
{
    public const double HideAfterSeconds = 3;

    private double _last = double.NegativeInfinity;

    /// <summary>Pointer movement, a click, a key, or a scroll.</summary>
    public void Interact() => _last = clock.Now;

    public double IdleSeconds => clock.Now - _last;

    /// <summary>Whether the hover chrome shows (also out of full screen: headers fade the same way).</summary>
    public bool ChromeShown => IdleSeconds < HideAfterSeconds;

    /// <summary>Whether the pointer hides: only in full screen, with the chrome.</summary>
    public bool PointerHidden(bool fullScreen) => fullScreen && !ChromeShown;
}
