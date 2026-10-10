// A window's stage: the SwapChainPanel its render thread presents into, the
// tile chrome above it, and the input that drives both (docs/UX.md):
//
// - the window's split tree (WindowState.Tree) laid out into cells; one
//   StageTile per leaf, pointing at its camera's shared core tile;
// - hover shows a tile's header (speaker, name) and fades it 3 s after the
//   pointer stops or leaves; a click selects; a double-click (or Space, or
//   Enter) focuses the tile with an animation of the same live tile and,
//   once it ends, has the leases switch its camera to the focus tier;
// - the designer: drag a divider (double-click evens it), drag a tile onto
//   another to swap them, split, remove, clear, show a camera, presets (the
//   context menu, the edit bar, and Delete in Edit Layout mode); cameras
//   dragged from the sidebar drop onto a tile;
// - mouse wheel (or a touchpad pinch, which arrives as Ctrl+wheel) zooms
//   around the pointer and dragging pans a zoomed tile, by changing only the
//   source rectangle the renderer samples;
// - the tile features: the corner indicators and sound badges, the alert strip
//   and its Dismiss (Enter), the Quality menu (kept in the layout leaf), M to
//   listen, and, in full screen, the chrome and the pointer hiding 3 s after
//   the last interaction (alert strips and indicators stay).
//
// Keys reach it through the window (ViewerWindow.OnKey -> HandleKey).

using System.Diagnostics;
using System.Runtime.InteropServices;
using Microsoft.UI;
using Microsoft.UI.Input;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using NvrView.Interop;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.ApplicationModel.DataTransfer;
using Windows.System;
using Windows.UI.Core;
using Point = Windows.Foundation.Point;
using Rect = NvrView.Shell.Rect;

namespace NvrView.App;

/// <summary>One window's arrangement and what the user is doing with it.</summary>
internal sealed class WindowState(WindowContent content, string name, LayoutNode tree)
{
    public WindowContent Content { get; set; } = content;

    public string Name { get; set; } = name;

    public LayoutNode Tree { get; set; } = tree;

    public NodePath? Selected { get; set; }

    public bool Editing { get; set; }
}

internal sealed partial class StageView : UserControl
{
    // ISwapChainPanelNative of WinUI 3 (microsoft.ui.xaml.media.dxinterop.h).
    private static readonly Guid SwapChainPanelNativeId = new("63aad0b8-7c24-40ff-85a8-640d944cc325");

    private static readonly SolidColorBrush EditOutline = new(Windows.UI.Color.FromArgb(0x90, 0x9A, 0xA4, 0xB0));
    private static readonly SolidColorBrush DropBrush = new(Windows.UI.Color.FromArgb(0x50, 0x3A, 0x96, 0xDD));
    private static readonly SolidColorBrush DropOutline = new(Windows.UI.Color.FromArgb(0xF0, 0x3A, 0x96, 0xDD));

    private readonly Viewer _viewer;
    private readonly AppWindow _appWindow;
    private readonly SwapChainPanel _panel = new();
    private readonly Canvas _editLayer = new() { IsHitTestVisible = false };
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
    private readonly List<Rectangle> _outlines = [];
    private readonly DispatcherQueueTimerHolder _timer;
    private StageTile[] _tiles = [];
    private LayoutFrames _frames = LayoutFrames.None;
    private StageTile? _hovered;
    private readonly ChromeIdle _idle;
    private bool _cursorHidden;
    private bool _diagnostics;
    private NodePath? _dropTarget;
    private StageTile? _focusDemand;

    private enum GestureKind
    {
        None,
        Divider,
        TileDrag,
        Pan,
    }

    private GestureKind _gesture;
    private NodePath _gesturePath;
    private Point _gestureFrom;
    private bool _dragging;
    private StageTile? _panTile;
    private float _panCenterX, _panCenterY;

    public StageView(Viewer viewer, AppWindow appWindow, WindowState state)
    {
        _viewer = viewer;
        _appWindow = appWindow;
        _idle = new ChromeIdle(viewer.Clock);
        State = state;
        IsTabStop = true;
        UseSystemFocusVisuals = false;
        AllowDrop = true;
        Background = new SolidColorBrush(Colors.Black);
        var root = new Grid();
        root.Children.Add(_panel);
        root.Children.Add(_editLayer);
        root.Children.Add(_chromeLayer);

        var add = new Button { Content = "Add a device…", HorizontalAlignment = HorizontalAlignment.Center };
        add.Click += (_, _) => SettingsRequested?.Invoke();
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
                    "S                            show or hide the sidebar",
                    "Esc                          leave focus, editing, then full screen",
                    "Mouse wheel, drag            digital zoom and pan; 0 resets",
                    "M, Ctrl+L, speaker button    listen (Ctrl+click on the speaker listens to several)",
                    "Enter                        dismiss the selected tile's alert (else focus)",
                    "Right-click > Quality        Auto, Clear, Fluent, Balanced for the tile",
                    "Ctrl+E                       edit the layout (drag dividers and tiles)",
                    "Delete                       remove the selected tile while editing",
                    "Ctrl+N                       new window",
                    "Ctrl+D                       diagnostics",
                    "Ctrl+,                       Settings (devices)",
                    "F1                           this list"),
            },
        };
        root.Children.Add(_help);
        Content = root;

        Renderer = null!;
        _panel.SizeChanged += (_, _) =>
        {
            PushSize();
            Relayout();
        };
        _panel.CompositionScaleChanged += (_, _) => PushSize();
        _viewer.Changed += RefreshAll;
        _viewer.Leases.TileChanged += OnTileChanged;
        _viewer.IndicatorsChanged += SyncOverlays;
        _viewer.Settings.Changed += SyncOverlays;
        _timer = new DispatcherQueueTimerHolder(DispatcherQueue, TimeSpan.FromMilliseconds(250), Tick);
        Loaded += (_, _) => Microsoft.UI.Xaml.Media.CompositionTarget.Rendering += OnRendering;
        Unloaded += (_, _) => Microsoft.UI.Xaml.Media.CompositionTarget.Rendering -= OnRendering;
        DragOver += OnDragOver;
        DragLeave += (_, _) => SetDropTarget(null);
        Drop += OnDrop;
    }

    /// <summary>The window's arrangement.</summary>
    public WindowState State { get; }

    /// <summary>The render thread (set once by the window).</summary>
    public StageRenderer Renderer { get; set; }

    /// <summary>The Settings dialog opener (set by the window).</summary>
    public Action? SettingsRequested { get; set; }

    /// <summary>The tree changed by a finished gesture or command (the window stores it).</summary>
    public event Action<LayoutNode>? Edited;

    /// <summary>Selection, focus, or editing changed (the edit bar follows).</summary>
    public event Action? StateChanged;

    /// <summary>
    /// Called while a tile's context menu is built, after the tile's own
    /// entries (Listen, Focus, Reset zoom) and before the layout entries, so
    /// other features can add theirs (a Quality submenu, for one).
    /// </summary>
    public static event Action<MenuFlyout, StageTile, StageView>? TileMenuOpening;

    /// <summary>This window's tiles, one per leaf, in reading order.</summary>
    public StageTile[] Tiles => _tiles;

    /// <summary>The selected tile (keyboard actions apply to it).</summary>
    public StageTile? Selected => State.Selected is { } p ? _tiles.FirstOrDefault(t => t.Path == p) : null;

    /// <summary>True while a tile is focused (or animating into focus).</summary>
    public bool IsFocused => Renderer.Focus is { Entering: true };

    /// <summary>True while a divider or tile drag is in progress (the window does not follow stored changes then).</summary>
    public bool IsInteracting => _gesture is GestureKind.Divider or GestureKind.TileDrag;

    /// <summary>Frames the UI thread updated the chrome (for the self-test).</summary>
    public long ChromeFrames { get; private set; }

    /// <summary>The gap between tiles: wider while editing, so dividers are easy to grab.</summary>
    public double Gap => State.Editing ? 6 : 2;

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

    // ---- tiles and layout ----

    /// <summary>Rebuilds the tiles for the current tree (after the window's content or tree changed).</summary>
    public void Reload()
    {
        IReadOnlyList<LeafInfo> leaves = State.Tree.Leaves;
        var old = _tiles;
        var next = new StageTile[leaves.Count];
        for (int i = 0; i < leaves.Count; i++)
        {
            LeafInfo leaf = leaves[i];
            TileQuality quality = QualityOf(State.Tree, leaf.Path);
            StageTile? same = old.FirstOrDefault(t => t.Path == leaf.Path && t.CameraOrNone == leaf.Camera);
            if (same is not null)
            {
                next[i] = same;
                _viewer.SetQuality(same, quality);
                continue;
            }
            var t = new StageTile(_viewer, leaf.Path, leaf.Camera, quality);
            t.Shared = leaf.Camera is { } c ? _viewer.Leases.Tile(c) : null;
            // The zoom follows the camera through layout changes (docs/UX.md "Digital zoom").
            if (old.FirstOrDefault(o => o.CameraOrNone is not null && o.CameraOrNone == leaf.Camera) is { } before)
            {
                t.Zoom = before.Zoom;
                t.CenterX = before.CenterX;
                t.CenterY = before.CenterY;
            }
            next[i] = t;
            _viewer.UpdateDemand(t);
        }
        foreach (StageTile gone in old.Where(o => Array.IndexOf(next, o) < 0))
        {
            _viewer.WithdrawDemand(gone);
        }
        _tiles = next;
        if (State.Selected is { } s && State.Tree[s] is not LayoutLeaf)
        {
            State.Selected = null;
        }
        if (Renderer.Focus is { } f && Array.IndexOf(next, f.Tile) < 0)
        {
            CancelFocus();
        }
        Relayout();
        SyncChromes();
        RefreshAll();
        StateChanged?.Invoke();
    }

    /// <summary>Lays the tree out for the panel's size and hands the cells to the renderer.</summary>
    private void Relayout()
    {
        double w = _panel.ActualWidth, h = _panel.ActualHeight;
        _frames = State.Tree.Frames(new Rect(0, 0, Math.Max(w, 0), Math.Max(h, 0)), Gap);
        var cells = new Rect[_tiles.Length];
        for (int i = 0; i < _tiles.Length; i++)
        {
            cells[i] = _frames.Leaf(_tiles[i].Path)?.Rect ?? default;
        }
        if (Renderer is not null)
        {
            Renderer.Layout = new StageLayout(_tiles, cells, w, h);
        }
        PlaceOutlines();
    }

    private void OnTileChanged(CameraRef camera, CameraTile? tile)
    {
        foreach (StageTile t in _tiles)
        {
            if (t.CameraOrNone == camera)
            {
                t.Shared = tile;
            }
        }
    }

    // ---- editing ----

    /// <summary>Applies a change to the tree; null from the change means nothing to do.</summary>
    public void Edit(Func<LayoutNode, LayoutNode?> change)
    {
        if (change(State.Tree) is not { } tree || ReferenceEquals(tree, State.Tree))
        {
            return;
        }
        State.Tree = tree;
        Reload();
        Edited?.Invoke(tree);
    }

    /// <summary>The selected tile, or the only one.</summary>
    private NodePath? Target => State.Selected ?? (State.Tree.LeafCount == 1 ? NodePath.Root : null);

    public bool CanSplit => Target is not null && State.Tree.LeafCount < LayoutNode.MaxLeaves;

    public void Split(SplitAxis axis)
    {
        if (Target is { } path && State.Tree.Splitting(path, axis) is { } s)
        {
            State.Selected = s.NewLeaf;
            Edit(_ => s.Tree);
        }
    }

    public void RemoveSelected()
    {
        if (State.Selected is { } path && State.Tree.Removing(path) is { } r)
        {
            State.Selected = r.Tree[r.Survivor] is LayoutLeaf ? r.Survivor : null;
            Edit(_ => r.Tree);
        }
    }

    public void Assign(CameraRef? camera, NodePath path) => Edit(t => t.Assigning(camera, path));

    public void Swap(NodePath from, NodePath to)
    {
        State.Selected = to;
        Edit(t => t.Swapping(from, to));
    }

    public void ApplyPreset(LayoutPreset preset)
    {
        List<CameraRef> cameras = [.. State.Tree.CameraList.OfType<CameraRef>()];
        State.Selected = null;
        Edit(_ => preset.Tree(cameras));
    }

    /// <summary>Sets (or with null removes) an app member of the tile at <paramref name="path"/>, saved with the layout (a tile's quality, for one).</summary>
    public void SetTileMember(NodePath path, string name, string? json) => Edit(t => t.SettingMember(path, name, json));

    public void Select(NodePath? path)
    {
        State.Selected = path;
        Interact();
        UpdateHover();
        StateChanged?.Invoke();
    }

    /// <summary>Enters or leaves Edit Layout mode.</summary>
    public void SetEditing(bool editing)
    {
        if (State.Editing == editing)
        {
            return;
        }
        if (IsFocused)
        {
            Unfocus();
        }
        State.Editing = editing;
        if (editing && State.Selected is null)
        {
            State.Selected = State.Tree.Leaves.FirstOrDefault().Path;
        }
        Relayout();
        RefreshAll();
        StateChanged?.Invoke();
    }

    // ---- focus ----

    public void ToggleFocus(StageTile tile)
    {
        if (Renderer.Focus is { Entering: true } f && f.Tile == tile)
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
        if (State.Editing)
        {
            return;
        }
        long now = Stopwatch.GetTimestamp();
        FocusAnimation? current = Renderer.Focus;
        State.Selected = tile.Path;
        if (current is { Entering: true } && current.Tile != tile)
        {
            // Switch the focused camera: no animation, tiers swap at once.
            Renderer.Focus = new FocusAnimation(tile, now, true, 1);
            SetFocusDemand(tile);
            RefreshAll();
            StateChanged?.Invoke();
            return;
        }
        float from = current is not null && current.Tile == tile ? current.Progress(now) : 0;
        var animation = new FocusAnimation(tile, now, true, from);
        Renderer.Focus = animation;
        // The focus tier once the animation is over (docs/UX.md).
        After((1 - from) * FocusAnimation.Seconds, () =>
        {
            if (ReferenceEquals(Renderer.Focus, animation))
            {
                SetFocusDemand(tile);
            }
        });
        RefreshAll();
        StateChanged?.Invoke();
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
        // The grid tier is ready almost at once; the tile shrinks meanwhile.
        SetFocusDemand(null);
        After(p * FocusAnimation.Seconds + 0.05, () =>
        {
            if (ReferenceEquals(Renderer.Focus, animation))
            {
                Renderer.Focus = null;
            }
        });
        RefreshAll();
        StateChanged?.Invoke();
    }

    /// <summary>
    /// Ends focus at once, without animation (the window's content is being
    /// replaced, or the focused tile went away). The stage keeps drawing:
    /// nothing is torn down, only the focus ends.
    /// </summary>
    public void CancelFocus()
    {
        Renderer.Focus = null;
        SetFocusDemand(null);
    }

    // The focused tile asks the tier planner for the focus tier (Auto: main).
    private void SetFocusDemand(StageTile? tile)
    {
        if (ReferenceEquals(_focusDemand, tile))
        {
            return;
        }
        if (_focusDemand is { } old)
        {
            _viewer.SetFocused(old, false);
        }
        _focusDemand = tile;
        if (tile is not null)
        {
            _viewer.SetFocused(tile, true);
        }
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

    public void ToggleFullScreen() =>
        _appWindow.SetPresenter(_appWindow.Presenter.Kind == AppWindowPresenterKind.FullScreen ? AppWindowPresenterKind.Overlapped : AppWindowPresenterKind.FullScreen);

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

    // ---- pointer ----

    /// <summary>The drawn tile under a point (the focused one on top).</summary>
    private StageTile? TileAt(Point p)
    {
        if (Renderer.Focus is { } f && f.Tile.Visible && Contains(f.Tile, p))
        {
            return f.Tile;
        }
        if (Renderer.Focus is { Entering: true })
        {
            return null;
        }
        // By cell, so empty tiles and letterbox bars count.
        if (_frames.LeafAt(p.X, p.Y) is { } leaf)
        {
            return _tiles.FirstOrDefault(t => t.Path == leaf.Path);
        }
        return null;
    }

    private static bool Contains(StageTile t, Point p) =>
        p.X >= t.X && p.Y >= t.Y && p.X < t.X + t.Width && p.Y < t.Y + t.Height;

    private bool CanEditLayout => Renderer.Focus is null;

    protected override void OnPointerMoved(PointerRoutedEventArgs e)
    {
        PointerPoint pp = e.GetCurrentPoint(this);
        Point p = pp.Position;
        Interact();
        switch (_gesture)
        {
            case GestureKind.Divider when _frames.Dividers.FirstOrDefault(d => d.Path == _gesturePath) is { Region.Width: > 0 } d:
                double ratio = LayoutGeometry.RatioDragging(d, p.X, p.Y, Gap);
                if (State.Tree.SettingRatio(ratio, _gesturePath) is { } tree)
                {
                    State.Tree = tree;
                    Relayout();
                }
                break;
            case GestureKind.TileDrag:
                if (!_dragging && Math.Abs(p.X - _gestureFrom.X) + Math.Abs(p.Y - _gestureFrom.Y) > 6)
                {
                    _dragging = true;
                }
                if (_dragging)
                {
                    NodePath? target = _frames.LeafAt(p.X, p.Y)?.Path;
                    SetDropTarget(target == _gesturePath ? null : target);
                }
                break;
            case GestureKind.Pan when _panTile is { Width: > 0 } drag:
                double dx = (p.X - _gestureFrom.X) / drag.Width / drag.Zoom, dy = (p.Y - _gestureFrom.Y) / drag.Height / drag.Zoom;
                double half = 0.5 / drag.Zoom;
                drag.CenterX = (float)Math.Clamp(_panCenterX - dx, half, 1 - half);
                drag.CenterY = (float)Math.Clamp(_panCenterY - dy, half, 1 - half);
                break;
        }
        _hovered = TileAt(p);
        if (_gesture == GestureKind.None)
        {
            DividerFrame? over = CanEditLayout ? _frames.DividerAt(p.X, p.Y, 8) : null;
            ProtectedCursor = over is { } dv
                ? InputSystemCursor.Create(dv.Axis == SplitAxis.Row ? InputSystemCursorShape.SizeWestEast : InputSystemCursorShape.SizeNorthSouth)
                : null;
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
        if (!pp.Properties.IsLeftButtonPressed)
        {
            return;
        }
        Point p = pp.Position;
        _gestureFrom = p;
        _dragging = false;
        if (CanEditLayout && _frames.DividerAt(p.X, p.Y, 8) is { } d)
        {
            _gesture = GestureKind.Divider;
            _gesturePath = d.Path;
            _ = CapturePointer(e.Pointer);
            return;
        }
        if (TileAt(p) is not { } t)
        {
            return;
        }
        Select(t.Path);
        if (t.Zoom > 1 && !State.Editing)
        {
            _gesture = GestureKind.Pan;
            _panTile = t;
            _panCenterX = t.CenterX;
            _panCenterY = t.CenterY;
        }
        else if (CanEditLayout)
        {
            _gesture = GestureKind.TileDrag;
            _gesturePath = t.Path;
        }
        else
        {
            return;
        }
        _ = CapturePointer(e.Pointer);
    }

    protected override void OnPointerReleased(PointerRoutedEventArgs e)
    {
        Point p = e.GetCurrentPoint(this).Position;
        GestureKind gesture = _gesture;
        _gesture = GestureKind.None;
        _panTile = null;
        ReleasePointerCapture(e.Pointer);
        switch (gesture)
        {
            case GestureKind.Divider:
                Edited?.Invoke(State.Tree);
                break;
            case GestureKind.TileDrag when _dragging:
                SetDropTarget(null);
                if (_frames.LeafAt(p.X, p.Y)?.Path is { } to && to != _gesturePath)
                {
                    Swap(_gesturePath, to);
                }
                break;
        }
        _dragging = false;
    }

    protected override void OnPointerCaptureLost(PointerRoutedEventArgs e)
    {
        if (_gesture == GestureKind.Divider)
        {
            Edited?.Invoke(State.Tree);
        }
        _gesture = GestureKind.None;
        _dragging = false;
        _panTile = null;
        SetDropTarget(null);
    }

    protected override void OnDoubleTapped(DoubleTappedRoutedEventArgs e)
    {
        if (e.OriginalSource is FrameworkElement { Parent: Button } or Button or FontIcon)
        {
            return;
        }
        Point p = e.GetPosition(this);
        if (CanEditLayout && _frames.DividerAt(p.X, p.Y, 8) is { } d)
        {
            Edit(t => t.SettingRatio(0.5, d.Path));
            e.Handled = true;
            return;
        }
        if (!State.Editing && TileAt(p) is { } t && (!t.IsEmpty || IsFocused))
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

    // ---- drag and drop (cameras from the sidebar) ----

    private void OnDragOver(object sender, DragEventArgs e)
    {
        Point p = e.GetPosition(this);
        if (CanEditLayout && e.DataView.Contains(StandardDataFormats.Text) && _frames.LeafAt(p.X, p.Y) is { } leaf)
        {
            e.AcceptedOperation = DataPackageOperation.Copy;
            e.DragUIOverride.Caption = "Show here";
            SetDropTarget(leaf.Path);
        }
        else
        {
            e.AcceptedOperation = DataPackageOperation.None;
            SetDropTarget(null);
        }
    }

    private async void OnDrop(object sender, DragEventArgs e)
    {
        Point p = e.GetPosition(this);
        SetDropTarget(null);
        if (!CanEditLayout || !e.DataView.Contains(StandardDataFormats.Text) || _frames.LeafAt(p.X, p.Y) is not { } leaf)
        {
            return;
        }
        DragOperationDeferral deferral = e.GetDeferral();
        try
        {
            if (CameraRefs.FromDragText(await e.DataView.GetTextAsync()) is { } camera)
            {
                Select(leaf.Path);
                Assign(camera, leaf.Path);
            }
        }
        finally
        {
            deferral.Complete();
        }
    }

    private void SetDropTarget(NodePath? path)
    {
        if (_dropTarget != path)
        {
            _dropTarget = path;
            PlaceOutlines();
        }
    }

    /// <summary>The edit-mode outlines and the drop or swap target, over the cells.</summary>
    private void PlaceOutlines()
    {
        IReadOnlyList<LeafFrame> leaves = _frames.Leaves;
        while (_outlines.Count < leaves.Count)
        {
            var r = new Rectangle { StrokeThickness = 1, StrokeDashArray = [4, 3], IsHitTestVisible = false };
            _outlines.Add(r);
            _editLayer.Children.Add(r);
        }
        for (int i = 0; i < _outlines.Count; i++)
        {
            Rectangle r = _outlines[i];
            if (i >= leaves.Count)
            {
                r.Visibility = Visibility.Collapsed;
                continue;
            }
            LeafFrame f = leaves[i];
            bool drop = _dropTarget == f.Path;
            r.Visibility = State.Editing || drop ? Visibility.Visible : Visibility.Collapsed;
            r.Stroke = drop ? DropOutline : EditOutline;
            r.StrokeThickness = drop ? 2 : 1;
            r.Fill = drop ? DropBrush : null;
            r.Width = Math.Max(f.Rect.Width, 0);
            r.Height = Math.Max(f.Rect.Height, 0);
            Canvas.SetLeft(r, f.Rect.X);
            Canvas.SetTop(r, f.Rect.Y);
        }
    }

    // ---- context menu ----

    protected override void OnRightTapped(RightTappedRoutedEventArgs e)
    {
        Point at = e.GetPosition(this);
        StageTile? t = TileAt(at);
        if (t is not null)
        {
            Select(t.Path);
        }
        var menu = new MenuFlyout();
        MenuFlyoutItem Item(IList<MenuFlyoutItemBase> into, string text, Action action, bool enabled = true)
        {
            var item = new MenuFlyoutItem { Text = text, IsEnabled = enabled };
            item.Click += (_, _) => action();
            into.Add(item);
            return item;
        }
        if (t is { CameraOrNone: { } camera })
        {
            Item(menu.Items, IsFocused && Renderer.Focus?.Tile == t ? "Back to the grid" : "Focus", () => ToggleFocus(t), !State.Editing);
            Item(menu.Items, t.Audible ? "Stop listening" : "Listen", () => _viewer.ToggleListening(camera, false));
            Item(menu.Items, "Reset zoom", () => ResetZoom(t), t.Zoom > 1);
            menu.Items.Add(QualityMenu.Create(t.Quality, _viewer.OffersBalanced(t), q => SetQuality(t, q)));
            if (_viewer.Alerts.IsActive(camera))
            {
                Item(menu.Items, "Dismiss alert", () => _viewer.DismissAlert(camera));
            }
        }
        if (t is not null)
        {
            TileMenuOpening?.Invoke(menu, t, this);
            menu.Items.Add(new MenuFlyoutSeparator());
        }
        if (t is not null && CanEditLayout)
        {
            Item(menu.Items, "Split side by side", () => Split(SplitAxis.Row), CanSplit);
            Item(menu.Items, "Split stacked", () => Split(SplitAxis.Col), CanSplit);
            Item(menu.Items, "Remove tile", RemoveSelected, State.Tree.LeafCount > 1 || !t.IsEmpty);
            if (!t.IsEmpty)
            {
                Item(menu.Items, "Clear tile", () => Assign(null, t.Path));
            }
            var show = new MenuFlyoutSubItem { Text = "Show camera" };
            foreach (DeviceEntry d in _viewer.Devices)
            {
                List<CameraRef> cameras = [.. _viewer.ListedChannels(d).SelectMany(c => Viewer.CamerasOf(d, c))];
                if (cameras.Count == 0)
                {
                    continue;
                }
                if (show.Items.Count > 0)
                {
                    show.Items.Add(new MenuFlyoutSeparator());
                }
                show.Items.Add(new MenuFlyoutItem { Text = d.DisplayName, IsEnabled = false });
                foreach (CameraRef c in cameras)
                {
                    var item = new ToggleMenuFlyoutItem { Text = _viewer.CameraName(c), IsChecked = t.CameraOrNone == c };
                    item.Click += (_, _) => Assign(c, t.Path);
                    show.Items.Add(item);
                }
            }
            show.IsEnabled = show.Items.Count > 0;
            menu.Items.Add(show);
            var presets = new MenuFlyoutSubItem { Text = "Layout preset" };
            foreach (LayoutPreset preset in LayoutPresets.All)
            {
                Item(presets.Items, preset.Title(), () => ApplyPreset(preset));
            }
            menu.Items.Add(presets);
            menu.Items.Add(new MenuFlyoutSeparator());
        }
        WindowMenu?.Invoke(menu);
        menu.Items.Add(IndicatorCornerMenu());
        Item(menu.Items, IsFullScreen ? "Leave full screen" : "Full screen", ToggleFullScreen);
        Item(menu.Items, _diagnostics ? "Hide diagnostics" : "Show diagnostics", ToggleDiagnostics);
        Item(menu.Items, "Keyboard shortcuts", ToggleHelp);
        Item(menu.Items, "Settings…", () => SettingsRequested?.Invoke());
        menu.ShowAt(this, at);
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

    /// <summary>A tile's quality (the Quality menu): kept in the layout leaf ("quality"; Auto is no member), asked of the tier planner.</summary>
    public void SetQuality(StageTile tile, TileQuality quality)
    {
        string? member = quality.ToMember();
        Edit(t => t.SettingMember(tile.Path, "quality", member is null ? null : System.Text.Json.Nodes.JsonValue.Create(member).ToJsonString()));
        _viewer.SetQuality(tile, quality);
    }

    /// <summary>The quality a layout leaf keeps (missing, or a value this app does not know, reads as Auto).</summary>
    public static TileQuality QualityOf(LayoutNode tree, NodePath path)
    {
        string? json = tree.Member(path, "quality");
        if (json is null)
        {
            return TileQuality.Auto;
        }
        try
        {
            return TileQualities.FromMember(System.Text.Json.Nodes.JsonNode.Parse(json) is System.Text.Json.Nodes.JsonValue v && v.TryGetValue(out string? text) ? text : null);
        }
        catch (System.Text.Json.JsonException)
        {
            return TileQuality.Auto;
        }
    }

    /// <summary>The window's own entries for the context menu (Edit Layout, Save as Layout, sidebar, new window).</summary>
    public Action<MenuFlyout>? WindowMenu { get; set; }

    // ---- keys (from the window) ----

    /// <summary>A key for the stage; true if it was used.</summary>
    public bool HandleKey(VirtualKey key, bool ctrl, bool repeat)
    {
        Interact();
        StageTile? s = Selected;
        if (s is null && _tiles.Length > 0 && key is VirtualKey.Left or VirtualKey.Right or VirtualKey.Up or VirtualKey.Down or VirtualKey.Space or VirtualKey.Enter or VirtualKey.M)
        {
            Select(_tiles[0].Path);
            s = _tiles[0];
        }
        if (IsFocused && Renderer.Focus is { } focused)
        {
            s = focused.Tile;
        }
        if (s is { IsEmpty: false } && !repeat)
        {
            static bool Down(VirtualKey k) => InputKeyboardSource.GetKeyStateForCurrentThread(k).HasFlag(CoreVirtualKeyStates.Down);
            TileAction action = TileKeyMap.Map(ToTileKey(key), ctrl, Down(VirtualKey.Menu), Down(VirtualKey.Shift), _viewer.Alerts.IsActive(s.Camera));
            if (action != TileAction.None)
            {
                ApplyAction(action, s);
                return true;
            }
        }
        switch (key)
        {
            case VirtualKey.F when !ctrl && !repeat:
                ToggleFullScreen();
                return true;
            case VirtualKey.Escape:
                if (IsFocused)
                {
                    Unfocus();
                }
                else if (State.Editing)
                {
                    SetEditing(false);
                }
                else if (IsFullScreen)
                {
                    ToggleFullScreen();
                }
                return true;
            case VirtualKey.Space or VirtualKey.Enter when s is not null && !repeat:
                if (!s.IsEmpty || IsFocused)
                {
                    ToggleFocus(IsFocused && Renderer.Focus is { } f ? f.Tile : s);
                }
                return true;
            case VirtualKey.Left or VirtualKey.Right or VirtualKey.Up or VirtualKey.Down when s is not null:
                if (Neighbor(IsFocused && Renderer.Focus is { } focus ? focus.Tile : s, key) is { } next)
                {
                    if (IsFocused)
                    {
                        if (!next.IsEmpty)
                        {
                            FocusOn(next);
                        }
                    }
                    else
                    {
                        Select(next.Path);
                    }
                }
                return true;
            case VirtualKey.Delete when State.Editing && !repeat:
                RemoveSelected();
                return true;
            case VirtualKey.Number0 or VirtualKey.NumberPad0 when s is not null:
                ResetZoom(s);
                return true;
            case VirtualKey.D when ctrl && !repeat:
                ToggleDiagnostics();
                return true;
            case (VirtualKey)188 when ctrl: // the comma key
                SettingsRequested?.Invoke();
                return true;
            case VirtualKey.F1 when !repeat:
                ToggleHelp();
                return true;
            default:
                return false;
        }
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
            case TileAction.ToggleFocus when !State.Editing:
                ToggleFocus(tile);
                break;
            case TileAction.ToggleListening when !tile.IsEmpty:
                _viewer.ToggleListening(tile.Camera, false);
                Select(tile.Path);
                break;
        }
    }

    /// <summary>The neighbor in that direction by position in the layout (docs/UX.md "Keyboard").</summary>
    public StageTile? Neighbor(StageTile from, VirtualKey key)
    {
        int index = Array.IndexOf(_tiles, from);
        if (index < 0)
        {
            return null;
        }
        Rect[] cells = [.. _tiles.Select(t => _frames.Leaf(t.Path)?.Rect ?? default)];
        (int dx, int dy) = key switch
        {
            VirtualKey.Left => (-1, 0),
            VirtualKey.Right => (1, 0),
            VirtualKey.Up => (0, -1),
            _ => (0, 1),
        };
        int n = LayoutGeometry.Neighbor(cells, index, dx, dy);
        return n >= 0 ? _tiles[n] : null;
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
        if (tile.CameraOrNone is not { } camera)
        {
            return;
        }
        bool ctrl = InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control).HasFlag(CoreVirtualKeyStates.Down);
        _viewer.ToggleListening(camera, ctrl);
        Select(tile.Path);
    }

    private void SyncChromes()
    {
        _chromeLayer.Children.Clear();
        var kept = _chromes.Where(c => _tiles.Contains(c.Tile)).ToDictionary(c => c.Tile);
        _chromes.Clear();
        foreach (StageTile t in _tiles)
        {
            TileChrome chrome = kept.GetValueOrDefault(t) ?? new TileChrome(t, SpeakerClicked) { DismissClicked = tile => _viewer.DismissAlert(tile.Camera) };
            chrome.Speaker.Visibility = t.IsEmpty ? Visibility.Collapsed : Visibility.Visible;
            _chromes.Add(chrome);
            _chromeLayer.Children.Add(chrome.Root);
        }
    }

    /// <summary>Texts, badges, and the empty-stage message after a state change.</summary>
    public void RefreshAll()
    {
        foreach (TileChrome c in _chromes)
        {
            StageTile t = c.Tile;
            c.Refresh(TitleOf(t), StatusText(t), t.Audible, _diagnostics ? DiagnosticsText(t) : null);
        }
        SyncOverlays();
        bool empty = _viewer.Devices.Count == 0 && State.Tree.Cameras.Count == 0;
        _empty.Visibility = empty ? Visibility.Visible : Visibility.Collapsed;
        if (empty)
        {
            _emptyText.Text = "No devices yet. Add an NVR or camera to start watching (Ctrl+, or right-click).";
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
            if (c.Tile.CameraOrNone is not { } camera)
            {
                c.SetAlert(null, blink);
                c.SetIndicators([], corner);
                c.SetSoundBadges([]);
                continue;
            }
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
    public string? StatusText(StageTile t)
    {
        if (t.CameraOrNone is not { } camera)
        {
            return State.Editing ? "Empty tile: drop a camera here, or pick one in the sidebar" : null;
        }
        if (t.Device is not { } device)
        {
            return "This camera's device was removed.";
        }
        if (DeviceText(device) is { } text)
        {
            return text;
        }
        if (t.Shared is not { } shared)
        {
            return _viewer.ChannelOf(camera) is { Connected: false } || (device.Channels.Count > 0 && _viewer.ChannelOf(camera) is null)
                ? "Camera offline"
                : "Starting…";
        }
        if (shared.Error is { } error)
        {
            return error;
        }
        if (shared.Video.MissingDecoder == NvCodec.H265)
        {
            return "This camera sends H.265 video. Install the HEVC Video Extension from the Microsoft Store to watch it.";
        }
        if (shared.Waiting is { } waiting)
        {
            return waiting + "…";
        }
        return shared.Video.Status switch
        {
            NvTileStatus.Undecodable => "This stream cannot be decoded." + (shared.Video.ConfigureError is { } why ? $"\n{why}" : ""),
            NvTileStatus.Recovering => "Recovering…",
            NvTileStatus.Waiting when t.PictureWidth == 0 => "Starting…",
            _ => null,
        };
    }

    private static string DiagnosticsText(StageTile t)
    {
        if (t.Shared is not { } s)
        {
            return "no stream";
        }
        string kind = s.Video.DecoderKind switch
        {
            NvrView.Video.DecoderKind.Hardware => "GPU",
            NvrView.Video.DecoderKind.Software or NvrView.Video.DecoderKind.SoftwareTexture => "CPU",
            _ => "?",
        };
        return $"{t.PictureWidth}x{t.PictureHeight} {s.ShownTier} · {kind} decode · {s.ReceivedRate:0.0} fps in";
    }

    private void UpdateHover()
    {
        long now = Stopwatch.GetTimestamp();
        bool recent = _idle.ChromeShown;
        NodePath? selected = State.Selected;
        foreach (TileChrome c in _chromes)
        {
            bool header = recent && c.Tile == _hovered;
            // The outline fades with the chrome, but stays while editing the layout.
            bool isSelected = c.Tile.Path == selected && (recent || State.Editing);
            c.SetHover(header, isSelected);
        }
    }

    /// <summary>
    /// The slow tick (every 250 ms): countdowns, the indicator hold, zoom-driven stream quality,
    /// the hover chrome's and the pointer's idle timeout. The self-test calls it directly on a manual clock.
    /// </summary>
    public void Tick()
    {
        _viewer.Tick();
        foreach (StageTile t in _tiles)
        {
            if (t.Zoom != t.DemandZoom)
            {
                _viewer.UpdateDemand(t);
            }
        }
        UpdateCursor();
        // Countdowns and fades that depend on time alone; status lines that
        // follow the video (starting, recovering).
        UpdateHover();
        foreach (TileChrome c in _chromes)
        {
            c.Refresh(TitleOf(c.Tile), StatusText(c.Tile), c.Tile.Audible, _diagnostics ? DiagnosticsText(c.Tile) : null);
        }
        SyncOverlays();
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
            if ((c.Tile.Audible || c.HeaderShown) && c.Tile.Shared is { } shared)
            {
                c.SetLevel(core.TileAudioLevel(shared.Id).Peak);
            }
        }
    }

    /// <summary>The window is closing: ends focus and stops the timers.</summary>
    public void TearDown()
    {
        CancelFocus();
        foreach (StageTile t in _tiles)
        {
            _viewer.WithdrawDemand(t);
        }
        if (_cursorHidden)
        {
            _cursorHidden = false;
            if (FullScreenOverride is null)
            {
                _ = ShowCursor(true);
            }
        }
        _timer.Stop();
        _viewer.Changed -= RefreshAll;
        _viewer.IndicatorsChanged -= SyncOverlays;
        _viewer.Settings.Changed -= SyncOverlays;
        _viewer.Leases.TileChanged -= OnTileChanged;
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
