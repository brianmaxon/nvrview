// The window's content: the SwapChainPanel the render thread presents into,
// the tile chrome above it, and the input that drives both (docs/UX.md):
//
// - hover shows a tile's header (speaker, name) and fades it out 3 s after
//   the pointer stops or leaves; a click selects, a double-click (or Space,
//   or Enter) focuses the tile with an animation of the same live tile and,
//   once the animation ends, switches it to the main stream; the sub stream
//   stays on screen until the core's aligned switch (docs/FFI.md);
// - F full screen, arrow keys move the selection by position (in focus
//   they switch the focused camera), 0 resets digital zoom, Ctrl+L listens,
//   Ctrl+D diagnostics, Ctrl+, Settings, F1 the list of shortcuts;
// - mouse wheel (or a touchpad pinch, which arrives as Ctrl+wheel) zooms
//   around the pointer and dragging pans, by changing only the source
//   rectangle the renderer samples;
// - the tile features: the corner indicators and sound badges, the alert strip
//   and its Dismiss (Enter), the Quality menu, M to listen, and, in full
//   screen, the chrome and the pointer hiding 3 s after the last interaction
//   (alert strips and indicators stay).

using System.Diagnostics;
using System.Runtime.InteropServices;
using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Input;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NvrView.Interop;
using NvrView.Tiles;
using Windows.Foundation;
using Windows.System;
using Windows.UI.Core;

namespace NvrView.App;

internal sealed partial class StageView : UserControl
{

    // ISwapChainPanelNative of WinUI 3 (microsoft.ui.xaml.media.dxinterop.h).
    private static readonly Guid SwapChainPanelNativeId = new("63aad0b8-7c24-40ff-85a8-640d944cc325");

    private readonly Viewer _viewer;
    private readonly AppWindow _appWindow;
    private readonly SwapChainPanel _panel = new();
    private readonly Canvas _chromeLayer = new() { Background = new SolidColorBrush(Colors.Transparent) };
    private readonly StackPanel _empty = new()
    {
        HorizontalAlignment = HorizontalAlignment.Center,
        VerticalAlignment = VerticalAlignment.Center,
        Spacing = 12,
    };
    private readonly TextBlock _emptyText = new()
    {
        Foreground = new SolidColorBrush(Colors.LightGray),
        FontSize = 16,
        TextAlignment = TextAlignment.Center,
        TextWrapping = TextWrapping.Wrap,
        MaxWidth = 520,
    };
    private readonly Border _help;
    private readonly List<TileChrome> _chromes = [];
    private readonly DispatcherQueueTimerHolder _timer;
    private StageTile[] _chromeTiles = [];
    private StageTile? _hovered;
    private readonly ChromeIdle _idle;
    private bool _cursorHidden;
    private bool _diagnostics;
    private StageTile? _dragTile;
    private Point _dragFrom;
    private float _dragCenterX, _dragCenterY;

    public StageView(Viewer viewer, AppWindow appWindow)
    {
        _viewer = viewer;
        _appWindow = appWindow;
        _idle = new ChromeIdle(viewer.Clock);
        IsTabStop = true;
        UseSystemFocusVisuals = false;
        Background = new SolidColorBrush(Colors.Black);
        var root = new Grid();
        root.Children.Add(_panel);
        root.Children.Add(_chromeLayer);

        var add = new Button { Content = "Add a device…", HorizontalAlignment = HorizontalAlignment.Center };
        add.Click += (_, _) => OpenSettings();
        _empty.Children.Add(_emptyText);
        _empty.Children.Add(add);
        root.Children.Add(_empty);

        _help = new Border
        {
            Background = new SolidColorBrush(Windows.UI.Color.FromArgb(0xE0, 0x20, 0x20, 0x20)),
            CornerRadius = new CornerRadius(8),
            Padding = new Thickness(20),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Visibility = Visibility.Collapsed,
            Child = new TextBlock
            {
                Foreground = new SolidColorBrush(Colors.White),
                FontFamily = new FontFamily("Consolas"),
                FontSize = 14,
                Text = string.Join("\n",
                    "Double-click, Space, Enter   focus the selected tile / back to the grid",
                    "Arrow keys                   select the neighboring tile (in focus: switch camera)",
                    "F                            full screen",
                    "Esc                          leave focus, then full screen",
                    "Mouse wheel, drag            digital zoom and pan; 0 resets",
                    "M, Ctrl+L, speaker button    listen (Ctrl+click on the speaker listens to several)",
                    "Enter                        dismiss the selected tile's alert (else focus)",
                    "Right-click > Quality        Auto, Clear, Fluent, Balanced for the tile",
                    "Ctrl+D                       diagnostics",
                    "Ctrl+,                       Settings (devices)",
                    "F1                           this list"),
            },
        };
        root.Children.Add(_help);
        Content = root;

        Renderer = null!;
        _panel.SizeChanged += (_, _) => PushSize();
        _panel.CompositionScaleChanged += (_, _) => PushSize();
        _viewer.Changed += RefreshAll;
        _viewer.IndicatorsChanged += SyncOverlays;
        _viewer.Settings.Changed += SyncOverlays;
        _timer = new DispatcherQueueTimerHolder(DispatcherQueue, TimeSpan.FromMilliseconds(250), Tick);
        Loaded += (_, _) =>
        {
            Microsoft.UI.Xaml.Media.CompositionTarget.Rendering += OnRendering;
            _ = Focus(FocusState.Programmatic);
        };
        Unloaded += (_, _) => Microsoft.UI.Xaml.Media.CompositionTarget.Rendering -= OnRendering;
        RefreshAll();
    }

    /// <summary>The render thread (set once by the window).</summary>
    public StageRenderer Renderer { get; set; }

    /// <summary>The Settings dialog opener (set by the window).</summary>
    public Action? SettingsRequested { get; set; }

    /// <summary>The selected tile (keyboard actions apply to it).</summary>
    public StageTile? Selected { get; private set; }

    /// <summary>True while a tile is focused (or animating into focus).</summary>
    public bool IsFocused => Renderer.Focus is { Entering: true };

    /// <summary>Puts an element in the stage's overlay for a moment (the self-test renders it to a bitmap); true adds, false removes.</summary>
    public void HostTemporarily(FrameworkElement element, bool add)
    {
        if (add)
        {
            Canvas.SetLeft(element, 20);
            Canvas.SetTop(element, 20);
            _chromeLayer.Children.Add(element);
        }
        else
        {
            _chromeLayer.Children.Remove(element);
        }
    }

    /// <summary>The chrome of a tile (for the self-test).</summary>
    public TileChrome? ChromeFor(StageTile tile) => _chromes.FirstOrDefault(c => c.Tile == tile);

    /// <summary>Frames the UI thread updated the chrome (for the self-test).</summary>
    public long ChromeFrames { get; private set; }

    public void PushSize() =>
        Renderer?.SetPanelSize(_panel.ActualWidth, _panel.ActualHeight, _panel.CompositionScaleX, _panel.CompositionScaleY);

    /// <summary>Hands the panel a swap chain (UI thread); releases the reference the render thread added.</summary>
    public unsafe void AttachSwapChain(nint swapChain)
    {
        nint unknown = ((WinRT.IWinRTObject)_panel).NativeObject.ThisPtr;
        Guid iid = SwapChainPanelNativeId;
        if (Marshal.QueryInterface(unknown, in iid, out nint native) >= 0)
        {
            var setSwapChain = (delegate* unmanaged[Stdcall]<nint, nint, int>)(*(void***)native)[3];
            int hr = setSwapChain(native, swapChain);
            Marshal.Release(native);
            if (hr < 0)
            {
                AppLog.Warn($"SetSwapChain failed (0x{hr:X8})");
            }
        }
        else
        {
            AppLog.Warn("the panel has no ISwapChainPanelNative");
        }
        if (swapChain != 0)
        {
            Marshal.Release(swapChain);
        }
    }

    private void OpenSettings() => SettingsRequested?.Invoke();

    // ---- focus ----

    public void ToggleFocus(StageTile tile)
    {
        if (Renderer.Focus is { Entering: true } f && f.Tile == tile.Id)
        {
            Unfocus();
        }
        else
        {
            FocusOn(tile);
        }
    }

    public void FocusOn(StageTile tile)
    {
        long now = Stopwatch.GetTimestamp();
        FocusAnimation? current = Renderer.Focus;
        if (current is { Entering: true } && current.Tile != tile.Id && _viewer.FindTile(current.Tile) is { } previous)
        {
            // Switch the focused camera: no animation, tiers swap at once.
            _viewer.SetFocused(previous, false);
            Renderer.Focus = new FocusAnimation(tile.Id, now, true, 1);
            _viewer.SetFocused(tile, true);
            Selected = tile;
            RefreshAll();
            return;
        }
        float from = current is not null && current.Tile == tile.Id ? current.Progress(now) : 0;
        var animation = new FocusAnimation(tile.Id, now, true, from);
        Renderer.Focus = animation;
        Selected = tile;
        // The main stream once the animation is over (docs/UX.md).
        After((1 - from) * FocusAnimation.Seconds, () =>
        {
            if (ReferenceEquals(Renderer.Focus, animation))
            {
                _viewer.SetFocused(tile, true);
            }
        });
        RefreshAll();
    }

    public void Unfocus()
    {
        if (Renderer.Focus is not { Entering: true } f)
        {
            return;
        }
        long now = Stopwatch.GetTimestamp();
        float p = f.Progress(now);
        var animation = new FocusAnimation(f.Tile, now, false, p);
        Renderer.Focus = animation;
        if (_viewer.FindTile(f.Tile) is { } tile)
        {
            // The sub stream is ready almost at once; the tile shrinks meanwhile.
            _viewer.SetFocused(tile, false);
        }
        After(p * FocusAnimation.Seconds + 0.05, () =>
        {
            if (ReferenceEquals(Renderer.Focus, animation))
            {
                Renderer.Focus = null;
            }
        });
        RefreshAll();
    }

    // One-shot timers stay referenced until they fire: an unreferenced
    // DispatcherQueueTimer can be collected and never fire.
    private readonly HashSet<Microsoft.UI.Dispatching.DispatcherQueueTimer> _pending = [];

    private void After(double seconds, Action action)
    {
        Microsoft.UI.Dispatching.DispatcherQueueTimer timer = DispatcherQueue.CreateTimer();
        timer.Interval = TimeSpan.FromSeconds(Math.Max(seconds, 0.001));
        timer.IsRepeating = false;
        timer.Tick += (t, _) =>
        {
            t.Stop();
            _pending.Remove(t);
            action();
        };
        _pending.Add(timer);
        timer.Start();
    }

    // ---- full screen ----

    /// <summary>Whether the window is full screen (<see cref="FullScreenOverride"/> stands in for it in the self-test).</summary>
    public bool IsFullScreen => FullScreenOverride ?? _appWindow.Presenter.Kind == AppWindowPresenterKind.FullScreen;

    /// <summary>Self-test: pretends the window is (or is not) full screen without changing the window.</summary>
    public bool? FullScreenOverride { get; set; }

    public void ToggleFullScreen()
    {
        bool full = IsFullScreen;
        _appWindow.SetPresenter(full ? AppWindowPresenterKind.Overlapped : AppWindowPresenterKind.FullScreen);
    }

    // ---- zoom ----

    public static void ZoomAround(StageTile t, double fx, double fy, double factor)
    {
        float z = t.Zoom;
        float z2 = (float)Math.Clamp(z * factor, 1, 8);
        // The picture point under the pointer stays under it.
        double w = 1 / z, x = Math.Clamp(t.CenterX - w / 2, 0, 1 - w);
        double h = 1 / z, y = Math.Clamp(t.CenterY - h / 2, 0, 1 - h);
        double px = x + fx * w, py = y + fy * h;
        double w2 = 1 / z2, h2 = 1 / z2;
        double x2 = Math.Clamp(px - fx * w2, 0, 1 - w2), y2 = Math.Clamp(py - fy * h2, 0, 1 - h2);
        t.CenterX = (float)(x2 + w2 / 2);
        t.CenterY = (float)(y2 + h2 / 2);
        t.Zoom = z2;
    }

    public static void ResetZoom(StageTile t)
    {
        t.Zoom = 1;
        t.CenterX = 0.5f;
        t.CenterY = 0.5f;
    }

    // ---- input ----

    private StageTile? TileAt(Point p)
    {
        StageTile[] tiles = _viewer.Tiles;
        // The focused tile is on top.
        if (Renderer.Focus is { } f && _viewer.FindTile(f.Tile) is { Visible: true } focused && Contains(focused, p))
        {
            return focused;
        }
        foreach (StageTile t in tiles)
        {
            if (t.Visible && Contains(t, p))
            {
                return t;
            }
        }
        return null;
    }

    private static bool Contains(StageTile t, Point p) =>
        p.X >= t.X && p.Y >= t.Y && p.X < t.X + t.Width && p.Y < t.Y + t.Height;

    protected override void OnPointerMoved(PointerRoutedEventArgs e)
    {
        Point p = e.GetCurrentPoint(this).Position;
        Interact();
        if (_dragTile is { } drag && e.GetCurrentPoint(this).Properties.IsLeftButtonPressed && drag.Width > 0)
        {
            double dx = (p.X - _dragFrom.X) / drag.Width / drag.Zoom, dy = (p.Y - _dragFrom.Y) / drag.Height / drag.Zoom;
            double half = 0.5 / drag.Zoom;
            drag.CenterX = (float)Math.Clamp(_dragCenterX - dx, half, 1 - half);
            drag.CenterY = (float)Math.Clamp(_dragCenterY - dy, half, 1 - half);
        }
        StageTile? t = TileAt(p);
        if (t != _hovered)
        {
            _hovered = t;
        }
        UpdateHover();
    }

    protected override void OnPointerExited(PointerRoutedEventArgs e)
    {
        _hovered = null;
        UpdateHover();
    }

    protected override void OnPointerPressed(PointerRoutedEventArgs e)
    {
        Interact();
        _ = Focus(FocusState.Pointer);
        PointerPoint pp = e.GetCurrentPoint(this);
        if (pp.Properties.IsLeftButtonPressed && TileAt(pp.Position) is { } t)
        {
            Selected = t;
            if (t.Zoom > 1)
            {
                _dragTile = t;
                _dragFrom = pp.Position;
                _dragCenterX = t.CenterX;
                _dragCenterY = t.CenterY;
                _ = CapturePointer(e.Pointer);
            }
            UpdateHover();
        }
    }

    protected override void OnPointerReleased(PointerRoutedEventArgs e)
    {
        _dragTile = null;
        ReleasePointerCapture(e.Pointer);
    }

    protected override void OnDoubleTapped(DoubleTappedRoutedEventArgs e)
    {
        if (e.OriginalSource is FrameworkElement { Parent: Button } or Button or FontIcon)
        {
            return;
        }
        if (TileAt(e.GetPosition(this)) is { } t)
        {
            ToggleFocus(t);
            e.Handled = true;
        }
    }

    protected override void OnPointerWheelChanged(PointerRoutedEventArgs e)
    {
        PointerPoint pp = e.GetCurrentPoint(this);
        if (TileAt(pp.Position) is { } t && t.Width > 0 && t.Height > 0 && t.PictureWidth > 0)
        {
            double fx = (pp.Position.X - t.X) / t.Width, fy = (pp.Position.Y - t.Y) / t.Height;
            ZoomAround(t, fx, fy, Math.Pow(1.0015, pp.Properties.MouseWheelDelta));
            e.Handled = true;
            Interact();
        }
    }

    protected override void OnRightTapped(RightTappedRoutedEventArgs e)
    {
        StageTile? t = TileAt(e.GetPosition(this));
        if (t is not null)
        {
            Selected = t;
        }
        var menu = new MenuFlyout();
        void Item(string text, Action action, bool enabled = true)
        {
            var item = new MenuFlyoutItem { Text = text, IsEnabled = enabled };
            item.Click += (_, _) => action();
            menu.Items.Add(item);
        }
        if (t is not null)
        {
            Item(IsFocused && Renderer.Focus?.Tile == t.Id ? "Back to the grid" : "Focus", () => ToggleFocus(t));
            Item(t.Audible ? "Stop listening" : "Listen", () => _viewer.ToggleListening(t, false));
            Item("Reset zoom", () => ResetZoom(t), t.Zoom > 1);
            menu.Items.Add(QualityMenu.Create(t.Quality, _viewer.OffersBalanced(t), q => _viewer.SetQuality(t, q)));
            if (_viewer.Alerts.IsActive(t.Camera))
            {
                Item("Dismiss alert", () => _viewer.DismissAlert(t.Camera));
            }
            menu.Items.Add(new MenuFlyoutSeparator());
        }
        menu.Items.Add(IndicatorCornerMenu());
        Item("Full screen", ToggleFullScreen);
        Item(_diagnostics ? "Hide diagnostics" : "Show diagnostics", ToggleDiagnostics);
        Item("Keyboard shortcuts", ToggleHelp);
        Item("Settings…", OpenSettings);
        menu.ShowAt(this, e.GetPosition(this));
        e.Handled = true;
    }

    /// <summary>The "Indicator corner" submenu: where the tiles' symbols sit (a setting, saved).</summary>
    private MenuFlyoutSubItem IndicatorCornerMenu()
    {
        var sub = new MenuFlyoutSubItem { Text = "Indicator corner" };
        foreach (ChromeCorner corner in Enum.GetValues<ChromeCorner>())
        {
            var item = new RadioMenuFlyoutItem { Text = corner.Title(), GroupName = "indicator-corner", IsChecked = corner == _viewer.Settings.IndicatorCorner };
            ChromeCorner choice = corner;
            item.Click += (_, _) => _viewer.Settings.IndicatorCorner = choice;
            sub.Items.Add(item);
        }
        return sub;
    }

    private static TileKey ToTileKey(VirtualKey key) => key switch
    {
        VirtualKey.Enter => TileKey.Enter,
        VirtualKey.Space => TileKey.Space,
        VirtualKey.M => TileKey.M,
        VirtualKey.L => TileKey.L,
        _ => TileKey.Other,
    };

    /// <summary>Carries out what a key means for the selected tile (see <see cref="TileKeyMap"/>).</summary>
    public void ApplyAction(TileAction action, StageTile tile)
    {
        switch (action)
        {
            case TileAction.Dismiss:
                _viewer.DismissAlert(tile.Camera);
                break;
            case TileAction.ToggleFocus:
                ToggleFocus(tile);
                break;
            case TileAction.ToggleListening:
                _viewer.ToggleListening(tile, false);
                Selected = tile;
                break;
        }
    }

    protected override void OnKeyDown(KeyRoutedEventArgs e)
    {
        Interact();
        bool KeyDown(VirtualKey k) => InputKeyboardSource.GetKeyStateForCurrentThread(k).HasFlag(CoreVirtualKeyStates.Down);
        bool ctrl = KeyDown(VirtualKey.Control);
        StageTile[] tiles = _viewer.Tiles;
        Selected ??= tiles.FirstOrDefault();
        StageTile? s = Selected;
        e.Handled = true;
        if (s is not null)
        {
            TileAction action = TileKeyMap.Map(ToTileKey(e.Key), ctrl, KeyDown(VirtualKey.Menu), KeyDown(VirtualKey.Shift), _viewer.Alerts.IsActive(s.Camera));
            if (action != TileAction.None)
            {
                ApplyAction(action, s);
                return;
            }
        }
        switch (e.Key)
        {
            case VirtualKey.F when !ctrl:
                ToggleFullScreen();
                break;
            case VirtualKey.Escape:
                if (IsFocused)
                {
                    Unfocus();
                }
                else if (IsFullScreen)
                {
                    ToggleFullScreen();
                }
                break;
            case VirtualKey.Space or VirtualKey.Enter when s is not null:
                // Shift+Space and the like are not shortcuts of ours.
                e.Handled = false;
                break;
            case VirtualKey.Left or VirtualKey.Right or VirtualKey.Up or VirtualKey.Down when s is not null:
                if (Neighbor(s, e.Key) is { } next)
                {
                    if (IsFocused)
                    {
                        FocusOn(next);
                    }
                    Selected = next;
                    Interact();
                    UpdateHover();
                }
                break;
            case VirtualKey.Number0 or VirtualKey.NumberPad0 when s is not null:
                ResetZoom(s);
                break;
            case VirtualKey.D when ctrl:
                ToggleDiagnostics();
                break;
            case (VirtualKey)188 when ctrl: // the comma key
                OpenSettings();
                break;
            case VirtualKey.F1:
                ToggleHelp();
                break;
            default:
                e.Handled = false;
                break;
        }
    }

    /// <summary>The nearest tile whose center lies in that direction from <paramref name="from"/>'s.</summary>
    public StageTile? Neighbor(StageTile from, VirtualKey key)
    {
        double cx = from.X + from.Width / 2, cy = from.Y + from.Height / 2;
        StageTile? best = null;
        double bestScore = double.MaxValue;
        foreach (StageTile t in _viewer.Tiles)
        {
            if (t == from || t.Width <= 0)
            {
                continue;
            }
            double dx = t.X + t.Width / 2 - cx, dy = t.Y + t.Height / 2 - cy;
            (double along, double across) = key switch
            {
                VirtualKey.Left => (-dx, dy),
                VirtualKey.Right => (dx, dy),
                VirtualKey.Up => (-dy, dx),
                _ => (dy, dx),
            };
            if (along <= 1)
            {
                continue;
            }
            double score = along + 2 * Math.Abs(across);
            if (score < bestScore)
            {
                bestScore = score;
                best = t;
            }
        }
        return best;
    }

    private void ToggleDiagnostics()
    {
        _diagnostics = !_diagnostics;
        RefreshAll();
    }

    private void ToggleHelp() =>
        _help.Visibility = _help.Visibility == Visibility.Visible ? Visibility.Collapsed : Visibility.Visible;

    // ---- chrome ----

    private void SpeakerClicked(StageTile tile)
    {
        bool ctrl = InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control).HasFlag(CoreVirtualKeyStates.Down);
        _viewer.ToggleListening(tile, ctrl);
        Selected = tile;
    }

    private void SyncChromes()
    {
        StageTile[] tiles = _viewer.Tiles;
        if (ReferenceEquals(tiles, _chromeTiles))
        {
            return;
        }
        _chromeTiles = tiles;
        _chromeLayer.Children.Clear();
        var kept = _chromes.Where(c => tiles.Contains(c.Tile)).ToDictionary(c => c.Tile);
        _chromes.Clear();
        foreach (StageTile t in tiles)
        {
            TileChrome chrome = kept.GetValueOrDefault(t) ?? new TileChrome(t, SpeakerClicked) { DismissClicked = tile => _viewer.DismissAlert(tile.Camera) };
            _chromes.Add(chrome);
            _chromeLayer.Children.Add(chrome.Root);
        }
        if (Selected is not null && !tiles.Contains(Selected))
        {
            Selected = null;
        }
    }

    /// <summary>Texts, badges, and the empty-stage message after a state change.</summary>
    public void RefreshAll()
    {
        SyncChromes();
        foreach (TileChrome c in _chromes)
        {
            StageTile t = c.Tile;
            c.Refresh(TitleOf(t), StatusText(t), t.Audible, _diagnostics ? DiagnosticsText(t) : null);
        }
        SyncOverlays();
        bool empty = _viewer.Tiles.Length == 0;
        _empty.Visibility = empty ? Visibility.Visible : Visibility.Collapsed;
        if (empty)
        {
            _emptyText.Text = EmptyText();
        }
        UpdateHover();
    }

    /// <summary>The header's title: the camera's name, and the quality when it is not Auto.</summary>
    public static string TitleOf(StageTile t) =>
        t.Quality.HeaderNote() is { } note ? $"{t.Name} · {note}" : t.Name;

    /// <summary>
    /// The corner indicators, the sound badges, and the alert strip of every tile, from the viewer's
    /// state. On events and the slow tick, never per frame.
    /// </summary>
    public void SyncOverlays()
    {
        ChromeCorner corner = _viewer.Settings.IndicatorCorner;
        DateTime now = _viewer.WallClock();
        bool blink = AlertBorder.ModeFor(_viewer.AnimationsEnabled()) == AlertBorder.Mode.Blinking;
        foreach (TileChrome c in _chromes)
        {
            CameraRef camera = c.Tile.Camera;
            c.SetAlert(_viewer.Alerts.For(camera)?.Lines(now), blink);
            c.SetIndicators(_viewer.Board.Items(camera), corner);
            List<string> sound = [];
            foreach (DetectionTracker.Active a in _viewer.Detections.For(camera))
            {
                if (a.Source == DetectionSource.LocalAudio)
                {
                    sound.Add(DetectionTracker.SoundBadgeText(a.Kind));
                }
            }
            c.SetSoundBadges(sound);
        }
    }

    private string EmptyText()
    {
        if (_viewer.Devices.Count == 0)
        {
            return "No devices yet. Add an NVR or camera to start watching (Ctrl+, or right-click).";
        }
        return string.Join("\n", _viewer.Devices.Select(d => $"{d.DisplayName}: {DeviceText(d) ?? "connected, no channels yet"}"));
    }

    /// <summary>The device's state in words, or null when connected.</summary>
    public static string? DeviceText(DeviceEntry d)
    {
        switch (d.State)
        {
            case "connected":
                return null;
            case "connecting":
                return "Connecting…";
            case "backoff":
                long left = (d.RetryAt - Stopwatch.GetTimestamp()) / Stopwatch.Frequency;
                return left > 0 ? $"Connection lost. Reconnecting in {left + 1} s…" : "Connection lost. Reconnecting…";
            case "login_failed":
                return "Login failed. Check the user name and password in Settings.";
            case "no_password":
                return "No saved password. Enter it in Settings.";
            case "invalid":
                return "The address is not valid. Check it in Settings.";
            case "closed":
                return "Disconnected.";
            default:
                return d.State;
        }
    }

    /// <summary>The tile's status line, or null while it plays normally.</summary>
    public static string? StatusText(StageTile t)
    {
        if (DeviceText(t.Device) is { } device)
        {
            return device;
        }
        if (t.Error is { } error)
        {
            return error;
        }
        if (t.Video.MissingDecoder == NvCodec.H265)
        {
            return "This camera sends H.265 video. Install the HEVC Video Extension from the Microsoft Store to watch it.";
        }
        if (t.Waiting is { } waiting)
        {
            return waiting + "…";
        }
        return t.Video.Status switch
        {
            NvTileStatus.Undecodable => "This stream cannot be decoded." + (t.Video.ConfigureError is { } why ? $"\n{why}" : ""),
            NvTileStatus.Recovering => "Recovering…",
            NvTileStatus.Waiting when t.PictureWidth == 0 => "Starting…",
            _ => null,
        };
    }

    private static string DiagnosticsText(StageTile t)
    {
        string kind = t.Video.DecoderKind switch
        {
            NvrView.Video.DecoderKind.Hardware => "GPU",
            NvrView.Video.DecoderKind.Software or NvrView.Video.DecoderKind.SoftwareTexture => "CPU",
            _ => "?",
        };
        return $"{t.PictureWidth}x{t.PictureHeight} {t.ShownTier} · {kind} decode · {t.ReceivedRate:0.0} fps in";
    }

    private void UpdateHover()
    {
        bool recent = _idle.ChromeShown;
        foreach (TileChrome c in _chromes)
        {
            bool header = recent && c.Tile == _hovered;
            bool selected = c.Tile == Selected && recent;
            if (c.HeaderShown != header || selected)
            {
                c.SetHover(header, selected);
            }
            else if (!selected)
            {
                c.SetHover(header, false);
            }
        }
    }

    /// <summary>
    /// The slow tick (every 250 ms): countdowns, the indicator hold, zoom-driven stream quality,
    /// the hover chrome's and the pointer's idle timeout. The self-test calls it directly on a manual clock.
    /// </summary>
    public void Tick()
    {
        _viewer.Tick();
        UpdateCursor();
        // Countdowns and fades that depend on time alone.
        UpdateHover();
        if (_viewer.Devices.Any(d => d.State == "backoff"))
        {
            RefreshAll();
        }
        else if (_diagnostics)
        {
            foreach (TileChrome c in _chromes)
            {
                c.Refresh(TitleOf(c.Tile), StatusText(c.Tile), c.Tile.Audible, DiagnosticsText(c.Tile));
            }
        }
        else
        {
            // Status lines that follow the video (starting, recovering).
            foreach (TileChrome c in _chromes)
            {
                c.Refresh(TitleOf(c.Tile), StatusText(c.Tile), c.Tile.Audible, null);
            }
        }
    }

    // ---- interaction and the pointer ----

    /// <summary>Pointer movement, a click, a key, or a scroll: the chrome (and, in full screen, the pointer) comes back.</summary>
    public void Interact()
    {
        _idle.Interact();
        UpdateCursor();
    }

    /// <summary>Whether the pointer is hidden (full screen, idle).</summary>
    public bool CursorHidden => _cursorHidden;

    /// <summary>Whether the hover chrome may show (not idle for 3 s).</summary>
    public bool ChromeShown => _idle.ChromeShown;

    [LibraryImport("user32.dll")]
    private static partial int ShowCursor([MarshalAs(UnmanagedType.Bool)] bool show);

    private void UpdateCursor()
    {
        bool hide = _idle.PointerHidden(IsFullScreen);
        if (hide == _cursorHidden)
        {
            return;
        }
        _cursorHidden = hide;
        if (FullScreenOverride is null)
        {
            // A count: every hide is paired with one show.
            _ = ShowCursor(!hide);
        }
    }

    private void OnRendering(object? sender, object e)
    {
        ChromeFrames++;
        NvCore core = _viewer.Core;
        foreach (TileChrome c in _chromes)
        {
            c.Place();
            if (c.Tile.Audible || c.HeaderShown)
            {
                c.SetLevel(core.TileAudioLevel(c.Tile.Id).Peak);
            }
        }
    }

    public void StopTimers()
    {
        if (_cursorHidden)
        {
            _cursorHidden = false;
            if (FullScreenOverride is null)
            {
                _ = ShowCursor(true);
            }
        }
        _timer.Stop();
        Microsoft.UI.Xaml.Media.CompositionTarget.Rendering -= OnRendering;
    }
}

/// <summary>A repeating DispatcherQueue timer.</summary>
internal sealed class DispatcherQueueTimerHolder
{
    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer _timer;

    public DispatcherQueueTimerHolder(Microsoft.UI.Dispatching.DispatcherQueue queue, TimeSpan interval, Action tick)
    {
        _timer = queue.CreateTimer();
        _timer.Interval = interval;
        _timer.IsRepeating = true;
        _timer.Tick += (_, _) => tick();
        _timer.Start();
    }

    public void Stop() => _timer.Stop();
}
