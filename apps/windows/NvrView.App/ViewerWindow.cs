// One viewer window: its own content (a stored layout, a group shown as an
// even grid, or an unsaved arrangement), stage and render thread, sidebar,
// edit bar, and the collapsed sidebar's edge reveal (docs/UX.md "Layouts",
// "Sidebar", "Keyboard"). Every window leases the cameras its layout shows,
// so a camera in several windows is streamed once; edits to a stored layout
// are saved back to the core's store, and a layout changed elsewhere (another
// window, another app) is followed, except while a drag here is changing it.
//
// The reveal: while the sidebar is collapsed, resting the pointer at the left
// edge for about 150 ms slides a copy of the sidebar over the stage, which is
// not resized; it slides away about half a second after the pointer leaves it,
// unless a drag, a menu, or a name being typed holds it. S, or the overlay's
// pin, docks the sidebar again. Timing lives in NvrView.Shell.SidebarReveal;
// one timer is armed for its next deadline (nothing runs while idle).

using System.Numerics;
using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Input;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.Graphics;
using Windows.System;
using Windows.UI.Core;
using static TerraFX.Interop.Windows.SWP;
using static TerraFX.Interop.Windows.Windows;
using Rect = NvrView.Shell.Rect;

namespace NvrView.App;

internal sealed class ViewerWindow
{
    public const double MinWidth = 640, MinHeight = 400;
    private const double EdgeWidth = 6;
    private const double OverlayHidden = -(SidebarView.DefaultWidth + 24);

    private readonly Viewer _viewer;
    private readonly bool _background;
    private readonly Grid _root = new();
    private readonly ColumnDefinition _sidebarColumn = new();
    private readonly ColumnDefinition _handleColumn = new() { Width = new GridLength(4) };
    private readonly Border _sidebarHost = new();
    private readonly ResizeHandle _handle = new();
    private readonly EditBar _editBar;
    private readonly Border _edge;
    private readonly Border _overlay;
    private readonly SidebarReveal _reveal = new();
    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer _revealTimer;
    private Dictionary<CameraRef, int> _held = [];

    /// <summary>Set while this window shows an unedited All Cameras grid, which follows the devices.</summary>
    private AllCamerasWatch? _allCamerasWatch;
    private bool _sidebarCollapsed;
    private bool _fullScreenSidebar;
    private double _sidebarWidth = SidebarView.DefaultWidth;
    private Rect _windowedFrame;
    private bool _wasFullScreen;
    private bool _asking;
    private bool _closed;

    public ViewerWindow(Viewer viewer, CoreHost host, WindowContent content, SavedWindow? restore, bool background)
    {
        _viewer = viewer;
        _background = background;
        Window = new Window { Title = "nvrview" };
        AppWindow = Window.AppWindow;
        var state = new WindowState(content, "", LayoutNode.Empty);
        Stage = new StageView(viewer, AppWindow, state);
        Microsoft.UI.Dispatching.DispatcherQueue ui = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        Renderer = new StageRenderer(host, chain => ui.TryEnqueue(() => Stage.AttachSwapChain(chain)));
        Stage.Renderer = Renderer;
        Renderer.Circles = viewer.Circles;
        Stage.SettingsRequested = OpenSettings;
        Stage.UpdateTitle = () => App.Instance.Updater?.MenuTitle ?? "Check for updates…";
        Stage.UpdateRequested = () => App.Instance.Updater?.MenuAction();
        Stage.Edited += OnEdited;
        Stage.StateChanged += () => _editBar!.Update();
        Stage.WindowMenu = AddWindowMenu;

        Sidebar = new SidebarView(viewer, this, overlay: false);
        OverlaySidebar = new SidebarView(viewer, this, overlay: true);
        OverlaySidebar.PinRequested += () => SetSidebarDocked(true);
        OverlaySidebar.MenuClosed += () => ArmReveal();
        _editBar = new EditBar(this);

        // Columns: sidebar | resize handle | stage area.
        _root.ColumnDefinitions.Add(_sidebarColumn);
        _root.ColumnDefinitions.Add(_handleColumn);
        _root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        _root.Background = new SolidColorBrush(Colors.Black);
        _root.RequestedTheme = ElementTheme.Dark;
        _sidebarHost.Child = Sidebar;
        _root.Children.Add(_sidebarHost);
        Grid.SetColumn(_handle, 1);
        _handle.Dragged += dx => SetSidebarWidth(_sidebarWidth + dx);
        _handle.Released += () => Changed?.Invoke();
        _root.Children.Add(_handle);

        var stageArea = new Grid();
        stageArea.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        stageArea.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        stageArea.Children.Add(_editBar.Root);
        Grid.SetRow(Stage, 1);
        stageArea.Children.Add(Stage);
        // The edge strip and the overlay sit above the stage: revealing never resizes it.
        _edge = new Border
        {
            Width = EdgeWidth,
            HorizontalAlignment = HorizontalAlignment.Left,
            Background = new SolidColorBrush(Colors.Transparent),
            Visibility = Visibility.Collapsed,
        };
        Grid.SetRowSpan(_edge, 2);
        _edge.PointerEntered += (_, _) => PointerAt(inEdge: true, inOverlay: false);
        _edge.PointerMoved += (_, _) => PointerAt(inEdge: true, inOverlay: false);
        _edge.PointerExited += (_, _) => PointerAt(inEdge: false, inOverlay: _reveal.IsShown && IsPointerOver(_overlay!));
        stageArea.Children.Add(_edge);
        _overlay = new Border
        {
            Width = SidebarView.DefaultWidth,
            HorizontalAlignment = HorizontalAlignment.Left,
            Child = OverlaySidebar,
            Translation = new Vector3((float)OverlayHidden, 0, 0),
            TranslationTransition = new Vector3Transition { Duration = TimeSpan.FromMilliseconds(180) },
            Opacity = 0,
            OpacityTransition = new ScalarTransition { Duration = TimeSpan.FromMilliseconds(180) },
            IsHitTestVisible = false,
            Shadow = new ThemeShadow(),
        };
        Grid.SetRowSpan(_overlay, 2);
        _overlay.PointerEntered += (_, _) => PointerAt(inEdge: false, inOverlay: true);
        _overlay.PointerExited += (_, _) => PointerAt(inEdge: false, inOverlay: false);
        stageArea.Children.Add(_overlay);
        Grid.SetColumn(stageArea, 2);
        _root.Children.Add(stageArea);
        _root.KeyDown += OnKey;
        Window.Content = _root;

        _revealTimer = ui.CreateTimer();
        _revealTimer.IsRepeating = false;
        _revealTimer.Tick += (_, _) => TickReveal();

        viewer.LayoutsChanged += OnLayoutsChanged;
        viewer.Changed += OnViewerChanged;
        AppWindow.Changed += OnAppWindowChanged;
        Window.Closed += (_, _) => Close();
        Window.Activated += (_, e) =>
        {
            if (e.WindowActivationState != WindowActivationState.Deactivated)
            {
                Activated?.Invoke(this);
            }
        };

        if (restore is not null)
        {
            _sidebarCollapsed = restore.SidebarCollapsed;
            if (restore.SidebarWidth >= 160)
            {
                _sidebarWidth = restore.SidebarWidth;
            }
            if (restore is { InsetX: { } ix, InsetY: { } iy })
            {
                Stage.CallCards.RestoreAnchor(new InsetAnchor(ix, iy));
            }
        }
        // Moving the call insets saves the window's record (their position belongs to the window).
        Stage.CallCards.AnchorChanged += () => Changed?.Invoke();
        ApplySidebar();
        Place(restore);
        Stage.PushSize();
        Renderer.Start();
        Show(content);
        if (restore is { FullScreen: true } && !background)
        {
            AppWindow.SetPresenter(AppWindowPresenterKind.FullScreen);
        }
    }

    public Window Window { get; }

    public AppWindow AppWindow { get; }

    public StageView Stage { get; }

    public StageRenderer Renderer { get; }

    public SidebarView Sidebar { get; }

    public SidebarView OverlaySidebar { get; }

    public WindowState State => Stage.State;

    /// <summary>What a relaunch should restore changed (content, frame, full screen, sidebar).</summary>
    public event Action? Changed;

    /// <summary>The window became the active one (the user is working in it).</summary>
    public event Action<ViewerWindow>? Activated;

    /// <summary>The window closed (after its renderer stopped and its leases were given back).</summary>
    public event Action<ViewerWindow>? Closed;

    /// <summary>The reveal's clock (the self-test drives a manual one).</summary>
    public IViewerClock RevealClock { get; set; } = new SystemViewerClock();

    public bool RevealShown => _reveal.IsShown;

    /// <summary>The sidebar is docked beside the stage now (not collapsed; in full screen, shown with S).</summary>
    public bool SidebarDocked => IsFullScreen ? _fullScreenSidebar : !_sidebarCollapsed;

    private bool IsFullScreen => AppWindow.Presenter.Kind == AppWindowPresenterKind.FullScreen;

    // ---- placement ----

    private void Place(SavedWindow? restore)
    {
        // The test runs read the displays too: the start path is the same as
        // a double-click's but for where the window ends up.
        List<Rect> screens = Screens();
        if (restore is not null && SavedWindow.Placement(restore.Frame, screens, MinWidth, MinHeight) is { } f)
        {
            AppWindow.MoveAndResize(new RectInt32((int)f.X, (int)f.Y, (int)f.Width, (int)f.Height));
        }
        else if (_background)
        {
            AppWindow.MoveAndResize(new RectInt32(40, 40, 1024, 640));
        }
        else
        {
            AppWindow.Resize(new SizeInt32(1280, 800));
        }
        if (_background)
        {
            // Behind every other window, without activation: the person at the
            // computer keeps working (a test run never takes focus).
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
        RememberFrame();
    }

    /// <summary>Times reading the displays failed (the startup test wants none).</summary>
    public static int ScreenReadFailures { get; private set; }

    /// <summary>The work areas of the displays, the primary first (empty when they cannot be read).</summary>
    public static List<Rect> Screens()
    {
        List<Rect> screens = [];
        try
        {
            screens.Add(Work(DisplayArea.Primary));
            // By index, never foreach: the list FindAll returns does not
            // answer the QueryInterface for IIterable that the projection's
            // enumerator asks for (E_NOINTERFACE, an InvalidCastException
            // that ended the app at its first window), while Count and the
            // indexer work.
            IReadOnlyList<DisplayArea> all = DisplayArea.FindAll();
            for (int i = 0; i < all.Count; i++)
            {
                Rect r = Work(all[i]);
                if (!screens.Contains(r))
                {
                    screens.Add(r);
                }
            }
        }
        catch (Exception e) when (e is ArgumentException or InvalidOperationException or InvalidCastException
            or System.Runtime.InteropServices.COMException)
        {
            ScreenReadFailures++;
            AppLog.Warn($"reading the displays failed: {e.Message}");
        }
        return screens;
    }

    private static Rect Work(DisplayArea d) => new(d.WorkArea.X, d.WorkArea.Y, d.WorkArea.Width, d.WorkArea.Height);

    private void RememberFrame()
    {
        if (AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Restored } && !IsFullScreen)
        {
            _windowedFrame = new Rect(AppWindow.Position.X, AppWindow.Position.Y, AppWindow.Size.Width, AppWindow.Size.Height);
        }
    }

    private void OnAppWindowChanged(AppWindow sender, AppWindowChangedEventArgs args)
    {
        if (args.DidPositionChange || args.DidSizeChange)
        {
            RememberFrame();
            Changed?.Invoke();
        }
        if (args.DidPresenterChange)
        {
            bool full = IsFullScreen;
            if (full != _wasFullScreen)
            {
                _wasFullScreen = full;
                // The sidebar closes in full screen and comes back on the way out
                // if it was open; S shows it meanwhile (docs/UX.md "Full screen chrome").
                _fullScreenSidebar = false;
                ApplySidebar();
                Changed?.Invoke();
            }
        }
    }

    /// <summary>What a relaunch restores.</summary>
    public SavedWindow ToSaved()
    {
        WindowContent content = State.Content is WindowContent.Scratch ? new WindowContent.Scratch(State.Name, State.Tree) : State.Content;
        InsetAnchor? inset = Stage.CallCards.Anchor;
        return new SavedWindow(content, _windowedFrame, IsFullScreen, _sidebarCollapsed, _sidebarWidth, inset?.X, inset?.Y);
    }

    // ---- content ----

    /// <summary>Every online camera of every connected device (up to the layout limit) as an even grid, unsaved.</summary>
    public static WindowContent AllCameras(Viewer viewer) =>
        new WindowContent.Scratch(NvrView.Shell.AllCameras.Name, LayoutNode.Balanced(viewer.OnlineCameras));

    /// <summary>Shows a layout, group, or arrangement in this window. A focused tile only loses its focus (nothing is torn down).</summary>
    public void Show(WindowContent content)
    {
        Stage.CancelFocus();
        State.Selected = null;
        _allCamerasWatch = null;
        switch (content)
        {
            case WindowContent.Layout(string id) when _viewer.Layout(id) is { } l:
                State.Content = content;
                State.Name = l.Name;
                State.Tree = l.Root;
                break;
            case WindowContent.Group(string id) when _viewer.Group(id) is { } g:
                State.Content = content;
                State.Name = g.Name;
                State.Tree = LayoutNode.Balanced(g.Cameras);
                break;
            case WindowContent.Scratch(string name, LayoutNode root):
                State.Content = content;
                State.Name = name;
                State.Tree = root;
                if (name == NvrView.Shell.AllCameras.Name)
                {
                    _allCamerasWatch = new AllCamerasWatch([.. root.CameraList.OfType<CameraRef>()]);
                }
                break;
            default:
                // A layout or group that no longer exists.
                Show(AllCameras(_viewer));
                return;
        }
        SyncLeases();
        Stage.Reload();
        UpdateTitle();
        Sidebar.Refresh();
        OverlaySidebar.Refresh();
        Changed?.Invoke();
    }

    /// <summary>Leases exactly the cameras the layout shows (each tile counts).</summary>
    private void SyncLeases()
    {
        Dictionary<CameraRef, int> wanted = _closed ? [] : State.Tree.CameraCounts();
        _viewer.Leases.Update(_held, wanted);
        _held = wanted;
    }

    private void OnEdited(LayoutNode tree)
    {
        _allCamerasWatch?.MarkEdited();
        SyncLeases();
        if (State.Content is WindowContent.Layout(string id))
        {
            SavedLayout l = _viewer.Layout(id) ?? new SavedLayout(id, State.Name, tree);
            _viewer.SaveLayout(l with { Root = tree, Name = State.Name });
        }
        else
        {
            State.Content = new WindowContent.Scratch(State.Name, tree);
            Changed?.Invoke();
        }
        UpdateTitle();
        _editBar.Update();
    }

    private void OnLayoutsChanged()
    {
        if (State.Content is not WindowContent.Layout(string id))
        {
            return;
        }
        if (_viewer.Layout(id) is { } l)
        {
            // Changed elsewhere: follow, unless a drag here is still changing
            // it (the drag stores its result when it ends).
            if (!Stage.IsInteracting && (l.Root != State.Tree || l.Name != State.Name))
            {
                State.Tree = l.Root;
                State.Name = l.Name;
                SyncLeases();
                Stage.Reload();
                UpdateTitle();
            }
        }
        else
        {
            // Deleted elsewhere: keep what is on screen, unsaved.
            State.Content = new WindowContent.Scratch(State.Name, State.Tree);
            UpdateTitle();
            Changed?.Invoke();
        }
        _editBar.Update();
    }

    private void OnViewerChanged()
    {
        // An unedited All Cameras window follows devices connecting and channels
        // going on and offline. Rebuilding keeps the cameras that stay (the
        // leases are shared per camera, so their streams do not restart). Not
        // while a tile is focused or the window is being edited.
        if (State.Editing)
        {
            _allCamerasWatch?.MarkEdited();
        }
        if (_allCamerasWatch is { } watch && !Stage.IsFocused && watch.Refresh(_viewer.OnlineCameras))
        {
            Show(AllCameras(_viewer));
        }
    }

    private void UpdateTitle()
    {
        string kind = State.Content switch
        {
            WindowContent.Group => " (group)",
            WindowContent.Scratch => " (unsaved)",
            _ => "",
        };
        Window.Title = $"{State.Name}{kind} - nvrview";
        _editBar.Update();
    }

    /// <summary>A camera picked in the sidebar: into the selected tile (or the only one), else the first empty tile, else shown alone.</summary>
    public void CameraPicked(CameraRef camera)
    {
        NodePath? target = State.Selected ?? (State.Tree.LeafCount == 1 ? NodePath.Root : null);
        if (target is { } path && Stage.Renderer.Focus is null)
        {
            Stage.Assign(camera, path);
        }
        else if (State.Tree.Leaves.FirstOrDefault(l => l.Camera is null) is { Path: var empty } leaf && leaf.Camera is null && State.Tree.Leaves.Any(l => l.Camera is null))
        {
            Stage.Select(empty);
            Stage.Assign(camera, empty);
        }
        else
        {
            Show(new WindowContent.Scratch(_viewer.CameraName(camera), LayoutNode.Leaf(camera)));
        }
    }

    public async void NewLayout()
    {
        if (await AskName("New layout", _viewer.UniqueLayoutName("Layout")) is not { } name)
        {
            return;
        }
        List<CameraRef> cameras = [.. State.Tree.CameraList.OfType<CameraRef>()];
        if (_viewer.CreateLayout(name, LayoutPreset.TwoByTwo.Tree(cameras)) is { } id)
        {
            Show(new WindowContent.Layout(id));
            Stage.SetEditing(true);
        }
    }

    /// <summary>
    /// A new layout in a template's shape, filled with the online cameras in All Cameras order (empty tiles when
    /// there are fewer), opened for editing.
    /// </summary>
    public async void NewLayoutFromTemplate(LayoutTemplate template)
    {
        if (await AskName("New layout", _viewer.UniqueLayoutName("Layout")) is not { } name)
        {
            return;
        }
        if (_viewer.CreateLayout(name, template.Tree(_viewer.OnlineCameras)) is { } id)
        {
            Show(new WindowContent.Layout(id));
            Stage.SetEditing(true);
        }
    }

    /// <summary>One item per template (shape icon, tile count, shape), smallest first.</summary>
    public void AddTemplateItems(IList<MenuFlyoutItemBase> items)
    {
        foreach (LayoutTemplate t in LayoutTemplates.All)
        {
            LayoutTemplate template = t;
            var item = new MenuFlyoutItem { Text = template.Title(), Icon = LayoutIcon.Create(template.Shape()) };
            item.Click += (_, _) => NewLayoutFromTemplate(template);
            items.Add(item);
        }
    }

    public async void SaveAsLayout()
    {
        if (await AskName("Save as layout", _viewer.UniqueLayoutName(State.Name)) is { } name)
        {
            SaveAsLayout(name);
        }
    }

    /// <summary>Stores the window's arrangement as a new layout and shows it; its id, or null.</summary>
    public string? SaveAsLayout(string name)
    {
        bool editing = State.Editing;
        if (_viewer.CreateLayout(name, State.Tree) is not { } id)
        {
            return null;
        }
        Show(new WindowContent.Layout(id));
        Stage.SetEditing(editing);
        return id;
    }

    public void Rename(string name)
    {
        name = name.Trim();
        if (name.Length == 0 || name == State.Name)
        {
            return;
        }
        State.Name = name;
        if (State.Content is WindowContent.Layout(string id))
        {
            _viewer.RenameLayout(id, name);
        }
        else
        {
            State.Content = new WindowContent.Scratch(name, State.Tree);
            Changed?.Invoke();
        }
        UpdateTitle();
    }

    /// <summary>Asks for a name in a dialog on this window; null when canceled or empty.</summary>
    public async Task<string?> AskName(string title, string initial)
    {
        if (_asking || Window.Content.XamlRoot is null)
        {
            return null;
        }
        _asking = true;
        try
        {
            var box = new TextBox { Text = initial, Width = 280 };
            box.SelectAll();
            var dialog = new ContentDialog
            {
                XamlRoot = Window.Content.XamlRoot,
                Title = title,
                Content = box,
                PrimaryButtonText = "OK",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Primary,
            };
            ContentDialogResult result = await dialog.ShowAsync();
            string name = box.Text.Trim();
            return result == ContentDialogResult.Primary && name.Length > 0 ? name : null;
        }
        finally
        {
            _asking = false;
        }
    }

    public void OpenSettings()
    {
        if (Window.Content.XamlRoot is { } root)
        {
            _ = SettingsDialog.ShowAsync(root, _viewer, Program.Options.Development);
        }
    }

    private void AddWindowMenu(MenuFlyout menu)
    {
        void Item(string text, Action action, bool enabled = true)
        {
            var item = new MenuFlyoutItem { Text = text, IsEnabled = enabled };
            item.Click += (_, _) => action();
            menu.Items.Add(item);
        }
        Item(State.Editing ? "Done editing" : "Edit layout", () => Stage.SetEditing(!State.Editing));
        if (State.Content is not WindowContent.Layout)
        {
            Item("Save as layout…", SaveAsLayout);
        }
        Item("New layout…", NewLayout);
        var templates = new MenuFlyoutSubItem { Text = "New layout from template" };
        AddTemplateItems(templates.Items);
        menu.Items.Add(templates);
        Item(SidebarDocked ? "Hide sidebar" : "Show sidebar", ToggleSidebar);
        Item("New window", () => App.Instance.OpenWindow(ToSaved().Content));
        menu.Items.Add(new MenuFlyoutSeparator());
    }

    // ---- keys ----

    private void OnKey(object sender, KeyRoutedEventArgs e)
    {
        // Single keys never fire while text is edited (Settings, a name).
        if (FocusManager.GetFocusedElement(Window.Content.XamlRoot) is TextBox or PasswordBox or NumberBox or AutoSuggestBox)
        {
            return;
        }
        bool ctrl = InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control).HasFlag(CoreVirtualKeyStates.Down);
        bool repeat = e.KeyStatus.WasKeyDown;
        switch (e.Key)
        {
            case VirtualKey.S when !ctrl:
                if (!repeat)
                {
                    ToggleSidebar();
                }
                e.Handled = true;
                return;
            case VirtualKey.N when ctrl:
                if (!repeat)
                {
                    App.Instance.OpenWindow(ToSaved().Content);
                }
                e.Handled = true;
                return;
            case VirtualKey.E when ctrl:
                if (!repeat)
                {
                    Stage.SetEditing(!State.Editing);
                }
                e.Handled = true;
                return;
        }
        // A sidebar control with the keyboard focus keeps its own arrows, Space, and Return.
        if (e.Key is VirtualKey.Left or VirtualKey.Right or VirtualKey.Up or VirtualKey.Down or VirtualKey.Space or VirtualKey.Enter
            && FocusManager.GetFocusedElement(Window.Content.XamlRoot) is DependencyObject d && Within(d, _sidebarHost, _overlay))
        {
            return;
        }
        e.Handled = Stage.HandleKey(e.Key, ctrl, repeat);
    }

    private static bool Within(DependencyObject d, params UIElement[] roots)
    {
        for (DependencyObject? p = d; p is not null; p = VisualTreeHelper.GetParent(p))
        {
            if (roots.Any(r => ReferenceEquals(r, p)))
            {
                return true;
            }
        }
        return false;
    }

    // ---- sidebar ----

    /// <summary>S: shows or hides the docked sidebar.</summary>
    public void ToggleSidebar() => SetSidebarDocked(!SidebarDocked);

    public void SetSidebarDocked(bool docked)
    {
        if (IsFullScreen)
        {
            // Shown with S until S again; the saved choice stays as it was.
            _fullScreenSidebar = docked;
        }
        else
        {
            _sidebarCollapsed = !docked;
        }
        ApplySidebar();
        Changed?.Invoke();
    }

    private void SetSidebarWidth(double width)
    {
        _sidebarWidth = Math.Clamp(width, 180, 420);
        ApplySidebar();
    }

    private void ApplySidebar()
    {
        bool docked = SidebarDocked;
        _sidebarColumn.Width = new GridLength(docked ? _sidebarWidth : 0);
        _handleColumn.Width = new GridLength(docked ? 4 : 0);
        _sidebarHost.Visibility = docked ? Visibility.Visible : Visibility.Collapsed;
        _handle.Visibility = _sidebarHost.Visibility;
        _edge.Visibility = docked ? Visibility.Collapsed : Visibility.Visible;
        _reveal.SetEnabled(!docked);
        ApplyReveal();
        ArmReveal();
    }

    private static bool IsPointerOver(UIElement e) => e.IsHitTestVisible && e.Visibility == Visibility.Visible && e.Opacity > 0;

    /// <summary>Where the pointer is for the reveal (also the self-test's simulated pointer).</summary>
    public void PointerAt(bool inEdge, bool inOverlay)
    {
        _reveal.Pointer(inEdge, inOverlay, RevealClock.Now);
        ArmReveal();
    }

    /// <summary>Runs what is due (the timer; the self-test calls it after moving its manual clock).</summary>
    public void TickReveal()
    {
        RevealHolds holds = RevealHolds.None;
        if (OverlaySidebar.IsDragging)
        {
            holds |= RevealHolds.Drag;
        }
        if (OverlaySidebar.IsMenuOpen)
        {
            holds |= RevealHolds.Menu;
        }
        if (OverlaySidebar.IsEditingText)
        {
            holds |= RevealHolds.Editing;
        }
        if (_reveal.Tick(RevealClock.Now, holds))
        {
            ApplyReveal();
        }
        ArmReveal();
    }

    private void ArmReveal()
    {
        _revealTimer.Stop();
        if (_reveal.Deadline is { } deadline && RevealClock is SystemViewerClock)
        {
            _revealTimer.Interval = TimeSpan.FromSeconds(Math.Max(deadline - RevealClock.Now, 0.001));
            _revealTimer.Start();
        }
    }

    private void ApplyReveal()
    {
        bool shown = _reveal.IsShown;
        _overlay.Translation = new Vector3(shown ? 0 : (float)OverlayHidden, 0, 0);
        _overlay.Opacity = shown ? 1 : 0;
        _overlay.IsHitTestVisible = shown;
        if (shown)
        {
            OverlaySidebar.Refresh();
        }
    }

    // ---- closing ----

    /// <summary>Stops this window's rendering and gives its leases back (idempotent).</summary>
    public void Close()
    {
        if (_closed)
        {
            return;
        }
        _closed = true;
        _revealTimer.Stop();
        _viewer.LayoutsChanged -= OnLayoutsChanged;
        _viewer.Changed -= OnViewerChanged;
        AppWindow.Changed -= OnAppWindowChanged;
        Stage.TearDown();
        Renderer.Dispose();
        Stage.AttachSwapChain(0);
        Sidebar.Detach();
        OverlaySidebar.Detach();
        SyncLeases();
        Closed?.Invoke(this);
    }
}

/// <summary>The bar above the stage in Edit Layout mode: name, split, remove, presets, Save as layout, Done.</summary>
internal sealed class EditBar
{
    private readonly ViewerWindow _window;
    private readonly TextBox _name = new() { Width = 200, PlaceholderText = "Layout name" };
    private readonly Button _splitRow = Make("", "Split side by side");
    private readonly Button _splitCol = Make("", "Split stacked");
    private readonly Button _remove = Make("", "Remove tile (Delete)");
    private readonly DropDownButton _presets = new() { Content = "Preset" };
    private readonly Button _saveAs = new() { Content = "Save as layout…" };
    private readonly Button _done = new() { Content = "Done" };

    public EditBar(ViewerWindow window)
    {
        _window = window;
        Root = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            Padding = new Thickness(10, 6, 10, 6),
            Background = new SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0x26, 0x28, 0x2B)),
            Visibility = Visibility.Collapsed,
        };
        Root.Children.Add(_name);
        Root.Children.Add(_splitRow);
        Root.Children.Add(_splitCol);
        Root.Children.Add(_remove);
        var flyout = new MenuFlyout();
        foreach (LayoutPreset p in LayoutPresets.All)
        {
            var item = new MenuFlyoutItem { Text = p.Title() };
            item.Click += (_, _) => _window.Stage.ApplyPreset(p);
            flyout.Items.Add(item);
        }
        _presets.Flyout = flyout;
        Root.Children.Add(_presets);
        Root.Children.Add(_saveAs);
        Root.Children.Add(_done);
        _name.LostFocus += (_, _) => _window.Rename(_name.Text);
        _name.KeyDown += (_, e) =>
        {
            if (e.Key == VirtualKey.Enter)
            {
                _window.Rename(_name.Text);
                _ = _window.Stage.Focus(FocusState.Programmatic);
                e.Handled = true;
            }
        };
        _splitRow.Click += (_, _) => _window.Stage.Split(SplitAxis.Row);
        _splitCol.Click += (_, _) => _window.Stage.Split(SplitAxis.Col);
        _remove.Click += (_, _) => _window.Stage.RemoveSelected();
        _saveAs.Click += (_, _) => _window.SaveAsLayout();
        _done.Click += (_, _) => _window.Stage.SetEditing(false);
    }

    public StackPanel Root { get; }

    private static Button Make(string glyph, string tip)
    {
        var b = new Button { Content = new FontIcon { Glyph = glyph, FontSize = 14 } };
        ToolTipService.SetToolTip(b, tip);
        return b;
    }

    public void Update()
    {
        WindowState s = _window.State;
        Root.Visibility = s.Editing ? Visibility.Visible : Visibility.Collapsed;
        if (_name.FocusState == FocusState.Unfocused)
        {
            _name.Text = s.Name;
        }
        _splitRow.IsEnabled = _splitCol.IsEnabled = _window.Stage.CanSplit;
        _remove.IsEnabled = s.Selected is not null;
        _saveAs.Visibility = s.Content is WindowContent.Layout ? Visibility.Collapsed : Visibility.Visible;
    }
}

/// <summary>The thin handle between the sidebar and the stage that resizes the sidebar.</summary>
internal sealed partial class ResizeHandle : Grid
{
    private double _lastX = double.NaN;

    public ResizeHandle()
    {
        Background = new SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0x18, 0x19, 0x1B));
        ProtectedCursor = InputSystemCursor.Create(InputSystemCursorShape.SizeWestEast);
        PointerPressed += (_, e) =>
        {
            _lastX = e.GetCurrentPoint(null).Position.X;
            _ = CapturePointer(e.Pointer);
            e.Handled = true;
        };
        PointerMoved += (_, e) =>
        {
            if (double.IsNaN(_lastX))
            {
                return;
            }
            double x = e.GetCurrentPoint(null).Position.X;
            Dragged?.Invoke(x - _lastX);
            _lastX = x;
        };
        PointerReleased += (_, e) =>
        {
            _lastX = double.NaN;
            ReleasePointerCapture(e.Pointer);
            Released?.Invoke();
        };
        PointerCaptureLost += (_, _) => _lastX = double.NaN;
    }

    public event Action<double>? Dragged;

    public event Action? Released;
}
