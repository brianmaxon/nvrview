// A doorbell call in its own window (docs/UX.md "A call in its own window"). The call stays in
// Viewer.Calls; its Presentation says where it shows. CallWindows opens and closes the windows
// from it; each CallWindow is a WinUI window with its own SwapChainPanel and StageRenderer that
// draws the doorbell's shared tile (the same decoded pictures as every viewer window; no swap
// chain is ever moved between windows) into the transparent hole of the call card, the same
// card the viewer windows show as an inset. Like a viewer window's renderer, it recovers from a
// lost GPU device on its own and rebuilds its swap chain.
//
// Leases: the window holds its own lease on the doorbell and takes it before the viewer windows
// let their insets go (Pop Out), and the viewer windows take theirs before the window lets its
// own go (Pop In), so the doorbell's tile is never without a holder. The window's stream demand
// is Auto: the main stream while the picture is drawn wider than the sub-stream's pictures
// (CallWindowQuality).
//
// Call windows are not viewer windows: not in windows.json, not counted when the last viewer
// window closes, and closed with their call (closed, the device removed, the app quitting). A call
// in a window never ends by inactivity (Call.Expired).

using System.Runtime.InteropServices;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Input;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.Graphics;
using VirtualKey = Windows.System.VirtualKey;
using static TerraFX.Interop.Windows.SWP;
using static TerraFX.Interop.Windows.Windows;
using Rect = NvrView.Shell.Rect;

namespace NvrView.App;

/// <summary>The choices of "Calls open in" for a doorbell, in the Settings and the tile menu.</summary>
internal static class CallPresentationMenu
{
    /// <summary>Default (naming what the setting gives now), Window, Inset; <c>null</c> is Default.</summary>
    public static IReadOnlyList<(string Title, CallPresentation? Value)> Choices(bool global) =>
    [
        ($"Default ({(global ? "Window" : "Inset")})", null),
        ("Window", CallPresentation.Window),
        ("Inset", CallPresentation.Inset),
    ];
}

/// <summary>The app's call windows, one per call shown in a window (see the file comment).</summary>
internal sealed class CallWindows
{
    private readonly Viewer _viewer;
    private readonly CoreHost _host;
    private readonly Func<ViewerWindow?> _active;
    private readonly bool _background;
    private readonly Dictionary<CameraRef, CallWindow> _windows = [];
    private readonly DispatcherQueueTimer _saveTimer;
    private WindowRect? _frame;
    private bool _frameDirty;
    private bool _closedAll;

    /// <param name="active">The viewer window the user is working in (its screen is where "Top right of the active screen" goes).</param>
    /// <param name="background">A test run: windows stay behind the others, never take the focus, and never float.</param>
    public CallWindows(Viewer viewer, CoreHost host, Func<ViewerWindow?> active, bool background)
    {
        _viewer = viewer;
        _host = host;
        _active = active;
        _background = background;
        _frame = viewer.Settings.CallWindowFrame;
        _saveTimer = DispatcherQueue.GetForCurrentThread().CreateTimer();
        _saveTimer.Interval = TimeSpan.FromMilliseconds(500);
        _saveTimer.IsRepeating = false;
        _saveTimer.Tick += (_, _) => FlushFrame();
        // Before any viewer window's cards: a window that ends is closed first, and Pop In orders its own leases.
        viewer.CallsChanged += Sync;
    }

    /// <summary>The window showing a camera's call, if it has one.</summary>
    public CallWindow? WindowFor(CameraRef camera) => _windows.GetValueOrDefault(camera.ChannelKey);

    public int Count => _windows.Count;

    /// <summary>Why the last window could not be opened, if it could not (self-test).</summary>
    public string? LastError { get; private set; }

    /// <summary>Whether the last window opened took the focus (self-test): a ring never does; Pop Out and Start Conversation, the user's own actions, do.</summary>
    public bool? LastOpenActivated { get; private set; }

    /// <summary>The pin: on top of other windows (on by default; remembered).</summary>
    public bool Pinned
    {
        get => _viewer.Settings.CallWindowOnTop;
        set
        {
            _viewer.Settings.CallWindowOnTop = value;
            foreach (CallWindow w in _windows.Values)
            {
                w.ApplyPinned(value);
            }
        }
    }

    /// <summary>"Open call windows" (Settings).</summary>
    public CallWindowPlacement.Mode PlacementMode
    {
        get => _viewer.Settings.CallWindowPlacementMode;
        set => _viewer.Settings.CallWindowPlacementMode = value;
    }

    /// <summary>The frame the user last left a call window at (physical pixels, virtual-screen origin); written to the settings a moment after the last move.</summary>
    public WindowRect? RememberedFrame
    {
        get => _frame;
        set
        {
            _frame = value;
            _frameDirty = true;
            _saveTimer.Stop();
            _saveTimer.Start();
        }
    }

    private void FlushFrame()
    {
        _saveTimer.Stop();
        if (_frameDirty)
        {
            _frameDirty = false;
            _viewer.Settings.CallWindowFrame = _frame;
        }
    }

    // ---- following the calls ----

    /// <summary>
    /// After every change of the calls: a call that ended closes its window; a call shown in a
    /// window without one gets it (a ring opens it without taking the focus from what the user is
    /// doing; Start Conversation, which the user chose, brings it forward); the windows show the
    /// calls as they are now.
    /// </summary>
    public void Sync()
    {
        if (_closedAll)
        {
            return;
        }
        foreach ((CameraRef camera, CallWindow w) in _windows.ToArray())
        {
            if (_viewer.Calls.CallFor(camera) is null)
            {
                _windows.Remove(camera);
                w.Close();
            }
        }
        foreach (CameraRef camera in _viewer.Calls.Cameras)
        {
            if (!_windows.ContainsKey(camera) && _viewer.Calls.CallFor(camera) is { Presentation: CallPresentation.Window } call)
            {
                _ = Open(camera, activate: call.Origin == CallOrigin.Conversation);
            }
        }
        foreach (CallWindow w in _windows.Values)
        {
            w.Refresh();
        }
    }

    /// <summary>
    /// Pop Out: the window opens first (its lease taken), then the call's presentation changes and
    /// the viewer windows let their insets go. An explicit Pop Out brings the window forward.
    /// </summary>
    public void PopOut(CameraRef camera)
    {
        camera = camera.ChannelKey;
        if (_viewer.Calls.CallFor(camera) is not { } call)
        {
            return;
        }
        if (_windows.GetValueOrDefault(camera) is { } existing)
        {
            existing.Show(activate: true);
        }
        else if (Open(camera, activate: true) is null)
        {
            return;
        }
        if (call.Presentation != CallPresentation.Window)
        {
            _viewer.Calls.SetPresentation(camera, CallPresentation.Window);
        }
    }

    /// <summary>
    /// Pop In: the viewer windows take their insets back (and their leases, in the same turn) before
    /// the window closes and lets its own go; the call gets a fresh minute.
    /// </summary>
    public void PopIn(CameraRef camera)
    {
        camera = camera.ChannelKey;
        _windows.Remove(camera, out CallWindow? w);
        _viewer.Calls.SetPresentation(camera, CallPresentation.Inset);
        w?.Close();
    }

    /// <summary>The user closed the window itself (its close button, Alt+F4): the call ends.</summary>
    internal void ClosedByUser(CallWindow w)
    {
        _windows.Remove(w.Camera);
        _viewer.Calls.End(w.Camera, "closed");
    }

    /// <summary>Esc and the window's Close: ends the call; the window closes with it.</summary>
    internal void EndCall(CallWindow w) => _viewer.Calls.End(w.Camera, "closed");

    /// <summary>The user moved or resized a window: that frame is where the next one opens.</summary>
    internal void Moved(WindowRect frame) => RememberedFrame = frame;

    /// <summary>Closes every window (shutdown) and saves the frame.</summary>
    public void CloseAll()
    {
        _closedAll = true;
        foreach (CallWindow w in _windows.Values.ToArray())
        {
            w.Close();
        }
        _windows.Clear();
        FlushFrame();
    }

    private CallWindow? Open(CameraRef camera, bool activate)
    {
        CallWindow w;
        try
        {
            w = new CallWindow(this, _viewer, _host, camera, _background);
        }
        catch (Exception e)
        {
            LastError = e.ToString();
            AppLog.Warn($"the call window could not be opened: {e.Message}");
            return null;
        }
        WindowRect frame = Frame(_windows.Count);
        _windows[camera] = w;
        w.ApplyPinned(Pinned);
        w.Place(frame);
        LastOpenActivated = activate;
        w.Refresh();
        w.Show(activate);
        return w;
    }

    // ---- placement ----

    private ViewerWindow? ActiveViewer => _active();

    /// <summary>The visible area of the screen holding the active viewer window, in pixels (null: unknown).</summary>
    public WindowRect? ActiveWorkArea()
    {
        if (ActiveViewer is not { } v)
        {
            return null;
        }
        try
        {
            Windows.Graphics.RectInt32 a = DisplayArea.GetFromWindowId(v.AppWindow.Id, DisplayAreaFallback.Nearest).WorkArea;
            return new WindowRect(a.X, a.Y, a.Width, a.Height);
        }
        catch (Exception e) when (e is ArgumentException or InvalidOperationException or COMException)
        {
            return null;
        }
    }

    /// <summary>The window's DIP-to-pixel scale on the active viewer window's screen.</summary>
    private double Scale
    {
        get
        {
            double s = ActiveViewer?.Window.Content?.XamlRoot?.RasterizationScale ?? 1;
            return s > 0 ? s : 1;
        }
    }

    /// <summary>The frame for a new call window with <paramref name="index"/> windows open already (physical pixels).</summary>
    public WindowRect Frame(int index)
    {
        List<WindowRect> screens = [.. ViewerWindow.Screens().Select(r => new WindowRect(r.X, r.Y, r.Width, r.Height))];
        WindowRect? remembered = RememberedFrame;
        CallWindowPlacement.Mode mode = PlacementMode;
        if (remembered is null)
        {
            // No frame yet: the default size (scaled for the display) at the top right.
            remembered = new WindowRect(0, 0, CallWindowPlacement.DefaultWidth * Scale, CallWindowPlacement.DefaultHeight * Scale);
            mode = CallWindowPlacement.Mode.ActiveScreen;
        }
        return CallWindowPlacement.Frame(mode, remembered, screens, ActiveWorkArea(), index);
    }
}

/// <summary>One call's window (see the file comment).</summary>
internal sealed class CallWindow
{
    private const double MinWidth = 320, MinHeight = 280;

    private readonly CallWindows _owner;
    private readonly Viewer _viewer;
    private readonly bool _background;
    private readonly SwapChainPanel _panel = new();
    private readonly Grid _root = new() { Background = new SolidColorBrush(Microsoft.UI.Colors.Black) };
    private OverlayTile[] _published = [];
    private WindowRect? _placed;
    private bool _placing;
    private bool _released;
    private bool _askedMain;
    private double? _subWidth;

    public CallWindow(CallWindows owner, Viewer viewer, CoreHost host, CameraRef camera, bool background)
    {
        _owner = owner;
        _viewer = viewer;
        _background = background;
        Camera = camera;
        Window = new Window { Title = "nvrview" };
        AppWindow = Window.AppWindow;
        if (AppWindow.Presenter is OverlappedPresenter presenter)
        {
            presenter.IsMaximizable = false;
        }
        Card = new CallCard(viewer, camera) { Mode = CallCardMode.Window, Placed = true };
        Card.PinChanged += on => owner.Pinned = on;
        _root.RequestedTheme = ElementTheme.Dark;
        _root.Children.Add(_panel);
        _root.Children.Add(Card);
        _root.KeyDown += (_, e) =>
        {
            _viewer.Calls.Use(Camera);
            e.Handled = HandleKey(e.Key);
        };
        Window.Content = _root;

        // The doorbell's tile stays open while the window shows it; its demand starts at the sub stream.
        Card.Tile.Shared = viewer.Leases.Acquire(camera);
        viewer.UpdateDemand(Card.Tile);
        viewer.Leases.TileChanged += OnTileChanged;

        DispatcherQueue ui = DispatcherQueue.GetForCurrentThread();
        Renderer = new StageRenderer(host, chain => ui.TryEnqueue(() =>
        {
            if (_released)
            {
                if (chain != 0)
                {
                    Marshal.Release(chain);
                }
                return;
            }
            SwapChainAttach.To(_panel, chain);
        }));
        _panel.SizeChanged += (_, _) => PushSize();
        _panel.CompositionScaleChanged += (_, _) => PushSize();
        AppWindow.Changed += OnAppWindowChanged;
        Window.Closed += (_, _) =>
        {
            // Closing the window itself ends the call; the manager closes it for every other reason.
            if (!_released)
            {
                Release();
                owner.ClosedByUser(this);
            }
        };
        Microsoft.UI.Xaml.Media.CompositionTarget.Rendering += OnRendering;
        Renderer.Start();
    }

    public CameraRef Camera { get; }

    public Window Window { get; }

    public AppWindow AppWindow { get; }

    public StageRenderer Renderer { get; }

    /// <summary>The call's card in its Window mode: header with Pop In and the pin, the picture, captions, Talk, and the reply.</summary>
    public CallCard Card { get; }

    /// <summary>Whether the window asks for the main stream now (self-test).</summary>
    public bool AsksForMainStream => _askedMain;

    /// <summary>The pin as applied to the window (self-test; a test run never floats, so the native flag is not set there).</summary>
    public bool Pinned { get; private set; }

    /// <summary>The frame the app placed the window at (self-test).</summary>
    public WindowRect? PlacedFrame => _placed;

    /// <summary>The window's frame as it is now, in pixels.</summary>
    public WindowRect CurrentFrame => new(AppWindow.Position.X, AppWindow.Position.Y, AppWindow.Size.Width, AppWindow.Size.Height);

    public bool IsReleased => _released;

    private void PushSize() =>
        Renderer.SetPanelSize(_panel.ActualWidth, _panel.ActualHeight, _panel.CompositionScaleX, _panel.CompositionScaleY);

    private void OnTileChanged(CameraRef camera, CameraTile? tile)
    {
        if (!_released && camera.ChannelKey == Camera)
        {
            Card.Tile.Shared = tile;
        }
    }

    // ---- placement and showing ----

    /// <summary>Moves and resizes the window without remembering the frame.</summary>
    public void Place(WindowRect frame)
    {
        _placing = true;
        try
        {
            AppWindow.MoveAndResize(new RectInt32((int)Math.Round(frame.X), (int)Math.Round(frame.Y), (int)Math.Round(frame.Width), (int)Math.Round(frame.Height)));
        }
        finally
        {
            _placing = false;
        }
        _placed = frame;
    }

    /// <summary>
    /// Shows the window. A ring's window is shown without activation (the user keeps typing where
    /// they are); an explicit Pop Out or Start Conversation activates it. A test run never takes the
    /// focus: the window goes behind the others.
    /// </summary>
    public void Show(bool activate)
    {
        if (_background)
        {
            AppWindow.Show(false);
            nint hwnd = WinRT.Interop.WindowNative.GetWindowHandle(Window);
            unsafe
            {
                _ = SetWindowPos((TerraFX.Interop.Windows.HWND)hwnd, TerraFX.Interop.Windows.HWND.HWND_BOTTOM, 0, 0, 0, 0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        else
        {
            AppWindow.Show(activate);
        }
        // The reply field waits for a click or Tab instead of taking the typing the window opened over (Esc ends the call).
        _ = Card.Focus(FocusState.Programmatic);
    }

    /// <summary>On top of other windows while pinned (also over full-screen apps is unmeasured); an ordinary window otherwise. A test run never floats.</summary>
    public void ApplyPinned(bool on)
    {
        Pinned = on;
        Card.SetPinned(on);
        if (!_background && AppWindow.Presenter is OverlappedPresenter presenter)
        {
            presenter.IsAlwaysOnTop = on;
        }
    }

    private void OnAppWindowChanged(AppWindow sender, AppWindowChangedEventArgs args)
    {
        if (_released || _placing || !(args.DidPositionChange || args.DidSizeChange))
        {
            return;
        }
        if (AppWindow.Presenter is not OverlappedPresenter { State: OverlappedPresenterState.Restored })
        {
            return;
        }
        double scale = _panel.XamlRoot?.RasterizationScale ?? 1;
        if (args.DidSizeChange && (AppWindow.Size.Width < MinWidth * scale || AppWindow.Size.Height < MinHeight * scale))
        {
            _placing = true;
            try
            {
                AppWindow.Resize(new SizeInt32(
                    (int)Math.Max(AppWindow.Size.Width, MinWidth * scale), (int)Math.Max(AppWindow.Size.Height, MinHeight * scale)));
            }
            finally
            {
                _placing = false;
            }
        }
        WindowRect frame = CurrentFrame;
        if (_placed is { } p && Math.Abs(p.X - frame.X) < 1 && Math.Abs(p.Y - frame.Y) < 1
            && Math.Abs(p.Width - frame.Width) < 1 && Math.Abs(p.Height - frame.Height) < 1)
        {
            return;
        }
        _owner.Moved(frame);
    }

    // ---- showing the call ----

    /// <summary>Shows the call as it is now.</summary>
    public void Refresh()
    {
        if (_released || _viewer.Calls.CallFor(Camera) is not { } call)
        {
            return;
        }
        Card.Update();
        string title = $"{(call.Origin == CallOrigin.Conversation && call.Presses == 0 ? "Conversation" : "Doorbell")} · {_viewer.CameraName(Camera)}";
        if (Window.Title != title)
        {
            Window.Title = title;
        }
    }

    // Every UI frame: where the picture's hole is (handed to this window's renderer), and the
    // stream demand for the size it is drawn at.
    private void OnRendering(object? sender, object e)
    {
        if (_released)
        {
            return;
        }
        Rect? hole = Card.HoleIn(_panel);
        OverlayTile[] now = hole is { } r ? [new OverlayTile(Card.Tile, r)] : [];
        if (!now.SequenceEqual(_published))
        {
            _published = now;
            Renderer.Overlays = now;
        }
        if (Card.Tile.PictureWidth > 0 && Card.Tile.ShownTier == "sub")
        {
            _subWidth = Card.Tile.PictureWidth;
        }
        UpdateDemand((hole?.Width ?? 0) * _panel.CompositionScaleX);
    }

    /// <summary>Auto for the window: the main stream while the picture area is wider than the sub-stream's pictures.</summary>
    private void UpdateDemand(double drawnWidth)
    {
        bool main = CallWindowQuality.WantsMain(drawnWidth, _subWidth, _askedMain);
        if (main != _askedMain)
        {
            _askedMain = main;
            _viewer.SetFocused(Card.Tile, main);
        }
    }

    // ---- keys and closing ----

    /// <summary>
    /// Esc ends the call unless the reply field is being edited (then it only leaves the field);
    /// Ctrl+W closes the window, which ends the call. Tab moves between the controls and Return
    /// sends the reply (the card's own).
    /// </summary>
    public bool HandleKey(VirtualKey key)
    {
        if (key == VirtualKey.Escape)
        {
            if (Card.IsEditingReply)
            {
                _ = Card.Focus(FocusState.Programmatic);
            }
            else
            {
                _owner.EndCall(this);
            }
            return true;
        }
        if (key == VirtualKey.W && InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control).HasFlag(Windows.UI.Core.CoreVirtualKeyStates.Down))
        {
            _owner.EndCall(this);
            return true;
        }
        return false;
    }

    /// <summary>Closes the window (idempotent) and gives its lease back; the call itself is untouched.</summary>
    public void Close()
    {
        if (_released)
        {
            return;
        }
        Release();
        Window.Close();
    }

    // Stops drawing and gives back everything the window holds.
    private void Release()
    {
        _released = true;
        Microsoft.UI.Xaml.Media.CompositionTarget.Rendering -= OnRendering;
        _viewer.Leases.TileChanged -= OnTileChanged;
        AppWindow.Changed -= OnAppWindowChanged;
        Card.Tile.Shared = null;
        Renderer.Overlays = [];
        Renderer.Dispose();
        SwapChainAttach.To(_panel, 0);
        _viewer.WithdrawDemand(Card.Tile);
        _viewer.Leases.Release(Camera);
    }
}
