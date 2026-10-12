// The Recordings window (docs/UX.md "Recordings"): Ctrl+Alt+R, or "Recordings"
// in the header's "..." menu. A window of its own, not a sidebar section: live
// view stays as it is while recordings are searched and played, and the window
// can sit on another screen. The player is the middle of the window, with the
// transport and the day's timeline lanes under it; on the left (collapsible)
// the calendar, the incident list with its filters, and the cameras to search;
// on the right (collapsible) the selected incident's details and actions; the
// header has the two pane buttons, the shown day with previous and next, and
// Player or Downloads. One instance: opening it again brings it forward.
//
// The window hosts its own render thread and swap chain for the player
// (never a panel moved between windows); the playback's tiles are tiles of
// the shared core like live ones, so a GPU device loss replaces their decoders
// with everything else. The window's state is RecordingsBrowser (NvrView.Shell,
// tested on Linux); this file and its partial parts only draw it and pass
// the user's actions to it.

using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.Graphics;
using static TerraFX.Interop.Windows.SWP;
using static TerraFX.Interop.Windows.Windows;
using Rect = NvrView.Shell.Rect;

namespace NvrView.App;

internal sealed partial class RecordingsWindow
{
    private static readonly Windows.UI.Color PanelBack = Windows.UI.Color.FromArgb(0xFF, 0x20, 0x21, 0x24);
    private static readonly Windows.UI.Color Muted = Windows.UI.Color.FromArgb(0xFF, 0xA0, 0xA3, 0xA8);

    private readonly Viewer _viewer;
    private readonly CoreHost _host;
    private readonly bool _background;
    private readonly DispatcherQueue _ui = DispatcherQueue.GetForCurrentThread();
    private readonly DispatcherQueueTimer _clock;
    private readonly DispatcherQueueTimer _progress;
    private readonly DispatcherQueueTimer _loudness;
    private readonly Grid _root = new() { RequestedTheme = ElementTheme.Dark, Background = new SolidColorBrush(PanelBack) };
    private readonly ColumnDefinition _sidebarColumn = new() { Width = new GridLength(0) };
    private readonly ColumnDefinition _inspectorColumn = new() { Width = new GridLength(0) };
    private readonly Grid _body = new();
    private readonly Border _sidebarHost = new() { Width = RecordingsPanes.SidebarWidth, BorderThickness = new Thickness(0, 0, 1, 0), BorderBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(0x40, 0x80, 0x80, 0x80)) };
    private readonly Border _inspectorHost = new() { Width = RecordingsPanes.InspectorWidth, BorderThickness = new Thickness(1, 0, 0, 0), BorderBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(0x40, 0x80, 0x80, 0x80)) };
    private readonly Grid _centerHost = new();
    private readonly TextBlock _dayTitle = new() { VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, FontSize = 15 };
    private readonly Button _sidebarButton = new();
    private readonly Button _previousDay = new();
    private readonly Button _nextDay = new();
    private readonly ToggleButton _playerTab = new() { Content = "Player" };
    private readonly ToggleButton _downloadsTab = new();
    private readonly Button _inspectorButton = new();
    private readonly Grid _notice = new() { Visibility = Visibility.Collapsed, Padding = new Thickness(12, 6, 12, 6), BorderThickness = new Thickness(0, 1, 0, 0), BorderBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(0x40, 0x80, 0x80, 0x80)) };
    private readonly TextBlock _noticeText = new() { TextWrapping = TextWrapping.Wrap, Foreground = new SolidColorBrush(Muted), MaxLines = 2, VerticalAlignment = VerticalAlignment.Center };
    private bool _placing;
    private bool _closed;
    private bool _refreshScheduled;
    private bool _transportScheduled;
    private (bool Sidebar, bool Inspector) _shown = (false, false);
    private double _width;

    private RecordingsWindow(Viewer viewer, CoreHost host, RecordingsService service, bool background)
    {
        _viewer = viewer;
        _host = host;
        _background = background;
        Service = service;
        Created++;
        Window = new Window { Title = "Recordings - nvrview" };
        AppWindow = Window.AppWindow;
        var exportSettings = new ExportSettings(ExportSettingsPath);
        exportSettings.Load();
        Browser = new RecordingsBrowser(service, exportSettings);

        AttachExport(host);
        BuildHeader(out Grid header);
        BuildSidebar();
        BuildPlayer();
        BuildInspector();
        BuildDownloads();

        _body.ColumnDefinitions.Add(_sidebarColumn);
        _body.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star), MinWidth = 0 });
        _body.ColumnDefinitions.Add(_inspectorColumn);
        Grid.SetColumn(_sidebarHost, 0);
        Grid.SetColumn(_centerHost, 1);
        Grid.SetColumn(_inspectorHost, 2);
        _body.Children.Add(_sidebarHost);
        _body.Children.Add(_centerHost);
        _body.Children.Add(_inspectorHost);

        var dismiss = new Button { Content = "Dismiss", Padding = new Thickness(8, 2, 8, 2) };
        dismiss.Click += (_, _) =>
        {
            Service.Notice = null;
            Browser.Message = null;
            Browser.Player.Dismiss();
            RefreshNotice();
        };
        _notice.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        _notice.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _notice.Children.Add(_noticeText);
        Grid.SetColumn(dismiss, 1);
        _notice.Children.Add(dismiss);

        _root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        _root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        _root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        Grid.SetRow(header, 0);
        Grid.SetRow(_body, 1);
        Grid.SetRow(_notice, 2);
        _root.Children.Add(header);
        _root.Children.Add(_body);
        _root.Children.Add(_notice);
        Window.Content = _root;
        _root.SizeChanged += (_, e) =>
        {
            _width = e.NewSize.Width;
            ApplyPanes();
        };

        Browser.Changed += ScheduleRefresh;
        Browser.Player.Changed += ScheduleTransport;
        Service.DownloadsChanged += OnDownloadsChanged;
        AppWindow.Changed += OnAppWindowChanged;
        Window.Closed += (_, _) => OnClosed();

        // The scrubber's reading, ten times a second (nv_playback_position is lock-free and answers from memory).
        _clock = _ui.CreateTimer();
        _clock.Interval = TimeSpan.FromMilliseconds(100);
        _clock.IsRepeating = true;
        _clock.Tick += (_, _) => Browser.Player.Tick();
        _clock.Start();
        // Download progress alone redraws at most four times a second.
        _progress = _ui.CreateTimer();
        _progress.Interval = TimeSpan.FromMilliseconds(250);
        _progress.IsRepeating = true;
        _progress.Tick += (_, _) =>
        {
            Service.FlushProgress();
            if (Browser.Export.Busy)
            {
                RefreshExport();
            }
        };
        _progress.Start();
        // Today's loudness now and then: live seconds arrive with no event (FFI-RECORDINGS.md section 2a, a pull API).
        _loudness = _ui.CreateTimer();
        _loudness.Interval = TimeSpan.FromSeconds(10);
        _loudness.IsRepeating = true;
        _loudness.Tick += (_, _) =>
        {
            if (Browser.IsToday)
            {
                Browser.ReloadLoudness(force: true);
                RefreshLanes();
            }
        };
        _loudness.Start();

        Place();
        Refresh();
    }

    /// <summary>The open Recordings window, if any (there is at most one).</summary>
    public static RecordingsWindow? Current { get; private set; }

    /// <summary>How many Recordings windows were ever made (the self-test checks that a second open makes none).</summary>
    public static int Created { get; private set; }

    public Window Window { get; }

    public AppWindow AppWindow { get; }

    /// <summary>The recordings model this window shows.</summary>
    public RecordingsService Service { get; }

    /// <summary>What the window shows.</summary>
    public RecordingsBrowser Browser { get; }

    /// <summary>The window is built and its content loaded.</summary>
    public bool IsLoaded => _root.IsLoaded;

    /// <summary>Which side panes are shown now (a narrow window gives way).</summary>
    public (bool Sidebar, bool Inspector) ShownPanes => _shown;

    /// <summary>The width the panes share, in DIPs (what the panes were chosen for).</summary>
    public double ContentWidth => _width;

    /// <summary>Whether the window has been closed.</summary>
    public bool IsClosed => _closed;

    /// <summary>
    /// Opens the window, or brings the open one forward. <paramref name="background"/> keeps it behind the others and
    /// never activates it (a test run).
    /// </summary>
    public static RecordingsWindow Open(Viewer viewer, CoreHost host, RecordingsService service, bool background = false)
    {
        bool fresh = Current is null;
        RecordingsWindow w = Current ??= new RecordingsWindow(viewer, host, service, background);
        if (fresh)
        {
            w.Reveal();
        }
        else
        {
            w.BringToFront();
        }
        return w;
    }

    /// <summary>Closes the window if open (the app is quitting).</summary>
    public static void CloseCurrent() => Current?.Close();

    public void Close()
    {
        if (!_closed)
        {
            Window.Close();
        }
    }

    // ---- header ----

    private static Button IconButton(string glyph, string tip)
    {
        var b = new Button
        {
            Content = new FontIcon { Glyph = glyph, FontSize = 14 },
            Width = 36,
            Height = 32,
            Padding = new Thickness(0),
            Background = new SolidColorBrush(Colors.Transparent),
            BorderThickness = new Thickness(0),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(b, tip);
        ToolTipService.SetToolTip(b, tip);
        return b;
    }

    private void BuildHeader(out Grid header)
    {
        header = new Grid { Height = 40, Padding = new Thickness(8, 0, 8, 0), BorderThickness = new Thickness(0, 0, 0, 1), BorderBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(0x40, 0x80, 0x80, 0x80)) };
        // Columns: sidebar button, previous day, the day (star), next day, Player | Downloads, a gap, details button.
        GridLength[] widths = [GridLength.Auto, GridLength.Auto, new GridLength(1, GridUnitType.Star), GridLength.Auto, GridLength.Auto, new GridLength(8), GridLength.Auto];
        foreach (GridLength w in widths)
        {
            header.ColumnDefinitions.Add(new ColumnDefinition { Width = w });
        }
        _sidebarButton.Content = new FontIcon { Glyph = "\uE700", FontSize = 14 };
        StyleIcon(_sidebarButton, "Calendar and incidents");
        _sidebarButton.Click += (_, _) => TogglePane(inspector: false);
        _previousDay.Content = new FontIcon { Glyph = "\uE76B", FontSize = 12 };
        StyleIcon(_previousDay, "Previous day");
        _previousDay.Click += (_, _) => Browser.PreviousDay();
        _nextDay.Content = new FontIcon { Glyph = "\uE76C", FontSize = 12 };
        StyleIcon(_nextDay, "Next day");
        _nextDay.Click += (_, _) => Browser.NextDay();
        _dayTitle.Margin = new Thickness(6, 0, 6, 0);
        var tabs = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        _playerTab.Click += (_, _) => ShowCenter(RecordingsCenter.Player);
        _downloadsTab.Click += (_, _) => ShowCenter(RecordingsCenter.Downloads);
        tabs.Children.Add(_playerTab);
        tabs.Children.Add(_downloadsTab);
        _inspectorButton.Content = new FontIcon { Glyph = "\uE946", FontSize = 14 };
        StyleIcon(_inspectorButton, "Incident details");
        _inspectorButton.Click += (_, _) => TogglePane(inspector: true);
        Grid.SetColumn(_sidebarButton, 0);
        Grid.SetColumn(_previousDay, 1);
        Grid.SetColumn(_dayTitle, 2);
        Grid.SetColumn(_nextDay, 3);
        Grid.SetColumn(tabs, 4);
        Grid.SetColumn(_inspectorButton, 6);
        header.Children.Add(_sidebarButton);
        header.Children.Add(_previousDay);
        header.Children.Add(_dayTitle);
        header.Children.Add(_nextDay);
        header.Children.Add(tabs);
        header.Children.Add(_inspectorButton);
    }

    private static void StyleIcon(Button b, string tip)
    {
        b.Width = 36;
        b.Height = 32;
        b.Padding = new Thickness(0);
        b.Background = new SolidColorBrush(Colors.Transparent);
        b.BorderThickness = new Thickness(0);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(b, tip);
        ToolTipService.SetToolTip(b, tip);
    }

    private void ShowCenter(RecordingsCenter center)
    {
        Browser.Center = center;
        RefreshCenter();
    }

    private void RefreshCenter()
    {
        bool player = Browser.Center == RecordingsCenter.Player;
        _playerTab.IsChecked = player;
        _downloadsTab.IsChecked = !player;
        _playerColumn.Visibility = player ? Visibility.Visible : Visibility.Collapsed;
        _downloadsPanel.Visibility = player ? Visibility.Collapsed : Visibility.Visible;
        int n = Service.Downloads.Count;
        _downloadsTab.Content = n > 0 ? $"Downloads ({n})" : "Downloads";
    }

    // ---- panes ----

    /// <summary>The header's pane buttons: turning a pane on in a window too narrow for it grows the window.</summary>
    public void TogglePane(bool inspector)
    {
        if (inspector)
        {
            Browser.ShowInspector = !Browser.ShowInspector;
        }
        else
        {
            Browser.ShowSidebar = !Browser.ShowSidebar;
        }
        double need = RecordingsPanes.CenterMin + (Browser.ShowSidebar ? RecordingsPanes.SidebarWidth : 0)
            + (Browser.ShowInspector ? RecordingsPanes.InspectorWidth : 0);
        if (need > _width)
        {
            Grow(need);
        }
        ApplyPanes();
    }

    // The window grows (within its screen) so the player keeps its room.
    private void Grow(double widthDips)
    {
        double scale = _root.XamlRoot?.RasterizationScale ?? 1;
        DisplayArea area = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary);
        int wanted = (int)Math.Ceiling((widthDips - _width) * scale) + AppWindow.Size.Width;
        int room = area.WorkArea.X + area.WorkArea.Width - AppWindow.Position.X;
        int width = Math.Min(wanted, Math.Max(room, AppWindow.Size.Width));
        if (width > AppWindow.Size.Width)
        {
            _placing = true;
            try
            {
                AppWindow.Resize(new SizeInt32(width, AppWindow.Size.Height));
            }
            finally
            {
                _placing = false;
            }
        }
    }

    private void ApplyPanes()
    {
        (bool sidebar, bool inspector) = RecordingsPanes.Shown(_width, Browser.ShowSidebar, Browser.ShowInspector);
        _shown = (sidebar, inspector);
        _sidebarColumn.Width = new GridLength(sidebar ? RecordingsPanes.SidebarWidth : 0);
        _inspectorColumn.Width = new GridLength(inspector ? RecordingsPanes.InspectorWidth : 0);
        _sidebarHost.Visibility = sidebar ? Visibility.Visible : Visibility.Collapsed;
        _inspectorHost.Visibility = inspector ? Visibility.Visible : Visibility.Collapsed;
        ToolTipService.SetToolTip(_sidebarButton, sidebar ? "Hide the calendar and incidents" : "Show the calendar and incidents");
        ToolTipService.SetToolTip(_inspectorButton, inspector ? "Hide the incident's details" : "Show the incident's details");
    }

    // ---- refreshing ----

    private void ScheduleRefresh()
    {
        if (_refreshScheduled || _closed)
        {
            return;
        }
        _refreshScheduled = true;
        _ = _ui.TryEnqueue(() =>
        {
            _refreshScheduled = false;
            if (!_closed)
            {
                Refresh();
            }
        });
    }

    private void ScheduleTransport()
    {
        if (_transportScheduled || _closed)
        {
            return;
        }
        _transportScheduled = true;
        _ = _ui.TryEnqueue(() =>
        {
            _transportScheduled = false;
            if (!_closed)
            {
                RefreshPlayer();
                RefreshNotice();
            }
        });
    }

    private void OnDownloadsChanged()
    {
        if (!_closed)
        {
            RefreshCenter();
            RefreshDownloads();
        }
    }

    /// <summary>Draws everything from the browser's state (also called by the self-test).</summary>
    public void Refresh()
    {
        _dayTitle.Text = Browser.DayTitle;
        _nextDay.IsEnabled = !Browser.IsToday;
        RefreshCenter();
        RefreshSidebar();
        RefreshPlayer();
        RefreshLanes();
        RefreshInspector();
        RefreshDownloads();
        RefreshNotice();
        ApplyPanes();
    }

    private void RefreshNotice()
    {
        string? text = Service.Notice ?? Browser.Message ?? Browser.Player.Message;
        _noticeText.Text = text ?? "";
        _notice.Visibility = text is null ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>The message line's text, if shown (self-test).</summary>
    public string? NoticeShown => _notice.Visibility == Visibility.Visible ? _noticeText.Text : null;

    // ---- placement ----

    private void Place()
    {
        _placing = true;
        try
        {
            if (_background)
            {
                AppWindow.MoveAndResize(new RectInt32(80, 80, 1280, 800));
            }
            else
            {
                nint hwnd = WinRT.Interop.WindowNative.GetWindowHandle(Window);
                double scale = GetDpiForWindow((TerraFX.Interop.Windows.HWND)hwnd) / 96.0;
                AppWindow.Resize(new SizeInt32((int)(1280 * scale), (int)(800 * scale)));
            }
        }
        finally
        {
            _placing = false;
        }
    }

    /// <summary>The window's frame as it is now, in pixels.</summary>
    public WindowRect CurrentFrame => new(AppWindow.Position.X, AppWindow.Position.Y, AppWindow.Size.Width, AppWindow.Size.Height);

    /// <summary>Resizes the window in physical pixels (self-test).</summary>
    public void ResizeTo(int width, int height)
    {
        _placing = true;
        try
        {
            AppWindow.Resize(new SizeInt32(width, height));
        }
        finally
        {
            _placing = false;
        }
    }

    private void Reveal()
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
            Window.Activate();
        }
    }

    private void BringToFront()
    {
        if (_background)
        {
            Reveal();
            return;
        }
        if (AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized } presenter)
        {
            presenter.Restore();
        }
        Window.Activate();
    }

    private void OnAppWindowChanged(AppWindow sender, AppWindowChangedEventArgs args)
    {
        if (_closed || _placing || !args.DidSizeChange)
        {
            return;
        }
        // The least content size: smaller windows keep the player's room by hiding side panes instead.
        double scale = _root.XamlRoot?.RasterizationScale ?? 1;
        int minWidth = (int)(RecordingsPanes.WindowMinWidth * scale), minHeight = (int)(RecordingsPanes.WindowMinHeight * scale);
        if (AppWindow.Size.Width < minWidth || AppWindow.Size.Height < minHeight)
        {
            _placing = true;
            try
            {
                AppWindow.Resize(new SizeInt32(Math.Max(AppWindow.Size.Width, minWidth), Math.Max(AppWindow.Size.Height, minHeight)));
            }
            finally
            {
                _placing = false;
            }
        }
    }

    // ---- closing ----

    // The playback closes with the window (its plays stop on the device), the window's hold ends, and the render thread stops.
    private void OnClosed()
    {
        if (_closed)
        {
            return;
        }
        _closed = true;
        _clock.Stop();
        _progress.Stop();
        _loudness.Stop();
        Browser.Changed -= ScheduleRefresh;
        Browser.Player.Changed -= ScheduleTransport;
        Service.DownloadsChanged -= OnDownloadsChanged;
        AppWindow.Changed -= OnAppWindowChanged;
        ClosePlayerView();
        Browser.Close();
        if (ReferenceEquals(Current, this))
        {
            Current = null;
        }
    }
}
