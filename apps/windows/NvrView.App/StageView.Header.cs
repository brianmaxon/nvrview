// The tile header's actions (docs/UX.md "Tile header", docs/CAPTURE.md "The header model"): one
// place that turns a TileHeaderItem into what it does, for the header's buttons, the "..." menu,
// the right-click menu, and the keys alike. The menus are built from TileHeaderLayout.Menu, so they
// have the header's order and wording (the Mac app's StageView+Header.swift does the same).

using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.System;
using Windows.UI.Core;

namespace NvrView.App;

/// <summary>The header items whose action lands later: their buttons, menu items and keys are in place and say so.</summary>
internal static class TileActionStub
{
    /// <summary>Every item is built now (Save Picture, recording, PTZ); the hook stays for an item that lands later.</summary>
    public static bool IsPending(TileHeaderItem item) => false;

    /// <summary>"Save Picture is coming soon".</summary>
    public static string Message(string label) => $"{label.TrimEnd('…')} is coming soon";
}

internal sealed partial class StageView
{
    /// <summary>How long a short message stays over the stage, on the viewer's clock.</summary>
    public const double ToastSeconds = 2.5;

    private double? _toastHideAt;
    private Action? _toastAction;

    /// <summary>What the header model needs to know about a tile: its camera's capabilities (the channels event) and its state. Null for an empty tile.</summary>
    internal TileHeaderTile? HeaderTileOf(StageTile t)
    {
        if (t.CameraOrNone is not { } camera)
        {
            return null;
        }
        ChannelInfo? info = _viewer.ChannelOf(camera);
        PtzCaps ptz = info?.PtzOf(camera.Lens) ?? default;
        Call? call = _viewer.Calls.CallFor(camera);
        bool dewarped = t.Fisheye is { } fe && fe.Mode.Dewarps();
        bool moved = dewarped && t.FisheyeSetting.Aim is not null;
        bool zoomed = !dewarped && t.Zoom > 1.0001f && t.PictureWidth > 0 && t.PictureHeight > 0 && t.ViewWidth > 0 && t.ViewHeight > 0;
        return new TileHeaderTile
        {
            Audio = true,
            Talk = info?.TalkOf(camera.Lens) == true,
            Doorbell = info?.Doorbell == true,
            PanTilt = ptz.PanTilt,
            Presets = ptz.Presets,
            Lens = ptz.ZoomFocus,
            Fisheye = _viewer.IsFisheye(camera),
            // A status over a picture that is still there ("Recovering" while a stream restarts) does not take the buttons away.
            Picture = StatusOf(t).Kind == TileMessage.None,
            Recording = FixedTile ? _viewer.Capture.CallRecording(camera) is not null : _viewer.Capture.IsRecorded(camera),
            Zoomed = zoomed || moved,
            Panned = !zoomed && !dewarped && Fills(t) && t.DigitalZoom.IsPanned,
            Alert = _viewer.Alerts.IsActive(camera),
            Listening = t.Audible,
            Conversation = call is not null,
            Call = FixedTile ? TileHeaderCall.Window : call is null ? TileHeaderCall.None : TileHeaderCall.Inset,
        };
    }

    // ---- what an item does ----

    /// <summary>A header button was clicked (<paramref name="from"/> is the button, under which a menu opens).</summary>
    private void HeaderItemClicked(StageTile t, TileHeaderItem item, FrameworkElement? from)
    {
        PerformHeaderItem(t, item, from);
    }

    /// <summary>Does what an item does for a tile; a key or a menu item passes no button.</summary>
    public void PerformHeaderItem(StageTile t, TileHeaderItem item, FrameworkElement? from)
    {
        if (t.CameraOrNone is not { } camera || HeaderTileOf(t) is not { } tile)
        {
            return;
        }
        Interact();
        Select(t.Path);
        if (TileActionStub.IsPending(item))
        {
            string label = TileHeaderLayout.Menu(tile).FirstOrDefault(e => e.Item == item)?.Label ?? "This";
            ShowToast(TileActionStub.Message(label));
            return;
        }
        switch (item)
        {
            case TileHeaderItem.SavePicture:
                _ = SavePictureAsync(t, PictureKind.View, ask: false);
                break;
            case TileHeaderItem.SavePictureAs:
                _ = SavePictureAsync(t, PictureKind.View, ask: true);
                break;
            case TileHeaderItem.SaveZoomedArea:
                _ = SavePictureAsync(t, PictureKind.ZoomedArea, ask: false);
                break;
            case TileHeaderItem.SaveOriginalFisheye:
                _ = SavePictureAsync(t, PictureKind.OriginalFisheye, ask: false);
                break;
            case TileHeaderItem.Record:
                // In the call window's own tile, Record is the call recording.
                if (FixedTile)
                {
                    _viewer.Capture.ToggleCall(camera);
                }
                else
                {
                    _viewer.Capture.ToggleLive(camera);
                }
                break;
            case TileHeaderItem.RecordMain:
                _viewer.Capture.StartLive(camera, RecordQuality.Main);
                break;
            case TileHeaderItem.RecordSub:
                _viewer.Capture.StartLive(camera, RecordQuality.Sub);
                break;
            case TileHeaderItem.Speaker:
                bool ctrl = InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control).HasFlag(CoreVirtualKeyStates.Down);
                _viewer.ToggleListening(camera, ctrl && from is not null);
                break;
            case TileHeaderItem.DismissAlert:
                _viewer.DismissAlert(camera);
                break;
            case TileHeaderItem.Conversation:
                if (_viewer.Calls.CallFor(camera) is null)
                {
                    _viewer.Calls.StartConversation(camera);
                }
                else
                {
                    _viewer.Calls.End(camera, "closed");
                }
                break;
            case TileHeaderItem.PanTilt:
                ShowPtz(PtzMode.Pad, t, from);
                break;
            case TileHeaderItem.Lens:
                ShowPtz(PtzMode.Lens, t, from);
                break;
            case TileHeaderItem.Presets:
                ShowPresets(t, from);
                break;
            case TileHeaderItem.ResetZoom or TileHeaderItem.ResetPosition:
                ResetTile(t);
                break;
            case TileHeaderItem.FillTile:
                SetTileFill(t, !Fills(t));
                break;
            case TileHeaderItem.Pop:
                // In the call window's own tile: Pop In. Elsewhere: Pop Out, which also brings an open window forward.
                if (FixedTile)
                {
                    _viewer.CallWindows?.PopIn(camera);
                }
                else
                {
                    _viewer.CallWindows?.PopOut(camera);
                }
                break;
            case TileHeaderItem.AllowTalkBack:
                _viewer.SetTalkAllowed(camera, !_viewer.Settings.TalkAllowed(camera));
                break;
            case TileHeaderItem.Quality or TileHeaderItem.Fisheye or TileHeaderItem.CallsOpenIn or TileHeaderItem.More:
                if (MenuFor(t, item) is { } menu)
                {
                    PopUp(menu, from ?? this, t);
                }
                break;
        }
        RefreshAll();
    }

    private void PopUp(MenuFlyout menu, FrameworkElement under, StageTile t)
    {
        if (under == this)
        {
            // From a key: the "..." position at the header's right end.
            menu.ShowAt(this, new Windows.Foundation.Point(Math.Max(t.FrameX + t.FrameWidth - 40, t.FrameX), t.FrameY + TileChrome.HeaderHeight));
        }
        else
        {
            menu.ShowAt(under);
        }
    }

    // ---- menus ----

    /// <summary>The menu a header button opens: Quality, Fisheye View, Calls open in, or "..." (the right-click menu without what the header shows).</summary>
    internal MenuFlyout? MenuFor(StageTile t, TileHeaderItem item)
    {
        if (t.CameraOrNone is not { } camera || HeaderTileOf(t) is not { } tile)
        {
            return null;
        }
        var flyout = new MenuFlyout();
        if (item == TileHeaderItem.More)
        {
            HashSet<TileHeaderItem> shown = [.. ChromeFor(t)?.HeaderLayout?.Visible ?? []];
            foreach (TileHeaderMenuEntry entry in TileHeaderLayout.Menu(tile).Where(e => !shown.Contains(e.Item)))
            {
                flyout.Items.Add(MenuItemFor(entry, t, camera));
            }
            return flyout;
        }
        if (TileHeaderLayout.Menu(tile).FirstOrDefault(e => e.Item == item) is not { } found
            || MenuItemFor(found, t, camera) is not MenuFlyoutSubItem sub)
        {
            return null;
        }
        // The submenu's items are the button's menu.
        foreach (MenuFlyoutItemBase i in sub.Items.ToArray())
        {
            sub.Items.Remove(i);
            flyout.Items.Add(i);
        }
        return flyout;
    }

    /// <summary>The "..." menu's item texts for a tile (self-test).</summary>
    internal IReadOnlyList<string> MoreMenuTexts(StageTile t) =>
        [.. (MenuFor(t, TileHeaderItem.More)?.Items ?? []).Select(i => i switch
        {
            MenuFlyoutItem m => m.Text,
            MenuFlyoutSubItem s => s.Text,
            _ => "",
        })];

    /// <summary>One header menu entry as a menu item: its label, its key hint, and a submenu where it has one.</summary>
    private MenuFlyoutItemBase MenuItemFor(TileHeaderMenuEntry entry, StageTile t, CameraRef camera)
    {
        MenuFlyoutItemBase made;
        switch (entry.Item)
        {
            case TileHeaderItem.Quality:
                {
                    MenuFlyoutSubItem q = QualityMenu.Create(t.Quality, _viewer.OffersBalanced(t), choice => SetQuality(t, choice));
                    q.Text = entry.Label;
                    return q;
                }
            case TileHeaderItem.Fisheye:
                {
                    MenuFlyoutSubItem view = FisheyeViewMenu(t);
                    view.Text = entry.Label;
                    // The mount belongs with the view (it is per camera).
                    view.Items.Add(new MenuFlyoutSeparator());
                    view.Items.Add(new MenuFlyoutItem { Text = "Mount", IsEnabled = false });
                    foreach (MenuFlyoutItemBase i in FisheyeMountMenu(camera).Items.ToArray())
                    {
                        view.Items.Add(i);
                    }
                    return view;
                }
            case TileHeaderItem.CallsOpenIn:
                return CallsOpenInMenu(camera, entry.Label);
            case TileHeaderItem.Presets:
                return PresetsSubItem(camera, entry.Label);
            case TileHeaderItem.FillTile:
                {
                    var fill = new ToggleMenuFlyoutItem { Text = entry.Label, IsChecked = Fills(t) };
                    fill.Click += (_, _) => PerformHeaderItem(t, TileHeaderItem.FillTile, null);
                    ToolTipService.SetToolTip(fill, "Crop this camera's picture to fill its tile; drag the picture to choose the part shown");
                    return fill;
                }
            case TileHeaderItem.AllowTalkBack:
                {
                    var allow = new ToggleMenuFlyoutItem { Text = entry.Label, IsChecked = _viewer.Settings.TalkAllowed(camera) };
                    allow.Click += (_, _) => PerformHeaderItem(t, TileHeaderItem.AllowTalkBack, null);
                    ToolTipService.SetToolTip(allow, "Lets Talk and typed replies play through this camera's speaker. Off until you allow it; every use is logged.");
                    return allow;
                }
            case TileHeaderItem.Entrance:
                {
                    // Windows has no entrance insets yet (docs/PARITY.md): the item is in its place, unavailable.
                    var entrance = new ToggleMenuFlyoutItem { Text = entry.Label, IsEnabled = false };
                    ToolTipService.SetToolTip(entrance, "Entrance insets are not on Windows yet");
                    return entrance;
                }
            default:
                {
                    TileHeaderItem what = entry.Item;
                    var item = new MenuFlyoutItem { Text = entry.Label };
                    if (entry.WindowsShortcut.Length > 0)
                    {
                        item.KeyboardAcceleratorTextOverride = entry.WindowsShortcut;
                    }
                    item.Click += (_, _) => PerformHeaderItem(t, what, null);
                    made = item;
                    if (TileActionStub.IsPending(what))
                    {
                        ToolTipService.SetToolTip(item, "Coming soon");
                    }
                    else if (what == TileHeaderItem.ResetPosition)
                    {
                        ToolTipService.SetToolTip(item, "Show the middle of the filled picture again");
                    }
                    else if (what == TileHeaderItem.Conversation && _viewer.Calls.CallFor(camera) is null)
                    {
                        ToolTipService.SetToolTip(item, "Captions of what the camera hears, Talk, and typed replies spoken through its speaker");
                    }
                    return made;
                }
        }
    }

    /// <summary>Calls open in &gt; Default, Window, Inset (per doorbell).</summary>
    private MenuFlyoutSubItem CallsOpenInMenu(CameraRef camera, string label)
    {
        var opens = new MenuFlyoutSubItem { Text = label };
        CallPresentation? chosen = _viewer.Settings.CallPresentationFor(camera);
        foreach ((string title, CallPresentation? value) in CallPresentationMenu.Choices(_viewer.Settings.CallsOpenInWindow))
        {
            CallPresentation? choice = value;
            var item = new RadioMenuFlyoutItem { Text = title, GroupName = "call-opens-in", IsChecked = chosen == choice };
            item.Click += (_, _) => _viewer.Settings.SetCallPresentation(camera, choice);
            opens.Items.Add(item);
        }
        return opens;
    }

    // ---- keys ----

    /// <summary>A key for the selected tile that is a header action (P, Shift+P, R, T and their call window forms).</summary>
    private void ApplyItemAction(TileAction action, StageTile tile)
    {
        TileHeaderItem? item = action switch
        {
            TileAction.SavePicture => TileHeaderItem.SavePicture,
            TileAction.SavePictureAs => TileHeaderItem.SavePictureAs,
            TileAction.ToggleRecording => TileHeaderItem.Record,
            TileAction.ToggleConversation => TileHeaderItem.Conversation,
            _ => null,
        };
        if (item is TileHeaderItem.Record && !FixedTile && tile.CameraOrNone is { } camera && _viewer.Calls.CallFor(camera) is not null && CtrlShiftDown())
        {
            // Ctrl+Shift+R in a viewer window records the selected tile's call while its camera's call is open; R otherwise.
            Interact();
            _viewer.Capture.ToggleCall(camera);
            RefreshAll();
            return;
        }
        if (item is { } i)
        {
            PerformHeaderItem(tile, i, null);
        }
    }

    private static bool CtrlShiftDown() =>
        InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control).HasFlag(CoreVirtualKeyStates.Down)
        && InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Shift).HasFlag(CoreVirtualKeyStates.Down);

    /// <summary>
    /// The tile keys alone (listen, capture, conversation, reset zoom) for the selected tile, without the window-level keys; the
    /// call window routes these. True if used.
    /// </summary>
    public bool HandleTileKey(VirtualKey key, bool ctrl)
    {
        if (Selected is not { IsEmpty: false } s)
        {
            return false;
        }
        static bool Down(VirtualKey k) => InputKeyboardSource.GetKeyStateForCurrentThread(k).HasFlag(CoreVirtualKeyStates.Down);
        TileAction action = TileKeyMap.Map(ToTileKey(key), ctrl, Down(VirtualKey.Menu), Down(VirtualKey.Shift), _viewer.Alerts.IsActive(s.Camera));
        if (action == TileAction.ToggleFocus && FixedTile)
        {
            return false;
        }
        if (action != TileAction.None)
        {
            Interact();
            ApplyAction(action, s);
            return true;
        }
        if (key is VirtualKey.Number0 or VirtualKey.NumberPad0 && !ctrl)
        {
            Interact();
            ResetTile(s);
            return true;
        }
        return false;
    }

    // ---- the short message over the stage ----

    /// <summary>Shows <paramref name="text"/> over the stage for a few seconds (on the viewer's clock).</summary>
    public void ShowToast(string text, string? actionTitle = null, Action? action = null)
    {
        _toastText.Text = text;
        AutomationProperties.SetName(_toast, text);
        _toastAction = action;
        _toastButton.Content = actionTitle;
        _toastButton.Visibility = actionTitle is not null && action is not null ? Visibility.Visible : Visibility.Collapsed;
        // A message with a button takes the pointer and stays longer.
        _toast.IsHitTestVisible = _toastButton.Visibility == Visibility.Visible;
        _toast.Visibility = Visibility.Visible;
        _toastHideAt = _viewer.Clock.Now + (_toast.IsHitTestVisible ? ActionToastSeconds : ToastSeconds);
    }

    /// <summary>The title of the button on the message now, null when it has none (self-test).</summary>
    public string? ToastActionTitle => _toast.Visibility == Visibility.Visible && _toastButton.Visibility == Visibility.Visible ? _toastButton.Content as string : null;

    /// <summary>Presses the message's button as a click does: its action runs and the message goes.</summary>
    public void PressToastAction()
    {
        Action? action = _toastAction;
        _toastHideAt = 0;
        action?.Invoke();
    }

    /// <summary>What the message says now, null when none shows (self-test).</summary>
    public string? ToastText => _toast.Visibility == Visibility.Visible ? _toastText.Text : null;

    private void TickToast(double now)
    {
        if (_toastHideAt is { } at && now >= at)
        {
            _toastHideAt = null;
            _toast.Visibility = Visibility.Collapsed;
        }
    }
}
