// The Recordings window's center (docs/UX.md "Recordings"): the player with up
// to four cameras of the day drawn by the window's own StageRenderer (the
// app's D3D11 tile renderer; a swap chain panel never moves between windows),
// the transport (back and forward 10 s, play and pause, the device time at the
// playhead, the speed, Split Here, mute and volume, the one-camera button),
// the scrubber with its Incident and Day switch, and the day's timeline lanes.

using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NvrView.Interop;
using NvrView.Shell;
using NvrView.Tiles;
using Rect = NvrView.Shell.Rect;

namespace NvrView.App;

internal sealed partial class RecordingsWindow
{
    private readonly Grid _playerColumn = new();
    private readonly Grid _playerArea = new() { Background = new SolidColorBrush(Colors.Black) };
    private readonly SwapChainPanel _panel = new();
    private readonly Canvas _labels = new() { IsHitTestVisible = false };
    private readonly TextBlock _emptyText = new() { HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center, Foreground = new SolidColorBrush(Muted), TextAlignment = TextAlignment.Center, TextWrapping = TextWrapping.Wrap, IsHitTestVisible = false, MaxWidth = 360 };
    private readonly Button _back = new() { Content = "−10 s" };
    private readonly Button _forward = new() { Content = "+10 s" };
    private readonly Button _play = new();
    private readonly FontIcon _playIcon = new() { Glyph = "", FontSize = 14 };
    private readonly TextBlock _time = new() { VerticalAlignment = VerticalAlignment.Center, FontFamily = new FontFamily("Consolas"), MinWidth = 62, Text = "--:--:--" };
    private readonly DropDownButton _speed = new() { Content = "1×" };
    private readonly TextBlock _speedHint = new() { FontSize = 12, VerticalAlignment = VerticalAlignment.Center, Foreground = new SolidColorBrush(Muted) };
    private readonly Button _splitHere = new() { Content = "Split Here" };
    private readonly ToggleButton _mute = new();
    private readonly Slider _volume = new() { Minimum = 0, Maximum = 100, Value = 100, Width = 90, VerticalAlignment = VerticalAlignment.Center };
    private readonly Button _oneCamera = new();
    private readonly ScrubBar _scrub = new();
    private readonly TextBlock _scrubStart = new() { FontSize = 11, Foreground = new SolidColorBrush(Muted), VerticalAlignment = VerticalAlignment.Center, Width = 56 };
    private readonly TextBlock _scrubEnd = new() { FontSize = 11, Foreground = new SolidColorBrush(Muted), VerticalAlignment = VerticalAlignment.Center, Width = 56, TextAlignment = TextAlignment.Right };
    private readonly ToggleButton _scopeIncident = new() { Content = "Incident" };
    private readonly ToggleButton _scopeDay = new() { Content = "Day" };
    private readonly LanesView _lanes = new();
    private readonly RowDefinition _lanesRow = new() { Height = new GridLength(0) };
    private readonly Dictionary<ulong, StageTile> _stageTiles = [];
    private StageRenderer? _renderer;
    private Rect?[] _cellRects = [];
    private bool _settingVolume;

    private void BuildPlayer()
    {
        _playerColumn.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star), MinHeight = RecordingsPanes.PlayerMin });
        _playerColumn.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        _playerColumn.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        _playerColumn.RowDefinitions.Add(_lanesRow);

        // The picture area: the window's own swap chain, labels over it, and a line when nothing plays.
        _playerArea.Children.Add(_panel);
        _playerArea.Children.Add(_labels);
        _playerArea.Children.Add(_emptyText);
        _playerArea.Tapped += OnPlayerTapped;
        _playerArea.DoubleTapped += OnPlayerDoubleTapped;
        _playerArea.RightTapped += OnPlayerRightTapped;
        _playerArea.SizeChanged += (_, _) =>
        {
            PushSize();
            UpdateStage();
        };
        _panel.CompositionScaleChanged += (_, _) => PushSize();
        _renderer = new StageRenderer(_host, chain => _ui.TryEnqueue(() =>
        {
            if (_closed)
            {
                if (chain != 0)
                {
                    System.Runtime.InteropServices.Marshal.Release(chain);
                }
                return;
            }
            SwapChainAttach.To(_panel, chain);
        }));
        _renderer.Start();
        Grid.SetRow(_playerArea, 0);
        _playerColumn.Children.Add(_playerArea);

        // Transport.
        var controls = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, Margin = new Thickness(10, 6, 10, 0) };
        _back.Click += (_, _) => Browser.Player.Skip(-TransportState.SkipMs);
        _forward.Click += (_, _) => Browser.Player.Skip(TransportState.SkipMs);
        _play.Content = _playIcon;
        _play.Click += (_, _) => Browser.Player.TogglePlay();
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_play, "Play");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_back, "Back 10 seconds");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_forward, "Forward 10 seconds");
        ToolTipService.SetToolTip(_back, "Back 10 seconds");
        ToolTipService.SetToolTip(_forward, "Forward 10 seconds");
        ToolTipService.SetToolTip(_time, "The device's time at the playhead");
        var speedMenu = new MenuFlyout();
        foreach (double rate in PlaybackSpeed.All)
        {
            var item = new RadioMenuFlyoutItem { Text = PlaybackSpeed.Label(rate), GroupName = "playback-speed", Tag = rate };
            double r = rate;
            item.Click += (_, _) => Browser.Player.SetRate(r);
            speedMenu.Items.Add(item);
        }
        _speed.Flyout = speedMenu;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_speed, "Speed");
        controls.Children.Add(_back);
        controls.Children.Add(_play);
        controls.Children.Add(_forward);
        controls.Children.Add(_time);
        controls.Children.Add(_speed);
        controls.Children.Add(_speedHint);

        var more = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, Margin = new Thickness(10, 0, 10, 0) };
        _splitHere.Click += (_, _) =>
        {
            if (Browser.PlayingIncident is { } incident)
            {
                Browser.SplitHere(incident);
            }
        };
        ToolTipService.SetToolTip(_splitHere, "Make the incident two at the playhead");
        _mute.Content = new FontIcon { Glyph = "", FontSize = 14 };
        _mute.Click += (_, _) => Browser.Player.Muted = _mute.IsChecked == true;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_mute, "Mute");
        ToolTipService.SetToolTip(_mute, "Mute");
        _volume.ValueChanged += (_, e) =>
        {
            if (!_settingVolume)
            {
                Browser.Player.Volume = (float)(e.NewValue / 100.0);
            }
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_volume, "Volume");
        _oneCamera.Content = new FontIcon { Glyph = "", FontSize = 14 };
        _oneCamera.Click += (_, _) =>
        {
            PlayerGridMode now = Browser.Player.Mode;
            int index = PlayerGrid.AudibleIndex(now, Browser.Player.Cells.Count) ?? 0;
            Browser.Player.DoubleClick(index);
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_oneCamera, "Show one camera");
        ToolTipService.SetToolTip(_oneCamera, "Show one camera, or all");
        more.Children.Add(_splitHere);
        more.Children.Add(_mute);
        more.Children.Add(_volume);
        more.Children.Add(_oneCamera);

        var transport = new StackPanel { Spacing = 4 };
        transport.Children.Add(controls);
        transport.Children.Add(more);
        Grid.SetRow(transport, 1);
        _playerColumn.Children.Add(transport);

        // Scrubber with the Incident | Day switch.
        var scrubRow = new Grid { Margin = new Thickness(10, 4, 10, 4), ColumnSpacing = 6 };
        scrubRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        scrubRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        scrubRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        scrubRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(_scrub, 1);
        Grid.SetColumn(_scrubEnd, 2);
        var scope = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 2 };
        scope.Children.Add(_scopeIncident);
        scope.Children.Add(_scopeDay);
        Grid.SetColumn(scope, 3);
        scrubRow.Children.Add(_scrubStart);
        scrubRow.Children.Add(_scrub);
        scrubRow.Children.Add(_scrubEnd);
        scrubRow.Children.Add(scope);
        _scopeIncident.Click += (_, _) => SetScrubScope(ScrubScope.Incident);
        _scopeDay.Click += (_, _) => SetScrubScope(ScrubScope.Day);
        _scrub.Seeked += (ms, final) => Browser.Player.Seek(ms, exact: final);
        Grid.SetRow(scrubRow, 2);
        _playerColumn.Children.Add(scrubRow);

        _lanes.Margin = new Thickness(10, 0, 10, 6);
        _lanes.LaneClicked += (camera, second, near) => Browser.LaneClick(camera, second, near);
        _lanes.ViewportRequested += view =>
        {
            Browser.Timeline = view;
            RefreshLanes();
        };
        Grid.SetRow(_lanes, 3);
        _playerColumn.Children.Add(_lanes);

        _centerHost.Children.Add(_playerColumn);
    }

    private void SetScrubScope(ScrubScope scope)
    {
        Browser.ScrubScope = scope;
        RefreshPlayer();
    }

    private void PushSize() =>
        _renderer?.SetPanelSize(_panel.ActualWidth, _panel.ActualHeight, _panel.CompositionScaleX, _panel.CompositionScaleY);

    /// <summary>Stops the render thread and lets go of the swap chain (the window is closing).</summary>
    private void ClosePlayerView()
    {
        if (_renderer is { } r)
        {
            _renderer = null;
            r.Layout = StageLayout.Empty;
            r.Dispose();
            SwapChainAttach.To(_panel, 0);
        }
        _stageTiles.Clear();
    }

    /// <summary>The window's renderer (self-test).</summary>
    internal StageRenderer? Renderer => _renderer;

    // ---- refreshing ----

    private void RefreshPlayer()
    {
        RecordingsPlayer player = Browser.Player;
        TransportState t = player.Transport;
        bool open = player.Playback is not null;
        _playIcon.Glyph = t.ShowsPlay ? "" : "";
        string playName = t.ShowsPlay ? "Play" : "Pause";
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_play, playName);
        ToolTipService.SetToolTip(_play, playName);
        _play.IsEnabled = _back.IsEnabled = _forward.IsEnabled = _speed.IsEnabled = open;
        _time.Text = open ? TransportState.Clock(t.PositionMs) : "--:--:--";
        _speed.Content = PlaybackSpeed.Label(player.Rate);
        foreach (RadioMenuFlyoutItem item in ((MenuFlyout)_speed.Flyout).Items.OfType<RadioMenuFlyoutItem>())
        {
            item.IsChecked = item.Tag is double r && r == player.Rate;
        }
        _speedHint.Text = PlaybackSpeed.Hint(player.Rate) ?? "";
        _mute.IsChecked = player.Muted;
        ((FontIcon)_mute.Content).Glyph = player.Muted ? "" : "";
        _settingVolume = true;
        _volume.Value = player.Volume * 100;
        _settingVolume = false;
        _oneCamera.IsEnabled = player.Cells.Count > 1;
        _volume.IsEnabled = _mute.IsEnabled = open;
        _splitHere.IsEnabled = Browser.PlayingIncident is { } playing && Browser.SplitSecond(playing) is not null;
        _scopeIncident.IsChecked = Browser.ScrubScope == ScrubScope.Incident;
        _scopeDay.IsChecked = Browser.ScrubScope == ScrubScope.Day;

        if (Browser.ScrubRange is { } range)
        {
            // Marks: the events of the shown lanes inside the range.
            IEnumerable<TimeMark> marks = Browser.Lanes.SelectMany(l => l.Events)
                .Where(e => e.End * 1000 >= range.Start && e.Start * 1000 <= range.End)
                .Select(e => new TimeMark(e.Start * 1000, e.End * 1000, e.Kind));
            _scrub.Set(range.Start, range.End, marks, t.PositionMs);
            _scrubStart.Text = TransportState.Clock(range.Start);
            _scrubEnd.Text = TransportState.Clock(range.End);
        }
        else
        {
            _scrub.Set(0, 0, [], 0);
            _scrubStart.Text = _scrubEnd.Text = "";
        }
        _lanes.Set(Browser.Lanes, Browser.Day, Browser.Timeline, open ? t.PositionMs : null);
        _emptyText.Text = open ? ""
            : Browser.Player.Message is { } m ? m
            : Browser.Cameras.Count == 0 ? "Choose cameras to search, then pick an incident to play."
            : "Select an incident to play it, or click a camera's lane.";
        UpdateStage();
    }

    private void RefreshLanes()
    {
        double height = Math.Max(_playerColumn.ActualHeight, RecordingsPanes.WindowMinHeight - 80);
        _lanesRow.Height = new GridLength(RecordingsPanes.LanesHeight(height, Browser.Lanes.Count));
        _lanes.Set(Browser.Lanes, Browser.Day, Browser.Timeline, Browser.Player.Playback is not null ? Browser.Player.PositionMs : null);
    }

    // ---- the picture area ----

    private StageTile TileFor(PlayerCell cell)
    {
        if (_stageTiles.TryGetValue(cell.Tile, out StageTile? known))
        {
            return known;
        }
        var camera = new CameraRef(cell.Camera.Device, cell.Camera.Channel, cell.Camera.Lens);
        var tile = new StageTile(_viewer, NodePath.Root, camera, TileQuality.Auto);
        if (_viewer.FindDevice(cell.Camera.Device) is { } device && _host.VideoOf(cell.Tile) is { } video)
        {
            tile.Shared = new CameraTile(cell.Tile, device, camera, video);
        }
        _stageTiles[cell.Tile] = tile;
        return tile;
    }

    private void UpdateStage()
    {
        if (_renderer is not { } renderer)
        {
            return;
        }
        RecordingsPlayer player = Browser.Player;
        double w = _playerArea.ActualWidth, h = _playerArea.ActualHeight;
        List<PlayerCell> cells = player.Cells;
        foreach (ulong gone in _stageTiles.Keys.Where(id => cells.All(c => c.Tile != id)).ToList())
        {
            _stageTiles.Remove(gone);
        }
        _cellRects = PlayerGrid.Cells(cells.Count, player.Mode, new Rect(0, 0, w, h));
        var tiles = new List<StageTile>();
        var rects = new List<Rect>();
        _labels.Children.Clear();
        for (int i = 0; i < cells.Count; i++)
        {
            if (_cellRects[i] is not { } r)
            {
                continue;
            }
            tiles.Add(TileFor(cells[i]));
            rects.Add(r);
            AddLabel(cells[i], r);
        }
        renderer.Layout = new StageLayout([.. tiles], [.. rects], w, h);
    }

    private void AddLabel(PlayerCell cell, Rect r)
    {
        string? state = cell.State switch
        {
            "buffering" => "Loading…",
            "late" => "Catching up…",
            "no_recording" => "No recording at this time",
            "ended" => "End of this camera's recording",
            "failed" => cell.Reason switch
            {
                "device_in_use" => "Playing in another window",
                "limit" => "Too many plays on this device",
                "undecodable" => "This picture cannot be decoded",
                _ => "Could not play",
            },
            _ => null,
        };
        var name = new TextBlock { Text = Service.CameraName(cell.Camera), FontSize = 12, Foreground = new SolidColorBrush(Colors.White), Padding = new Thickness(6, 2, 6, 2) };
        var chip = new Border { Child = name, Background = new SolidColorBrush(Windows.UI.Color.FromArgb(0x90, 0, 0, 0)), CornerRadius = new CornerRadius(3) };
        Canvas.SetLeft(chip, r.X + 6);
        Canvas.SetTop(chip, r.Y + 6);
        _labels.Children.Add(chip);
        if (state is not null)
        {
            var message = new TextBlock { Text = state, FontSize = 13, Foreground = new SolidColorBrush(Colors.White), TextAlignment = TextAlignment.Center, Width = Math.Max(r.Width - 12, 10), TextWrapping = TextWrapping.Wrap };
            Canvas.SetLeft(message, r.X + 6);
            Canvas.SetTop(message, r.Y + r.Height / 2 - 10);
            _labels.Children.Add(message);
        }
    }

    private int CellAt(Windows.Foundation.Point p)
    {
        for (int i = 0; i < _cellRects.Length; i++)
        {
            if (_cellRects[i] is { } r && r.Contains(p.X, p.Y))
            {
                return i;
            }
        }
        return -1;
    }

    private void OnPlayerTapped(object sender, TappedRoutedEventArgs e)
    {
        int i = CellAt(e.GetPosition(_playerArea));
        if (i >= 0)
        {
            Browser.Player.Click(i);
        }
    }

    private void OnPlayerDoubleTapped(object sender, DoubleTappedRoutedEventArgs e)
    {
        int i = CellAt(e.GetPosition(_playerArea));
        if (i >= 0)
        {
            Browser.Player.DoubleClick(i);
        }
    }

    private void OnPlayerRightTapped(object sender, RightTappedRoutedEventArgs e)
    {
        Windows.Foundation.Point point = e.GetPosition(_playerArea);
        int i = CellAt(point);
        if (i < 0)
        {
            return;
        }
        RecordingsPlayer player = Browser.Player;
        var menu = new MenuFlyout();
        var alone = new MenuFlyoutItem { Text = "Show Alone", IsEnabled = player.Cells.Count > 1 };
        alone.Click += (_, _) => player.Mode = new PlayerGridMode.Single(i);
        var large = new MenuFlyoutItem { Text = "Make Large", IsEnabled = player.Cells.Count > 1 };
        large.Click += (_, _) => player.Mode = new PlayerGridMode.Emphasis(i);
        var all = new MenuFlyoutItem { Text = "Show All Cameras" };
        all.Click += (_, _) => player.Mode = PlayerGridMode.Tiled;
        menu.Items.Add(alone);
        menu.Items.Add(large);
        menu.Items.Add(all);
        if (Browser.PlayingIncident is { Cameras.Count: > 1 } incident && i < player.Cells.Count)
        {
            RecordingCamera camera = player.Cells[i].Camera;
            menu.Items.Add(new MenuFlyoutSeparator());
            var remove = new MenuFlyoutItem { Text = "Remove from Incident" };
            remove.Click += (_, _) => Browser.Remove(camera, incident.Key);
            menu.Items.Add(remove);
        }
        menu.ShowAt(_playerArea, point);
        e.Handled = true;
    }

    // ---- self-test views ----

    /// <summary>The transport as drawn: play button shows play, the time text, the speed text and hint.</summary>
    public (bool ShowsPlay, string Time, string Speed, string Hint, bool SplitEnabled) TransportShown() =>
        (_playIcon.Glyph == "", _time.Text, _speed.Content?.ToString() ?? "", _speedHint.Text, _splitHere.IsEnabled);

    /// <summary>The player area's size in DIPs.</summary>
    public (double Width, double Height) PlayerAreaSize => (_playerArea.ActualWidth, _playerArea.ActualHeight);

    /// <summary>The cells as laid out for the renderer (self-test).</summary>
    public Rect?[] CellRects => _cellRects;

    /// <summary>The scrubber (self-test).</summary>
    internal ScrubBar Scrubber => _scrub;

    /// <summary>The lanes view (self-test).</summary>
    internal LanesView LanesShown => _lanes;

    /// <summary>The scrubber's end labels as drawn.</summary>
    public (string Start, string End) ScrubLabels => (_scrubStart.Text, _scrubEnd.Text);

    /// <summary>The window's player column is shown (not the downloads).</summary>
    public bool PlayerColumnShown => _playerColumn.Visibility == Visibility.Visible;

    /// <summary>The labels drawn over the picture, in order.</summary>
    public List<string> LabelTexts() =>
        [.. _labels.Children.OfType<Border>().Select(b => ((TextBlock)b.Child).Text)];
}
