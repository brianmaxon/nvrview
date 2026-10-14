// The window's alert insets (docs/UX.md "Alert insets", "Doorbell calls"): what happens on a camera
// the window is not showing, a doorbell's call, a smoke or CO alarm, a person at an entrance, appears
// in an inset over the stage. The rules are InsetBoard (NvrView.Tiles/AlertInsets.cs, the same as
// the macOS app's, with the same test vectors); this connects one board per window to the viewer:
// detection events in, the open calls followed (a call's inset lives as long as the call), the
// swap with the stage, Tab and Esc, the grid, and the clock. The cards, the leases and the pictures
// are CallStack (CallPanel.cs). The call window's tile has no insets of its own.

using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using NvrView.Tiles;
using Windows.System;
using Windows.UI.Core;

namespace NvrView.App;

internal sealed partial class StageView
{
    private InsetBoard _board = new();
    private InsetGridSetting? _gridOverride;
    private Border? _takeoverCover;
    private TextBlock? _takeoverText;

    /// <summary>The window's insets.</summary>
    public InsetBoard Board => _board;

    /// <summary>
    /// Starts the board over, with nothing on it (the self-test: the board's times are the viewer clock's, which the steps
    /// replace by clocks of their own; open calls come back on it at once).
    /// </summary>
    internal void ResetInsets()
    {
        _board = new InsetBoard();
        _calls.Sync();
    }

    /// <summary>How many insets this window draws: its own grid, else the Settings default.</summary>
    public InsetGridSetting InsetGrid => _gridOverride ?? _viewer.Settings.InsetGrid;

    /// <summary>This window's own grid, null while it follows the Settings default (saved with the window).</summary>
    public InsetGridSetting? InsetGridOverride => _gridOverride;

    /// <summary>The window's own grid was chosen or dropped (the window saves its record).</summary>
    public event Action? InsetGridChanged;

    /// <summary>A grid restored with the window (not a change to save).</summary>
    public void RestoreInsetGrid(InsetGridSetting? grid)
    {
        _gridOverride = grid;
        _calls.Sync();
    }

    /// <summary>This window's own rows and/or columns (the other keeps its value).</summary>
    public void SetInsetGrid(int? rows, int? columns)
    {
        InsetGridSetting current = InsetGrid;
        _gridOverride = new InsetGridSetting(rows ?? current.Rows, columns ?? current.Columns);
        InsetGridChanged?.Invoke();
        _calls.Sync();
        _calls.InvalidateArrange();
    }

    /// <summary>Back to the Settings default for this window.</summary>
    public void UseDefaultInsetGrid()
    {
        if (_gridOverride is null)
        {
            return;
        }
        _gridOverride = null;
        InsetGridChanged?.Invoke();
        _calls.Sync();
        _calls.InvalidateArrange();
    }

    // The Settings changed (the grid, the corner): the board's capacity and the column follow.
    private void OnInsetSettingsChanged()
    {
        _calls.Sync();
        _calls.InvalidateArrange();
    }

    /// <summary>The inset menu's "Inset Grid" items: rows, columns, and back to the Settings default.</summary>
    internal void FillGridMenu(MenuFlyoutSubItem parent)
    {
        parent.Items.Clear();
        InsetGridSetting current = InsetGrid;
        var rows = new MenuFlyoutSubItem { Text = "Rows" };
        for (int r = InsetGridSetting.MinRows; r <= InsetGridSetting.MaxRows; r++)
        {
            int value = r;
            var item = new ToggleMenuFlyoutItem { Text = value.ToString(System.Globalization.CultureInfo.InvariantCulture), IsChecked = value == current.Rows };
            item.Click += (_, _) => SetInsetGrid(value, null);
            rows.Items.Add(item);
        }
        var columns = new MenuFlyoutSubItem { Text = "Columns" };
        for (int c = InsetGridSetting.MinColumns; c <= InsetGridSetting.MaxColumns; c++)
        {
            int value = c;
            var item = new ToggleMenuFlyoutItem { Text = value.ToString(System.Globalization.CultureInfo.InvariantCulture), IsChecked = value == current.Columns };
            item.Click += (_, _) => SetInsetGrid(null, value);
            columns.Items.Add(item);
        }
        parent.Items.Add(rows);
        parent.Items.Add(columns);
        if (_gridOverride is not null)
        {
            var settings = new MenuFlyoutItem { Text = "Use the Settings Default" };
            settings.Click += (_, _) => UseDefaultInsetGrid();
            parent.Items.Add(settings);
        }
    }

    // ---- what the window shows ----

    // The camera the focused tile shows (while it is focused or animating into focus).
    private CameraRef? FocusedCamera => Renderer?.Focus is { Entering: true } f ? f.Tile.CameraOrNone : null;

    private static bool SameChannel(CameraRef? a, CameraRef b) => a is { } x && x.Device == b.Device && x.Channel == b.Channel;

    /// <summary>Whether the stage shows the camera now: the swapped-in camera, else the focused tile, else any tile of the layout.</summary>
    public bool ShowsCamera(CameraRef camera)
    {
        if (_board.Swap is { } s)
        {
            return s.Shown == camera;
        }
        return FocusedCamera is { } f ? f == camera : State.Tree.Cameras.Contains(camera);
    }

    /// <summary>The same for any lens of a channel (a sound alarm belongs to the channel's microphone).</summary>
    public bool ShowsChannel(CameraRef camera)
    {
        if (_board.Swap is { } s)
        {
            return SameChannel(s.Shown, camera);
        }
        return FocusedCamera is { } f ? SameChannel(f, camera) : State.Tree.CameraList.Any(c => SameChannel(c, camera));
    }

    /// <summary>The stage shows a layout: several tiles, none focused, no swap. A swap then keeps the whole layout in the return inset; a focused or single tile keeps just its camera.</summary>
    public bool StageShowsLayout => _board.Swap is null && FocusedCamera is null && State.Tree.LeafCount > 1;

    /// <summary>What the return inset of a layout is called.</summary>
    public string LayoutName
    {
        get
        {
            int n = State.Tree.Cameras.Count;
            return $"Layout, {n} camera{(n == 1 ? "" : "s")}";
        }
    }

    // The camera the stage shows, which a swap puts into the return inset.
    private CameraRef? DisplacedCamera => FocusedCamera ?? Selected?.CameraOrNone ?? State.Tree.CameraList.FirstOrDefault(c => c is not null);

    // ---- events ----

    // A detection event: a smoke or CO alarm, or a person at an entrance, for a camera the window does not show, opens an inset.
    // A doorbell press and Start Conversation are calls: their insets follow the call (FollowCalls).
    private void OnDetection(DetectionEvent e, DateTime at)
    {
        CameraRef camera = e.Source == DetectionSource.LocalAudio ? e.Camera.ChannelKey : e.Camera;
        if (InsetTrigger.For(e, _viewer.Settings.Entrance(camera)) is not { } kind || kind.IsCall)
        {
            return;
        }
        double now = _viewer.Clock.Now;
        if (e.Active)
        {
            // A call's inset on the same camera keeps its controls; a smoke alarm still outranks it.
            if (_board.ItemFor(camera) is { Kind.IsCall: true } && !kind.IsCritical)
            {
                return;
            }
            bool visible = kind.IsCritical ? ShowsChannel(camera) : ShowsCamera(camera);
            _board.Started(kind, camera, visible, now, at);
        }
        else
        {
            _board.Stopped(kind, camera, now);
        }
        _calls.Sync();
    }

    // ---- the calls ----

    // Gives each open call an item on the board (detached for a call in its own window, so Pop In shows it here as well;
    // none while the doorbell fills the stage: its controls show over the stage), follows its presentation, drops the
    // items of calls that ended, and counts the call's activity as use of its inset. The item counts as ended, so it
    // closes a minute after the call's last activity (or ends the call, which expires then too).
    private void FollowCalls()
    {
        double now = _viewer.Clock.Now;
        CameraRef? filling = FillingCamera;
        foreach (CameraRef camera in _viewer.Calls.Cameras)
        {
            if (_viewer.Calls.CallFor(camera) is not { } call)
            {
                continue;
            }
            bool detached = call.Presentation == CallPresentation.Window;
            bool fills = SameChannel(filling, camera) && !detached;
            InsetItem? item = _board.ItemFor(camera);
            if (item is null)
            {
                if (!fills || detached)
                {
                    InsetKind kind = call.Origin == CallOrigin.Press ? InsetKind.Doorbell : InsetKind.Conversation;
                    _board.Started(kind, camera, false, now, call.StartedAt, detached);
                    _board.Stopped(kind, camera, now);
                }
            }
            else if (item.Kind.IsCall)
            {
                bool swapped = _board.Swap?.Shown == camera;
                if (item.Detached != detached)
                {
                    if (!detached && fills && !swapped)
                    {
                        // Popped in while the doorbell fills this stage: its controls show over the stage, as for a press then.
                        _board.Remove(i => i.Camera == camera && !i.IsReturn);
                    }
                    else
                    {
                        _board.SetDetached(camera, detached, now);
                    }
                }
                else if (!detached && fills && !swapped)
                {
                    // The doorbell was focused: its controls move over the stage and the inset goes.
                    _board.Remove(i => i.Camera == camera && !i.IsReturn);
                }
            }
        }
        // The items of calls that ended.
        _board.Remove(i => i.Kind.IsCall && !i.IsReturn && _viewer.Calls.CallFor(i.Camera) is null);
        foreach (CameraRef camera in _viewer.Calls.Cameras)
        {
            if (_viewer.Calls.CallFor(camera) is { } call && _board.ItemFor(camera) is { Kind.IsCall: true })
            {
                // Speech, a press, a reply: the call's activity; talking holds it open.
                _board.Use(camera, call.Talk.IsTalking ? now : call.LastActivity);
            }
        }
    }

    /// <summary>
    /// After any change of the board's inputs (called by the cards' sync): the capacity, the calls, the alerts
    /// dismissed elsewhere (another window's tile header), devices that went away.
    /// </summary>
    internal void SyncInsetBoard()
    {
        if (FixedTile)
        {
            return;
        }
        _board.SetCapacity(InsetGrid.Capacity, _viewer.Clock.Now);
        FollowCalls();
        _board.DropSoundAlarms(c => !_viewer.Alerts.IsActive(c));
        _board.Remove(i => _viewer.FindDevice(i.Camera.Device) is null);
    }

    // ---- actions ----

    /// <summary>A click on an inset (or Tab): its camera takes the stage; the camera that was there goes into the return inset. A click on the return inset swaps back.</summary>
    internal void SwapInset(CameraRef camera)
    {
        _board.SwapIn(camera, DisplacedCamera, _viewer.Clock.Now, StageShowsLayout);
        // Clicking a call's inset is using the call.
        _viewer.Calls.Use(camera);
        _calls.Sync();
    }

    /// <summary>Esc or the close control: the inset goes. Dismissing a smoke or CO inset dismisses its alert too; closing a doorbell's inset ends its call.</summary>
    internal void DismissInset(CameraRef camera)
    {
        if (_board.Dismiss(camera) is { } item)
        {
            if (item.Kind.IsCritical)
            {
                _viewer.DismissAlert(item.Camera);
            }
            if (item.Kind.IsCall)
            {
                _viewer.Calls.End(item.Camera, "closed");
            }
        }
        _calls.Sync();
    }

    /// <summary>The close control of a card: the return inset swaps back, an inset is dismissed, and a call over the stage ends.</summary>
    internal void CloseInset(CameraRef camera)
    {
        if (_board.Items.Any(i => i.Camera == camera && i.IsReturn))
        {
            _board.SwapBack(_viewer.Clock.Now);
            _calls.Sync();
        }
        else if (_board.ItemFor(camera) is null)
        {
            _viewer.Calls.End(camera, "closed");
            _calls.Sync();
        }
        else
        {
            DismissInset(camera);
        }
    }

    /// <summary>A waiting camera was chosen from the "+N more" chip.</summary>
    internal void BringInInset(CameraRef camera)
    {
        if (_board.BringIn(camera, _viewer.Clock.Now))
        {
            _calls.Sync();
        }
    }

    /// <summary>The pointer entered or left an inset: hovering holds it open.</summary>
    internal void InsetHover(CameraRef camera, bool inside) => _board.Hover(camera, inside, _viewer.Clock.Now);

    /// <summary>
    /// Tab and Esc while the stage has the keyboard: true when an inset used the key (otherwise it goes on as
    /// before). Tab swaps the top inset with the stage (and back); Esc dismisses the swapped-in camera, else the top inset.
    /// </summary>
    internal bool HandleInsetKey(VirtualKey key, bool ctrl, bool repeat)
    {
        if (FixedTile || repeat || _board.IsEmpty)
        {
            return false;
        }
        static bool Down(VirtualKey k) => Microsoft.UI.Input.InputKeyboardSource.GetKeyStateForCurrentThread(k).HasFlag(CoreVirtualKeyStates.Down);
        switch (key)
        {
            case VirtualKey.Tab when !ctrl && !Down(VirtualKey.Shift) && !Down(VirtualKey.Menu):
                {
                    CameraRef? top = _board.Swap?.Shown ?? (_board.Shown.Count > 0 ? _board.Shown[0].Camera : null);
                    _board.SwapTop(DisplacedCamera, _viewer.Clock.Now, StageShowsLayout);
                    if (top is { } camera)
                    {
                        _viewer.Calls.Use(camera);
                    }
                    _calls.Sync();
                    return true;
                }
            case VirtualKey.Escape when !ctrl:
                if (_board.Swap is { } s)
                {
                    DismissInset(s.Shown);
                    return true;
                }
                if (_board.Shown.Count > 0)
                {
                    InsetItem top = _board.Shown[0];
                    if (top.IsReturn)
                    {
                        _board.SwapBack(null);
                        _calls.Sync();
                    }
                    else
                    {
                        DismissInset(top.Camera);
                    }
                    return true;
                }
                return false;
            default:
                return false;
        }
    }

    // ---- time ----

    // From the stage's slow tick: insets close on the viewer clock (a call's activity counts as using its inset), and a waiting
    // inset takes the place of an older one once that has had its ten seconds.
    private void TickInsets()
    {
        if (FixedTile || _board.IsEmpty)
        {
            return;
        }
        double now = _viewer.Clock.Now;
        FollowCalls();
        if (_board.Deadline is not { } deadline || now < deadline)
        {
            return;
        }
        // Closing, or a waiting inset taking the place of an older one.
        foreach (InsetItem closed in _board.Tick(now))
        {
            if (closed.Kind.IsCall)
            {
                _viewer.Calls.End(closed.Camera, "no activity");
            }
        }
        _calls.Sync();
    }

    // ---- the swapped-in camera over the stage ----

    // The cover over the stage while a camera is swapped in: its header with the close control, and it keeps the
    // pointer from reaching the layout under it (nothing of the layout is changed by the swap).
    private Border BuildTakeoverCover()
    {
        _takeoverText = new TextBlock
        {
            Foreground = new SolidColorBrush(Colors.White),
            FontSize = 12,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            TextTrimming = TextTrimming.CharacterEllipsis,
            VerticalAlignment = VerticalAlignment.Center,
        };
        var close = new Button { Content = "✕", Padding = new Thickness(8, 2, 8, 2), MinWidth = 0, MinHeight = 0 };
        AutomationProperties.SetName(close, "Close the swapped camera");
        ToolTipService.SetToolTip(close, "Close (Esc)");
        close.Click += (_, _) =>
        {
            if (_board.Swap is { } s)
            {
                CloseInset(s.Shown);
            }
        };
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.Children.Add(_takeoverText);
        Grid.SetColumn(close, 1);
        row.Children.Add(close);
        var banner = new Border
        {
            Background = new SolidColorBrush(Windows.UI.Color.FromArgb(0xD9, 0x1A, 0x1A, 0x1E)),
            CornerRadius = new CornerRadius(6),
            Padding = new Thickness(10, 4, 6, 4),
            Margin = new Thickness(12),
            MaxWidth = 620,
            HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Top,
            Child = row,
        };
        _takeoverCover = new Border { Background = new SolidColorBrush(Colors.Transparent), Visibility = Visibility.Collapsed, Child = banner };
        AutomationProperties.SetName(_takeoverCover, "Swapped camera");
        _takeoverCover.PointerPressed += (_, e) => e.Handled = true;
        _takeoverCover.PointerReleased += (_, e) => e.Handled = true;
        _takeoverCover.DoubleTapped += (_, e) => e.Handled = true;
        _takeoverCover.RightTapped += (_, e) => e.Handled = true;
        _takeoverCover.PointerWheelChanged += (_, e) => e.Handled = true;
        return _takeoverCover;
    }

    /// <summary>The swapped-in camera's header over the stage, and the layout's chrome out of the way while it covers it (called by the cards' sync).</summary>
    internal void UpdateTakeover()
    {
        if (_takeoverCover is null || _takeoverText is null)
        {
            return;
        }
        InsetBoard.SwapState? swap = _board.Swap;
        _takeoverCover.Visibility = swap is null ? Visibility.Collapsed : Visibility.Visible;
        _chromeLayer.Visibility = swap is null ? Visibility.Visible : Visibility.Collapsed;
        if (swap is not null)
        {
            _takeoverText.Text = $"{swap.Item.Header(_viewer.WallClock())} · {_viewer.CameraName(swap.Shown)} · click the inset or Tab to swap back";
        }
    }

    /// <summary>The header over the stage while a camera is swapped in, else empty (self-test).</summary>
    public string TakeoverText => _takeoverCover?.Visibility == Visibility.Visible ? _takeoverText?.Text ?? "" : "";
}
