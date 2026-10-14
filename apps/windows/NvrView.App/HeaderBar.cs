// The Windows header (docs/UX.md "Windows header"): the window's title bar,
// drawn by the app. Left: the app name and the current layout with a picker
// (switch layouts, new layout, save as layout). Middle: the app-wide quick
// actions that used to hide in the right-click menu (Find devices, Edit layout,
// Fill tiles, Full screen). Right: the "N recording" indicator (shown while recordings
// run; its menu stops one or all), Settings, and a "…" menu for the rest. The system's caption buttons
// stay (and so do snap layouts); the header leaves room for them and tells the
// system which of its controls take the pointer instead of dragging the window.
// What each action is and where it lives is NvrView.Tiles HeaderActions; the
// actions themselves are ViewerWindow.RunHeaderAction.
//
// A window has two headers: the docked one (the title bar of a normal window)
// and an overlay that slides over the stage in full screen.

using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using NvrView.Shell;
using NvrView.Tiles;

namespace NvrView.App;

internal sealed class HeaderBar
{
    private static readonly Windows.UI.Color Back = Windows.UI.Color.FromArgb(0xFF, 0x1C, 0x1D, 0x20);

    private readonly ViewerWindow _window;
    private readonly Viewer _viewer;
    private readonly ColumnDefinition _leftInset = new() { Width = new GridLength(0) };
    private readonly ColumnDefinition _rightInset = new() { Width = new GridLength(0) };
    private readonly TextBlock _layoutName = new() { VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis, MaxWidth = 220 };
    private readonly DropDownButton _picker = new();
    private readonly Dictionary<HeaderAction, ButtonBase> _buttons = [];
    private readonly Button _overflow;
    private readonly Button _recording = new()
    {
        VerticalAlignment = VerticalAlignment.Center,
        Visibility = Visibility.Collapsed,
        Margin = new Thickness(0, 0, 8, 0),
        Padding = new Thickness(8, 4, 8, 4),
        Background = new SolidColorBrush(Colors.Transparent),
        BorderThickness = new Thickness(0),
        Foreground = new SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0xFF, 0x5A, 0x5A)),
    };

    public HeaderBar(ViewerWindow window, Viewer viewer)
    {
        _window = window;
        _viewer = viewer;
        Root = new Grid { Background = new SolidColorBrush(Back), Height = TitleBarRegions.TallHeight, RequestedTheme = ElementTheme.Dark };
        Root.ColumnDefinitions.Add(_leftInset);
        Root.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Root.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Root.ColumnDefinitions.Add(_rightInset);

        // Left: app name, layout picker.
        var left = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10, Margin = new Thickness(12, 0, 8, 0), VerticalAlignment = VerticalAlignment.Center };
        left.Children.Add(new TextBlock { Text = "nvrview", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center });
        _picker.Content = _layoutName;
        _picker.Padding = new Thickness(10, 4, 8, 4);
        _picker.Flyout = LayoutMenu();
        AutomationProperties.SetName(_picker, "Layout");
        ToolTipService.SetToolTip(_picker, "Switch layout");
        left.Children.Add(_picker);
        Grid.SetColumn(left, 1);
        Root.Children.Add(left);

        // Middle: quick actions.
        var middle = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 2, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center };
        middle.Children.Add(Make(HeaderAction.FindDevices, "", toggle: false));
        middle.Children.Add(Make(HeaderAction.EditLayout, "", toggle: true));
        middle.Children.Add(Make(HeaderAction.FillTiles, "", toggle: true));
        middle.Children.Add(Make(HeaderAction.FullScreen, "", toggle: false));
        Grid.SetColumn(middle, 2);
        Root.Children.Add(middle);

        // Right: the recording indicator, Settings, the overflow menu.
        var right = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 2, Margin = new Thickness(8, 0, 8, 0), VerticalAlignment = VerticalAlignment.Center };
        AutomationProperties.SetName(_recording, "Recordings");
        ToolTipService.SetToolTip(_recording, "The recordings running now: stop one or all");
        var recordingMenu = new MenuFlyout();
        recordingMenu.Opening += (_, _) => FillRecordingMenu(recordingMenu);
        Track(recordingMenu);
        _recording.Flyout = recordingMenu;
        right.Children.Add(_recording);
        right.Children.Add(Make(HeaderAction.Settings, "", toggle: false));
        _overflow = new Button { Content = new FontIcon { Glyph = "", FontSize = 14 }, Width = 40, Height = 36, Padding = new Thickness(0), Background = new SolidColorBrush(Colors.Transparent), BorderThickness = new Thickness(0) };
        AutomationProperties.SetName(_overflow, "More");
        ToolTipService.SetToolTip(_overflow, "More");
        var menu = new MenuFlyout();
        menu.Opening += (_, _) => FillOverflow(menu);
        Track(menu);
        _overflow.Flyout = menu;
        right.Children.Add(_overflow);
        Grid.SetColumn(right, 3);
        Root.Children.Add(right);

        Update();
    }

    public Grid Root { get; }

    /// <summary>Flyouts of this header that are open (they hold the full screen reveal).</summary>
    public int OpenMenus { get; private set; }

    /// <summary>A menu of this header opened or closed.</summary>
    public event Action? MenuStateChanged;

    private ButtonBase Make(HeaderAction action, string glyph, bool toggle)
    {
        HeaderItem item = HeaderActions.Of(action);
        ButtonBase b = toggle ? new ToggleButton() : new Button();
        b.Content = new FontIcon { Glyph = glyph, FontSize = 14 };
        b.Width = 40;
        b.Height = 36;
        b.Padding = new Thickness(0);
        b.Background = new SolidColorBrush(Colors.Transparent);
        b.BorderThickness = new Thickness(0);
        b.Tag = action;
        b.Click += (_, _) => _window.RunHeaderAction(action);
        AutomationProperties.SetName(b, item.Title);
        ToolTipService.SetToolTip(b, Tip(item, on: false));
        _buttons[action] = b;
        return b;
    }

    private static string Tip(HeaderItem item, bool on) => item.Keys is { } k ? $"{item.TitleFor(on)} ({k})" : item.TitleFor(on);

    // ---- state ----

    /// <summary>Shows the window's current state: the layout's name, the toggles, the recording spot.</summary>
    public void Update()
    {
        _layoutName.Text = _window.ContentTitle;
        Set(HeaderAction.EditLayout, _window.State.Editing);
        Set(HeaderAction.FillTiles, _window.State.Fill);
        Set(HeaderAction.FullScreen, _window.Stage.IsFullScreen, toggleState: false);
        string? recording = RecordingIndicator.Text(_window.RecordingCount);
        _recording.Content = recording ?? "";
        _recording.Visibility = recording is null ? Visibility.Collapsed : Visibility.Visible;
    }

    private void Set(HeaderAction action, bool on, bool toggleState = true)
    {
        ButtonBase b = _buttons[action];
        HeaderItem item = HeaderActions.Of(action);
        AutomationProperties.SetName(b, item.TitleFor(on));
        ToolTipService.SetToolTip(b, Tip(item, on));
        if (toggleState && b is ToggleButton t && t.IsChecked != on)
        {
            t.IsChecked = on;
        }
    }

    /// <summary>Leaves room for the system's caption buttons (and an inset on the left), in units.</summary>
    public void SetInsets(double left, double right)
    {
        _leftInset.Width = new GridLength(Math.Max(left, 0));
        _rightInset.Width = new GridLength(Math.Max(right, 0));
    }

    public void SetHeight(double units) => Root.Height = Math.Max(units, 32);

    /// <summary>The controls that take the pointer instead of dragging the window, in units relative to the window's content.</summary>
    public IEnumerable<WindowRect> InteractiveBounds()
    {
        var elements = new List<FrameworkElement> { _picker, _overflow, _recording };
        elements.AddRange(_buttons.Values);
        foreach (FrameworkElement e in elements)
        {
            if (e.Visibility != Visibility.Visible || e.ActualWidth <= 0 || e.ActualHeight <= 0)
            {
                continue;
            }
            Windows.Foundation.Rect r = e.TransformToVisual(null).TransformBounds(new Windows.Foundation.Rect(0, 0, e.ActualWidth, e.ActualHeight));
            yield return new WindowRect(r.X, r.Y, r.Width, r.Height);
        }
    }

    // ---- menus ----

    private void Track(FlyoutBase flyout)
    {
        flyout.Opened += (_, _) =>
        {
            OpenMenus++;
            MenuStateChanged?.Invoke();
        };
        flyout.Closed += (_, _) =>
        {
            OpenMenus = Math.Max(OpenMenus - 1, 0);
            MenuStateChanged?.Invoke();
        };
    }

    private MenuFlyout LayoutMenu()
    {
        var menu = new MenuFlyout();
        menu.Opening += (_, _) => FillLayoutMenu(menu);
        Track(menu);
        return menu;
    }

    /// <summary>The layout picker's items for the window as it is now (also built by the self-test).</summary>
    public MenuFlyout BuildLayoutMenu()
    {
        var menu = new MenuFlyout();
        FillLayoutMenu(menu);
        return menu;
    }

    private void FillLayoutMenu(MenuFlyout menu)
    {
        menu.Items.Clear();
        WindowState state = _window.State;
        var all = new ToggleMenuFlyoutItem { Text = AllCameras.Name, IsChecked = state.Content is WindowContent.Scratch { Name: AllCameras.Name } };
        all.Click += (_, _) => _window.Show(ViewerWindow.AllCameras(_viewer));
        menu.Items.Add(all);
        foreach (SavedLayout layout in _viewer.Layouts)
        {
            SavedLayout l = layout;
            var item = new ToggleMenuFlyoutItem
            {
                Text = l.Name,
                IsChecked = state.Content is WindowContent.Layout { Id: var id } && id == l.Id,
                Icon = LayoutIcon.Create(l.Root),
            };
            item.Click += (_, _) => _window.Show(new WindowContent.Layout(l.Id));
            menu.Items.Add(item);
        }
        if (_viewer.Groups.Any())
        {
            var groups = new MenuFlyoutSubItem { Text = "Groups" };
            foreach (CameraGroup group in _viewer.Groups)
            {
                CameraGroup g = group;
                var item = new ToggleMenuFlyoutItem { Text = g.Name, IsChecked = state.Content is WindowContent.Group { Id: var gid } && gid == g.Id };
                item.Click += (_, _) => _window.Show(new WindowContent.Group(g.Id));
                groups.Items.Add(item);
            }
            menu.Items.Add(groups);
        }
        menu.Items.Add(new MenuFlyoutSeparator());
        AddAction(menu.Items, HeaderAction.NewLayout);
        var templates = new MenuFlyoutSubItem { Text = HeaderActions.Of(HeaderAction.NewLayoutFromTemplate).Title };
        _window.AddTemplateItems(templates.Items);
        menu.Items.Add(templates);
        if (state.Content is not WindowContent.Layout)
        {
            AddAction(menu.Items, HeaderAction.SaveAsLayout);
        }
    }

    /// <summary>The "…" menu's items for the window as it is now (also built by the self-test).</summary>
    public MenuFlyout BuildOverflow()
    {
        var menu = new MenuFlyout();
        FillOverflow(menu);
        return menu;
    }

    private void FillOverflow(MenuFlyout menu)
    {
        menu.Items.Clear();
        AddAction(menu.Items, HeaderAction.NewWindow);
        AddAction(menu.Items, HeaderAction.Recordings);
        AddAction(menu.Items, HeaderAction.StopAllRecordings).IsEnabled = _viewer.Capture.Board.Running.Count > 0;
        AddAction(menu.Items, HeaderAction.OpenRecordingsFolder);
        AddAction(menu.Items, HeaderAction.ToggleSidebar, on: _window.SidebarDocked);
        menu.Items.Add(_window.Stage.IndicatorCornerMenu());
        AddAction(menu.Items, HeaderAction.ShowFrameRates, on: _viewer.Settings.ShowFrameRates);
        AddAction(menu.Items, HeaderAction.Diagnostics, on: _window.Stage.Diagnostics);
        AddAction(menu.Items, HeaderAction.KeyboardShortcuts);
        menu.Items.Add(new MenuFlyoutSeparator());
        var update = new MenuFlyoutItem { Text = App.Instance.Updater?.MenuTitle ?? HeaderActions.Of(HeaderAction.CheckForUpdates).Title };
        update.Click += (_, _) => _window.RunHeaderAction(HeaderAction.CheckForUpdates);
        menu.Items.Add(update);
    }

    /// <summary>One catalog action as a menu item; <paramref name="on"/> picks the wording of a toggle that is on.</summary>
    private MenuFlyoutItem AddAction(IList<MenuFlyoutItemBase> into, HeaderAction action, bool on = false)
    {
        HeaderItem h = HeaderActions.Of(action);
        // The wording says what a click does: "Hide sidebar" while it is docked.
        var item = new MenuFlyoutItem { Text = h.TitleFor(on) };
        if (h.Keys is { } keys)
        {
            item.KeyboardAcceleratorTextOverride = keys;
        }
        item.Click += (_, _) => _window.RunHeaderAction(action);
        into.Add(item);
        return item;
    }

    /// <summary>The recording indicator's menu for the recordings now running (also built by the self-test): each with Stop, then Stop all and Open recordings folder.</summary>
    public MenuFlyout BuildRecordingMenu()
    {
        var menu = new MenuFlyout();
        FillRecordingMenu(menu);
        return menu;
    }

    private void FillRecordingMenu(MenuFlyout menu)
    {
        menu.Items.Clear();
        foreach (BoardEntry r in _viewer.Capture.Board.List)
        {
            ulong id = r.Id;
            string what = r.Camera is { } camera ? _viewer.CameraName(camera) : "recording";
            var stop = new MenuFlyoutItem { Text = $"Stop {(r.IsCall ? "call recording of " : "")}{what} ({RecordingText.Elapsed(r.DurationMs)})" };
            stop.Click += (_, _) => _viewer.Capture.Stop(id);
            menu.Items.Add(stop);
        }
        if (menu.Items.Count > 0)
        {
            menu.Items.Add(new MenuFlyoutSeparator());
        }
        AddAction(menu.Items, HeaderAction.StopAllRecordings).IsEnabled = _viewer.Capture.Board.Running.Count > 0;
        AddAction(menu.Items, HeaderAction.OpenRecordingsFolder);
    }

    // ---- the self-test ----

    /// <summary>The header's buttons as shown: (action, accessible name, enabled, visible).</summary>
    public List<(HeaderAction Action, string Name, bool Enabled, bool Visible)> Buttons() =>
        [.. _buttons.Select(kv => (kv.Key, AutomationProperties.GetName(kv.Value), kv.Value.IsEnabled, kv.Value.Visibility == Visibility.Visible))];

    public string LayoutLabel => _layoutName.Text;

    /// <summary>Whether the recording indicator shows (hidden until something records).</summary>
    public bool RecordingSpotVisible => _recording.Visibility == Visibility.Visible;

    /// <summary>The indicator's words while it shows (self-test).</summary>
    public string? RecordingSpotText => RecordingSpotVisible ? _recording.Content as string : null;

    public bool HasRecordingSpot => _recording.Parent is not null;

    /// <summary>Presses a button: the same path as a click.</summary>
    public void Press(HeaderAction action)
    {
        if (_buttons.TryGetValue(action, out ButtonBase? b) && b.IsEnabled)
        {
            _window.RunHeaderAction(action);
            Update();
        }
    }
}
