// A window's sidebar (docs/UX.md "Sidebar"): Layouts, Groups, and Devices
// with their cameras. Clicking a layout or group shows it in the window;
// clicking a camera puts it in the selected tile; cameras drag onto any tile
// or onto a group. A multi-lens camera (channels "lenses" >= 2) expands into
// Lens 1 and Lens 2 rows. Each camera row shows the corner indicators'
// symbols (IndicatorStrip) after its name, and each device row a
// summary of its cameras', so activity shows for cameras not on screen; they
// follow Viewer.IndicatorsChanged in place, without rebuilding the rows.
//
// Built in code from the viewer's state and rebuilt only when what it shows
// changed, never while a menu, a drag, or a name being typed is in progress
// (it catches up when that ends). The window shows one docked and, while the
// sidebar is collapsed, one in the edge-reveal overlay.

using System.Text;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.ApplicationModel.DataTransfer;

namespace NvrView.App;

/// <summary>What a row's detection symbols are for: a whole device (Channel null), a channel (Lens null: every lens), or one lens.</summary>
internal readonly record struct SidebarSymbolScope(Viewer Viewer, string DeviceKey, int? Channel, int? Lens)
{
    public IReadOnlyList<IndicatorItem> Items() => Channel is not { } ch
        ? Viewer.Board.Items(DeviceKey)
        : Lens is { } lens ? Viewer.Board.Items(new CameraRef(DeviceKey, ch, lens)) : Viewer.Board.Items(DeviceKey, ch);
}

internal sealed partial class SidebarView : UserControl
{
    public const double DefaultWidth = 260;

    private static readonly SolidColorBrush Secondary = new(Windows.UI.Color.FromArgb(0xFF, 0x9A, 0xA0, 0xA6));
    private static readonly SolidColorBrush Hover = new(Windows.UI.Color.FromArgb(0x30, 0x80, 0x80, 0x80));
    private static readonly SolidColorBrush SelectedBrush = new(Windows.UI.Color.FromArgb(0x50, 0x3A, 0x96, 0xDD));
    private static readonly SolidColorBrush Online = new(Windows.UI.Color.FromArgb(0xFF, 0x34, 0xC7, 0x59));
    private static readonly SolidColorBrush Offline = new(Windows.UI.Color.FromArgb(0xFF, 0xE0, 0x8A, 0x2E));
    private static readonly SolidColorBrush Clear = new(Colors.Transparent);

    private readonly Viewer _viewer;
    private readonly ViewerWindow _window;
    private readonly StackPanel _rows = new() { Spacing = 1, Padding = new Thickness(8, 4, 8, 10) };
    private readonly TextBlock _notice = new() { TextWrapping = TextWrapping.Wrap, FontSize = 12, Foreground = Secondary };
    private readonly Grid _noticeBox = new() { Padding = new Thickness(10, 6, 10, 6), Visibility = Visibility.Collapsed };
    private readonly CheckBox _hideOffline = new() { Content = "Hide offline channels", FontSize = 12, Margin = new Thickness(10, 0, 10, 6) };
    private readonly HashSet<string> _collapsedGroups = [];
    private readonly HashSet<CameraRef> _expandedLenses = [];
    private readonly List<(IndicatorStrip Strip, SidebarSymbolScope Scope)> _strips = [];
    private string _signature = "";
    private bool _dirty;
    private int _menus;

    public SidebarView(Viewer viewer, ViewerWindow window, bool overlay)
    {
        _viewer = viewer;
        _window = window;
        IsOverlay = overlay;
        var root = new Grid();
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        if (overlay)
        {
            var pin = new Button { Content = new FontIcon { Glyph = "", FontSize = 14 }, HorizontalAlignment = HorizontalAlignment.Right, Margin = new Thickness(6), Padding = new Thickness(8, 4, 8, 4) };
            ToolTipService.SetToolTip(pin, "Dock the sidebar (S)");
            pin.Click += (_, _) => PinRequested?.Invoke();
            root.Children.Add(pin);
        }
        var scroll = new ScrollViewer { Content = _rows, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled };
        Grid.SetRow(scroll, 1);
        root.Children.Add(scroll);
        var dismiss = new Button { Content = new FontIcon { Glyph = "", FontSize = 10 }, Padding = new Thickness(4), HorizontalAlignment = HorizontalAlignment.Right, VerticalAlignment = VerticalAlignment.Top };
        dismiss.Click += (_, _) =>
        {
            _viewer.Notice = null;
            Refresh();
        };
        _noticeBox.ColumnDefinitions.Add(new ColumnDefinition());
        _noticeBox.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _noticeBox.Children.Add(_notice);
        Grid.SetColumn(dismiss, 1);
        _noticeBox.Children.Add(dismiss);
        Grid.SetRow(_noticeBox, 2);
        root.Children.Add(_noticeBox);
        _hideOffline.IsChecked = viewer.HideOfflineChannels;
        _hideOffline.Click += (_, _) => _viewer.HideOfflineChannels = _hideOffline.IsChecked == true;
        ToolTipService.SetToolTip(_hideOffline, "An NVR lists every slot it has; most may be empty");
        Grid.SetRow(_hideOffline, 3);
        root.Children.Add(_hideOffline);
        Content = root;
        Background = new SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0x20, 0x22, 0x25));
        RequestedTheme = ElementTheme.Dark;
        _viewer.Changed += Refresh;
        _viewer.LayoutsChanged += Refresh;
        _viewer.GroupsChanged += Refresh;
        _viewer.IndicatorsChanged += UpdateSymbols;
        Unloaded += (_, _) => _menus = 0;
        Refresh();
    }

    /// <summary>The overlay's copy (it has a pin control).</summary>
    public bool IsOverlay { get; }

    /// <summary>The overlay's pin: dock the sidebar.</summary>
    public event Action? PinRequested;

    /// <summary>A camera is being dragged from here.</summary>
    public bool IsDragging { get; private set; }

    /// <summary>The rows end with the "Add a device in Settings…" link (no device configured).</summary>
    public bool ShowsAddDevicePrompt { get; private set; }

    /// <summary>A context menu of this sidebar is open.</summary>
    public bool IsMenuOpen => _menus > 0;

    /// <summary>A text box in here has the keyboard focus.</summary>
    public bool IsEditingText => XamlRoot is not null && FocusManager.GetFocusedElement(XamlRoot) is TextBox t && IsAncestorOf(t);

    private bool IsAncestorOf(DependencyObject d)
    {
        for (DependencyObject? p = d; p is not null; p = VisualTreeHelper.GetParent(p))
        {
            if (ReferenceEquals(p, this))
            {
                return true;
            }
        }
        return false;
    }

    /// <summary>Rebuilds the rows if what they show changed (deferred while a menu or drag is in progress).</summary>
    public void Refresh()
    {
        if (IsDragging || IsMenuOpen)
        {
            _dirty = true;
            return;
        }
        string signature = Signature();
        if (signature == _signature && !_dirty)
        {
            return;
        }
        _signature = signature;
        _dirty = false;
        Build();
    }

    private string Signature()
    {
        var sb = new StringBuilder();
        sb.Append(_window.State.Content).Append('|').Append(_viewer.HideOfflineChannels).Append('|').Append(_viewer.Notice).Append('\n');
        foreach (SavedLayout l in _viewer.Layouts)
        {
            sb.Append("L ").Append(l.Id).Append(' ').Append(l.Name).Append(' ').Append(l.Root.LeafCount).Append('\n');
        }
        foreach (CameraGroup g in _viewer.Groups)
        {
            sb.Append("G ").Append(g.Id).Append(' ').Append(g.Name).Append(' ').Append(_collapsedGroups.Contains(g.Id)).Append(string.Join(",", g.Cameras)).Append('\n');
        }
        foreach (DeviceEntry d in _viewer.Devices)
        {
            sb.Append("D ").Append(d.Key).Append(' ').Append(d.DisplayName).Append(' ').Append(d.State).Append('\n');
            foreach (ChannelInfo c in d.Channels.Values.OrderBy(c => c.Channel))
            {
                sb.Append(c.Channel).Append(c.Name).Append(c.Connected).Append(c.Lenses).Append(c.Detects?.Length ?? -1).Append(';');
            }
            sb.Append('\n');
        }
        sb.Append(string.Join(",", _expandedLenses)).Append('|');
        foreach (CameraRef c in _viewer.AllCameras)
        {
            if (_viewer.IsAudible(c))
            {
                sb.Append(c).Append(',');
            }
        }
        return sb.ToString();
    }

    // ---- rows ----

    private void Build()
    {
        _rows.Children.Clear();
        _strips.Clear();
        _hideOffline.IsChecked = _viewer.HideOfflineChannels;
        _notice.Text = _viewer.Notice ?? "";
        _noticeBox.Visibility = _viewer.Notice is null ? Visibility.Collapsed : Visibility.Visible;

        Header("Layouts", "New layout…", () => _window.NewLayout());
        foreach (SavedLayout layout in _viewer.Layouts)
        {
            SavedLayout l = layout;
            Grid row = Row(l.Name, $"{l.Root.LeafCount}", 0, _window.State.Content is WindowContent.Layout { Id: var id } && id == l.Id,
                () => _window.Show(new WindowContent.Layout(l.Id)));
            ContextMenu(row, m =>
            {
                Item(m, "Open in new window", () => App.Instance.OpenWindow(new WindowContent.Layout(l.Id)));
                Item(m, "Rename…", async () =>
                {
                    if (await _window.AskName("Rename layout", l.Name) is { } name)
                    {
                        _viewer.RenameLayout(l.Id, name);
                    }
                });
                Item(m, "Duplicate", () => _viewer.DuplicateLayout(l.Id));
                m.Items.Add(new MenuFlyoutSeparator());
                Item(m, "Delete", () => _viewer.DeleteLayout(l.Id));
            });
            _rows.Children.Add(row);
        }

        Header("Groups", "New group…", async () =>
        {
            if (await _window.AskName("New group", "Group") is { } name)
            {
                _viewer.CreateGroup(name, []);
            }
        });
        foreach (CameraGroup group in _viewer.Groups)
        {
            CameraGroup g = group;
            bool collapsed = _collapsedGroups.Contains(g.Id);
            Grid row = Row(g.Name, $"{g.Cameras.Count}", 0, _window.State.Content is WindowContent.Group { Id: var gid } && gid == g.Id,
                () => _window.Show(new WindowContent.Group(g.Id)), chevron: collapsed ? false : true, toggle: () =>
                {
                    if (!_collapsedGroups.Remove(g.Id))
                    {
                        _collapsedGroups.Add(g.Id);
                    }
                    Refresh();
                });
            ContextMenu(row, m =>
            {
                Item(m, "Open in new window", () => App.Instance.OpenWindow(new WindowContent.Group(g.Id)));
                Item(m, "Rename…", async () =>
                {
                    if (await _window.AskName("Rename group", g.Name) is { } name)
                    {
                        _viewer.RenameGroup(g.Id, name);
                    }
                });
                m.Items.Add(new MenuFlyoutSeparator());
                Item(m, "Delete", () => _viewer.DeleteGroup(g.Id));
            });
            // Cameras dragged from the sidebar join the group.
            row.AllowDrop = true;
            row.DragOver += (_, e) =>
            {
                e.AcceptedOperation = e.DataView.Contains(StandardDataFormats.Text) ? DataPackageOperation.Copy : DataPackageOperation.None;
                e.DragUIOverride.Caption = $"Add to {g.Name}";
            };
            row.Drop += async (_, e) =>
            {
                DragOperationDeferral deferral = e.GetDeferral();
                try
                {
                    if (CameraRefs.FromDragText(await e.DataView.GetTextAsync()) is { } camera)
                    {
                        _viewer.AddToGroup(g.Id, camera);
                    }
                }
                finally
                {
                    deferral.Complete();
                }
            };
            _rows.Children.Add(row);
            if (!collapsed)
            {
                foreach (CameraRef camera in g.Cameras)
                {
                    CameraRef c = camera;
                    _rows.Children.Add(CameraRow(c, _viewer.CameraName(c), 1, new SidebarSymbolScope(_viewer, c.Device, c.Channel, c.Lens),
                        extra: m => Item(m, "Remove from group", () => _viewer.RemoveFromGroup(g.Id, c))));
                }
            }
        }

        Header("Devices", null, null);
        foreach (DeviceEntry device in _viewer.Devices)
        {
            DeviceEntry d = device;
            List<ChannelInfo> listed = [.. _viewer.ListedChannels(d)];
            Grid row = Row(d.DisplayName, ChannelFilter.CountText(listed.Count, d.Channels.Count) ?? "", 0, false, null, bold: true,
                symbols: new SidebarSymbolScope(_viewer, d.Key, null, null), state: StateWord(d));
            ContextMenu(row, m =>
            {
                Item(m, "Reconnect", () => _viewer.Reconnect(d));
                Item(m, "New group from this device", () => _viewer.CreateGroup(d.DisplayName, listed.SelectMany(c => Viewer.CamerasOf(d, c))));
                Item(m, "Show all cameras in new window", () => App.Instance.OpenWindow(new WindowContent.Scratch(d.DisplayName,
                    LayoutNode.Balanced(listed.SelectMany(c => Viewer.CamerasOf(d, c)).Take(LayoutNode.MaxLeaves)))));
                Item(m, "Settings…", () => _window.OpenSettings());
            });
            _rows.Children.Add(row);
            foreach (ChannelInfo channel in listed)
            {
                var first = new CameraRef(d.Key, channel.Channel);
                if (channel.Lenses >= 2)
                {
                    bool expanded = _expandedLenses.Contains(first);
                    _rows.Children.Add(CameraRow(first, _viewer.ChannelName(first), 1, new SidebarSymbolScope(_viewer, d.Key, channel.Channel, null),
                        chevron: expanded, toggle: () =>
                        {
                            if (!_expandedLenses.Remove(first))
                            {
                                _expandedLenses.Add(first);
                            }
                            Refresh();
                        }));
                    if (expanded)
                    {
                        foreach (CameraRef lens in Viewer.CamerasOf(d, channel))
                        {
                            _rows.Children.Add(CameraRow(lens, Viewer.LensName(lens.Lens), 2, new SidebarSymbolScope(_viewer, d.Key, channel.Channel, lens.Lens)));
                        }
                    }
                }
                else
                {
                    _rows.Children.Add(CameraRow(first, _viewer.CameraName(first), 1, new SidebarSymbolScope(_viewer, d.Key, channel.Channel, 0)));
                }
            }
        }
        ShowsAddDevicePrompt = _viewer.Devices.Count == 0;
        if (ShowsAddDevicePrompt)
        {
            var add = new HyperlinkButton { Content = "Add a device in Settings…", Padding = new Thickness(8, 2, 8, 2) };
            add.Click += (_, _) => _window.OpenSettings();
            _rows.Children.Add(add);
        }
    }

    private static (string Text, SolidColorBrush Brush)? StateWord(DeviceEntry d) => d.State switch
    {
        "connected" => null,
        "connecting" => ("connecting", Secondary),
        "backoff" => ("reconnecting", Offline),
        "login_failed" => ("login failed", Offline),
        "no_password" => ("needs password", Offline),
        _ => (d.State, Offline),
    };

    private void Header(string title, string? addTip, Action? add)
    {
        var grid = new Grid { Margin = new Thickness(0, 10, 0, 2) };
        grid.ColumnDefinitions.Add(new ColumnDefinition());
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.Children.Add(new TextBlock
        {
            Text = title,
            FontSize = 11,
            FontWeight = FontWeights.SemiBold,
            Foreground = Secondary,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(8, 0, 0, 0),
        });
        if (add is not null)
        {
            var button = new Button
            {
                Content = new FontIcon { Glyph = "", FontSize = 11 },
                Padding = new Thickness(6, 3, 6, 3),
                Background = Clear,
                BorderThickness = new Thickness(0),
            };
            ToolTipService.SetToolTip(button, addTip);
            button.Click += (_, _) => add();
            Grid.SetColumn(button, 1);
            grid.Children.Add(button);
        }
        _rows.Children.Add(grid);
    }

    /// <summary>A clickable row: optional disclosure chevron, title, symbols, detail on the right.</summary>
    private Grid Row(string title, string detail, int depth, bool selected, Action? click, bool? chevron = null, Action? toggle = null,
        bool bold = false, SidebarSymbolScope? symbols = null, (string Text, SolidColorBrush Brush)? state = null, Brush? dot = null, bool listening = false)
    {
        var row = new Grid
        {
            Background = selected ? SelectedBrush : Clear,
            CornerRadius = new CornerRadius(5),
            Padding = new Thickness(6 + 14 * depth, 4, 8, 4),
            ColumnSpacing = 6,
        };
        for (int i = 0; i < 6; i++)
        {
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = i == 4 ? new GridLength(1, GridUnitType.Star) : GridLength.Auto });
        }
        if (chevron is { } open)
        {
            var b = new Button
            {
                Content = new FontIcon { Glyph = open ? "" : "", FontSize = 9 },
                Padding = new Thickness(2),
                Background = Clear,
                BorderThickness = new Thickness(0),
                VerticalAlignment = VerticalAlignment.Center,
            };
            ToolTipService.SetToolTip(b, open ? "Collapse" : "Expand");
            b.Click += (_, _) => toggle?.Invoke();
            row.Children.Add(b);
        }
        if (dot is not null)
        {
            var e = new Microsoft.UI.Xaml.Shapes.Ellipse { Width = 7, Height = 7, Fill = dot, VerticalAlignment = VerticalAlignment.Center };
            Grid.SetColumn(e, 1);
            row.Children.Add(e);
        }
        var text = new TextBlock
        {
            Text = title,
            TextTrimming = TextTrimming.CharacterEllipsis,
            FontWeight = bold || selected ? FontWeights.SemiBold : FontWeights.Normal,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetColumn(text, 2);
        row.Children.Add(text);
        var right = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, VerticalAlignment = VerticalAlignment.Center };
        if (symbols is { } scope)
        {
            right.Children.Add(Symbols(scope));
        }
        if (listening)
        {
            right.Children.Add(new FontIcon { Glyph = "", FontSize = 11, Foreground = Secondary });
        }
        Grid.SetColumn(right, 3);
        row.Children.Add(right);
        var info = new TextBlock { FontSize = 11, VerticalAlignment = VerticalAlignment.Center, Foreground = state?.Brush ?? Secondary, Text = state?.Text ?? detail };
        Grid.SetColumn(info, 5);
        row.Children.Add(info);
        row.PointerEntered += (_, _) =>
        {
            if (!selected)
            {
                row.Background = Hover;
            }
        };
        row.PointerExited += (_, _) => row.Background = selected ? SelectedBrush : Clear;
        if (click is not null)
        {
            row.Tapped += (_, e) =>
            {
                if (e.OriginalSource is not FontIcon)
                {
                    click();
                }
            };
        }
        return row;
    }

    private Grid CameraRow(CameraRef camera, string title, int depth, SidebarSymbolScope symbols, bool? chevron = null, Action? toggle = null,
        Action<MenuFlyout>? extra = null)
    {
        bool offline = _viewer.ChannelOf(camera) is { Connected: false };
        Grid row = Row(title, "", depth, false, () => _window.CameraPicked(camera), chevron, toggle, symbols: symbols,
            dot: offline ? Offline : Online, listening: _viewer.IsAudible(camera));
        if (_viewer.ChannelOf(camera)?.Detects is { Length: > 0 })
        {
            ToolTipService.SetToolTip(row, $"{title}: detection by the camera's AI");
        }
        // Drag onto a tile (shows it there) or onto a group (adds it).
        row.CanDrag = true;
        row.DragStarting += (_, e) =>
        {
            e.Data.SetText(camera.DragText());
            e.Data.RequestedOperation = DataPackageOperation.Copy;
            IsDragging = true;
        };
        row.DropCompleted += (_, _) =>
        {
            IsDragging = false;
            if (_dirty)
            {
                Refresh();
            }
        };
        ContextMenu(row, m =>
        {
            Item(m, _window.Stage.State.Selected is not null ? "Show in selected tile" : "Show", () => _window.CameraPicked(camera));
            Item(m, "Open in new window", () => App.Instance.OpenWindow(new WindowContent.Scratch(_viewer.CameraName(camera), LayoutNode.Leaf(camera))));
            Item(m, _viewer.IsAudible(camera) ? "Stop listening" : "Listen", () => _viewer.ToggleListening(camera, false));
            var add = new MenuFlyoutSubItem { Text = "Add to group" };
            foreach (CameraGroup g in _viewer.Groups)
            {
                string id = g.Id;
                Item(add.Items, g.Name, () => _viewer.AddToGroup(id, camera), !g.Cameras.Contains(camera));
            }
            if (add.Items.Count > 0)
            {
                add.Items.Add(new MenuFlyoutSeparator());
            }
            Item(add.Items, "New group…", async () =>
            {
                if (await _window.AskName("New group", _viewer.CameraName(camera)) is { } name)
                {
                    _viewer.CreateGroup(name, [camera]);
                }
            });
            m.Items.Add(add);
            extra?.Invoke(m);
        });
        return row;
    }

    private IndicatorStrip Symbols(SidebarSymbolScope scope)
    {
        var strip = new IndicatorStrip(12) { VerticalAlignment = VerticalAlignment.Center };
        strip.SetItems(scope.Items());
        _strips.Add((strip, scope));
        return strip;
    }

    /// <summary>The symbols follow the detections in place (events and the slow tick).</summary>
    private void UpdateSymbols()
    {
        foreach ((IndicatorStrip strip, SidebarSymbolScope scope) in _strips)
        {
            strip.SetItems(scope.Items());
        }
    }

    // ---- menus ----

    private void ContextMenu(UIElement target, Action<MenuFlyout> fill)
    {
        target.RightTapped += (_, e) =>
        {
            var menu = new MenuFlyout();
            fill(menu);
            menu.Opened += (_, _) => _menus++;
            menu.Closed += (_, _) =>
            {
                _menus = Math.Max(_menus - 1, 0);
                MenuClosed?.Invoke();
                if (_dirty)
                {
                    Refresh();
                }
            };
            menu.ShowAt(target, e.GetPosition(target));
            e.Handled = true;
        };
    }

    /// <summary>A context menu of this sidebar closed (the reveal looks at the pointer again).</summary>
    public event Action? MenuClosed;

    private static void Item(MenuFlyout menu, string text, Action action, bool enabled = true) => Item(menu.Items, text, action, enabled);

    private static void Item(IList<MenuFlyoutItemBase> items, string text, Action action, bool enabled = true)
    {
        var item = new MenuFlyoutItem { Text = text, IsEnabled = enabled };
        item.Click += (_, _) => action();
        items.Add(item);
    }

    /// <summary>The window is closing.</summary>
    public void Detach()
    {
        _viewer.Changed -= Refresh;
        _viewer.LayoutsChanged -= Refresh;
        _viewer.GroupsChanged -= Refresh;
        _viewer.IndicatorsChanged -= UpdateSymbols;
    }
}
