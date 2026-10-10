// When the collapsed sidebar slides over the stage and when it leaves again
// (docs/UX.md "Sidebar"): resting the pointer at the window's left edge for
// about 150 ms reveals it as an overlay; it slides away about half a second
// after the pointer leaves it, unless a drag, an open menu, or text editing
// holds it. Pure state with the time passed in: the window arms one timer for
// Deadline and calls Tick when it fires, and tests drive it with a manual
// clock (NvrView.Tiles.ManualViewerClock; no real waiting).

using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>Things that keep the overlay up although the pointer is outside it.</summary>
[Flags]
public enum RevealHolds
{
    None = 0,

    /// <summary>A press or drag from the overlay is in progress.</summary>
    Drag = 1,

    /// <summary>A context menu is open.</summary>
    Menu = 2,

    /// <summary>A text box in the overlay is being edited.</summary>
    Editing = 4,
}

/// <summary>The reveal state machine (see the file comment).</summary>
public sealed class SidebarReveal
{
    private double? _dwellDeadline;
    private double? _hideDeadline;

    /// <summary>How long the pointer rests at the edge before the overlay slides in.</summary>
    public double Dwell { get; init; } = 0.15;

    /// <summary>How long after the pointer leaves the overlay slides away.</summary>
    public double HideDelay { get; init; } = 0.5;

    /// <summary>How soon to look again while a hold keeps the overlay up.</summary>
    public double Recheck { get; init; } = 0.25;

    /// <summary>Whether the overlay is visible (or sliding in).</summary>
    public bool IsShown { get; private set; }

    /// <summary>False while the sidebar is docked: nothing is revealed then.</summary>
    public bool IsEnabled { get; private set; } = true;

    /// <summary>When <see cref="Tick"/> next has something to do; null when idle.</summary>
    public double? Deadline => (_dwellDeadline, _hideDeadline) switch
    {
        (null, null) => null,
        ({ } d, null) => d,
        (null, { } h) => h,
        ({ } d, { } h) => Math.Min(d, h),
    };

    /// <summary>The sidebar was docked (false) or collapsed (true). Docking hides the overlay at once.</summary>
    public void SetEnabled(bool enabled)
    {
        IsEnabled = enabled;
        if (!enabled)
        {
            IsShown = false;
            _dwellDeadline = null;
            _hideDeadline = null;
        }
    }

    /// <summary>The pointer is now in the edge strip and/or over the overlay. Call on every enter, exit, and move.</summary>
    public void Pointer(bool inEdge, bool inOverlay, double now)
    {
        if (!IsEnabled)
        {
            return;
        }
        if (IsShown)
        {
            if (inEdge || inOverlay)
            {
                _hideDeadline = null;
            }
            else
            {
                _hideDeadline ??= now + HideDelay;
            }
        }
        else if (inEdge)
        {
            _dwellDeadline ??= now + Dwell;
        }
        else
        {
            _dwellDeadline = null;
        }
    }

    /// <summary>Runs what is due at <paramref name="now"/>; true if <see cref="IsShown"/> changed.</summary>
    public bool Tick(double now, RevealHolds holds = RevealHolds.None)
    {
        if (!IsEnabled)
        {
            return false;
        }
        if (_dwellDeadline is { } d && now >= d)
        {
            _dwellDeadline = null;
            if (!IsShown)
            {
                IsShown = true;
                return true;
            }
        }
        if (IsShown && _hideDeadline is { } h && now >= h)
        {
            if (holds == RevealHolds.None)
            {
                _hideDeadline = null;
                IsShown = false;
                return true;
            }
            _hideDeadline = now + Recheck;
        }
        return false;
    }
}
