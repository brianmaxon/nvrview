// The alert insets' column over a window's stage (CallStack): a card per inset of the window's board
// (StageView.Insets.cs), the "+N more" chip, the leases, the swapped-in camera and the layout in
// miniature drawn by the renderer. The card itself (CallCard) is CallPanel.cs.

using System.Globalization;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.System;
using Rect = NvrView.Shell.Rect;
using Size = Windows.Foundation.Size;

namespace NvrView.App;

/// <summary>
/// The insets of one window's board over its stage (see the file comment): a card for each inset
/// drawn, placed in a grid from the inset corner by <see cref="InsetGeometry"/> (the "+N more" chip
/// after the last), the call of a camera that fills the stage along its bottom, and the camera
/// swapped onto the stage drawn over all of it. Covers the whole stage; only the cards and the chip
/// take pointer input.
/// </summary>
internal sealed class CallStack : Panel
{
    /// <summary>The narrowest a card with a call's controls gets (the controls need the room).</summary>
    public const double MinCardWidth = 340;

    private readonly Viewer _viewer;
    private readonly Dictionary<CameraRef, CallCard> _cards = [];
    private readonly Dictionary<(NodePath, CameraRef), StageTile> _minis = [];
    private readonly Button _chip;
    private List<CallCard> _ordered = [];
    private Dictionary<CameraRef, int> _held = [];
    private OverlayTile[] _published = [];
    private StageTile? _takeover;
    private (LayoutNode Tree, double Width, double Height)? _miniKey;
    private IReadOnlyList<LeafFrame> _miniLeaves = [];
    private string _lastSignature = "";
    private int _more;
    private bool _noRoom;
    private bool _closed;

    /// <param name="viewer">The calls and the leases.</param>
    /// <param name="owner">The window's stage: its board, its grid, the camera that fills it.</param>
    public CallStack(Viewer viewer, StageView owner)
    {
        _viewer = viewer;
        Owner = owner;
        Visibility = Visibility.Collapsed;
        _viewer.Leases.TileChanged += OnTileChanged;
        _chip = new Button
        {
            FontSize = 11,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            Padding = new Thickness(10, 2, 10, 2),
            MinHeight = 0,
            MinWidth = 0,
            CornerRadius = new CornerRadius(4),
            Background = new SolidColorBrush(Windows.UI.Color.FromArgb(0xB3, 0x00, 0x00, 0x00)),
            Foreground = new SolidColorBrush(Colors.White),
            Opacity = 0,
            IsHitTestVisible = false,
        };
        AutomationProperties.SetName(_chip, "More insets");
        _chip.Click += (_, _) => ShowWaitingMenu();
        Children.Add(_chip);
    }

    /// <summary>The window's stage (the board and what the cards ask of it).</summary>
    public StageView Owner { get; }

    private InsetAnchor? _anchor;
    private InsetFrame? _column;
    private InsetFrame? _dragFrom;

    /// <summary>
    /// Where the user put the column (a fraction of the free space), null while it sits in the
    /// Settings corner. Belongs to the window and is saved with it.
    /// </summary>
    public InsetAnchor? Anchor => _anchor;

    /// <summary>The user moved the column or reset it (the window saves its record).</summary>
    public event Action? AnchorChanged;

    /// <summary>The column of cards as last placed (the union of their frames), null with none.</summary>
    public InsetFrame? Column => _column;

    /// <summary>A position restored with the window (not a change to save).</summary>
    public void RestoreAnchor(InsetAnchor? anchor)
    {
        _anchor = anchor;
        InvalidateArrange();
    }

    /// <summary>A drag starts from the column as it is now; false when there is no column.</summary>
    internal bool BeginDrag()
    {
        _dragFrom = _column;
        return _dragFrom is not null;
    }

    /// <summary>The column's top left moved by (<paramref name="dx"/>, <paramref name="dy"/>) from where the drag began; each axis snaps near an edge.</summary>
    internal void DragBy(double dx, double dy)
    {
        if (_dragFrom is not { } c || ActualWidth <= 0 || ActualHeight <= 0)
        {
            return;
        }
        var moved = InsetPlacement.Anchor(c.X + dx, c.Y + dy, c.Width, c.Height, ActualWidth, ActualHeight);
        if (_anchor != moved)
        {
            _anchor = moved;
            InvalidateArrange();
        }
    }

    /// <summary>The drag ended; a real one stores the position.</summary>
    internal void EndDrag(bool moved)
    {
        _dragFrom = null;
        if (moved)
        {
            AnchorChanged?.Invoke();
        }
    }

    /// <summary>Back to the Settings corner.</summary>
    public void ResetPosition()
    {
        if (_anchor is null)
        {
            return;
        }
        _anchor = null;
        InvalidateArrange();
        AnchorChanged?.Invoke();
    }

    /// <summary>The cards, oldest inset first (the one over the stage, if any, last).</summary>
    public IReadOnlyList<CallCard> Cards => [.. Enumerable.Reverse(_ordered).Concat(_cards.Values.Where(c => c.Mode == CallCardMode.Stage))];

    /// <summary>The leases the cards hold, per camera (self-test).</summary>
    public int LeasesHeld => _held.Values.Sum();

    /// <summary>The pictures last handed to the renderer.</summary>
    public IReadOnlyList<OverlayTile> Overlays => _published;

    /// <summary>The camera swapped onto the stage has a picture (and a stream demand) here.</summary>
    public StageTile? TakeoverTile => _takeover;

    /// <summary>The "+N more" chip's text while it shows, else empty (self-test).</summary>
    public string ChipText => _chip.Opacity > 0 ? (_chip.Content as string ?? "") : "";

    /// <summary>The insets that did not fit the window's height or width, after the board's waiting ones (self-test).</summary>
    public bool Truncated => _noRoom;

    /// <summary>The miniature layout's tiles drawn now (self-test).</summary>
    public IReadOnlyList<StageTile> MiniatureTiles => [.. _minis.Values];

    /// <summary>
    /// Brings the cards in step with the board (after it follows the calls) and what fills the
    /// stage: a card per inset drawn, the call that fills the stage over it, the swapped-in camera.
    /// </summary>
    public void Sync()
    {
        if (_closed)
        {
            return;
        }
        Owner.SyncInsetBoard();
        InsetBoard board = Owner.Board;
        CameraRef? over = CallInsets.OverStage(_viewer.Calls.InsetCameras, Owner.FillingCamera);
        bool changed = false;
        var wantedCards = new HashSet<CameraRef>();
        var ordered = new List<CallCard>();
        foreach (InsetItem item in board.Shown)
        {
            CallCard card = CardFor(item.Camera, ref changed);
            if (card.Mode != CallCardMode.Inset)
            {
                card.Mode = CallCardMode.Inset;
                changed = true;
            }
            card.Update(item, Owner.LayoutName);
            ordered.Add(card);
            wantedCards.Add(item.Camera);
        }
        if (over is { } o && _viewer.Calls.CallFor(o) is not null && wantedCards.Add(o))
        {
            CallCard card = CardFor(o, ref changed);
            if (card.Mode != CallCardMode.Stage)
            {
                card.Mode = CallCardMode.Stage;
                changed = true;
            }
            card.Update();
        }
        foreach (CameraRef gone in _cards.Keys.Where(c => !wantedCards.Contains(c)).ToArray())
        {
            _cards[gone].Tile.Shared = null;
            Children.Remove(_cards[gone]);
            _cards.Remove(gone);
            changed = true;
        }
        _ordered = ordered;

        // One lease per card with a picture (a layout in miniature: one per camera of the layout, the
        // swapped-in camera one more): the new ones are taken before any is given back, so a camera
        // kept is never closed in between.
        var wanted = new Dictionary<CameraRef, int>();
        void Want(CameraRef camera) => wanted[camera] = wanted.GetValueOrDefault(camera) + 1;
        bool layoutCard = false;
        foreach (CallCard card in ordered)
        {
            if (card.Item is { ShowsLayout: true })
            {
                layoutCard = true;
                foreach (CameraRef camera in Owner.State.Tree.Cameras)
                {
                    Want(camera);
                }
            }
            else
            {
                Want(card.Camera);
            }
        }
        CameraRef? swapped = board.Swap?.Shown;
        if (swapped is { } sc)
        {
            Want(sc);
        }
        _viewer.Leases.Update(_held, wanted);
        _held = wanted;
        foreach (CallCard card in _cards.Values)
        {
            card.Tile.Shared = card.Mode == CallCardMode.Inset && card.Item is not { ShowsLayout: true } ? _viewer.Leases.Tile(card.Camera) : null;
        }
        SyncTakeover(swapped);
        SyncMinis(layoutCard);
        Owner.UpdateTakeover();
        Visibility = _cards.Count > 0 || swapped is not null ? Visibility.Visible : Visibility.Collapsed;
        string signature = board.Signature() + "|" + Owner.InsetGrid;
        if (changed || signature != _lastSignature)
        {
            _lastSignature = signature;
            InvalidateMeasure();
        }
    }

    private CallCard CardFor(CameraRef camera, ref bool changed)
    {
        if (!_cards.TryGetValue(camera, out CallCard? card))
        {
            card = new CallCard(_viewer, camera);
            _cards[camera] = card;
            Children.Add(card);
            changed = true;
        }
        return card;
    }

    // The swapped-in camera's picture over the whole stage, asking for the main stream (as a focused tile does).
    private void SyncTakeover(CameraRef? swapped)
    {
        if (_takeover is { } old && swapped != old.CameraOrNone)
        {
            _viewer.WithdrawDemand(old);
            old.Shared = null;
            _takeover = null;
        }
        if (swapped is { } camera)
        {
            if (_takeover is null)
            {
                _takeover = new StageTile(_viewer, NodePath.Root, camera, TileQuality.Auto);
                _viewer.SetFocused(_takeover, true);
            }
            _takeover.Shared = _viewer.Leases.Tile(camera);
        }
    }

    // The tiles of a whole layout drawn in miniature: one per leaf of the window's layout, from the cameras' shared tiles.
    private void SyncMinis(bool wanted)
    {
        if (!wanted)
        {
            foreach (StageTile t in _minis.Values)
            {
                t.Shared = null;
            }
            _minis.Clear();
            return;
        }
        var keep = new HashSet<(NodePath, CameraRef)>();
        foreach (LeafInfo leaf in Owner.State.Tree.Leaves)
        {
            if (leaf.Camera is { } camera)
            {
                keep.Add((leaf.Path, camera));
            }
        }
        foreach ((NodePath, CameraRef) gone in _minis.Keys.Where(k => !keep.Contains(k)).ToArray())
        {
            _minis[gone].Shared = null;
            _minis.Remove(gone);
        }
    }

    /// <summary>The window is closing: every card goes and its lease is given back.</summary>
    public void Close()
    {
        if (_closed)
        {
            return;
        }
        _closed = true;
        _viewer.Leases.TileChanged -= OnTileChanged;
        foreach (CallCard card in _cards.Values)
        {
            card.Tile.Shared = null;
        }
        if (_takeover is { } t)
        {
            _viewer.WithdrawDemand(t);
            t.Shared = null;
            _takeover = null;
        }
        foreach (StageTile mini in _minis.Values)
        {
            mini.Shared = null;
        }
        _minis.Clear();
        _cards.Clear();
        _ordered = [];
        Children.Clear();
        _viewer.Leases.Update(_held, new Dictionary<CameraRef, int>());
        _held = [];
        _published = [];
    }

    // The core tile opened or closed (a reconnect, a refused tier): the pictures follow.
    private void OnTileChanged(CameraRef camera, CameraTile? tile)
    {
        if (_held.ContainsKey(camera) && _cards.TryGetValue(camera, out CallCard? card) && card.Mode == CallCardMode.Inset && card.Item is not { ShowsLayout: true })
        {
            card.Tile.Shared = tile;
        }
        if (_takeover is { } t && t.CameraOrNone == camera)
        {
            t.Shared = tile;
        }
        foreach (KeyValuePair<(NodePath, CameraRef), StageTile> mini in _minis)
        {
            if (mini.Key.Item2 == camera)
            {
                mini.Value.Shared = tile;
            }
        }
    }

    /// <summary>
    /// Once per UI frame: where each picture goes, handed to the renderer when it changed: the
    /// swapped-in camera over the whole stage, then each inset's picture (a layout in miniature as
    /// the layout's own tiles scaled into the inset); a picture whose size changed resizes its card.
    /// </summary>
    public void Publish(StageRenderer renderer, UIElement stage)
    {
        List<OverlayTile> now = [];
        if (_takeover is { } takeover && stage is FrameworkElement fe && fe.ActualWidth >= 1 && fe.ActualHeight >= 1)
        {
            now.Add(new OverlayTile(takeover, new Rect(0, 0, fe.ActualWidth, fe.ActualHeight)));
        }
        // Oldest first, so the top of the stack is drawn last.
        foreach (CallCard card in Enumerable.Reverse(_ordered))
        {
            if (card.Mode != CallCardMode.Inset)
            {
                continue;
            }
            if (card.SizedFor != (card.Tile.PictureWidth, card.Tile.PictureHeight))
            {
                InvalidateMeasure();
            }
            if (card.HoleIn(stage) is not { } rect)
            {
                continue;
            }
            if (card.Item is { ShowsLayout: true })
            {
                now.AddRange(Miniature(rect));
            }
            else
            {
                now.Add(new OverlayTile(card.Tile, rect));
            }
        }
        renderer.Covered = _takeover is not null;
        if (!now.SequenceEqual(_published))
        {
            _published = [.. now];
            renderer.Overlays = _published;
        }
    }

    // The window's layout drawn into the inset's hole: the layout's own arrangement of the tiles, their cameras' pictures fitted into the scaled cells.
    private List<OverlayTile> Miniature(Rect hole)
    {
        LayoutNode tree = Owner.State.Tree;
        if (_miniKey is not { } k || !ReferenceEquals(k.Tree, tree) || k.Width != hole.Width || k.Height != hole.Height)
        {
            _miniLeaves = tree.Frames(new Rect(0, 0, hole.Width, hole.Height), 1).Leaves;
            _miniKey = (tree, hole.Width, hole.Height);
        }
        var tiles = new List<OverlayTile>();
        foreach (LeafFrame leaf in _miniLeaves)
        {
            if (leaf.Camera is not { } camera)
            {
                continue;
            }
            if (!_minis.TryGetValue((leaf.Path, camera), out StageTile? mini))
            {
                mini = new StageTile(_viewer, leaf.Path, camera, TileQuality.Auto) { Shared = _viewer.Leases.Tile(camera) };
                _minis[(leaf.Path, camera)] = mini;
            }
            tiles.Add(new OverlayTile(mini, new Rect(hole.X + leaf.Rect.X, hole.Y + leaf.Rect.Y, leaf.Rect.Width, leaf.Rect.Height)));
        }
        return tiles;
    }

    // The "+N more" chip's list: the cameras waiting for room; choosing one brings it in (the lowest inset waits instead).
    private void ShowWaitingMenu()
    {
        var menu = new MenuFlyout();
        foreach (InsetItem item in Owner.Board.Waiting)
        {
            CameraRef camera = item.Camera;
            var entry = new MenuFlyoutItem { Text = $"{_viewer.CameraName(camera)} · {item.Kind.Text}" };
            entry.Click += (_, _) => Owner.BringInInset(camera);
            menu.Items.Add(entry);
        }
        if (menu.Items.Count > 0)
        {
            menu.ShowAt(_chip);
        }
    }

    private static double Finite(double value, double fallback) => double.IsFinite(value) ? value : fallback;

    // The grid for this stage: how many columns fit (a call's controls need a wider card), and how wide each is.
    private (int Columns, double Width) Columns(double stageWidth, bool anyCall)
    {
        double min = anyCall ? MinCardWidth : InsetGeometry.MinWidth;
        int columns = InsetGeometry.FittingColumns(stageWidth, Owner.InsetGrid.Columns, min);
        return (columns, InsetGeometry.Width(stageWidth, columns, min));
    }

    private InsetAnchor EffectiveAnchor => _anchor ?? InsetAnchor.FromCorner(_viewer.Settings.InsetCorner);

    protected override Size MeasureOverride(Size availableSize)
    {
        double w = Finite(availableSize.Width, ActualWidth), h = Finite(availableSize.Height, ActualHeight);
        CallCard[] insets = [.. _ordered];
        (int columns, double width) = Columns(w, insets.Any(c => c.IsCall));
        foreach (CallCard card in insets)
        {
            card.Width = width;
            card.SizePicture(width, w, h);
            card.Measure(new Size(width, double.PositiveInfinity));
        }
        foreach (CallCard card in _cards.Values.Where(c => c.Mode == CallCardMode.Stage))
        {
            card.Width = InsetGeometry.StagePanel(w, h, 0, null).Width;
            card.Measure(new Size(card.Width, double.PositiveInfinity));
        }
        // What does not fit counts in the chip with the waiting ones (which insets fit does not depend on where the column sits).
        InsetGridSetting grid = Owner.InsetGrid;
        int fit = InsetGeometry.Grid([.. insets.Select(c => c.DesiredSize.Height)], width, grid.Rows, columns, InsetAnchor.FromCorner(ChromeCorner.TopLeft), w, h).Count;
        int cut = insets.Length - fit;
        _noRoom = cut > 0;
        _more = Owner.Board.More + cut;
        _chip.Content = _noRoom ? $"+{_more} more (no room)" : $"+{_more} more";
        ToolTipService.SetToolTip(_chip, _noRoom ? "The window is too small to draw the whole inset grid" : "Click to bring one in");
        bool show = _more > 0 && fit > 0;
        _chip.Opacity = show ? 1 : 0;
        _chip.IsHitTestVisible = show;
        _chip.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        return new Size(w, h);
    }

    protected override Size ArrangeOverride(Size finalSize)
    {
        double w = finalSize.Width, h = finalSize.Height;
        CallCard[] insets = [.. _ordered];
        (int columns, double width) = Columns(w, insets.Any(c => c.IsCall));
        InsetGridSetting grid = Owner.InsetGrid;
        InsetAnchor anchor = EffectiveAnchor;
        // Newest first: the newest inset sits nearest the corner.
        IReadOnlyList<InsetFrame> frames = InsetGeometry.Grid([.. insets.Select(c => c.DesiredSize.Height)], width, grid.Rows, columns, anchor, w, h);
        for (int i = 0; i < insets.Length; i++)
        {
            CallCard card = insets[i];
            card.Placed = i < frames.Count;
            // A card that does not fit is kept off the stage (collapsing it would let it fit again).
            InsetFrame f = card.Placed ? frames[i] : new InsetFrame(-10_000, -10_000, width, card.DesiredSize.Height);
            card.Frame = f;
            card.Arrange(new Windows.Foundation.Rect(f.X, f.Y, f.Width, f.Height));
        }
        InsetFrame? column = InsetGeometry.Union(frames);
        _column = column;
        foreach (CallCard card in _cards.Values.Where(c => c.Mode == CallCardMode.Stage))
        {
            InsetFrame f = InsetGeometry.StagePanel(w, h, card.DesiredSize.Height, column);
            card.Placed = true;
            card.Frame = f;
            card.Arrange(new Windows.Foundation.Rect(f.X, f.Y, f.Width, f.Height));
        }
        // The chip sits after the last inset: below it, or above when the column grows upward.
        Size chip = _chip.DesiredSize;
        if (_chip.Opacity > 0 && frames.Count > 0)
        {
            InsetFrame last = frames[^1];
            double y = anchor.GrowsUpward ? last.Y - 6 - chip.Height : last.Bottom + 6;
            double x = Math.Min(Math.Max(last.X, 0), Math.Max(w - chip.Width, 0));
            _chip.Arrange(new Windows.Foundation.Rect(x, Math.Min(Math.Max(y, 2), Math.Max(h - chip.Height - 2, 2)), chip.Width, chip.Height));
        }
        else
        {
            _chip.Arrange(new Windows.Foundation.Rect(-10_000, -10_000, chip.Width, chip.Height));
        }
        return finalSize;
    }
}
