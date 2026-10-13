// The recording player's picture features (docs/UX.md "Recordings", "Digital zoom", "Fisheye cameras", "Save
// picture"): every tile the player draws has what a live tile has for its picture. The player's tiles are
// StageTiles drawn by the window's StageRenderer, so zoom, Fill and the dewarp are the renderer's own; this file
// connects them to the window:
//
// - Where the state lives: RecordingsPlayer.Views (PlayerTileViews in NvrView.Shell, tested on Linux), per
//   camera. It is published to the tile (what the render thread reads) when it changes, and read back when a
//   zoom changes (StageTile.ZoomChanged).
// - Input: the wheel and a touch pinch zoom around the pointer (StageView.ZoomAround and PinchAround, the live
//   tiles' own), a drag moves a zoomed or filled picture (StageView.PicturePans) or turns a dewarped fisheye
//   view, and the keys 0, P and Shift+P act on the tile under the pointer, else the large one.
// - Chrome: a TileChrome per tile (the live tiles' header laid out by TileHeaderLayout with its "..." overflow,
//   the zoom map, the selection outline). The tile's header model is flagged Playback, so only the picture's
//   actions are offered.
// - Save: PictureSaving, the one path live tiles use.
//
// Playback streams are fixed per play, so the live tiles' zoom-driven quality switch has nothing to do here. The
// macOS app does the same in RecordingsPlayer+Pictures.swift and PlayerStage.swift.

using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NvrView.Shell;
using NvrView.Tiles;
using NvrView.Video;
using Windows.System;
using Windows.UI.Core;
using DispatcherQueueTimer = Microsoft.UI.Dispatching.DispatcherQueueTimer;
using Point = Windows.Foundation.Point;

namespace NvrView.App;

internal sealed partial class RecordingsWindow
{
    /// <summary>What a drag that began on a player tile does.</summary>
    private enum PlayerGesture
    {
        None,

        /// <summary>The picture follows the pointer (zoomed, or filled with part hidden).</summary>
        Pan,

        /// <summary>The dewarped fisheye view turns.</summary>
        Turn,
    }

    private readonly Canvas _chromeLayer = new();
    private readonly Dictionary<StageTile, TileChrome> _chromes = [];
    private readonly Dictionary<StageTile, bool> _hadPicture = [];
    private readonly FisheyeZoomEasing<StageTile> _fisheyeZoom = new();
    private readonly Border _toast = new()
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
    private readonly TextBlock _toastText = new() { Foreground = new SolidColorBrush(Colors.White), FontSize = 12, FontWeight = Microsoft.UI.Text.FontWeights.Medium, TextTrimming = TextTrimming.CharacterEllipsis };
    private readonly HyperlinkButton _toastButton = new() { Padding = new Thickness(0), Visibility = Visibility.Collapsed, VerticalAlignment = VerticalAlignment.Center };
    private Action? _toastAction;
    private DispatcherQueueTimer? _toastTimer;
    private StageTile? _hoveredTile, _selectedTile, _dragTile;
    private PlayerGesture _gesture;
    private Point _dragFrom;
    private TileZoom _panFrom;
    private FisheyeAim _aimFrom;
    private bool _dragMoved;
    private bool _suppressTap;

    /// <summary>The seconds a message with a button stays (without one: <see cref="ToastSeconds"/>).</summary>
    private const double PlayerActionToastSeconds = 6;

    private const double ToastSeconds = 2.5;

    /// <summary>Saves finished, whether or not they worked (self-test).</summary>
    internal int PictureSaves { get; private set; }

    /// <summary>The last finished save (self-test).</summary>
    internal PictureSaveRecord? LastPictureSave { get; private set; }

    /// <summary>Sees the picture a save draws, under the device lock, at the moment it is drawn (self-test).</summary>
    internal Action<NvrView.Video.TilePicture>? PictureObserver { get; set; }

    /// <summary>Stands in for the save picker of Save Picture As (self-test): gets the suggested file name and the format, answers the chosen path, or null for Cancel.</summary>
    internal Func<string, PictureFormat, Task<string?>>? ChoosePictureFile { get; set; }

    private void BuildPlayerPictures()
    {
        _playerArea.Children.Add(_chromeLayer);
        _toastButton.Click += (_, _) =>
        {
            Action? action = _toastAction;
            HideToast();
            action?.Invoke();
        };
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 14 };
        row.Children.Add(_toastText);
        row.Children.Add(_toastButton);
        _toast.Child = row;
        AutomationProperties.SetLiveSetting(_toast, AutomationLiveSetting.Polite);
        _playerArea.Children.Add(_toast);
        _toastTimer = _ui.CreateTimer();
        _toastTimer.IsRepeating = false;
        _toastTimer.Tick += (_, _) => HideToast();

        _playerArea.PointerPressed += (_, e) =>
        {
            PointerPoint pp = e.GetCurrentPoint(_playerArea);
            if (pp.Properties.IsLeftButtonPressed && PlayerPressedAt(pp.Position))
            {
                _ = _playerArea.CapturePointer(e.Pointer);
            }
        };
        _playerArea.PointerMoved += (_, e) =>
        {
            PointerPoint pp = e.GetCurrentPoint(_playerArea);
            SetHovered(TileAt(pp.Position));
            if (_gesture != PlayerGesture.None && !pp.Properties.IsLeftButtonPressed)
            {
                PlayerReleased();
            }
            else
            {
                PlayerMovedTo(pp.Position);
            }
            UpdatePlayerCursor(pp.Position);
        };
        _playerArea.PointerReleased += (_, e) =>
        {
            _playerArea.ReleasePointerCapture(e.Pointer);
            PlayerReleased();
        };
        _playerArea.PointerCaptureLost += (_, e) =>
        {
            // The button is still down when a header appears under the pointer or a system gesture takes capture: the drag goes on.
            if (!e.GetCurrentPoint(_playerArea).Properties.IsLeftButtonPressed)
            {
                PlayerReleased();
            }
        };
        _playerArea.PointerExited += (_, _) => SetHovered(null);
        _playerArea.PointerWheelChanged += (_, e) =>
        {
            PointerPoint pp = e.GetCurrentPoint(_playerArea);
            if (PlayerWheelAt(pp.Position, pp.Properties.MouseWheelDelta))
            {
                e.Handled = true;
            }
        };
        // A two-finger pinch on a touch screen zooms the tile where the fingers are.
        _playerArea.ManipulationMode = ManipulationModes.Scale;
        _playerArea.ManipulationStarted += (_, e) =>
        {
            if (e.PointerDeviceType == Microsoft.UI.Input.PointerDeviceType.Touch)
            {
                // The first finger began a drag; the pinch replaces it.
                PlayerReleased();
            }
        };
        _playerArea.ManipulationDelta += (_, e) =>
        {
            if (e.PointerDeviceType == Microsoft.UI.Input.PointerDeviceType.Touch && PlayerPinchAt(e.Position, e.Delta.Scale))
            {
                e.Handled = true;
            }
        };
        Microsoft.UI.Xaml.Media.CompositionTarget.Rendering += OnPlayerRendering;
    }

    private void ClosePlayerPictures()
    {
        Microsoft.UI.Xaml.Media.CompositionTarget.Rendering -= OnPlayerRendering;
        _toastTimer?.Stop();
        _chromes.Clear();
        _chromeLayer.Children.Clear();
    }

    // ---- the tiles ----

    private static RecordingCamera CameraOf(StageTile t) => new(t.Camera.Device, t.Camera.Channel, t.Camera.Lens);

    private PlayerTileViews Views => Browser.Player.Views;

    /// <summary>The tile of the player's cell <paramref name="index"/> (self-test).</summary>
    internal StageTile? PlayerTile(int index) =>
        index >= 0 && index < Browser.Player.Cells.Count && _stageTiles.TryGetValue(Browser.Player.Cells[index].Tile, out StageTile? t) ? t : null;

    /// <summary>The chrome of a player tile (self-test).</summary>
    internal TileChrome? PlayerChrome(StageTile t) => _chromes.GetValueOrDefault(t);

    /// <summary>The tile drawn under a point of the player area.</summary>
    private StageTile? TileAt(Point p)
    {
        if (Browser.PlayerView != PlayerView.Cameras)
        {
            return null;
        }
        int i = CellAt(p);
        return i >= 0 ? PlayerTile(i) : null;
    }

    /// <summary>Gives a new tile what the player kept for its camera (what the render thread reads).</summary>
    private void InitPlayerTile(StageTile tile)
    {
        tile.ZoomChanged = OnTileZoomChanged;
        PublishView(tile);
    }

    private void PublishView(StageTile t)
    {
        RecordingCamera camera = CameraOf(t);
        t.LoadZoom(Views.Zoom(camera));
        t.WantsFill = Views.Fills(camera);
        PublishFisheye(t);
    }

    /// <summary>Hands the render thread what a tile dewarps with (null: draw it as it is).</summary>
    private void PublishFisheye(StageTile t)
    {
        FisheyeTileSetting setting = Views.Fisheye(CameraOf(t));
        t.FisheyeSetting = setting;
        FisheyeChoice? next = null;
        if (t.CameraOrNone is { } camera && setting.Mode.Dewarps() && _viewer.IsFisheye(camera))
        {
            FisheyeMount mount = _viewer.Settings.Mount(camera);
            next = new FisheyeChoice(setting.Mode, setting.AimFor(mount), mount);
        }
        if (next != t.Fisheye)
        {
            t.Fisheye = next;
        }
    }

    // A tile's zoom or crop changed (wheel, pinch, drag, reset): kept for its camera, as the player's own state.
    private void OnTileZoomChanged(StageTile t)
    {
        RecordingCamera camera = CameraOf(t);
        if (Views.Zoom(camera) != t.DigitalZoom)
        {
            Views.SetZoom(camera, t.DigitalZoom);
        }
    }

    /// <summary>Whether the tile fills (its own Fill tile choice).</summary>
    internal bool PlayerFills(StageTile t) => Views.Fills(CameraOf(t));

    /// <summary>Fill Tile (the tile's menu): this camera's tile fills, or fits.</summary>
    internal void SetPlayerFill(StageTile t, bool fill)
    {
        Views.SetFill(CameraOf(t), fill);
        PublishView(t);
        RefreshPlayerChrome(t);
    }

    // The tiles and chromes after the player's layout changed.
    private void SyncPlayerChromes(IReadOnlyList<StageTile> tiles)
    {
        foreach (StageTile gone in _chromes.Keys.Where(t => !tiles.Contains(t)).ToList())
        {
            _chromes.Remove(gone);
            _hadPicture.Remove(gone);
            _fisheyeZoom.Cancel(gone);
        }
        _chromeLayer.Children.Clear();
        foreach (StageTile t in tiles)
        {
            if (!_chromes.TryGetValue(t, out TileChrome? chrome))
            {
                chrome = new TileChrome(t)
                {
                    ItemClicked = PerformPlayerItem,
                    HeaderStateChanged = RefreshPlayerChrome,
                };
                chrome.SetMapShown(true);
                _chromes[t] = chrome;
                InitPlayerTile(t);
            }
            _chromeLayer.Children.Add(chrome.Root);
            RefreshPlayerChrome(t);
        }
        if (_hoveredTile is { } h && !tiles.Contains(h))
        {
            _hoveredTile = null;
        }
        if (_selectedTile is { } s && !tiles.Contains(s))
        {
            _selectedTile = null;
        }
        if (_dragTile is { } d && !tiles.Contains(d))
        {
            _dragTile = null;
            _gesture = PlayerGesture.None;
        }
    }

    /// <summary>What the header model needs to know about a player tile: only the picture's own actions are offered (<see cref="TileHeaderTile.Playback"/>).</summary>
    internal TileHeaderTile PlayerHeaderTile(StageTile t)
    {
        RecordingCamera camera = CameraOf(t);
        return new TileHeaderTile
        {
            Playback = true,
            Fisheye = t.CameraOrNone is { } c && _viewer.IsFisheye(c),
            Picture = t.PictureWidth > 0 && t.PictureHeight > 0,
            Zoomed = Views.IsZoomed(camera),
            Panned = Views.IsPanned(camera),
        };
    }

    private void RefreshPlayerChrome(StageTile t)
    {
        if (_chromes.GetValueOrDefault(t) is { } chrome)
        {
            _hadPicture[t] = t.PictureWidth > 0 && t.PictureHeight > 0;
            chrome.Refresh(StageView.TitleOf(t), null, false, null, PlayerHeaderTile(t));
        }
    }

    // Per display refresh: the chromes follow their tiles, a Virtual PTZ zoom eases, and a header learns its tile got a picture.
    private void OnPlayerRendering(object? sender, object e)
    {
        StepPlayerFisheye(_viewer.Clock.Now);
        foreach ((StageTile t, TileChrome chrome) in _chromes)
        {
            chrome.Place();
            if ((t.PictureWidth > 0 && t.PictureHeight > 0) != _hadPicture.GetValueOrDefault(t))
            {
                RefreshPlayerChrome(t);
            }
        }
    }

    private void SetHovered(StageTile? t)
    {
        if (_hoveredTile == t)
        {
            return;
        }
        _hoveredTile = t;
        UpdatePlayerHover();
    }

    private void UpdatePlayerHover()
    {
        foreach ((StageTile t, TileChrome chrome) in _chromes)
        {
            chrome.SetHover(t == _hoveredTile, t == _selectedTile);
        }
    }

    private void SelectPlayerTile(StageTile? t)
    {
        _selectedTile = t;
        UpdatePlayerHover();
    }

    // The pointer over a picture a drag moves is a hand, below the header.
    private void UpdatePlayerCursor(Point p)
    {
        bool pans = _gesture == PlayerGesture.None && TileAt(p) is { } t
            && (StageView.PicturePans(t) || t.Fisheye is { } fe && fe.Mode.Dewarps())
            && !OnShownPlayerHeader(t, p);
        _playerArea.SetCursor(pans ? InputSystemCursor.Create(InputSystemCursorShape.Hand) : null);
    }

    private bool OnShownPlayerHeader(StageTile t, Point p) =>
        _chromes.GetValueOrDefault(t) is { HeaderShown: true }
        && p.X >= t.FrameX && p.X < t.FrameX + t.FrameWidth && p.Y >= t.FrameY && p.Y < t.FrameY + Math.Min(TileChrome.HeaderHeight, t.FrameHeight);

    // ---- pointer ----

    /// <summary>The left button went down at <paramref name="p"/> (the pointer handler, and the self-test's pointer path): whether a drag from there moves the picture or turns the view.</summary>
    internal bool PlayerPressedAt(Point p)
    {
        _dragFrom = p;
        _dragMoved = false;
        _gesture = PlayerGesture.None;
        if (TileAt(p) is not { } t)
        {
            return false;
        }
        SelectPlayerTile(t);
        bool header = OnShownPlayerHeader(t, p);
        if (!header && t.Fisheye is { } fe && fe.Mode.Dewarps())
        {
            _gesture = PlayerGesture.Turn;
            _dragTile = t;
            _aimFrom = fe.Aim;
            _fisheyeZoom.Cancel(t);
        }
        else if (!header && StageView.PicturePans(t))
        {
            _gesture = PlayerGesture.Pan;
            _dragTile = t;
            _panFrom = t.DigitalZoom;
        }
        return _gesture != PlayerGesture.None;
    }

    /// <summary>The pointer moved to <paramref name="p"/> with the button down (the handler, and the self-test's simulated drags).</summary>
    internal void PlayerMovedTo(Point p)
    {
        if (_dragTile is not { } t)
        {
            return;
        }
        if (Math.Abs(p.X - _dragFrom.X) + Math.Abs(p.Y - _dragFrom.Y) > 3)
        {
            _dragMoved = true;
        }
        switch (_gesture)
        {
            case PlayerGesture.Turn when t is { Width: > 0, Height: > 0, Fisheye: { } fe }:
                SetPlayerAim(t, _aimFrom.Dragged(p.X - _dragFrom.X, p.Y - _dragFrom.Y, t.Width, t.Height, fe.Mode, fe.Mount));
                break;
            case PlayerGesture.Pan when t is { ViewWidth: > 0, ViewHeight: > 0 }:
                // The picture follows the pointer, in units of the tile's area.
                t.DigitalZoom = _panFrom.Panned((p.X - _dragFrom.X) / t.ViewWidth, (p.Y - _dragFrom.Y) / t.ViewHeight, t.ZoomBase);
                break;
        }
    }

    /// <summary>The button went up (the handler, and the self-test).</summary>
    internal void PlayerReleased()
    {
        // A drag that moved the picture is not a click: the tap that follows it does not pick the cell.
        _suppressTap = _gesture != PlayerGesture.None && _dragMoved;
        _gesture = PlayerGesture.None;
        _dragTile = null;
    }

    /// <summary>Drags from <paramref name="from"/> to <paramref name="to"/> through the pointer handlers' own steps (self-test).</summary>
    internal void SimulatePlayerDrag(Point from, Point to, int steps = 4)
    {
        if (!PlayerPressedAt(from))
        {
            return;
        }
        int n = Math.Max(steps, 1);
        for (int i = 1; i <= n; i++)
        {
            double k = i / (double)n;
            PlayerMovedTo(new Point(from.X + (to.X - from.X) * k, from.Y + (to.Y - from.Y) * k));
        }
        PlayerReleased();
    }

    /// <summary>What a plain drag starting at <paramref name="p"/> would do (self-test): "pan", "turn", or null.</summary>
    internal string? PlayerDragKind(Point p) =>
        TileAt(p) is not { } t || OnShownPlayerHeader(t, p) ? null
        : t.Fisheye is { } fe && fe.Mode.Dewarps() ? "turn"
        : StageView.PicturePans(t) ? "pan"
        : null;

    /// <summary>The wheel at <paramref name="p"/> (the handler, and the self-test): digital zoom around the pointer, or the Virtual PTZ's field of view; whether it was used.</summary>
    internal bool PlayerWheelAt(Point p, int delta)
    {
        if (TileAt(p) is not { } t)
        {
            return false;
        }
        if (t.Fisheye is { } fe && fe.Mode.Dewarps())
        {
            // Virtual PTZ zooms (eased); the other dewarped views do nothing.
            if (fe.Mode == FisheyeViewMode.Ptz)
            {
                ZoomPlayerFisheye(t, FisheyePinch.WheelFactor(delta));
            }
            return true;
        }
        if (t.ViewWidth > 0 && t.ViewHeight > 0 && t.PictureWidth > 0)
        {
            // Around the pointer's place in the tile's area (bars included).
            double fx = Math.Clamp((p.X - t.ViewX) / t.ViewWidth, 0, 1), fy = Math.Clamp((p.Y - t.ViewY) / t.ViewHeight, 0, 1);
            StageView.ZoomAround(t, fx, fy, Math.Pow(1.0015, delta));
            return true;
        }
        return false;
    }

    /// <summary>A pinch step at <paramref name="p"/> (the handler, and the self-test): <paramref name="scale"/> above 1 spreads and zooms in.</summary>
    internal bool PlayerPinchAt(Point p, double scale)
    {
        if (TileAt(p) is not { } t)
        {
            return false;
        }
        if (t.Fisheye is { } fe && fe.Mode.Dewarps())
        {
            if (fe.Mode == FisheyeViewMode.Ptz)
            {
                ZoomPlayerFisheye(t, FisheyePinch.Factor(scale));
            }
            return true;
        }
        StageView.PinchAround(t, p.X, p.Y, scale);
        return true;
    }

    // ---- fisheye ----

    private void SetPlayerAim(StageTile t, FisheyeAim aim)
    {
        Views.SetAim(CameraOf(t), aim);
        PublishFisheye(t);
    }

    /// <summary>A fisheye tile's view (the menu): the aim is kept.</summary>
    internal void SetPlayerFisheyeMode(StageTile t, FisheyeViewMode mode)
    {
        _fisheyeZoom.Cancel(t);
        Views.SetFisheyeMode(CameraOf(t), mode);
        PublishFisheye(t);
        RefreshPlayerChrome(t);
    }

    /// <summary>A fisheye camera's mount (saved per camera): the tiles follow at once.</summary>
    internal void SetPlayerFisheyeMount(StageTile t, FisheyeMount mount)
    {
        if (t.CameraOrNone is { } camera)
        {
            _viewer.Settings.SetMount(camera, mount);
        }
        foreach (StageTile other in _chromes.Keys)
        {
            PublishFisheye(other);
        }
    }

    /// <summary>Zooms a Virtual PTZ tile by <paramref name="factor"/> (more than 1 zooms in), eased over the next refreshes.</summary>
    internal void ZoomPlayerFisheye(StageTile t, double factor)
    {
        if (t.Fisheye is not { Mode: FisheyeViewMode.Ptz } fe || !double.IsFinite(factor) || factor <= 0)
        {
            return;
        }
        double from = _fisheyeZoom.Target(t) ?? fe.Aim.Fov;
        _fisheyeZoom.Set(t, from / factor, _viewer.Clock.Now);
    }

    /// <summary>Whether a Virtual PTZ zoom is still easing (self-test).</summary>
    internal bool IsEasingPlayerFisheye => _fisheyeZoom.IsEasing;

    /// <summary>One step of the Virtual PTZ zoom easing (every display refresh, on the viewer's clock).</summary>
    internal void StepPlayerFisheye(double now)
    {
        if (!_fisheyeZoom.IsEasing)
        {
            return;
        }
        foreach ((StageTile t, double fov) in _fisheyeZoom.Tick(now, k => k.Fisheye?.Aim.Fov ?? k.FisheyeSetting.Aim?.Fov ?? 90))
        {
            if (t.Fisheye is { } fe)
            {
                SetPlayerAim(t, (fe.Aim with { Fov = fov }).Clamped(fe.Mount));
            }
        }
    }

    /// <summary>0 on a dewarped tile: back to the mount's default direction and field of view.</summary>
    private bool ResetPlayerFisheye(StageTile t)
    {
        if (t.Fisheye is not { } fe || !fe.Mode.Dewarps())
        {
            return false;
        }
        _fisheyeZoom.Cancel(t);
        Views.ResetFisheye(CameraOf(t));
        PublishFisheye(t);
        return true;
    }

    /// <summary>The header's reset control, Reset Position and 0: a moved virtual camera goes back to its default, else the digital zoom to 1x (a filled crop to the middle).</summary>
    internal void ResetPlayerTile(StageTile t)
    {
        if (!ResetPlayerFisheye(t))
        {
            StageView.ResetZoom(t);
        }
    }

    // ---- what an item does ----

    /// <summary>A header button or menu item for a tile; <paramref name="from"/> is the button, under which a menu opens (null from a key or a menu).</summary>
    internal void PerformPlayerItem(StageTile t, TileHeaderItem item, FrameworkElement? from)
    {
        SelectPlayerTile(t);
        switch (item)
        {
            case TileHeaderItem.SavePicture:
                _ = SavePlayerPictureAsync(t, PictureKind.View, ask: false);
                break;
            case TileHeaderItem.SavePictureAs:
                _ = SavePlayerPictureAsync(t, PictureKind.View, ask: true);
                break;
            case TileHeaderItem.SaveZoomedArea:
                _ = SavePlayerPictureAsync(t, PictureKind.ZoomedArea, ask: false);
                break;
            case TileHeaderItem.SaveOriginalFisheye:
                _ = SavePlayerPictureAsync(t, PictureKind.OriginalFisheye, ask: false);
                break;
            case TileHeaderItem.ResetZoom or TileHeaderItem.ResetPosition:
                ResetPlayerTile(t);
                break;
            case TileHeaderItem.FillTile:
                SetPlayerFill(t, !PlayerFills(t));
                break;
            case TileHeaderItem.Fisheye or TileHeaderItem.More:
                if (PlayerMenuFor(t, item) is { } menu)
                {
                    if (from is not null)
                    {
                        menu.ShowAt(from);
                    }
                    else
                    {
                        menu.ShowAt(_playerArea, new Point(Math.Max(t.FrameX + t.FrameWidth - 40, t.FrameX), t.FrameY + TileChrome.HeaderHeight));
                    }
                }
                break;
        }
        RefreshPlayerChrome(t);
    }

    /// <summary>The menu a header button opens: Fisheye View, or "..." (the right-click menu without what the header shows).</summary>
    internal MenuFlyout? PlayerMenuFor(StageTile t, TileHeaderItem item)
    {
        TileHeaderTile tile = PlayerHeaderTile(t);
        var flyout = new MenuFlyout();
        if (item == TileHeaderItem.More)
        {
            HashSet<TileHeaderItem> shown = [.. PlayerChrome(t)?.HeaderLayout?.Visible ?? []];
            foreach (TileHeaderMenuEntry entry in TileHeaderLayout.Menu(tile).Where(e => !shown.Contains(e.Item)))
            {
                flyout.Items.Add(PlayerMenuItem(entry, t));
            }
            return flyout;
        }
        if (TileHeaderLayout.Menu(tile).FirstOrDefault(e => e.Item == item) is not { } found || PlayerMenuItem(found, t) is not MenuFlyoutSubItem sub)
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

    /// <summary>One header menu entry as a menu item: its label, its key hint, and a submenu where it has one.</summary>
    private MenuFlyoutItemBase PlayerMenuItem(TileHeaderMenuEntry entry, StageTile t)
    {
        switch (entry.Item)
        {
            case TileHeaderItem.Fisheye:
                {
                    var view = new MenuFlyoutSubItem { Text = entry.Label };
                    foreach (FisheyeViewMode mode in Enum.GetValues<FisheyeViewMode>())
                    {
                        var item = new RadioMenuFlyoutItem { Text = mode.Title(), GroupName = "player-fisheye-view", IsChecked = mode == Views.Fisheye(CameraOf(t)).Mode };
                        FisheyeViewMode choice = mode;
                        item.Click += (_, _) => SetPlayerFisheyeMode(t, choice);
                        view.Items.Add(item);
                    }
                    // The mount belongs with the view (it is per camera).
                    view.Items.Add(new MenuFlyoutSeparator());
                    view.Items.Add(new MenuFlyoutItem { Text = "Mount", IsEnabled = false });
                    FisheyeMount current = t.CameraOrNone is { } camera ? _viewer.Settings.Mount(camera) : FisheyeMount.Ceiling;
                    foreach (FisheyeMount mount in Enum.GetValues<FisheyeMount>())
                    {
                        var item = new RadioMenuFlyoutItem { Text = mount.Title(), GroupName = "player-fisheye-mount", IsChecked = mount == current };
                        FisheyeMount choice = mount;
                        item.Click += (_, _) => SetPlayerFisheyeMount(t, choice);
                        view.Items.Add(item);
                    }
                    return view;
                }
            case TileHeaderItem.FillTile:
                {
                    var fill = new ToggleMenuFlyoutItem { Text = entry.Label, IsChecked = PlayerFills(t) };
                    fill.Click += (_, _) => PerformPlayerItem(t, TileHeaderItem.FillTile, null);
                    ToolTipService.SetToolTip(fill, "Crop this camera's picture to fill its tile; drag the picture to choose the part shown");
                    return fill;
                }
            default:
                {
                    TileHeaderItem what = entry.Item;
                    var item = new MenuFlyoutItem { Text = entry.Label };
                    if (entry.WindowsShortcut.Length > 0)
                    {
                        item.KeyboardAcceleratorTextOverride = entry.WindowsShortcut;
                    }
                    item.Click += (_, _) => PerformPlayerItem(t, what, null);
                    if (what == TileHeaderItem.ResetPosition)
                    {
                        ToolTipService.SetToolTip(item, "Show the middle of the filled picture again");
                    }
                    return item;
                }
        }
    }

    // ---- keys ----

    /// <summary>
    /// The tile keys (0, P, Shift+P) for the tile under the pointer, else the selected one, else the large (or first) one.
    /// True if used.
    /// </summary>
    internal bool HandlePlayerKey(VirtualKey key, bool ctrl, bool alt, bool shift)
    {
        if (Browser.PlayerView != PlayerView.Cameras || PlayerKeyTile() is not { } t)
        {
            return false;
        }
        if (key is VirtualKey.Number0 or VirtualKey.NumberPad0 && !ctrl && !alt)
        {
            ResetPlayerTile(t);
            return true;
        }
        if (key == VirtualKey.P)
        {
            TileAction action = TileKeyMap.Map(TileKey.P, ctrl, alt, shift, false);
            TileHeaderItem? item = action switch
            {
                TileAction.SavePicture => TileHeaderItem.SavePicture,
                TileAction.SavePictureAs => TileHeaderItem.SavePictureAs,
                _ => null,
            };
            if (item is { } i)
            {
                PerformPlayerItem(t, i, null);
                return true;
            }
        }
        return false;
    }

    private StageTile? PlayerKeyTile()
    {
        if (_hoveredTile is { } hovered)
        {
            return hovered;
        }
        if (_selectedTile is { } selected)
        {
            return selected;
        }
        RecordingsPlayer player = Browser.Player;
        return PlayerGrid.AudibleIndex(player.Mode, player.Cells.Count) is int i ? PlayerTile(i) : null;
    }

    // ---- save picture ----

    /// <summary>Saves the picture of a player tile (docs/CAPTURE.md "Save picture"): the same path and rules as a live tile's.</summary>
    internal Task SavePlayerPictureAsync(StageTile t, PictureKind kind, bool ask) =>
        PictureSaving.SaveAsync(_viewer, t, kind, ask, AskWhere, ShowToast, FinishSave, PictureObserver);

    private async Task<string?> AskWhere(string suggested, PictureFormat format)
    {
        if (ChoosePictureFile is { } hook)
        {
            return await hook(suggested, format);
        }
        return await PictureSaving.PickAsync(AppWindow.Id, suggested, format);
    }

    private void FinishSave(PictureSaveResult result, PictureKind kind, PicturePlan? plan, int pw, int ph)
    {
        LastPictureSave = new PictureSaveRecord(result, kind, plan, pw, ph);
        PictureSaves++;
        PictureSaving.Confirm(result, ShowToast);
    }

    // ---- the short message over the player ----

    /// <summary>Shows <paramref name="text"/> over the player for a few seconds.</summary>
    internal void ShowToast(string text, string? actionTitle = null, Action? action = null)
    {
        _toastText.Text = text;
        AutomationProperties.SetName(_toast, text);
        _toastAction = action;
        _toastButton.Content = actionTitle;
        _toastButton.Visibility = actionTitle is not null && action is not null ? Visibility.Visible : Visibility.Collapsed;
        // A message with a button takes the pointer and stays longer.
        _toast.IsHitTestVisible = _toastButton.Visibility == Visibility.Visible;
        _toast.Visibility = Visibility.Visible;
        if (_toastTimer is { } timer)
        {
            timer.Interval = TimeSpan.FromSeconds(_toast.IsHitTestVisible ? PlayerActionToastSeconds : ToastSeconds);
            timer.Start();
        }
    }

    private void HideToast()
    {
        _toastTimer?.Stop();
        _toast.Visibility = Visibility.Collapsed;
    }

    /// <summary>What the message says now, null when none shows (self-test).</summary>
    internal string? ToastText => _toast.Visibility == Visibility.Visible ? _toastText.Text : null;

    /// <summary>The title of the button on the message now, null when it has none (self-test).</summary>
    internal string? ToastActionTitle => _toast.Visibility == Visibility.Visible && _toastButton.Visibility == Visibility.Visible ? _toastButton.Content as string : null;
}
