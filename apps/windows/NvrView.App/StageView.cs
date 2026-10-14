// A window's stage: the SwapChainPanel its render thread presents into, the
// tile chrome above it, and the input that drives both (docs/UX.md):
//
// - the window's split tree (WindowState.Tree) laid out into cells; one
//   StageTile per leaf, pointing at its camera's shared core tile;
// - hover shows a tile's header (its buttons from the shared TileHeaderLayout, and the name) and fades it 3 s after the
//   pointer stops or leaves; a click selects; a double-click (or Space, or
//   Enter) focuses the tile with an animation of the same live tile and,
//   once it ends, has the leases switch its camera to the focus tier;
// - the designer: drag a divider (double-click evens it), drag a tile onto
//   another to swap them, split, remove, clear, show a camera, presets (the
//   context menu, the edit bar, and Delete in Edit Layout mode); cameras
//   dragged from the sidebar drop onto a tile;
// - mouse wheel (or a touchpad pinch, which arrives as Ctrl+wheel) zooms
//   around the pointer and dragging pans a zoomed tile: the picture grows
//   over the tile's bars until it fills the tile, then crops (TileZoom);
//   a drag on a filled picture with part hidden moves its crop, and a drag
//   on a tile's shown header swaps it (docs/UX.md "Fill Tiles"); the zoom,
//   crop and Fill choice are kept per tile in WindowState.Views and saved
//   with the window;
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
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
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
    private LayoutNode _tree = tree.WithTileIds();

    public WindowContent Content { get; set; } = content;

    public string Name { get; set; } = name;

    /// <summary>
    /// The arrangement shown. Every leaf has a tile identity (<see cref="TileIdentity.WithTileIds"/>, given on
    /// every set), and the views of tiles no longer in it are forgotten.
    /// </summary>
    public LayoutNode Tree
    {
        get => _tree;
        set
        {
            _tree = value.WithTileIds();
            Views.Prune(ViewsKey, _tree);
        }
    }

    public NodePath? Selected { get; set; }

    public bool Editing { get; set; }

    /// <summary>
    /// The window's Fill Tiles switch, and per tile its digital zoom (at 1x, its filled crop's position) and own
    /// Fill choice, in picture coordinates (docs/UX.md "Digital zoom", "Fill Tiles"). Keyed by layout, tile and
    /// camera (<see cref="TileViewKey"/>), so two tiles of one camera are independent views; a view stays through
    /// focus, layout switches and the switch to the main stream, moves with its camera in a swap, and starts
    /// over when its tile is given another camera. Saved with the window.
    /// </summary>
    public TileViews<TileViewKey> Views { get; set; } = new();

    /// <summary>Fill Tiles: tiles without a choice of their own crop their pictures to fill instead of fitting.</summary>
    public bool Fill => Views.Fill;

    /// <summary>The name this content's tile views go by.</summary>
    public string ViewsKey => Content.ViewsKey;

    /// <summary>The view key of the tile at <paramref name="path"/> (null while it shows no camera).</summary>
    public TileViewKey? ViewKey(NodePath path) => _tree.TileKeyAt(path, ViewsKey);

    /// <summary>The window shows <paramref name="content"/> under another name (an unsaved arrangement saved, a group edited): the views go along.</summary>
    public void Rename(WindowContent content)
    {
        Views.Rename(ViewsKey, content.ViewsKey);
        Content = content;
    }
}

internal sealed partial class StageView : UserControl
{
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
    private readonly Border _toast;
    private readonly TextBlock _toastText;
    private readonly HyperlinkButton _toastButton;
    private readonly CallStack _calls;
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

        /// <summary>A drag in a dewarped fisheye view turns (and in Virtual PTZ tilts) it.</summary>
        Fisheye,
    }

    private FisheyeAim _fisheyeFrom;
    private readonly FisheyeZoomEasing<StageTile> _fisheyeZoom = new();

    private GestureKind _gesture;
    private NodePath _gesturePath;
    private Point _gestureFrom;
    private bool _dragging;
    private StageTile? _panTile;
    private TileZoom _panFrom;

    /// <param name="fixedTile">The call window's mode (docs/UX.md "A call in its own window"): one tile, always selected, with the header, menu and digital zoom of any tile but no layout editing, focus, swap or drops, and no call cards.</param>
    public StageView(Viewer viewer, AppWindow appWindow, WindowState state, bool fixedTile = false)
    {
        _viewer = viewer;
        _appWindow = appWindow;
        FixedTile = fixedTile;
        _idle = new ChromeIdle(viewer.Clock);
        State = state;
        IsTabStop = true;
        UseSystemFocusVisuals = false;
        AllowDrop = !fixedTile;
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
                    "Wheel, pinch, drag           digital zoom and pan; 0 or the header's button resets",
                    "M, Ctrl+L, speaker button    listen (Ctrl+click on the speaker listens to several)",
                    "Enter                        dismiss the selected tile's alert (else focus)",
                    "P, Shift+P                   save picture, save picture as",
                    "R                            record the selected tile's camera (again to stop)",
                    "Ctrl+Shift+R                 record the selected tile's call, in a call window too",
                    "T                            start or end a conversation on the selected tile",
                    "Arrows                       move the selected tile's camera while held (a camera that pans and tilts; Alt+arrows move the selection)",
                    "= -, Alt+1 to 9              lens zoom in and out (a lens camera); recall preset 1 to 9",
                    "Menu key, Shift+F10          the selected tile's menu",
                    "Right-click > Quality        Auto, Clear, Fluent, Balanced for the tile",
                    "Right-click > Start Conversation   captions, Talk, and a typed reply",
                    "Ctrl+E                       edit the layout (drag dividers and tiles)",
                    "Delete                       remove the selected tile while editing",
                    "Ctrl+N                       new window",
                    "Ctrl+D                       diagnostics",
                    "Ctrl+,                       Settings (devices)",
                    "Ctrl+Alt+R                   Recordings",
                    "F1                           this list"),
            },
        };
        _calls = new CallStack(viewer, this);
        if (!fixedTile)
        {
            // The swapped-in camera's cover (its header, the pointer kept off the layout) under the insets.
            root.Children.Add(BuildTakeoverCover());
            root.Children.Add(_calls);
        }
        root.Children.Add(_help);
        _toastText = new TextBlock { Foreground = new SolidColorBrush(Colors.White), FontSize = 12, FontWeight = Microsoft.UI.Text.FontWeights.Medium, TextTrimming = TextTrimming.CharacterEllipsis };
        _toast = new Border
        {
            Background = new SolidColorBrush(Windows.UI.Color.FromArgb(0xD9, 0x1A, 0x1A, 0x1A)),
            CornerRadius = new CornerRadius(8),
            Padding = new Thickness(14, 7, 14, 7),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Bottom,
            Margin = new Thickness(12, 12, 12, 22),
            Visibility = Visibility.Collapsed,
            IsHitTestVisible = false,
        };
        _toastButton = new HyperlinkButton { Padding = new Thickness(0), Visibility = Visibility.Collapsed, VerticalAlignment = VerticalAlignment.Center };
        _toastButton.Click += (_, _) =>
        {
            Action? action = _toastAction;
            _toastHideAt = 0;
            action?.Invoke();
        };
        var toastRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 14 };
        toastRow.Children.Add(_toastText);
        toastRow.Children.Add(_toastButton);
        _toast.Child = toastRow;
        AutomationProperties.SetLiveSetting(_toast, AutomationLiveSetting.Polite);
        root.Children.Add(_toast);
        Content = root;

        Renderer = null!;
        _panel.SizeChanged += (_, _) =>
        {
            PushSize();
            Relayout();
        };
        _panel.CompositionScaleChanged += (_, _) => PushSize();
        _viewer.Changed += RefreshAll;
        if (!fixedTile)
        {
            _viewer.CallsChanged += _calls.Sync;
            _viewer.DetectionReceived += OnDetection;
        }
        _viewer.Leases.TileChanged += OnTileChanged;
        InstallPtz();
        _viewer.IndicatorsChanged += SyncOverlays;
        _viewer.Settings.Changed += SyncOverlays;
        // The insets' corner and grid follow the settings (and the indicators' corner, when the inset corner is opposite it).
        if (!fixedTile)
        {
            _viewer.Settings.Changed += OnInsetSettingsChanged;
        }
        // A mount chosen in Settings (or another window) reaches this window's tiles at once.
        _viewer.Settings.Changed += PublishAllFisheye;
        // A touch screen's two-finger pinch (a touchpad's arrives as Ctrl+wheel).
        ManipulationMode = ManipulationModes.Scale;
        _timer = new DispatcherQueueTimerHolder(DispatcherQueue, TimeSpan.FromMilliseconds(250), Tick);
        Loaded += (_, _) => Microsoft.UI.Xaml.Media.CompositionTarget.Rendering += OnRendering;
        Unloaded += (_, _) => Microsoft.UI.Xaml.Media.CompositionTarget.Rendering -= OnRendering;
        DragOver += OnDragOver;
        DragLeave += (_, _) => SetDropTarget(null);
        Drop += OnDrop;
    }

    /// <summary>The window's arrangement.</summary>
    public WindowState State { get; }

    /// <summary>The call window's single fixed tile mode (see the constructor).</summary>
    public bool FixedTile { get; }

    /// <summary>The panel's DIP-to-pixel scale (the call window sizes its stream demand by pixels drawn).</summary>
    public double PanelScale => _panel.CompositionScaleX;

    /// <summary>The call cards over the stage (self-test).</summary>
    public CallStack CallCards => _calls;

    /// <summary>
    /// The camera that fills the stage now, if one does (the focused tile's, else the only
    /// tile's): its call shows its controls over the stage instead of a card.
    /// </summary>
    public CameraRef? FillingCamera =>
        _board.Swap?.Shown ?? CallInsets.Filling(Renderer?.Focus is { Entering: true } f ? f.Tile.CameraOrNone : null, State.Tree.CameraList);

    /// <summary>The render thread (set once by the window).</summary>
    public StageRenderer Renderer { get; set; }

    /// <summary>The Settings window opener (set by the window).</summary>
    public Action? SettingsRequested { get; set; }

    /// <summary>The Recordings window opener (set by the window).</summary>
    public Action? RecordingsRequested { get; set; }

    /// <summary>The tree changed by a finished gesture or command (the window stores it).</summary>
    public event Action<LayoutNode>? Edited;

    /// <summary>Selection, focus, or editing changed (the edit bar follows).</summary>
    public event Action? StateChanged;

    /// <summary>A zoom, a crop or a Fill choice changed (the window saves its record).</summary>
    public event Action? ViewsChanged;


    /// <summary>This window's tiles, one per leaf, in reading order.</summary>
    public StageTile[] Tiles => _tiles;

    /// <summary>The selected tile (keyboard actions apply to it).</summary>
    public StageTile? Selected => State.Selected is { } p ? _tiles.FirstOrDefault(t => t.Path == p) : null;

    /// <summary>True while a tile is focused (or animating into focus).</summary>
    public bool IsFocused => Renderer.Focus is { Entering: true };

    /// <summary>True while a divider or tile drag is in progress (the window does not follow stored changes then).</summary>
    public bool IsInteracting => _gesture is GestureKind.Divider or GestureKind.TileDrag;

    /// <summary>The empty stage's "Add a device…" prompt is showing.</summary>
    public bool ShowsAddDevicePrompt => _empty.Visibility == Visibility.Visible;

    /// <summary>Frames the UI thread updated the chrome (for the self-test).</summary>
    public long ChromeFrames { get; private set; }

    /// <summary>The gap between tiles: wider while editing, so dividers are easy to grab.</summary>
    public double Gap => State.Editing ? 6 : 2;

    public void PushSize() =>
        Renderer?.SetPanelSize(_panel.ActualWidth, _panel.ActualHeight, _panel.CompositionScaleX, _panel.CompositionScaleY);

    /// <summary>Hands the panel a swap chain (UI thread); releases the reference the render thread added.</summary>
    public void AttachSwapChain(nint swapChain) => SwapChainAttach.To(_panel, swapChain);

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
            FisheyeTileSetting fisheye = FisheyeTileSetting.FromJson(State.Tree.Member(leaf.Path, FisheyeTileSetting.Member));
            if (same is not null)
            {
                next[i] = same;
                _viewer.SetQuality(same, quality);
                same.FisheyeSetting = fisheye;
                PublishFisheye(same);
                // Another layout may have put another view on the same place.
                PublishView(same);
                continue;
            }
            var t = new StageTile(_viewer, leaf.Path, leaf.Camera, quality) { FisheyeSetting = fisheye };
            t.Shared = leaf.Camera is { } c ? _viewer.Leases.Tile(c) : null;
            // The tile's own zoom, crop and Fill choice (docs/UX.md "Digital zoom", "Fill Tiles").
            PublishView(t);
            t.ZoomChanged = OnTileZoomChanged;
            PublishFisheye(t);
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
        if (FixedTile)
        {
            // The selection is always that tile.
            State.Selected = State.Tree.Leaves.FirstOrDefault().Path;
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
        Edited?.Invoke(State.Tree);
    }

    /// <summary>The selected tile, or the only one.</summary>
    private NodePath? Target => State.Selected ?? (State.Tree.LeafCount == 1 ? NodePath.Root : null);

    public bool CanSplit => Target is not null && State.Tree.LeafCount < LayoutNode.MaxLeaves;

    public void Split(SplitAxis axis)
    {
        if (FixedTile)
        {
            return;
        }
        if (Target is { } path && State.Tree.Splitting(path, axis) is { } s)
        {
            State.Selected = s.NewLeaf;
            Edit(_ => s.Tree);
        }
    }

    public void RemoveSelected()
    {
        if (!FixedTile && State.Selected is { } path && State.Tree.Removing(path) is { } r)
        {
            State.Selected = r.Tree[r.Survivor] is LayoutLeaf ? r.Survivor : null;
            Edit(_ => r.Tree);
        }
    }

    /// <summary>Shows <paramref name="camera"/> in the tile at <paramref name="path"/>; a camera new to the tile starts at the default view.</summary>
    public void Assign(CameraRef? camera, NodePath path)
    {
        if (!FixedTile)
        {
            Edit(t => t.Assigning(camera, path));
        }
    }

    /// <summary>Swaps the cameras of two tiles; each view goes with its camera (the tiles keep their identities, members and places).</summary>
    public void Swap(NodePath from, NodePath to)
    {
        if (FixedTile || from == to || State.Tree.Swapping(from, to) is not { } swapped)
        {
            return;
        }
        State.Views.SwapViews(State.ViewsKey, State.Tree, from, to);
        State.Selected = to;
        Edit(_ => swapped);
    }

    public void ApplyPreset(LayoutPreset preset)
    {
        if (FixedTile)
        {
            return;
        }
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
        if (State.Editing == editing || FixedTile)
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
        if (FixedTile)
        {
            return;
        }
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
        if (State.Editing || FixedTile)
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
        if (!FixedTile)
        {
            _calls.Sync();
        }
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

    /// <summary>
    /// Zooms by <paramref name="factor"/> around (fx, fy), a point of the tile's area (0..1): the
    /// picture point there stays under it, and the picture grows over the bars until it fills the
    /// tile (NvrView.Tiles TileZoom).
    /// </summary>
    public static void ZoomAround(StageTile t, double fx, double fy, double factor) =>
        t.DigitalZoom = t.DigitalZoom.ZoomedBy(factor, fx, fy, t.ZoomBase);

    /// <summary>Back to 1x and, for a filled picture, its crop back to the middle (Reset zoom, Reset position, 0).</summary>
    public static void ResetZoom(StageTile t) => t.DigitalZoom = TileZoom.None;

    /// <summary>Puts the tile's own zoom, crop and Fill choice on it (what the render thread reads).</summary>
    private void PublishView(StageTile t)
    {
        if (State.ViewKey(t.Path) is { } k)
        {
            t.LoadZoom(State.Views.Zoom(k));
            t.WantsFill = State.Views.Fills(k);
        }
    }

    // A tile's zoom or crop changed (wheel, pinch, drag, reset): kept for that tile only, and saved.
    private void OnTileZoomChanged(StageTile t)
    {
        if (State.ViewKey(t.Path) is not { } k)
        {
            return;
        }
        TileZoom z = t.DigitalZoom;
        if (State.Views.Zoom(k) == z)
        {
            return;
        }
        State.Views.SetZoom(k, z);
        ViewsChanged?.Invoke();
    }

    /// <summary>Whether the tile fills (its own Fill tile choice, else the window's Fill Tiles).</summary>
    public bool Fills(StageTile t) => State.ViewKey(t.Path) is { } k ? State.Views.Fills(k) : State.Fill;

    /// <summary>Whether a tile of the window's content has a Fill choice of its own (the switch is mixed).</summary>
    public bool FillMixed => State.Views.MixedIn(State.ViewsKey);

    /// <summary>Fill tile (the tile's menu): this tile fills, or fits.</summary>
    public void SetTileFill(StageTile t, bool fill)
    {
        if (State.ViewKey(t.Path) is not { } k || State.Views.Fills(k) == fill)
        {
            return;
        }
        State.Views.SetFill(k, fill);
        FillChanged();
    }

    private void FillChanged()
    {
        foreach (StageTile t in _tiles)
        {
            PublishView(t);
        }
        StateChanged?.Invoke();
        ViewsChanged?.Invoke();
    }

    /// <summary>
    /// Whether a plain drag on the tile's picture moves the picture (zoomed in, or a filled picture with part
    /// hidden) instead of swapping the tile. A dewarped fisheye view turns instead.
    /// </summary>
    public static bool PicturePans(StageTile t) =>
        !(t.Fisheye is { } fe && fe.Mode.Dewarps()) && t.PictureWidth > 0 && t.PictureHeight > 0 && t.ViewWidth > 0 && t.ViewHeight > 0
        && t.DigitalZoom.CanPan(t.ZoomBase);

    /// <summary>Whether the point is on the tile's shown hover header, where a drag swaps the tile.</summary>
    private bool OnShownHeader(StageTile t, Point p) =>
        ChromeFor(t) is { HeaderShown: true }
        && p.X >= t.FrameX && p.X < t.FrameX + t.FrameWidth && p.Y >= t.FrameY && p.Y < t.FrameY + Math.Min(TileChrome.HeaderHeight, t.FrameHeight);

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

    // The tile's whole area, bars included (a zoomed picture grows over them).
    private static bool Contains(StageTile t, Point p) =>
        p.X >= t.ViewX && p.Y >= t.ViewY && p.X < t.ViewX + t.ViewWidth && p.Y < t.ViewY + t.ViewHeight;

    private bool CanEditLayout => !FixedTile && Renderer.Focus is null;

    protected override void OnPointerMoved(PointerRoutedEventArgs e)
    {
        PointerPoint pp = e.GetCurrentPoint(this);
        Interact();
        PointerMove(pp.Position, pp.Properties.IsLeftButtonPressed);
        UpdatePointerShape(pp.Position);
    }

    /// <summary>
    /// Every pointer move (the handler, and the self-test's pointer path): a gesture goes on while the button
    /// is down, wherever the move arrives from, and ends at the first move without it (a release this element
    /// did not see).
    /// </summary>
    internal void PointerMove(Point p, bool buttonDown)
    {
        if (_gesture != GestureKind.None && !buttonDown)
        {
            PointerReleasedAt(p);
            return;
        }
        PointerMovedTo(p);
    }

    /// <summary>The pointer moved to <paramref name="p"/> (the pointer handler, and the self-test's simulated drags).</summary>
    internal void PointerMovedTo(Point p)
    {
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
            case GestureKind.Fisheye when _panTile is { Width: > 0, Height: > 0, Fisheye: { } fe } turn:
                FisheyeAim aim = _fisheyeFrom.Dragged(p.X - _gestureFrom.X, p.Y - _gestureFrom.Y, turn.Width, turn.Height, fe.Mode, fe.Mount);
                SetFisheyeAim(turn, aim, save: false);
                break;
            case GestureKind.Pan when _panTile is { ViewWidth: > 0, ViewHeight: > 0 } drag:
                // The picture follows the pointer, in units of the tile's area.
                double dx = (p.X - _gestureFrom.X) / drag.ViewWidth, dy = (p.Y - _gestureFrom.Y) / drag.ViewHeight;
                drag.DigitalZoom = _panFrom.Panned(dx, dy, drag.ZoomBase);
                break;
        }
    }

    // What the pointer is over when nothing is dragged: a divider (resize), a picture a drag pans (a hand,
    // below the header), or anything else (the arrow).
    private void UpdatePointerShape(Point p)
    {
        _hovered = TileAt(p);
        if (_gesture == GestureKind.None)
        {
            DividerFrame? over = CanEditLayout ? _frames.DividerAt(p.X, p.Y, 8) : null;
            bool pans = over is null && !State.Editing && _hovered is { } h && PicturePans(h) && !OnShownHeader(h, p);
            ProtectedCursor = over is { } dv
                ? InputSystemCursor.Create(dv.Axis == SplitAxis.Row ? InputSystemCursorShape.SizeWestEast : InputSystemCursorShape.SizeNorthSouth)
                : pans ? InputSystemCursor.Create(InputSystemCursorShape.Hand) : null;
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
        if (PointerPressedAt(pp.Position) != GestureKind.None)
        {
            _ = CapturePointer(e.Pointer);
        }
    }

    /// <summary>
    /// The left button went down at <paramref name="p"/> (the pointer handler, and the self-test's simulated
    /// drags): which gesture a drag from there makes. On a tile's shown header, or a picture with nothing
    /// hidden, or while editing the layout, the tile is dragged to swap; on a zoomed or filled-and-cropped
    /// picture, the picture pans; in a dewarped fisheye view, the view turns.
    /// </summary>
    private GestureKind PointerPressedAt(Point p)
    {
        _gestureFrom = p;
        _dragging = false;
        _gesture = GestureKind.None;
        if (CanEditLayout && _frames.DividerAt(p.X, p.Y, 8) is { } d)
        {
            _gesture = GestureKind.Divider;
            _gesturePath = d.Path;
            return _gesture;
        }
        if (TileAt(p) is not { } t)
        {
            return _gesture;
        }
        Select(t.Path);
        bool header = OnShownHeader(t, p);
        if (!State.Editing && !header && t.Fisheye is { } fe && fe.Mode.Dewarps())
        {
            _gesture = GestureKind.Fisheye;
            _panTile = t;
            _fisheyeFrom = fe.Aim;
            _fisheyeZoom.Cancel(t);
        }
        else if (!State.Editing && !header && PicturePans(t))
        {
            _gesture = GestureKind.Pan;
            _panTile = t;
            _panFrom = t.DigitalZoom;
        }
        else if (CanEditLayout)
        {
            _gesture = GestureKind.TileDrag;
            _gesturePath = t.Path;
        }
        return _gesture;
    }

    /// <summary>The left button went down at <paramref name="p"/> through the pointer path (self-test); whether a gesture began.</summary>
    internal bool PointerDown(Point p) => PointerPressedAt(p) != GestureKind.None;

    /// <summary>The left button went up at <paramref name="p"/> through the pointer path (self-test).</summary>
    internal void PointerUp(Point p) => PointerReleasedAt(p);

    /// <summary>What a plain drag starting at <paramref name="p"/> would do (self-test): "pan", "turn", "swap", or null.</summary>
    internal string? DragKind(Point p)
    {
        if (TileAt(p) is not { } t)
        {
            return null;
        }
        bool header = OnShownHeader(t, p);
        return !State.Editing && !header && t.Fisheye is { } fe && fe.Mode.Dewarps() ? "turn"
            : !State.Editing && !header && PicturePans(t) ? "pan"
            : CanEditLayout ? "swap" : null;
    }

    /// <summary>Drags from <paramref name="from"/> to <paramref name="to"/> through the pointer handlers' own steps (self-test).</summary>
    internal void SimulateDrag(Point from, Point to, int steps = 4)
    {
        if (PointerPressedAt(from) == GestureKind.None)
        {
            return;
        }
        int n = Math.Max(steps, 1);
        for (int i = 1; i <= n; i++)
        {
            double k = i / (double)n;
            PointerMovedTo(new Point(from.X + (to.X - from.X) * k, from.Y + (to.Y - from.Y) * k));
        }
        PointerReleasedAt(to);
    }

    protected override void OnPointerReleased(PointerRoutedEventArgs e)
    {
        Point p = e.GetCurrentPoint(this).Position;
        // Read before releasing: losing capture resets the gesture state.
        GestureKind gesture = _gesture;
        bool dragged = _dragging;
        StageTile? turned = _panTile;
        NodePath dragFrom = _gesturePath;
        _gesture = GestureKind.None;
        ReleasePointerCapture(e.Pointer);
        _gesture = gesture;
        _dragging = dragged;
        _panTile = turned;
        _gesturePath = dragFrom;
        PointerReleasedAt(p);
    }

    /// <summary>The left button went up at <paramref name="p"/>: the gesture ends (the pointer handler, and the self-test).</summary>
    private void PointerReleasedAt(Point p)
    {
        GestureKind gesture = _gesture;
        StageTile? turned = _panTile;
        bool dragged = _dragging;
        NodePath dragFrom = _gesturePath;
        _gesture = GestureKind.None;
        _panTile = null;
        switch (gesture)
        {
            case GestureKind.Fisheye when turned is not null:
                SaveFisheye(turned);
                break;
            case GestureKind.Divider:
                Edited?.Invoke(State.Tree);
                break;
            case GestureKind.TileDrag when dragged:
                SetDropTarget(null);
                if (_frames.LeafAt(p.X, p.Y)?.Path is { } to && to != dragFrom)
                {
                    Swap(dragFrom, to);
                }
                break;
        }
        _dragging = false;
    }

    protected override void OnPointerCaptureLost(PointerRoutedEventArgs e) =>
        PointerCaptureLostWith(e.GetCurrentPoint(this).Properties.IsLeftButtonPressed);

    /// <summary>
    /// Capture went away (the handler, and the self-test's pointer path). While the button is still down the
    /// gesture goes on: the moves still bubble here from the chrome above the picture (a header that appeared
    /// under the pointer, a system gesture taking capture), so a drag pans 1:1 until the button comes up. A
    /// drag that lost its button ends.
    /// </summary>
    internal void PointerCaptureLostWith(bool buttonDown)
    {
        if (buttonDown && _gesture != GestureKind.None)
        {
            return;
        }
        if (_gesture == GestureKind.Divider)
        {
            Edited?.Invoke(State.Tree);
        }
        if (_gesture == GestureKind.Fisheye && _panTile is { } turned)
        {
            SaveFisheye(turned);
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
        if (TileAt(pp.Position) is { Fisheye: { } fe } ft && fe.Mode.Dewarps())
        {
            // Virtual PTZ zooms (eased), and so does a touchpad pinch (Ctrl+wheel); the other dewarped views do nothing.
            if (fe.Mode == FisheyeViewMode.Ptz)
            {
                ZoomFisheye(ft, FisheyePinch.WheelFactor(pp.Properties.MouseWheelDelta));
            }
            e.Handled = true;
            Interact();
            return;
        }
        if (TileAt(pp.Position) is { } t && t.ViewWidth > 0 && t.ViewHeight > 0 && t.PictureWidth > 0)
        {
            // Around the pointer's place in the tile's area (bars included).
            double fx = Math.Clamp((pp.Position.X - t.ViewX) / t.ViewWidth, 0, 1), fy = Math.Clamp((pp.Position.Y - t.ViewY) / t.ViewHeight, 0, 1);
            ZoomAround(t, fx, fy, Math.Pow(1.0015, pp.Properties.MouseWheelDelta));
            e.Handled = true;
            Interact();
        }
    }

    /// <summary>A two-finger pinch on a touch screen: zooms a Virtual PTZ tile by the step's scale (the tile where the fingers are).</summary>
    protected override void OnManipulationStarted(ManipulationStartedRoutedEventArgs e) =>
        ManipulationBegan(e.PointerDeviceType == PointerDeviceType.Touch);

    /// <summary>
    /// A manipulation began (the handler, and the self-test's pointer path). Only a touch pinch replaces a drag;
    /// a mouse or pen drag goes on (with the stage's Scale mode a mouse drag also starts a manipulation, which
    /// used to end the pan after its first few pixels).
    /// </summary>
    internal void ManipulationBegan(bool touch)
    {
        if (!touch)
        {
            return;
        }
        // The first finger began a drag; the pinch replaces it (its view is saved, not left half-turned).
        if (_gesture == GestureKind.Fisheye && _panTile is { } turned)
        {
            SaveFisheye(turned);
            _gesture = GestureKind.None;
            _panTile = null;
        }
        // The same for a digital zoom's pan: the pinch's zoom must not be overwritten by the drag's starting zoom.
        else if (_gesture == GestureKind.Pan)
        {
            _gesture = GestureKind.None;
            _panTile = null;
        }
    }

    protected override void OnManipulationDelta(ManipulationDeltaRoutedEventArgs e)
    {
        if (TileAt(e.Position) is { Fisheye: { Mode: FisheyeViewMode.Ptz } } ft)
        {
            PinchFisheye(ft, e.Delta.Scale);
            e.Handled = true;
            Interact();
        }
        else if (TileAt(e.Position) is { } t && !(t.Fisheye is { } fe && fe.Mode.Dewarps()))
        {
            PinchDigital(t, e.Position.X, e.Position.Y, e.Delta.Scale);
            e.Handled = true;
            Interact();
        }
    }

    /// <summary>
    /// One pinch step on a touch screen zooms the tile's digital zoom around the point between the fingers
    /// (<paramref name="x"/>, <paramref name="y"/> in the stage's units), as the wheel does around the pointer.
    /// </summary>
    public void PinchDigital(StageTile t, double x, double y, double scale) => PinchAround(t, x, y, scale);

    /// <summary>One pinch step on any tile drawn by a <see cref="StageRenderer"/> (a live tile's or a recording player's): see <see cref="PinchDigital"/>.</summary>
    public static void PinchAround(StageTile t, double x, double y, double scale)
    {
        if (t.ViewWidth <= 0 || t.ViewHeight <= 0 || t.PictureWidth <= 0)
        {
            return;
        }
        double fx = Math.Clamp((x - t.ViewX) / t.ViewWidth, 0, 1), fy = Math.Clamp((y - t.ViewY) / t.ViewHeight, 0, 1);
        ZoomAround(t, fx, fy, FisheyePinch.Factor(scale));
    }

    /// <summary>One pinch step on a tile (<paramref name="scale"/> above 1 spreads: zooms in); only Virtual PTZ zooms.</summary>
    public void PinchFisheye(StageTile t, double scale) => ZoomFisheye(t, FisheyePinch.Factor(scale));

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
        // The menu holds actions of the tile under the pointer only; the app-wide actions are in the
        // window's header (docs/UX.md "Windows header"). Nothing under the pointer: no menu.
        if (t is not null && BuildTileMenu(t) is { } menu)
        {
            menu.ShowAt(this, at);
        }
        e.Handled = true;
    }

    /// <summary>
    /// The context menu of a tile: the header model's list for its camera (the same order and wording as the header; docs/UX.md
    /// "Tile header"), then the layout's own items (Focus, split, remove, clear, show camera, presets); none of those in the call
    /// window's tile. Null when it would be empty.
    /// </summary>
    internal MenuFlyout? BuildTileMenu(StageTile t)
    {
        var menu = new MenuFlyout();
        MenuFlyoutItem Item(IList<MenuFlyoutItemBase> into, string text, Action action, bool enabled = true)
        {
            var item = new MenuFlyoutItem { Text = text, IsEnabled = enabled };
            item.Click += (_, _) => action();
            into.Add(item);
            return item;
        }
        if (t.CameraOrNone is { } camera && HeaderTileOf(t) is { } tile)
        {
            foreach (TileHeaderMenuEntry entry in TileHeaderLayout.Menu(tile))
            {
                menu.Items.Add(MenuItemFor(entry, t, camera));
            }
        }
        if (FixedTile)
        {
            return menu.Items.Count > 0 ? menu : null;
        }
        if (menu.Items.Count > 0)
        {
            menu.Items.Add(new MenuFlyoutSeparator());
        }
        if (!t.IsEmpty)
        {
            Item(menu.Items, IsFocused && Renderer.Focus?.Tile == t ? "Back to the Grid" : "Focus", () => ToggleFocus(t), !State.Editing);
        }
        if (CanEditLayout)
        {
            Item(menu.Items, "Split Side by Side", () => Split(SplitAxis.Row), CanSplit);
            Item(menu.Items, "Split Stacked", () => Split(SplitAxis.Col), CanSplit);
            Item(menu.Items, "Remove Tile", RemoveSelected, State.Tree.LeafCount > 1 || !t.IsEmpty);
            if (!t.IsEmpty)
            {
                Item(menu.Items, "Clear Tile", () => Assign(null, t.Path));
            }
            var show = new MenuFlyoutSubItem { Text = "Show Camera" };
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
            var presets = new MenuFlyoutSubItem { Text = "Layout Preset" };
            foreach (LayoutPreset preset in LayoutPresets.All)
            {
                Item(presets.Items, preset.Title(), () => ApplyPreset(preset));
            }
            menu.Items.Add(presets);
        }
        return menu.Items.Count > 0 ? menu : null;
    }

    // ---- fisheye views (docs/UX.md "Fisheye cameras") ----

    /// <summary>The "Fisheye view" submenu: Original, Panorama, Double Panorama, Quad, Virtual PTZ.</summary>
    private MenuFlyoutSubItem FisheyeViewMenu(StageTile t)
    {
        var sub = new MenuFlyoutSubItem { Text = "Fisheye view" };
        foreach (FisheyeViewMode mode in Enum.GetValues<FisheyeViewMode>())
        {
            var item = new RadioMenuFlyoutItem { Text = mode.Title(), GroupName = "fisheye-view", IsChecked = mode == t.FisheyeSetting.Mode };
            FisheyeViewMode choice = mode;
            item.Click += (_, _) => SetFisheyeMode(t, choice);
            sub.Items.Add(item);
        }
        return sub;
    }

    /// <summary>The "Fisheye mount" submenu of a fisheye camera (a per-camera setting, saved).</summary>
    private MenuFlyoutSubItem FisheyeMountMenu(CameraRef camera)
    {
        var sub = new MenuFlyoutSubItem { Text = "Fisheye mount" };
        foreach (FisheyeMount mount in Enum.GetValues<FisheyeMount>())
        {
            var item = new RadioMenuFlyoutItem { Text = mount.Title(), GroupName = "fisheye-mount", IsChecked = mount == _viewer.Settings.Mount(camera) };
            FisheyeMount choice = mount;
            item.Click += (_, _) => SetFisheyeMount(camera, choice);
            sub.Items.Add(item);
        }
        return sub;
    }

    /// <summary>A tile's fisheye view (the menu): saved in its layout leaf ("fisheye"); the view's aim is kept.</summary>
    public void SetFisheyeMode(StageTile tile, FisheyeViewMode mode)
    {
        tile.FisheyeSetting = tile.FisheyeSetting with { Mode = mode };
        PublishFisheye(tile);
        SaveFisheye(tile);
        RefreshAll();
    }

    /// <summary>A fisheye camera's mount (saved per camera); this window's tiles follow at once, others on their next tick.</summary>
    public void SetFisheyeMount(CameraRef camera, FisheyeMount mount)
    {
        _viewer.Settings.SetMount(camera, mount);
        PublishAllFisheye();
    }

    private void PublishAllFisheye()
    {
        foreach (StageTile t in _tiles)
        {
            PublishFisheye(t);
        }
    }

    /// <summary>Hands the render thread what a tile dewarps with (null: draw it as it is).</summary>
    public void PublishFisheye(StageTile t)
    {
        FisheyeChoice? next = null;
        if (t.CameraOrNone is { } camera && t.FisheyeSetting.Mode.Dewarps() && _viewer.IsFisheye(camera))
        {
            FisheyeMount mount = _viewer.Settings.Mount(camera);
            next = new FisheyeChoice(t.FisheyeSetting.Mode, t.FisheyeSetting.AimFor(mount), mount);
        }
        if (next != t.Fisheye)
        {
            t.Fisheye = next;
        }
    }

    private void SetFisheyeAim(StageTile t, FisheyeAim aim, bool save)
    {
        t.FisheyeSetting = t.FisheyeSetting with { Aim = aim };
        PublishFisheye(t);
        if (save)
        {
            SaveFisheye(t);
        }
    }

    /// <summary>Writes the tile's fisheye view to its layout leaf (at the end of a drag or zoom, not on every refresh).</summary>
    private void SaveFisheye(StageTile t)
    {
        string? json = t.FisheyeSetting.ToJson();
        if (State.Tree.Member(t.Path, FisheyeTileSetting.Member) != (json is null ? null : LayoutJson.Canonical(json)))
        {
            SetTileMember(t.Path, FisheyeTileSetting.Member, json);
        }
    }

    /// <summary>0 on a dewarped tile: back to the mount's default direction and field of view.</summary>
    private bool ResetFisheye(StageTile t)
    {
        if (t.Fisheye is not { } fe || !fe.Mode.Dewarps())
        {
            return false;
        }
        _fisheyeZoom.Cancel(t);
        t.FisheyeSetting = t.FisheyeSetting with { Aim = null };
        PublishFisheye(t);
        SaveFisheye(t);
        return true;
    }

    /// <summary>Zooms a Virtual PTZ tile by <paramref name="factor"/> (more than 1 zooms in), eased over the next refreshes.</summary>
    public void ZoomFisheye(StageTile t, double factor)
    {
        if (t.Fisheye is not { Mode: FisheyeViewMode.Ptz } fe || !double.IsFinite(factor) || factor <= 0)
        {
            return;
        }
        double from = _fisheyeZoom.Target(t) ?? fe.Aim.Fov;
        _fisheyeZoom.Set(t, from / factor, _viewer.Clock.Now);
    }

    /// <summary>One step of the Virtual PTZ zoom easing (every display refresh, on the viewer's clock); saved when it ends.</summary>
    public void StepFisheyeZoom(double now)
    {
        if (!_fisheyeZoom.IsEasing)
        {
            return;
        }
        foreach ((StageTile t, double fov) in _fisheyeZoom.Tick(now, k => k.Fisheye?.Aim.Fov ?? k.FisheyeSetting.Aim?.Fov ?? 90))
        {
            if (t.Fisheye is { } fe)
            {
                SetFisheyeAim(t, (fe.Aim with { Fov = fov }).Clamped(fe.Mount), save: _fisheyeZoom.Target(t) is null);
            }
        }
    }

    /// <summary>Whether a Virtual PTZ zoom is still easing (self-test).</summary>
    public bool IsEasingFisheyeZoom => _fisheyeZoom.IsEasing;

    /// <summary>The "Indicator corner" submenu: where the tiles' symbols sit (a setting, saved).</summary>
    internal MenuFlyoutSubItem IndicatorCornerMenu()
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

    // ---- keys (from the window) ----

    /// <summary>A key for the stage; true if it was used.</summary>
    public bool HandleKey(VirtualKey key, bool ctrl, bool repeat)
    {
        Interact();
        // Tab and Esc go to the alert insets first (Tab swaps the top one with the stage, Esc dismisses).
        if (key is VirtualKey.Tab or VirtualKey.Escape && HandleInsetKey(key, ctrl, repeat))
        {
            return true;
        }
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
            // The context-menu key and Shift+F10 open the tile's menu at its header.
            if (key == VirtualKey.Application || (key == VirtualKey.F10 && Down(VirtualKey.Shift)))
            {
                if (BuildTileMenu(s) is { } menu)
                {
                    menu.ShowAt(this, new Windows.Foundation.Point(s.FrameX + Math.Min(s.FrameWidth, 120) / 2, s.FrameY + TileChrome.HeaderHeight));
                }
                return true;
            }
        }
        switch (key)
        {
            case VirtualKey.F when !ctrl && !repeat && !FixedTile:
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
                if (!ResetFisheye(s))
                {
                    ResetZoom(s);
                }
                return true;
            case VirtualKey.R when ctrl && !repeat && InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Menu).HasFlag(CoreVirtualKeyStates.Down):
                RecordingsRequested?.Invoke();
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
        VirtualKey.P => TileKey.P,
        VirtualKey.R => TileKey.R,
        VirtualKey.T => TileKey.T,
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
            case TileAction.SavePicture or TileAction.SavePictureAs or TileAction.ToggleRecording or TileAction.ToggleConversation:
                ApplyItemAction(action, tile);
                break;
            case TileAction.ToggleFocus when !State.Editing && !FixedTile:
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

    /// <summary>
    /// Fill Tiles on or off for this window: pictures crop to fill their tiles, or fit with bars. Every tile
    /// follows (the tiles' own Fill tile choices are cleared).
    /// </summary>
    public void SetFill(bool fill)
    {
        if (State.Fill == fill && !FillMixed)
        {
            return;
        }
        State.Views.SetWindowFill(fill);
        FillChanged();
    }

    /// <summary>Whether the tiles show their diagnostics line.</summary>
    public bool Diagnostics => _diagnostics;

    /// <summary>Whether the tiles show their frame-rate readout (header menu, remembered).</summary>
    public bool FrameRates => _viewer.Settings.ShowFrameRates;

    public void ToggleFrameRates() => _viewer.Settings.ShowFrameRates = !_viewer.Settings.ShowFrameRates;

    /// <summary>"Device 15.0 fps · Decoded 15.0 fps": what the camera sends and what this tile decoded.</summary>
    internal static string FrameRateText(StageTile t) =>
        t.Shared is null ? "Device – fps · Decoded – fps" : $"Device {t.ReceivedRate:0.0} fps · Decoded {t.DecodedRate:0.0} fps";

    public void ToggleDiagnostics()
    {
        _diagnostics = !_diagnostics;
        RefreshAll();
        StateChanged?.Invoke();
    }

    public void ToggleHelp() =>
        _help.Visibility = _help.Visibility == Visibility.Visible ? Visibility.Collapsed : Visibility.Visible;

    // ---- chrome ----

    private void SyncChromes()
    {
        _chromeLayer.Children.Clear();
        var kept = _chromes.Where(c => _tiles.Contains(c.Tile)).ToDictionary(c => c.Tile);
        _chromes.Clear();
        foreach (StageTile t in _tiles)
        {
            TileChrome chrome = kept.GetValueOrDefault(t) ?? new TileChrome(t)
            {
                DismissClicked = tile => _viewer.DismissAlert(tile.Camera),
                RetryClicked = RetryClicked,
                ItemClicked = HeaderItemClicked,
                HeaderStateChanged = tile => { if (ChromeFor(tile) is { } c) { RefreshChrome(c); } },
            };
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
            RefreshChrome(c);
        }
        SyncOverlays();
        // A tile that fills the stage (focus, a one-tile layout) moves its call over the stage.
        if (!FixedTile)
        {
            _calls.Sync();
        }
        bool empty = _viewer.Devices.Count == 0 && State.Tree.Cameras.Count == 0;
        _empty.Visibility = empty ? Visibility.Visible : Visibility.Collapsed;
        if (empty)
        {
            _emptyText.Text = "No devices yet. Add an NVR or camera to start watching (Ctrl+, or right-click).";
        }
        UpdateHover();
    }

    /// <summary>The header's title: the camera's name, and the quality when it is not Auto.</summary>
    public static string TitleOf(StageTile t)
    {
        string title = t.Quality.HeaderNote() is { } note ? $"{t.Name} · {note}" : t.Name;
        return t.Fisheye is { } fe && fe.Mode.Dewarps() ? $"{title} · {fe.Mode.Title()}" : title;
    }

    /// <summary>
    /// The corner indicators, the sound badges, and the alert strip of every tile, from the viewer's
    /// state. On events and the slow tick, never per frame.
    /// </summary>
    public void SyncOverlays()
    {
        ChromeCorner corner = _viewer.Settings.IndicatorCorner;
        bool rates = _viewer.Settings.ShowFrameRates;
        DateTime now = _viewer.WallClock();
        bool blink = AlertBorder.ModeFor(_viewer.AnimationsEnabled()) == AlertBorder.Mode.Blinking;
        foreach (TileChrome c in _chromes)
        {
            c.SetRates(rates && c.Tile.CameraOrNone is not null ? FrameRateText(c.Tile) : null);
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

    private readonly TileRetryWatch<StageTile> _retryWatch = new();

    /// <summary>Title, status line, and the Retry button of one tile.</summary>
    private void RefreshChrome(TileChrome c)
    {
        (string? text, TileMessage kind) = StatusOf(c.Tile);
        c.Refresh(TitleOf(c.Tile), text, c.Tile.Audible, _diagnostics ? DiagnosticsText(c.Tile) : null, HeaderTileOf(c.Tile),
            c.Tile.CameraOrNone is { } camera ? _viewer.Capture.Board.Pill(camera) : null);
        c.SetRetry(_retryWatch.Offered(c.Tile, kind, _viewer.Clock.Now));
    }

    /// <summary>The Retry button was pressed (or invoked by the self-test).</summary>
    // The header's reset control: a moved virtual camera goes back to its default, else the digital zoom to 1x.
    private void ResetTile(StageTile tile)
    {
        if (!ResetFisheye(tile))
        {
            ResetZoom(tile);
        }
        Interact();
    }

    private void RetryClicked(StageTile tile)
    {
        _retryWatch.Restart(tile, _viewer.Clock.Now);
        _viewer.RetryTile(tile);
        if (_chromes.FirstOrDefault(c => c.Tile == tile) is { } chrome)
        {
            RefreshChrome(chrome);
        }
    }

    /// <summary>The device's state as a message a retry may or may not help.</summary>
    private static TileMessage DeviceMessage(DeviceEntry d) => d.State switch
    {
        "connecting" => TileMessage.Connecting,
        "backoff" => TileMessage.Reconnecting,
        "login_failed" => TileMessage.LoginFailed,
        "no_password" => TileMessage.NoPassword,
        "invalid" => TileMessage.InvalidAddress,
        "closed" => TileMessage.Disconnected,
        _ => TileMessage.None,
    };

    /// <summary>The tile's status line, or null while it plays normally.</summary>
    public string? StatusText(StageTile t) => StatusOf(t).Text;

    /// <summary>The tile's status line and what kind of message it is.</summary>
    public (string? Text, TileMessage Kind) StatusOf(StageTile t)
    {
        if (t.CameraOrNone is not { } camera)
        {
            return (State.Editing ? "Empty tile: drop a camera here, or pick one in the sidebar" : null, TileMessage.None);
        }
        if (t.Device is not { } device)
        {
            return ("This camera's device was removed.", TileMessage.DeviceRemoved);
        }
        if (DeviceText(device) is { } text)
        {
            return (text, DeviceMessage(device));
        }
        if (t.Shared is not { } shared)
        {
            return _viewer.ChannelOf(camera) is { Connected: false } || (device.Channels.Count > 0 && _viewer.ChannelOf(camera) is null)
                ? ("Camera offline", TileMessage.CameraOffline)
                : ("Starting…", TileMessage.Starting);
        }
        // A refusal is shown until a picture arrives: the core tries a refused stream again.
        if (shared.Error is { } error && t.PictureWidth == 0)
        {
            return (error, TileMessage.Refused);
        }
        if (shared.Video.MissingDecoder == NvCodec.H265)
        {
            return ("This camera sends H.265 video. Install the HEVC Video Extension from the Microsoft Store to watch it.", TileMessage.Undecodable);
        }
        if (shared.Waiting is { } waiting)
        {
            return (waiting + "…", TileMessage.WaitingForCamera);
        }
        return shared.Video.Status switch
        {
            NvTileStatus.Undecodable => ("This stream cannot be decoded." + (shared.Video.ConfigureError is { } why ? $"\n{why}" : ""), TileMessage.Undecodable),
            NvTileStatus.Recovering => ("Recovering…", t.PictureWidth == 0 ? TileMessage.Recovering : TileMessage.None),
            NvTileStatus.Waiting when t.PictureWidth == 0 => ("Starting…", TileMessage.Starting),
            _ => (null, TileMessage.None),
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

    /// <summary>Shows a tile's hover header as the pointer over it does (self-test).</summary>
    internal void Hover(StageTile? t)
    {
        _hovered = t;
        Interact();
        UpdateHover();
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
            c.SetMapShown(recent);
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
            // A camera is known to be fisheye only once its capabilities are read.
            PublishFisheye(t);
            if (t.Zoom != t.DemandZoom)
            {
                _viewer.UpdateDemand(t);
            }
        }
        UpdateCursor();
        TickInsets();
        TickToast(_viewer.Clock.Now);
        // Countdowns and fades that depend on time alone; status lines that
        // follow the video (starting, recovering).
        UpdateHover();
        foreach (TileChrome c in _chromes)
        {
            RefreshChrome(c);
        }
        SyncOverlays();
    }

    // ---- interaction and the pointer ----

    /// <summary>Pointer movement, a click, a key, or a scroll: the chrome (and, in full screen, the pointer) comes back.</summary>
    public void Interact()
    {
        _idle.Interact();
        UpdateCursor();
        // Using the call window's tile is using the call.
        if (FixedTile && _tiles.Length > 0 && _tiles[0].CameraOrNone is { } call)
        {
            _viewer.Calls.Use(call);
        }
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
        StepFisheyeZoom(_viewer.Clock.Now);
        if (Renderer is not null && !FixedTile)
        {
            _calls.Publish(Renderer, _panel);
        }
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
        _viewer.CallsChanged -= _calls.Sync;
        _viewer.DetectionReceived -= OnDetection;
        _viewer.IndicatorsChanged -= SyncOverlays;
        _viewer.Settings.Changed -= SyncOverlays;
        _viewer.Settings.Changed -= OnInsetSettingsChanged;
        // The cards give their leases back; nothing more is drawn over the grid.
        _calls.Close();
        Renderer.Overlays = [];
        Renderer.Covered = false;
        _viewer.Leases.TileChanged -= OnTileChanged;
        // A held move ends with its window.
        RemovePtz();
        ClosePtzFlyout();
        _viewer.Ptz.StopAll();
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

/// <summary>Hands a SwapChainPanel its swap chain (ISwapChainPanelNative); every window's panel, the viewer windows' and the call windows', goes through here.</summary>
internal static class SwapChainAttach
{
    // ISwapChainPanelNative of WinUI 3 (microsoft.ui.xaml.media.dxinterop.h).
    private static readonly Guid SwapChainPanelNativeId = new("63aad0b8-7c24-40ff-85a8-640d944cc325");

    /// <summary>Sets the panel's swap chain (UI thread; 0 detaches) and releases the reference the render thread added.</summary>
    public static unsafe void To(SwapChainPanel panel, nint swapChain)
    {
        nint unknown = ((WinRT.IWinRTObject)panel).NativeObject.ThisPtr;
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
}
