// The Recordings window's left pane (docs/UX.md "Recordings"): the month's
// calendar with a dot on days that have recordings, the search state, the
// incident filters (kinds, Locked, camera scope, Merge), the incident list
// with fixed-width separated thumbnails and "+N", and the cameras to search
// with Camera Groups.

using Microsoft.UI;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using NvrView.Interop;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.System;
using Windows.UI.Core;
using Rect = NvrView.Shell.Rect;

namespace NvrView.App;

internal sealed partial class RecordingsWindow
{
    private static readonly Windows.UI.Color Accent = Windows.UI.Color.FromArgb(0xFF, 0x4C, 0x9B, 0xE8);

    private readonly StackPanel _calendarPanel = new() { Spacing = 6 };
    private readonly TextBlock _indexState = new() { FontSize = 12, Foreground = new SolidColorBrush(Muted), VerticalAlignment = VerticalAlignment.Center };
    private readonly Button _refreshButton = new();
    private readonly Dictionary<KindFilter, ToggleButton> _kindButtons = [];
    private readonly ToggleButton _lockedButton = new() { Content = "Locked" };
    private readonly ComboBox _scopeBox = new() { HorizontalAlignment = HorizontalAlignment.Stretch, MinWidth = 120 };
    private readonly Button _mergeButton = new() { Content = "Merge", Visibility = Visibility.Collapsed };
    private readonly StackPanel _incidentList = new();
    private readonly StackPanel _camerasPanel = new() { Spacing = 4 };
    private readonly Expander _camerasExpander = new() { HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch, IsExpanded = true };
    private bool _refreshingSidebar;

    private void BuildSidebar()
    {
        var column = new Grid();
        column.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        column.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        column.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        column.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        var top = new StackPanel { Spacing = 8, Margin = new Thickness(10) };
        top.Children.Add(_calendarPanel);
        var state = new Grid();
        state.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        state.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _refreshButton.Content = new FontIcon { Glyph = "", FontSize = 12 };
        _refreshButton.Padding = new Thickness(6, 3, 6, 3);
        _refreshButton.Background = new SolidColorBrush(Colors.Transparent);
        _refreshButton.BorderThickness = new Thickness(0);
        _refreshButton.Click += (_, _) => Browser.Refresh();
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_refreshButton, "Search this day again");
        ToolTipService.SetToolTip(_refreshButton, "Search this day on the device again");
        Grid.SetColumn(_refreshButton, 1);
        state.Children.Add(_indexState);
        state.Children.Add(_refreshButton);
        top.Children.Add(state);
        Grid.SetRow(top, 0);
        column.Children.Add(top);

        var filters = new StackPanel { Spacing = 6, Margin = new Thickness(10, 8, 10, 8) };
        var kinds = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 2 };
        foreach (KindFilter f in KindFilters.All)
        {
            var b = new ToggleButton { Content = f.Label(), Padding = new Thickness(6, 2, 6, 2), FontSize = 12 };
            KindFilter filter = f;
            b.Click += (_, _) => Browser.SetKindFilter(filter, b.IsChecked == true);
            ToolTipService.SetToolTip(b, $"Only {f.Label()}");
            _kindButtons[f] = b;
            kinds.Children.Add(b);
        }
        filters.Children.Add(kinds);
        var second = new Grid { ColumnSpacing = 6 };
        second.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        second.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        second.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _lockedButton.Padding = new Thickness(6, 2, 6, 2);
        _lockedButton.FontSize = 12;
        _lockedButton.Click += (_, _) => Browser.LockedOnly = _lockedButton.IsChecked == true;
        ToolTipService.SetToolTip(_lockedButton, "Only locked incidents");
        Grid.SetColumn(_scopeBox, 1);
        _scopeBox.SelectionChanged += (_, _) =>
        {
            if (!_refreshingSidebar && _scopeBox.SelectedItem is ComboBoxItem { Tag: IncidentScope scope })
            {
                Browser.Scope = scope;
            }
        };
        _mergeButton.Padding = new Thickness(8, 2, 8, 2);
        _mergeButton.Click += (_, _) => Browser.Merge();
        ToolTipService.SetToolTip(_mergeButton, "Join the selected incidents into one");
        Grid.SetColumn(_mergeButton, 2);
        second.Children.Add(_lockedButton);
        second.Children.Add(_scopeBox);
        second.Children.Add(_mergeButton);
        filters.Children.Add(second);
        var filterBox = new Border { Child = filters, BorderThickness = new Thickness(0, 1, 0, 1), BorderBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(0x40, 0x80, 0x80, 0x80)) };
        Grid.SetRow(filterBox, 1);
        column.Children.Add(filterBox);

        var scroll = new ScrollViewer { Content = _incidentList, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled };
        Grid.SetRow(scroll, 2);
        column.Children.Add(scroll);

        var cameras = new StackPanel { Spacing = 6 };
        var camerasScroll = new ScrollViewer { Content = _camerasPanel, MaxHeight = 180, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        cameras.Children.Add(camerasScroll);
        var groups = new Button { Content = "Camera Groups…", Padding = new Thickness(8, 3, 8, 3) };
        groups.Click += (_, _) => _ = EditGroupsAsync();
        ToolTipService.SetToolTip(groups, "Cameras that cover the same area; their events join one incident");
        cameras.Children.Add(groups);
        _camerasExpander.Header = new TextBlock { Text = "Cameras to Search", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, FontSize = 13 };
        _camerasExpander.Content = cameras;
        _camerasExpander.Margin = new Thickness(6, 0, 6, 6);
        Grid.SetRow(_camerasExpander, 3);
        column.Children.Add(_camerasExpander);
        _sidebarHost.Child = column;
    }

    private void RefreshSidebar()
    {
        _refreshingSidebar = true;
        try
        {
            RefreshCalendar();
            (bool busy, int pending, bool waiting) = Service.Searching;
            _indexState.Text = busy ? (pending > 0 ? $"Searching, {pending} left" : "Searching")
                : waiting ? "Waiting for the device"
                : Service.EnabledCameras.Count == 0 ? "No cameras chosen" : "Up to date";
            _refreshButton.IsEnabled = Browser.Cameras.Count > 0;
            foreach ((KindFilter f, ToggleButton b) in _kindButtons)
            {
                b.IsChecked = Browser.ActiveKinds.Contains(f);
            }
            _lockedButton.IsChecked = Browser.LockedOnly;
            _mergeButton.Visibility = Browser.SelectedKeys.Count >= 2 ? Visibility.Visible : Visibility.Collapsed;
            RefreshScopes();
            RefreshIncidentList();
            RefreshCameras();
        }
        finally
        {
            _refreshingSidebar = false;
        }
    }

    // ---- calendar ----

    private void RefreshCalendar()
    {
        _calendarPanel.Children.Clear();
        long today = Service.Today;
        long month = Browser.Month;
        var title = new Grid();
        title.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        title.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        title.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Button previous = SmallButton("", "Previous month");
        previous.Click += (_, _) => Browser.ShowMonth(-1);
        Button next = SmallButton("", "Next month");
        next.IsEnabled = DeviceTime.Month(month).Next <= today;
        next.Click += (_, _) => Browser.ShowMonth(1);
        var label = new TextBlock { Text = DeviceTime.MonthTitle(month), HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, FontSize = 13 };
        Grid.SetColumn(label, 1);
        Grid.SetColumn(next, 2);
        title.Children.Add(previous);
        title.Children.Add(label);
        title.Children.Add(next);
        _calendarPanel.Children.Add(title);

        var grid = new Grid { ColumnSpacing = 2, RowSpacing = 2 };
        for (int c = 0; c < 7; c++)
        {
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        }
        int first = (int)System.Globalization.CultureInfo.CurrentCulture.DateTimeFormat.FirstDayOfWeek + 1;
        string[] names = ["S", "M", "T", "W", "T", "F", "S"];
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        for (int c = 0; c < 7; c++)
        {
            var d = new TextBlock { Text = names[(first - 1 + c) % 7], FontSize = 11, HorizontalAlignment = HorizontalAlignment.Center, Foreground = new SolidColorBrush(Muted) };
            Grid.SetColumn(d, c);
            grid.Children.Add(d);
        }
        List<long?[]> weeks = DeviceTime.Weeks(month, first);
        for (int r = 0; r < weeks.Count; r++)
        {
            grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
            for (int c = 0; c < 7; c++)
            {
                if (weeks[r][c] is not { } day)
                {
                    continue;
                }
                bool selected = day == Browser.Day, recorded = Browser.RecordedDays.Contains(day), future = day > today;
                var content = new StackPanel { Spacing = 1, HorizontalAlignment = HorizontalAlignment.Center };
                content.Children.Add(new TextBlock
                {
                    Text = DeviceTime.DayOfMonth(day).ToString(System.Globalization.CultureInfo.InvariantCulture),
                    FontSize = 12,
                    HorizontalAlignment = HorizontalAlignment.Center,
                    FontWeight = recorded ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Normal,
                    Foreground = new SolidColorBrush(future ? Muted : Colors.White),
                });
                content.Children.Add(new Microsoft.UI.Xaml.Shapes.Ellipse
                {
                    Width = 4,
                    Height = 4,
                    Fill = new SolidColorBrush(recorded ? (selected ? Colors.White : Accent) : Colors.Transparent),
                });
                var b = new Button
                {
                    Content = content,
                    Padding = new Thickness(0, 2, 0, 2),
                    HorizontalAlignment = HorizontalAlignment.Stretch,
                    HorizontalContentAlignment = HorizontalAlignment.Center,
                    MinWidth = 0,
                    IsEnabled = !future,
                    Background = new SolidColorBrush(selected ? Accent : Colors.Transparent),
                    BorderBrush = new SolidColorBrush(day == today && !selected ? Accent : Colors.Transparent),
                    BorderThickness = new Thickness(1),
                    Tag = day,
                };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(b, DeviceTime.LongDay(day) + (recorded ? ", has recordings" : ""));
                b.Click += (_, _) => Browser.Show(day);
                Grid.SetRow(b, r + 1);
                Grid.SetColumn(b, c);
                grid.Children.Add(b);
            }
        }
        _calendarPanel.Children.Add(grid);
    }

    private static Button SmallButton(string glyph, string tip)
    {
        var b = new Button
        {
            Content = new FontIcon { Glyph = glyph, FontSize = 11 },
            Padding = new Thickness(6, 3, 6, 3),
            Background = new SolidColorBrush(Colors.Transparent),
            BorderThickness = new Thickness(0),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(b, tip);
        ToolTipService.SetToolTip(b, tip);
        return b;
    }

    /// <summary>The calendar's day buttons (self-test): day, whether it is marked as having recordings, whether it can be chosen.</summary>
    public List<(long Day, bool Recorded, bool Enabled)> CalendarDays()
    {
        var list = new List<(long, bool, bool)>();
        foreach (UIElement child in ((Grid)_calendarPanel.Children[^1]).Children)
        {
            if (child is Button { Tag: long day } b)
            {
                list.Add((day, Browser.RecordedDays.Contains(day), b.IsEnabled));
            }
        }
        return list;
    }

    // ---- scope ----

    private void RefreshScopes()
    {
        List<(IncidentScope Scope, string Title)> scopes = Browser.Scopes;
        _scopeBox.Items.Clear();
        foreach ((IncidentScope scope, string title) in scopes)
        {
            var item = new ComboBoxItem { Content = title, Tag = scope };
            _scopeBox.Items.Add(item);
            if (Equals(scope, Browser.Scope))
            {
                _scopeBox.SelectedItem = item;
            }
        }
    }

    // ---- incidents ----

    private static Windows.UI.Color KindColor(string kind) => KindFilters.Indicator(kind) switch
    {
        IndicatorKind.Person => Windows.UI.Color.FromArgb(0xFF, 0x3B, 0x82, 0xF6),
        IndicatorKind.Vehicle => Windows.UI.Color.FromArgb(0xFF, 0xA8, 0x55, 0xF7),
        IndicatorKind.Animal => Windows.UI.Color.FromArgb(0xFF, 0x22, 0xC5, 0x5E),
        IndicatorKind.Motion => Windows.UI.Color.FromArgb(0xFF, 0x9C, 0xA3, 0xAF),
        IndicatorKind.SmokeAlarm or IndicatorKind.CoAlarm => Windows.UI.Color.FromArgb(0xFF, 0xF5, 0x9E, 0x0B),
        IndicatorKind.Visitor => Windows.UI.Color.FromArgb(0xFF, 0x14, 0xB8, 0xA6),
        _ => Windows.UI.Color.FromArgb(0xFF, 0xA1, 0x62, 0x07),
    };

    private void RefreshIncidentList()
    {
        _incidentList.Children.Clear();
        if (Browser.Incidents.Count == 0)
        {
            _incidentList.Children.Add(new TextBlock
            {
                Text = Browser.Cameras.Count == 0 ? "Choose cameras to search below." : "No incidents this day.",
                Margin = new Thickness(12),
                Foreground = new SolidColorBrush(Muted),
                TextWrapping = TextWrapping.Wrap,
            });
            return;
        }
        foreach (Incident incident in Browser.Incidents)
        {
            _incidentList.Children.Add(IncidentRow(incident));
        }
    }

    private FrameworkElement IncidentRow(Incident incident)
    {
        bool selected = Browser.Selection.Contains(incident.Key);
        var grid = new Grid { Padding = new Thickness(10, 6, 10, 6), ColumnSpacing = 8, Tag = incident.Key };
        grid.Background = new SolidColorBrush(selected ? Windows.UI.Color.FromArgb(0x55, 0x4C, 0x9B, 0xE8) : Colors.Transparent);
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(IncidentRowStrip.Width + 28) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        // A strip of covers of a fixed width whatever the number of cameras, so every row's text starts at the same place.
        (int shown, int more) = IncidentRowStrip.Slots(incident.Cameras.Count);
        var strip = new StackPanel { Orientation = Orientation.Horizontal, Spacing = IncidentRowStrip.Spacing, Width = IncidentRowStrip.Width, VerticalAlignment = VerticalAlignment.Top };
        IReadOnlyList<ThumbnailKey> keys = incident.CoverKeys;
        for (int i = 0; i < shown; i++)
        {
            strip.Children.Add(Thumbnail(keys[i], IncidentRowStrip.ThumbnailWidth, IncidentRowStrip.ThumbnailHeight));
        }
        var stripHost = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 2 };
        stripHost.Children.Add(strip);
        if (more > 0)
        {
            stripHost.Children.Add(new TextBlock { Text = $"+{more}", FontSize = 11, VerticalAlignment = VerticalAlignment.Center, Foreground = new SolidColorBrush(Muted) });
        }
        grid.Children.Add(stripHost);

        var text = new StackPanel { Spacing = 1 };
        text.Children.Add(new TextBlock { Text = incident.Title, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, FontSize = 13, TextTrimming = TextTrimming.CharacterEllipsis });
        text.Children.Add(new TextBlock { Text = DeviceTime.Span(incident.Start, incident.End), FontSize = 11, Foreground = new SolidColorBrush(Muted) });
        var kinds = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
        foreach (string kind in incident.Kinds.Select(k => KindFilters.Indicator(k)).Distinct().Select(k => incident.Kinds.First(x => KindFilters.Indicator(x) == k)))
        {
            kinds.Children.Add(new TextBlock { Text = KindFilters.Indicator(kind).Label(), FontSize = 11, Foreground = new SolidColorBrush(KindColor(kind)) });
        }
        if (incident.Locked)
        {
            kinds.Children.Add(new FontIcon { Glyph = "", FontSize = 10, Foreground = new SolidColorBrush(Muted) });
        }
        if (incident.Downloads.Count > 0)
        {
            kinds.Children.Add(new FontIcon { Glyph = "", FontSize = 10, Foreground = new SolidColorBrush(Muted) });
        }
        text.Children.Add(kinds);
        text.Children.Add(new TextBlock
        {
            Text = incident.Group ?? string.Join(", ", incident.Cameras.Select(c => Service.CameraName(c.Camera))),
            FontSize = 11,
            Foreground = new SolidColorBrush(Muted),
            TextTrimming = TextTrimming.CharacterEllipsis,
        });
        Grid.SetColumn(text, 1);
        grid.Children.Add(text);

        string key = incident.Key;
        grid.Tapped += (_, e) =>
        {
            bool extend = InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control).HasFlag(CoreVirtualKeyStates.Down);
            Browser.Click(key, extend);
            e.Handled = true;
        };
        grid.ContextFlyout = IncidentMenu(incident);
        return grid;
    }

    private MenuFlyout IncidentMenu(Incident incident)
    {
        var menu = new MenuFlyout();
        var play = new MenuFlyoutItem { Text = "Play" };
        play.Click += (_, _) => Browser.Click(incident.Key, extend: false);
        menu.Items.Add(play);
        if (Browser.SplitSecond(incident) is not null)
        {
            var split = new MenuFlyoutItem { Text = "Split Here" };
            split.Click += (_, _) => Browser.SplitHere(incident);
            menu.Items.Add(split);
        }
        if (incident.Cameras.Count > 1)
        {
            var remove = new MenuFlyoutSubItem { Text = "Remove from Incident" };
            foreach (IncidentCamera c in incident.Cameras)
            {
                var item = new MenuFlyoutItem { Text = Service.CameraName(c.Camera) };
                RecordingCamera camera = c.Camera;
                item.Click += (_, _) => Browser.Remove(camera, incident.Key);
                remove.Items.Add(item);
            }
            menu.Items.Add(remove);
        }
        menu.Items.Add(new MenuFlyoutSeparator());
        var lockItem = new MenuFlyoutItem { Text = incident.Locked ? "Unlock" : "Lock" };
        lockItem.Click += (_, _) => Browser.SetLocked(incident.Key, !incident.Locked);
        menu.Items.Add(lockItem);
        var download = new MenuFlyoutItem { Text = "Download" };
        download.Click += (_, _) => Browser.Download(incident.Key);
        menu.Items.Add(download);
        if (incident.Edited)
        {
            var revert = new MenuFlyoutItem { Text = "Revert Edits" };
            revert.Click += (_, _) => Browser.Revert(incident.Key);
            menu.Items.Add(revert);
        }
        return menu;
    }

    /// <summary>The incident rows as shown (self-test): key and title.</summary>
    public List<(string Key, string Title)> IncidentRows() =>
        [.. _incidentList.Children.OfType<Grid>().Where(g => g.Tag is string).Select(g => ((string)g.Tag, Browser.Incidents.First(i => i.Key == (string)g.Tag).Title))];

    /// <summary>Clicks an incident row the way a user does (self-test).</summary>
    public void ClickIncident(string key, bool extend = false) => Browser.Click(key, extend);

    // ---- thumbnails ----

    private FrameworkElement Thumbnail(ThumbnailKey key, double width, double height)
    {
        Service.RequestThumbnail(key);
        ThumbnailState? state = Service.Thumbnails[key];
        var box = new Grid { Width = width, Height = height, Background = new SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0x10, 0x10, 0x12)), CornerRadius = new CornerRadius(3) };
        switch (state)
        {
            case ThumbnailState.Image image:
                var picture = new Microsoft.UI.Xaml.Controls.Image { Stretch = Stretch.UniformToFill, Width = width, Height = height };
                box.Children.Add(picture);
                _ = LoadImageAsync(picture, image.Data);
                break;
            case ThumbnailState.Empty:
                box.Children.Add(new FontIcon { Glyph = "", FontSize = 14, Foreground = new SolidColorBrush(Muted), Opacity = 0.5 });
                ToolTipService.SetToolTip(box, "No recording in this range");
                break;
            case ThumbnailState.Failed:
                box.Children.Add(new FontIcon { Glyph = "", FontSize = 14, Foreground = new SolidColorBrush(Muted), Opacity = 0.5 });
                break;
            default:
                box.Children.Add(new ProgressRing { Width = 16, Height = 16, IsActive = true });
                break;
        }
        return box;
    }

    private static async Task LoadImageAsync(Microsoft.UI.Xaml.Controls.Image target, byte[] data)
    {
        try
        {
            using var stream = new Windows.Storage.Streams.InMemoryRandomAccessStream();
            await stream.WriteAsync(System.Runtime.InteropServices.WindowsRuntime.WindowsRuntimeBufferExtensions.AsBuffer(data));
            stream.Seek(0);
            var bitmap = new BitmapImage();
            await bitmap.SetSourceAsync(stream);
            target.Source = bitmap;
        }
        catch (Exception e) when (e is InvalidOperationException or System.Runtime.InteropServices.COMException)
        {
            AppLog.Limited("thumbnail-image", 3, $"a thumbnail could not be shown: {e.Message}");
        }
    }

    // ---- cameras to search ----

    private void RefreshCameras()
    {
        _camerasPanel.Children.Clear();
        ((TextBlock)_camerasExpander.Header).Text = $"Cameras to Search ({Service.EnabledCameras.Count})";
        if (_viewer.Devices.Count == 0)
        {
            _camerasPanel.Children.Add(new TextBlock { Text = "Add a device in Settings first.", Foreground = new SolidColorBrush(Muted) });
            return;
        }
        foreach (DeviceEntry device in _viewer.Devices)
        {
            List<RecordingCamera> cameras = [.. device.Channels.Values.OrderBy(c => c.Channel)
                .SelectMany(c => Viewer.CamerasOf(device, c)).Select(r => new RecordingCamera(device.Key, r.Channel, r.Lens))];
            int on = cameras.Count(Service.IsEnabled);
            var all = new CheckBox { Content = device.DisplayName, IsChecked = cameras.Count > 0 && on == cameras.Count, IsEnabled = cameras.Count > 0, MinWidth = 0 };
            all.Click += (_, _) => Service.SetEnabled(device.Key, all.IsChecked == true ? cameras : []);
            _camerasPanel.Children.Add(all);
            foreach (RecordingCamera c in cameras)
            {
                var box = new CheckBox
                {
                    Content = Service.CameraName(c),
                    IsChecked = Service.IsEnabled(c),
                    Margin = new Thickness(16, 0, 0, 0),
                    MinWidth = 0,
                };
                RecordingCamera camera = c;
                box.Click += (_, _) => Service.SetEnabled(camera, box.IsChecked == true);
                _camerasPanel.Children.Add(box);
            }
        }
    }

    // ---- camera groups ----

    private async Task EditGroupsAsync()
    {
        var editor = new CameraGroupsEditor(Service, Browser.Cameras);
        var dialog = new ContentDialog
        {
            Title = "Camera Groups",
            Content = editor.Root,
            PrimaryButtonText = "Save",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = _root.XamlRoot,
            RequestedTheme = ElementTheme.Dark,
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary)
        {
            Service.PutGroups(editor.Result());
        }
    }
}

/// <summary>
/// Edits the camera groups: cameras that cover the same area, so their events join one incident
/// (docs/UX.md "Recordings"). Changing a group changes its incidents.
/// </summary>
internal sealed class CameraGroupsEditor
{
    private readonly RecordingsService _service;
    private readonly List<RecordingCamera> _cameras;
    private readonly List<(TextBox Name, Dictionary<RecordingCamera, CheckBox> Boxes)> _groups = [];
    private readonly StackPanel _list = new() { Spacing = 10 };

    public CameraGroupsEditor(RecordingsService service, List<RecordingCamera> cameras)
    {
        _service = service;
        _cameras = cameras;
        Root = new StackPanel { Spacing = 8, MinWidth = 360 };
        Root.Children.Add(new TextBlock
        {
            Text = "Cameras in a group cover the same area, so their events join one incident. Changing a group changes its incidents.",
            TextWrapping = TextWrapping.Wrap,
            FontSize = 12,
        });
        foreach (IncidentGroups.Group g in service.Groups.Groups)
        {
            AddGroup(g.Name, g.Cameras);
        }
        Root.Children.Add(new ScrollViewer { Content = _list, MaxHeight = 320 });
        var add = new Button { Content = "Add Group" };
        add.Click += (_, _) => AddGroup($"Group {_groups.Count + 1}", []);
        Root.Children.Add(add);
    }

    public StackPanel Root { get; }

    private void AddGroup(string name, IReadOnlyList<RecordingCamera> members)
    {
        var panel = new StackPanel { Spacing = 2 };
        var nameBox = new TextBox { Text = name, PlaceholderText = "Group name" };
        panel.Children.Add(nameBox);
        var boxes = new Dictionary<RecordingCamera, CheckBox>();
        foreach (RecordingCamera c in _cameras)
        {
            var box = new CheckBox { Content = _service.CameraName(c), IsChecked = members.Contains(c), Margin = new Thickness(12, 0, 0, 0), MinWidth = 0 };
            boxes[c] = box;
            panel.Children.Add(box);
        }
        _groups.Add((nameBox, boxes));
        _list.Children.Add(panel);
    }

    /// <summary>The groups as edited (names and cameras; empty ones are dropped by the put).</summary>
    public IncidentGroups Result() => new(
        [.. _groups.Select(g => new IncidentGroups.Group(g.Name.Text, [.. g.Boxes.Where(b => b.Value.IsChecked == true).Select(b => b.Key)]))],
        _service.Groups.CameraNames);
}
