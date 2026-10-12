// When the header is on screen (docs/UX.md "Windows header"). In a normal
// window it is always there, as the title bar. In full screen it is hidden and
// slides over the stage like the collapsed sidebar does: resting the pointer at
// the window's top edge reveals it, and it slides away after the pointer leaves
// it, unless a menu, a drag, or text editing holds it. The timing is the
// sidebar's (SidebarReveal): this only adds the full screen switch. Pure state
// with the time passed in; tests drive it with a manual clock.

namespace NvrView.Shell;

/// <summary>Whether the header is visible, and the reveal in full screen.</summary>
public sealed class HeaderChrome
{
    private readonly SidebarReveal _reveal = new();

    public HeaderChrome()
    {
        // Nothing is revealed while a normal window shows the header in its title bar.
        _reveal.SetEnabled(false);
    }

    /// <summary>Whether the window is full screen (the header is then an overlay).</summary>
    public bool IsFullScreen { get; private set; }

    /// <summary>The header takes its row at the top of the window (a normal window).</summary>
    public bool IsDocked => !IsFullScreen;

    /// <summary>The header is on screen: always when docked, else while revealed.</summary>
    public bool IsVisible => IsDocked || _reveal.IsShown;

    /// <summary>The overlay is shown over the stage (full screen only).</summary>
    public bool IsOverlayShown => IsFullScreen && _reveal.IsShown;

    /// <summary>When <see cref="Tick"/> next has something to do; null when idle.</summary>
    public double? Deadline => _reveal.Deadline;

    /// <summary>Entering full screen hides the header at once; leaving docks it again.</summary>
    public void SetFullScreen(bool fullScreen)
    {
        IsFullScreen = fullScreen;
        _reveal.SetEnabled(fullScreen);
    }

    /// <summary>The pointer is in the top edge strip and/or over the header overlay.</summary>
    public void Pointer(bool inEdge, bool inHeader, double now) => _reveal.Pointer(inEdge, inHeader, now);

    /// <summary>Runs what is due at <paramref name="now"/>; true if <see cref="IsVisible"/> changed.</summary>
    public bool Tick(double now, RevealHolds holds = RevealHolds.None) => _reveal.Tick(now, holds);
}
