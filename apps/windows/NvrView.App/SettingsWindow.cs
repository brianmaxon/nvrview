// The Settings window (docs/UX.md "Settings window"): a resizable window of its
// own, not a dialog, so a tall section scrolls. A pane on the left lists the
// sections (Devices, Cameras, Alerts & calls, Capture, Updates, About); each section's
// content sits in a ScrollViewer of its own (SettingsPages builds the content).
// There is one instance: opening Settings again brings it to the front, on the
// section asked for. It remembers its size and position (view-settings.json),
// closes with the app, and stops the device search when it closes.

using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.Graphics;
using static TerraFX.Interop.Windows.SWP;
using static TerraFX.Interop.Windows.Windows;
using Rect = NvrView.Shell.Rect;

namespace NvrView.App;

internal sealed class SettingsWindow
{
    public const double MinWidth = 520, MinHeight = 300;
    private const double DefaultWidth = 900, DefaultHeight = 640;

    private readonly Viewer _viewer;
    private readonly SettingsPages _pages;
    private readonly NavigationView _nav = new();
    private readonly Dictionary<SettingsSection, ScrollViewer> _scrollers = [];
    private readonly Dictionary<SettingsSection, NavigationViewItem> _items = [];
    private readonly DispatcherQueueTimer _saveTimer;
    private readonly bool _background;
    private bool _placing;
    private bool _frameDirty;
    private bool _closed;

    private SettingsWindow(Viewer viewer, bool development, SettingsPagesHooks? hooks, bool background)
    {
        _viewer = viewer;
        _background = background;
        Created++;
        Window = new Window { Title = "Settings - nvrview" };
        AppWindow = Window.AppWindow;
        _pages = SettingsPages.Create(viewer, development, hooks, () => WinRT.Interop.WindowNative.GetWindowHandle(Window));

        _nav.PaneDisplayMode = NavigationViewPaneDisplayMode.Left;
        _nav.IsSettingsVisible = false;
        _nav.IsBackButtonVisible = NavigationViewBackButtonVisible.Collapsed;
        _nav.IsPaneToggleButtonVisible = false;
        _nav.OpenPaneLength = 190;
        _nav.AlwaysShowHeader = false;
        foreach (SettingsSection section in SettingsSections.All)
        {
            var item = new NavigationViewItem
            {
                Content = section.Title(),
                Tag = section,
                Icon = new FontIcon { Glyph = section.Glyph() },
            };
            _items[section] = item;
            _nav.MenuItems.Add(item);

            // The page is a column of controls under the section's title; the ScrollViewer lets a
            // section taller than the window scroll to its last control.
            var column = new StackPanel { Spacing = 16, Margin = new Thickness(28, 20, 28, 28), HorizontalAlignment = HorizontalAlignment.Left };
            column.Children.Add(new TextBlock
            {
                Text = section.Title(),
                Style = (Style)Application.Current.Resources["TitleTextBlockStyle"],
            });
            column.Children.Add(_pages.Pages[section]);
            _scrollers[section] = new ScrollViewer
            {
                Content = column,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
                VerticalScrollMode = ScrollMode.Auto,
                ZoomMode = ZoomMode.Disabled,
            };
        }
        _nav.SelectionChanged += (_, e) =>
        {
            if (e.SelectedItem is NavigationViewItem { Tag: SettingsSection section })
            {
                Show(section);
            }
        };
        Window.Content = _nav;

        _saveTimer = DispatcherQueue.GetForCurrentThread().CreateTimer();
        _saveTimer.Interval = TimeSpan.FromMilliseconds(500);
        _saveTimer.IsRepeating = false;
        _saveTimer.Tick += (_, _) => FlushFrame();
        AppWindow.Changed += OnAppWindowChanged;
        Window.Closed += (_, _) => OnClosed();

        Place();
    }

    /// <summary>The open Settings window, if any (there is at most one).</summary>
    public static SettingsWindow? Current { get; private set; }

    /// <summary>How many Settings windows were ever made (the self-test checks that a second open makes none).</summary>
    public static int Created { get; private set; }

    public Window Window { get; }

    public AppWindow AppWindow { get; }

    /// <summary>The section shown.</summary>
    public SettingsSection Section { get; private set; } = SettingsSection.Devices;

    /// <summary>The window is built and its content loaded.</summary>
    public bool IsLoaded => Window.Content is FrameworkElement { IsLoaded: true };

    /// <summary>
    /// Opens the window on a section, or brings the open one to the front on it. <paramref name="findDevices"/> shows the
    /// Devices section's new-device form and starts the network search.
    /// </summary>
    public static SettingsWindow Open(Viewer viewer, bool development, SettingsSection? section = null, bool findDevices = false,
        SettingsPagesHooks? hooks = null, bool background = false)
    {
        bool fresh = Current is null;
        SettingsWindow w = Current ??= new SettingsWindow(viewer, development, hooks, background);
        w.Show(section ?? (fresh ? SettingsSection.Devices : w.Section));
        if (fresh)
        {
            w._pages.Opened();
            w.Reveal();
        }
        else
        {
            w.BringToFront();
        }
        if (findDevices)
        {
            w.Show(SettingsSection.Devices);
            w._pages.FindDevices();
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

    /// <summary>Shows a section (the pane follows).</summary>
    public void Show(SettingsSection section)
    {
        Section = section;
        if (!ReferenceEquals(_nav.SelectedItem, _items[section]))
        {
            _nav.SelectedItem = _items[section];
            return; // the selection event comes back here with the section selected
        }
        _nav.Content = _scrollers[section];
        _pages.Shown(section);
    }

    /// <summary>The pane's entries, top to bottom.</summary>
    public IEnumerable<string> SectionTitles => _nav.MenuItems.OfType<NavigationViewItem>().Select(i => i.Content?.ToString() ?? "");

    /// <summary>The scroller showing now.</summary>
    public ScrollViewer? ShownScroller => _nav.Content as ScrollViewer;

    /// <summary>A frame change is waiting to be written to the settings.</summary>
    public bool FramePending => _frameDirty;

    /// <summary>The scroller of a section (the self-test scrolls it).</summary>
    public ScrollViewer ScrollerOf(SettingsSection section) => _scrollers[section];

    /// <summary>The last element of a section's page (the self-test checks that scrolling reaches it).</summary>
    public FrameworkElement LastElementOf(SettingsSection section) =>
        _pages.Pages[section] is Panel p && p.Children.Count > 0 && p.Children[^1] is FrameworkElement last ? last : _pages.Pages[section];

    /// <summary>Resizes the window in physical pixels without remembering the frame (self-test).</summary>
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

    /// <summary>The window's frame as it is now, in physical pixels.</summary>
    public WindowRect CurrentFrame => new(AppWindow.Position.X, AppWindow.Position.Y, AppWindow.Size.Width, AppWindow.Size.Height);

    /// <summary>Writes a pending frame to the settings now (the timer does it a moment after the last move).</summary>
    public void FlushFrame()
    {
        _saveTimer.Stop();
        if (_frameDirty && !_closed)
        {
            _frameDirty = false;
            _viewer.Settings.SettingsWindowFrame = CurrentFrame;
        }
    }

    // ---- placement and showing ----

    private void Place()
    {
        _placing = true;
        try
        {
            if (_viewer.Settings.SettingsWindowFrame is { } saved
                && SavedWindow.Placement(new Rect(saved.X, saved.Y, saved.Width, saved.Height), ViewerWindow.Screens(), MinWidth, MinHeight) is { } f)
            {
                AppWindow.MoveAndResize(new RectInt32((int)f.X, (int)f.Y, (int)f.Width, (int)f.Height));
            }
            else if (_background)
            {
                AppWindow.MoveAndResize(new RectInt32(60, 60, (int)DefaultWidth, (int)DefaultHeight));
            }
            else
            {
                nint hwnd = WinRT.Interop.WindowNative.GetWindowHandle(Window);
                double scale = GetDpiForWindow((TerraFX.Interop.Windows.HWND)hwnd) / 96.0;
                AppWindow.Resize(new SizeInt32((int)(DefaultWidth * scale), (int)(DefaultHeight * scale)));
            }
        }
        finally
        {
            _placing = false;
        }
    }

    /// <summary>Shows the new window: activated, except in a test run, which keeps it behind the others.</summary>
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

    /// <summary>The window is already open: forward, restored from a minimized state; a test run leaves it where it is.</summary>
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
        if (_closed || _placing || !(args.DidPositionChange || args.DidSizeChange))
        {
            return;
        }
        if (AppWindow.Presenter is OverlappedPresenter { State: not OverlappedPresenterState.Restored })
        {
            return; // a maximized or minimized frame is not the one to come back to
        }
        _frameDirty = true;
        _saveTimer.Stop();
        _saveTimer.Start();
    }

    private void OnClosed()
    {
        if (_closed)
        {
            return;
        }
        FlushFrame();
        _closed = true;
        _saveTimer.Stop();
        AppWindow.Changed -= OnAppWindowChanged;
        _pages.Closed();
        if (ReferenceEquals(Current, this))
        {
            Current = null;
        }
    }
}
