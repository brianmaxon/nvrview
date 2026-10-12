// Settings content, in the sections of the Settings window (SettingsWindow.cs;
// docs/UX.md "Settings window"): Devices (the list and the form for the devices
// to connect to: address, port, user, password; saving reconnects that device),
// Cameras (each camera's options: where its calls open, unmute on ring, talk-back,
// fisheye mount), Alerts & calls (the corners of the detection symbols and call
// cards, calls in their own window, transcripts, recording calls), Capture (where saved
// pictures and recordings go, the picture format, when a recording stops, share-safe names;
// CaptureSettingsPage has the recordings), Updates, and About. The password
// goes straight to the credential store and is never shown again: leaving the box
// empty when editing keeps the saved one. The new-device form opens with "Find
// Devices on This Network": a search of the local network starts when the form is
// shown and stops when it goes away (docs/UX.md, "Finding devices"); choosing a
// row fills the form's address and port.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NvrView.Shell;
using NvrView.Tiles;

namespace NvrView.App;

/// <summary>What the self-test drives and reads: the same actions a click does.</summary>
internal sealed class SettingsPagesHooks
{
    /// <summary>The "New device" button.</summary>
    public Action NewDevice = () => { };

    /// <summary>A click on a row of the found-devices list.</summary>
    public Action<DiscoveredDevice> Choose = _ => { };

    /// <summary>The "Search again" button.</summary>
    public Action SearchAgain = () => { };

    /// <summary>The form's fields as the user would read them: name, address, port.</summary>
    public Func<(string Name, string Host, double Port)> Form = () => ("", "", 0);

    /// <summary>The found-devices list as shown: (title, detail, enabled) per row, and whether the panel is visible.</summary>
    public Func<(bool Visible, List<(string Title, string Detail, bool Enabled)> Rows, string Status)> Panel =
        () => (false, [], "");

    /// <summary>The fisheye cameras' mount pickers as shown: (camera, label, mount shown) per row.</summary>
    public Func<List<(CameraRef Camera, string Label, FisheyeMount Mount)>> Mounts = () => [];

    /// <summary>Chooses a mount in a camera's picker, as a click on its item does.</summary>
    public Action<CameraRef, FisheyeMount> ChooseMount = (_, _) => { };

    /// <summary>The Capture page as shown: the pictures folder's text, the format, the share-safe names box, and whether Reset is enabled.</summary>
    public Func<(string Folder, PictureFormat Format, bool ShareSafe, bool CanReset)> Capture = () => ("", PictureFormat.Png, false, false);

    /// <summary>Chooses a format in the Capture page's picker.</summary>
    public Action<PictureFormat> ChooseFormat = _ => { };

    /// <summary>Sets the Capture page's share-safe names box, as a click does.</summary>
    public Action<bool> SetShareSafe = _ => { };

    /// <summary>Presses the Capture page's Reset button.</summary>
    public Action ResetFolder = () => { };
}

/// <summary>The pages of the Settings window, built once and alive until <see cref="Closed"/>.</summary>
internal sealed class SettingsPages
{
    private Action _opened = () => { };
    private Action _closed = () => { };
    private Action<SettingsSection> _shown = _ => { };
    private Action _findDevices = () => { };

    /// <summary>The window's handle, for the folder picker (set by the Settings window).</summary>
    public Func<nint>? WindowHandle { get; set; }

    private SettingsPages()
    {
    }

    /// <summary>The page of each section (a ScrollViewer's content).</summary>
    public Dictionary<SettingsSection, FrameworkElement> Pages { get; } = [];

    /// <summary>The window is on screen: a pending device search may start.</summary>
    public void Opened() => _opened();

    /// <summary>The window closed: nothing keeps listening and the device search stops.</summary>
    public void Closed() => _closed();

    /// <summary>A section is shown: the device search runs only while the new-device form is.</summary>
    public void Shown(SettingsSection section) => _shown(section);

    /// <summary>The new-device form, searching the network.</summary>
    public void FindDevices() => _findDevices();

    private static StackPanel Section(string title, UIElement body)
    {
        var section = new StackPanel { Spacing = 6 };
        section.Children.Add(new TextBlock { Text = title, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        section.Children.Add(body);
        return section;
    }

    /// <summary>The pages, not shown yet.</summary>
    public static SettingsPages Create(Viewer viewer, bool development, SettingsPagesHooks? hooks = null)
    {
        var result = new SettingsPages();
        var list = new ListView { Width = 220, Height = 260, SelectionMode = ListViewSelectionMode.Single };
        var name = new TextBox { Header = "Name", PlaceholderText = "NVR" };
        var host = new TextBox { Header = "Address", PlaceholderText = "192.0.2.10 or nvr.example" };
        var port = new NumberBox { Header = "Port", Value = 9000, Minimum = 1, Maximum = 65535, SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Hidden };
        var user = new TextBox { Header = "User" };
        var password = new PasswordBox { Header = "Password" };
        var state = new TextBlock { TextWrapping = TextWrapping.Wrap, Opacity = 0.8 };
        var save = new Button { Content = "Save and connect" };
        var remove = new Button { Content = "Remove" };
        var add = new Button { Content = "New device" };
        var note = new TextBlock
        {
            TextWrapping = TextWrapping.Wrap,
            Opacity = 0.7,
            FontSize = 12,
            Text = development
                ? "Development mode (--host or --mock-nvr): changes here last for this run only."
                : "Passwords are kept in Windows Credential Manager, never in a file.",
        };
        DeviceConfig? editing = null;
        bool opened = false;
        bool onDevices = true;

        // Find Devices on This Network: the list, its status, and Search again.
        var finder = new StackPanel { Spacing = 6 };
        var found = new ListView { Height = 140, SelectionMode = ListViewSelectionMode.None, IsItemClickEnabled = true };
        var findStatus = new TextBlock { TextWrapping = TextWrapping.Wrap, Opacity = 0.8, FontSize = 12 };
        var searchAgain = new Button { Content = "Search again" };
        finder.Children.Add(new TextBlock { Text = "Find Devices on This Network", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        finder.Children.Add(found);
        finder.Children.Add(findStatus);
        finder.Children.Add(searchAgain);

        string FindStatusText()
        {
            DiscoveryList d = viewer.Discovery;
            if (d.Unavailable)
            {
                return "This build cannot search the network for devices.";
            }
            if (d.IsSearching)
            {
                return "Searching\u2026";
            }
            if (d.PermissionLikelyBlocked)
            {
                return "Nothing answered, and the system refused to send on the local network. Allow NvrView in the system's local network settings, then search again. You can also type the address below.";
            }
            if (d.FoundNothing)
            {
                return "No devices found. If Windows Firewall asked about NvrView, allow it on private networks, then search again. You can also type the address below.";
            }
            string errors = d.Errors.Count > 0 ? " Some network interfaces could not be searched." : "";
            return d.Devices.Count switch
            {
                0 => "Devices that answer on this network appear here." + errors,
                1 => "1 device found." + errors,
                _ => $"{d.Devices.Count} devices found." + errors,
            };
        }

        string FindIcon(string kind) => kind switch { "nvr" => "\uE839", "camera" => "\uE714", _ => "\uE897" };

        // Rows for what answered so far; added and unsupported devices are shown but cannot be chosen.
        void RefreshFinder()
        {
            found.Items.Clear();
            foreach (DiscoveredDevice d in viewer.Discovery.Devices)
            {
                bool added = viewer.IsAdded(d);
                string note = added ? "Added" : !d.IsNative ? "Not supported yet" : "";
                string extra = string.Join(", ", new[]
                {
                    d.Channels is { } c ? (c == 1 ? "1 channel" : $"{c} channels") : "",
                    d.BatteryPercent is { } b ? $"{b}%" : "",
                    note,
                }.Where(s => s.Length > 0));
                var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
                row.Children.Add(new FontIcon { Glyph = FindIcon(d.Kind), FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Segoe Fluent Icons"), Opacity = 0.7 });
                var texts = new StackPanel();
                texts.Children.Add(new TextBlock { Text = d.Title });
                texts.Children.Add(new TextBlock { Text = extra.Length > 0 ? $"{d.Detail} \u00B7 {extra}" : d.Detail, Opacity = 0.7, FontSize = 12 });
                row.Children.Add(texts);
                found.Items.Add(new ListViewItem { Content = row, Tag = d, IsEnabled = !added && d.IsNative });
            }
            findStatus.Text = FindStatusText();
            findStatus.SetValue(ToolTipService.ToolTipProperty, viewer.Discovery.Errors.Count > 0 ? string.Join("\n", viewer.Discovery.Errors) : null);
        }

        void StartFinding()
        {
            if (opened && onDevices)
            {
                viewer.StartDiscovery();
            }
        }

        void StopFinding()
        {
            if (viewer.Discovery.IsSearching)
            {
                viewer.StopDiscovery();
            }
        }

        // A click on a row: the address and port (9000 when the answer had none) go into the form, and the
        // name while it is empty or still the default one.
        void Choose(DiscoveredDevice d)
        {
            if (!d.IsNative || viewer.IsAdded(d) || editing is not null)
            {
                return;
            }
            host.Text = d.Address;
            port.Value = d.SuggestedPort;
            string current = name.Text.Trim();
            if (d.Name is { Length: > 0 } n && (current.Length == 0 || current == "NVR"))
            {
                name.Text = n;
            }
        }

        found.ItemClick += (_, e) =>
        {
            if (e.ClickedItem is ListViewItem { Tag: DiscoveredDevice d })
            {
                Choose(d);
            }
        };
        searchAgain.Click += (_, _) => StartFinding();
        Action refresh = () => RefreshFinder();
        viewer.Changed += refresh;

        void Fill(DeviceConfig? c)
        {
            bool wasNew = editing is null;
            editing = c;
            finder.Visibility = c is null ? Visibility.Visible : Visibility.Collapsed;
            if (c is null && !wasNew)
            {
                StartFinding();
            }
            else if (c is not null)
            {
                StopFinding();
            }
            name.Text = c?.Name ?? "";
            host.Text = c?.Host ?? "";
            port.Value = c?.Port ?? 9000;
            user.Text = c?.User ?? "";
            password.Password = "";
            password.PlaceholderText = c is null ? "" : "(unchanged)";
            remove.IsEnabled = c is not null;
            DeviceEntry? entry = c is null ? null : viewer.Devices.FirstOrDefault(d => d.Config.Key == c.Key);
            state.Text = entry is null ? "" : StageView.DeviceText(entry) ?? $"Connected ({entry.Model ?? "device"}, {entry.Channels.Count} channels)";
        }

        void Reload(string? selectKey)
        {
            list.Items.Clear();
            foreach (DeviceEntry d in viewer.Devices)
            {
                list.Items.Add(new ListViewItem { Content = d.DisplayName, Tag = d.Config.Key });
            }
            int index = viewer.Devices.ToList().FindIndex(d => d.Config.Key == selectKey);
            list.SelectedIndex = index;
            if (index < 0)
            {
                Fill(null);
            }
        }

        list.SelectionChanged += (_, _) =>
        {
            if (list.SelectedItem is ListViewItem { Tag: string key } &&
                viewer.Devices.FirstOrDefault(d => d.Config.Key == key) is { } d)
            {
                Fill(d.Config.Copy());
            }
        };
        void NewDevice()
        {
            list.SelectedIndex = -1;
            Fill(null);
        }

        add.Click += (_, _) => NewDevice();
        save.Click += (_, _) =>
        {
            string h = host.Text.Trim();
            if (h.Length == 0 || user.Text.Trim().Length == 0)
            {
                state.Text = "Enter an address and a user.";
                return;
            }
            bool isNew = editing is null;
            if (isNew && password.Password.Length == 0)
            {
                state.Text = "Enter the password.";
                return;
            }
            DeviceConfig config = editing ?? new DeviceConfig();
            config.Name = name.Text.Trim();
            config.Host = h;
            config.Port = (ushort)Math.Clamp(double.IsNaN(port.Value) ? 9000 : port.Value, 1, 65535);
            config.User = user.Text.Trim();
            viewer.SaveDevice(config, password.Password);
            password.Password = "";
            Reload(config.Key);
            state.Text = "Connecting…";
        };
        remove.Click += (_, _) =>
        {
            if (editing is { } c)
            {
                viewer.ForgetDevice(c.Key);
                Reload(null);
            }
        };

        var form = new StackPanel { Spacing = 8, Width = 300 };
        form.Children.Add(finder);
        form.Children.Add(name);
        form.Children.Add(host);
        form.Children.Add(port);
        form.Children.Add(user);
        form.Children.Add(password);
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        buttons.Children.Add(save);
        buttons.Children.Add(remove);
        form.Children.Add(buttons);
        form.Children.Add(state);

        var left = new StackPanel { Spacing = 8 };
        left.Children.Add(list);
        left.Children.Add(add);

        // Where the tiles' detection symbols sit (docs/UX.md "Corner indicators"): kept in view-settings.json.
        var corner = new ComboBox { Header = "Detection symbols on tiles", MinWidth = 200 };
        foreach (ChromeCorner c in Enum.GetValues<ChromeCorner>())
        {
            corner.Items.Add(new ComboBoxItem { Content = c.Title(), Tag = c });
        }
        corner.SelectedIndex = (int)viewer.Settings.IndicatorCorner;
        corner.SelectionChanged += (_, _) =>
        {
            if (corner.SelectedItem is ComboBoxItem { Tag: ChromeCorner chosen })
            {
                viewer.Settings.IndicatorCorner = chosen;
            }
        };

        // Where doorbell call cards stack over the stage (docs/UX.md "Alert insets"): the user's
        // corner, else opposite the detection symbols; kept in view-settings.json.
        var insetCorner = new ComboBox { Header = "Doorbell call cards", MinWidth = 200 };
        insetCorner.Items.Add(new ComboBoxItem { Content = "Opposite the detection symbols" });
        foreach (ChromeCorner c in Enum.GetValues<ChromeCorner>())
        {
            insetCorner.Items.Add(new ComboBoxItem { Content = c.Title(), Tag = c });
        }
        insetCorner.SelectedIndex = viewer.Settings.InsetCornerChoice is { } chosenInset ? (int)chosenInset + 1 : 0;
        insetCorner.SelectionChanged += (_, _) =>
        {
            if (insetCorner.SelectedItem is ComboBoxItem item)
            {
                viewer.Settings.InsetCornerChoice = item.Tag is ChromeCorner chosen ? chosen : null;
            }
        };
        var corners = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 16 };
        corners.Children.Add(corner);
        corners.Children.Add(insetCorner);

        // Saved call transcripts (docs/UX.md "Doorbell calls"): off by default; kept in view-settings.json.
        var saveTranscripts = new CheckBox { Content = "Save call transcripts", IsChecked = viewer.Settings.SaveCallTranscripts };
        saveTranscripts.Click += (_, _) => viewer.Settings.SaveCallTranscripts = saveTranscripts.IsChecked == true;
        var openTranscripts = new Button { Content = "Open transcripts folder" };
        openTranscripts.Click += (_, _) => viewer.Transcripts.OpenFolder();
        var transcriptRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 16 };
        transcriptRow.Children.Add(saveTranscripts);
        transcriptRow.Children.Add(openTranscripts);
        var transcriptNote = new TextBlock
        {
            Text = "Calls are transcribed on this PC while they last. With saving on, each call's transcript goes into the log and into a text file in the Transcripts folder (the newest 500 files, at most 50 MB in all, are kept). With saving off (the default), the transcript is gone when the call ends. Recording a conversation may need the other person's consent where you live.",
            TextWrapping = TextWrapping.Wrap,
            MaxWidth = 560,
            Opacity = 0.7,
        };
        // Calls in their own window (docs/UX.md "A call in its own window"): the default for every
        // doorbell, where call windows open, and each camera's own choice.
        var callsInWindow = new CheckBox { Content = "Open calls in their own window", IsChecked = viewer.Settings.CallsOpenInWindow };
        var windowPlacement = new ComboBox { Header = "Open call windows", MinWidth = 200 };
        foreach (CallWindowPlacement.Mode m in Enum.GetValues<CallWindowPlacement.Mode>())
        {
            windowPlacement.Items.Add(new ComboBoxItem { Content = m.Title(), Tag = m });
        }
        windowPlacement.SelectedIndex = (int)viewer.Settings.CallWindowPlacementMode;
        windowPlacement.SelectionChanged += (_, _) =>
        {
            if (windowPlacement.SelectedItem is ComboBoxItem { Tag: CallWindowPlacement.Mode chosen })
            {
                viewer.Settings.CallWindowPlacementMode = chosen;
            }
        };
        var callRows = new StackPanel { Spacing = 6 };
        StackPanel callSection = Section("Calls open in", callRows);
        // "Unmute on ring even when not listening", per doorbell (docs/UX.md "Doorbell calls").
        var unmuteRows = new StackPanel { Spacing = 6 };
        StackPanel unmuteSection = Section("Unmute on ring even when not listening", unmuteRows);
        // "Allow talk-back", per camera (camera motion control, off until allowed; every use is logged).
        var talkRows = new StackPanel { Spacing = 6 };
        StackPanel talkSection = Section("Allow talk-back", talkRows);
        var talkBoxes = new List<(CameraRef Camera, string Label, CheckBox Box)>();
        var noCameras = new TextBlock { Text = "No cameras yet. Connect a device first.", Opacity = 0.7 };
        var unmuteBoxes = new List<(CameraRef Camera, string Label, CheckBox Box)>();
        var callPickers = new List<(CameraRef Camera, string Label, ComboBox Picker)>();
        bool buildingCalls = false;
        bool lastCallsGlobal = viewer.Settings.CallsOpenInWindow;
        bool firstCallBuild = true;

        void RefreshCallChoices()
        {
            var cameras = new List<(CameraRef Camera, string Label)>();
            foreach (DeviceEntry d in viewer.Devices)
            {
                foreach (ChannelInfo ch in d.Channels.Values.OrderBy(x => x.Channel))
                {
                    var camera = new CameraRef(d.Key, ch.Channel);
                    cameras.Add((camera, viewer.Devices.Count > 1 ? $"{d.DisplayName} \u00B7 {viewer.ChannelName(camera)}" : viewer.ChannelName(camera)));
                }
            }
            buildingCalls = true;
            bool global = viewer.Settings.CallsOpenInWindow;
            if (callPickers.Count != cameras.Count || !callPickers.Select(r => (r.Camera, r.Label)).SequenceEqual(cameras))
            {
                callPickers.Clear();
                callRows.Children.Clear();
                foreach ((CameraRef camera, string label) in cameras)
                {
                    var picker = new ComboBox { Header = label, MinWidth = 200 };
                    CameraRef cam = camera;
                    picker.SelectionChanged += (_, _) =>
                    {
                        if (!buildingCalls && picker.SelectedItem is ComboBoxItem { Tag: CallPresentationChoice choice })
                        {
                            viewer.Settings.SetCallPresentation(cam, choice.Value);
                        }
                    };
                    callPickers.Add((camera, label, picker));
                    callRows.Children.Add(picker);
                }
            }
            if (unmuteBoxes.Count != cameras.Count || !unmuteBoxes.Select(r => (r.Camera, r.Label)).SequenceEqual(cameras))
            {
                unmuteBoxes.Clear();
                unmuteRows.Children.Clear();
                foreach ((CameraRef camera, string label) in cameras)
                {
                    var box = new CheckBox { Content = label };
                    CameraRef cam = camera;
                    box.Click += (_, _) => viewer.Settings.SetUnmuteOnRing(cam, box.IsChecked == true);
                    unmuteBoxes.Add((camera, label, box));
                    unmuteRows.Children.Add(box);
                }
            }
            foreach ((CameraRef camera, _, CheckBox box) in unmuteBoxes)
            {
                bool on = viewer.Settings.UnmuteOnRing(camera);
                if (box.IsChecked != on)
                {
                    box.IsChecked = on;
                }
            }
            if (talkBoxes.Count != cameras.Count || !talkBoxes.Select(r => (r.Camera, r.Label)).SequenceEqual(cameras))
            {
                talkBoxes.Clear();
                talkRows.Children.Clear();
                foreach ((CameraRef camera, string label) in cameras)
                {
                    var box = new CheckBox { Content = label };
                    CameraRef cam = camera;
                    box.Click += (_, _) => viewer.SetTalkAllowed(cam, box.IsChecked == true);
                    talkBoxes.Add((camera, label, box));
                    talkRows.Children.Add(box);
                }
            }
            foreach ((CameraRef camera, _, CheckBox box) in talkBoxes)
            {
                bool on = viewer.Settings.TalkAllowed(camera);
                if (box.IsChecked != on)
                {
                    box.IsChecked = on;
                }
            }
            talkSection.Visibility = cameras.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
            noCameras.Visibility = cameras.Count > 0 ? Visibility.Collapsed : Visibility.Visible;
            unmuteSection.Visibility = cameras.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
            bool rebuild = firstCallBuild || global != lastCallsGlobal;
            firstCallBuild = false;
            lastCallsGlobal = global;
            foreach ((CameraRef camera, _, ComboBox picker) in callPickers)
            {
                // Default names what the setting gives now.
                if (rebuild || picker.Items.Count != 3)
                {
                    picker.Items.Clear();
                    foreach ((string title, CallPresentation? value) in CallPresentationMenu.Choices(global))
                    {
                        picker.Items.Add(new ComboBoxItem { Content = title, Tag = new CallPresentationChoice(value) });
                    }
                }
                int index = viewer.Settings.CallPresentationFor(camera) switch { null => 0, CallPresentation.Window => 1, _ => 2 };
                if (picker.SelectedIndex != index)
                {
                    picker.SelectedIndex = index;
                }
            }
            callSection.Visibility = cameras.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
            buildingCalls = false;
        }

        RefreshCallChoices();
        callsInWindow.Click += (_, _) =>
        {
            viewer.Settings.CallsOpenInWindow = callsInWindow.IsChecked == true;
            RefreshCallChoices();
        };
        Action refreshCalls = RefreshCallChoices;
        viewer.Changed += refreshCalls;
        var windowRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 16 };
        windowRow.Children.Add(callsInWindow);
        windowRow.Children.Add(windowPlacement);

        var alertsPage = new StackPanel { Spacing = 12 };
        alertsPage.Children.Add(corners);
        alertsPage.Children.Add(windowRow);
        alertsPage.Children.Add(transcriptRow);
        alertsPage.Children.Add(transcriptNote);
        alertsPage.Children.Add(CaptureSettingsPage.RecordCalls(viewer));

        // Fisheye cameras: how each is mounted (docs/UX.md "Fisheye cameras"), kept in view-settings.json
        // like the menu's choice; shown only when a connected camera reports a fisheye lens.
        var mounts = new StackPanel { Spacing = 6, Visibility = Visibility.Collapsed };
        var mountRows = new List<(CameraRef Camera, string Label, ComboBox Picker)>();
        bool buildingMounts = false;

        void RefreshMounts()
        {
            var fisheye = new List<(CameraRef Camera, string Label)>();
            foreach (DeviceEntry d in viewer.Devices)
            {
                foreach (ChannelInfo ch in d.Channels.Values.Where(x => x.Fisheye).OrderBy(x => x.Channel))
                {
                    var camera = new CameraRef(d.Key, ch.Channel);
                    fisheye.Add((camera, viewer.Devices.Count > 1 ? $"{d.DisplayName} \u00B7 {viewer.ChannelName(camera)}" : viewer.ChannelName(camera)));
                }
            }
            if (mountRows.Count == fisheye.Count && mountRows.Select(r => (r.Camera, r.Label)).SequenceEqual(fisheye))
            {
                buildingMounts = true;
                foreach (var r in mountRows)
                {
                    r.Picker.SelectedIndex = (int)viewer.Settings.Mount(r.Camera);
                }
                buildingMounts = false;
                return;
            }
            buildingMounts = true;
            mountRows.Clear();
            mounts.Children.Clear();
            mounts.Children.Add(new TextBlock { Text = "Fisheye cameras", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
            foreach ((CameraRef camera, string label) in fisheye)
            {
                var picker = new ComboBox { Header = $"{label}: mount", MinWidth = 200 };
                foreach (FisheyeMount m in Enum.GetValues<FisheyeMount>())
                {
                    picker.Items.Add(new ComboBoxItem { Content = m.Title(), Tag = m });
                }
                picker.SelectedIndex = (int)viewer.Settings.Mount(camera);
                CameraRef cam = camera;
                picker.SelectionChanged += (_, _) =>
                {
                    if (!buildingMounts && picker.SelectedItem is ComboBoxItem { Tag: FisheyeMount chosen })
                    {
                        viewer.Settings.SetMount(cam, chosen);
                    }
                };
                mountRows.Add((camera, label, picker));
                mounts.Children.Add(picker);
            }
            mounts.Visibility = fisheye.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
            buildingMounts = false;
        }

        RefreshMounts();
        Action refreshMounts = RefreshMounts;
        viewer.Changed += refreshMounts;

        var devicesPage = new Grid { ColumnSpacing = 16, RowSpacing = 12 };
        devicesPage.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        devicesPage.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        devicesPage.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        devicesPage.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        devicesPage.Children.Add(left);
        Grid.SetColumn(form, 1);
        devicesPage.Children.Add(form);
        Grid.SetRow(note, 1);
        Grid.SetColumnSpan(note, 2);
        devicesPage.Children.Add(note);

        var camerasPage = new StackPanel { Spacing = 16 };
        camerasPage.Children.Add(noCameras);
        camerasPage.Children.Add(callSection);
        camerasPage.Children.Add(unmuteSection);
        camerasPage.Children.Add(talkSection);
        camerasPage.Children.Add(mounts);

        // Settings > Capture (docs/UX.md "Save picture"): where pictures go, their format, and share-safe names.
        var capturePage = new StackPanel { Spacing = 12 };
        var folderText = new TextBlock { TextTrimming = TextTrimming.CharacterEllipsis, VerticalAlignment = VerticalAlignment.Center, MaxWidth = 380, Opacity = 0.85 };
        var chooseFolder = new Button { Content = "Choose\u2026" };
        var resetFolder = new Button { Content = "Reset" };
        var openFolder = new Button { Content = "Open pictures folder" };
        var formatPicker = new ComboBox { Header = "Format", MinWidth = 160 };
        foreach (PictureFormat f in Enum.GetValues<PictureFormat>())
        {
            formatPicker.Items.Add(new ComboBoxItem { Content = f.Title(), Tag = f });
        }
        var shareSafe = new CheckBox { Content = "Share-safe names" };
        bool buildingCapture = false;

        void RefreshCapture()
        {
            buildingCapture = true;
            folderText.Text = viewer.Settings.PicturesFolder;
            resetFolder.IsEnabled = viewer.Settings.PicturesFolder != ViewSettings.DefaultPicturesFolder;
            formatPicker.SelectedIndex = (int)viewer.Settings.PictureFormat;
            shareSafe.IsChecked = viewer.Settings.ShareSafePictureNames;
            buildingCapture = false;
        }

        RefreshCapture();
        chooseFolder.Click += async (_, _) =>
        {
            if (result.WindowHandle is not { } handle)
            {
                return;
            }
            var picker = new Windows.Storage.Pickers.FolderPicker { SuggestedStartLocation = Windows.Storage.Pickers.PickerLocationId.PicturesLibrary };
            picker.FileTypeFilter.Add("*");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, handle());
            Windows.Storage.StorageFolder? folder = await picker.PickSingleFolderAsync();
            if (folder is not null)
            {
                viewer.Settings.PicturesFolder = folder.Path;
            }
        };
        resetFolder.Click += (_, _) => viewer.Settings.PicturesFolder = ViewSettings.DefaultPicturesFolder;
        openFolder.Click += (_, _) =>
        {
            try
            {
                Directory.CreateDirectory(viewer.Settings.PicturesFolder);
                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo { FileName = viewer.Settings.PicturesFolder, UseShellExecute = true })?.Dispose();
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
            {
                AppLog.Warn($"the pictures folder could not be opened: {e.GetType().Name}");
            }
        };
        formatPicker.SelectionChanged += (_, _) =>
        {
            if (!buildingCapture && formatPicker.SelectedItem is ComboBoxItem { Tag: PictureFormat chosenFormat })
            {
                viewer.Settings.PictureFormat = chosenFormat;
            }
        };
        shareSafe.Click += (_, _) => viewer.Settings.ShareSafePictureNames = shareSafe.IsChecked == true;
        Action refreshCapture = RefreshCapture;
        viewer.Changed += refreshCapture;
        var folderRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        folderRow.Children.Add(new TextBlock { Text = "Save pictures to", VerticalAlignment = VerticalAlignment.Center });
        folderRow.Children.Add(folderText);
        folderRow.Children.Add(chooseFolder);
        folderRow.Children.Add(resetFolder);
        capturePage.Children.Add(folderRow);
        capturePage.Children.Add(openFolder);
        capturePage.Children.Add(formatPicker);
        capturePage.Children.Add(shareSafe);
        capturePage.Children.Add(new TextBlock
        {
            Text = "Pictures and recordings are named with the time and the camera's name. With share-safe names the camera is called Camera A, so a file can be shared without saying where it came from. JPEG is saved at quality 0.9.",
            TextWrapping = TextWrapping.Wrap,
            MaxWidth = 560,
            Opacity = 0.7,
        });
        // Recordings (docs/UX.md "Recording"): the same page, below the pictures.
        capturePage.Children.Add(CaptureSettingsPage.Recordings(viewer, () => result.WindowHandle?.Invoke() ?? 0));

        var updatesPage = new StackPanel { Spacing = 8 };
        if (App.Instance.Updater is { } updater)
        {
            updatesPage.Children.Add(UpdateSettingsPanel.Build(updater));
        }
        else
        {
            updatesPage.Children.Add(new TextBlock { Text = "Updates are not available in this run.", Opacity = 0.7 });
        }

        var aboutPage = new StackPanel { Spacing = 6 };
        aboutPage.Children.Add(new TextBlock { Text = "NvrView", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, FontSize = 18 });
        aboutPage.Children.Add(new TextBlock { Text = $"Version {App.Instance.Updater?.Current ?? typeof(SettingsPages).Assembly.GetName().Version?.ToString(3) ?? "0.0.0"}" });
        aboutPage.Children.Add(new TextBlock { Text = "A native viewer for Reolink NVRs and cameras.", Opacity = 0.8 });
        aboutPage.Children.Add(new TextBlock { Text = "Licensed MIT OR Apache-2.0. Press F1 in a viewer window for the keyboard shortcuts.", Opacity = 0.8, TextWrapping = TextWrapping.Wrap });

        result.Pages[SettingsSection.Devices] = devicesPage;
        result.Pages[SettingsSection.Cameras] = camerasPage;
        result.Pages[SettingsSection.AlertsAndCalls] = alertsPage;
        result.Pages[SettingsSection.Capture] = capturePage;
        result.Pages[SettingsSection.Updates] = updatesPage;
        result.Pages[SettingsSection.About] = aboutPage;

        Reload(viewer.Devices.FirstOrDefault()?.Config.Key);
        // The search runs while the Devices page shows the new-device form, and stops when either goes away.
        result._opened = () =>
        {
            opened = true;
            RefreshFinder();
            if (editing is null && onDevices)
            {
                StartFinding();
            }
        };
        result._shown = section =>
        {
            onDevices = section == SettingsSection.Devices;
            if (!opened)
            {
                return;
            }
            if (onDevices && editing is null)
            {
                StartFinding();
            }
            else if (!onDevices)
            {
                StopFinding();
            }
        };
        result._findDevices = () =>
        {
            NewDevice();
            if (!viewer.Discovery.IsSearching)
            {
                StartFinding();
            }
        };
        result._closed = () =>
        {
            opened = false;
            viewer.Changed -= refresh;
            viewer.Changed -= refreshMounts;
            viewer.Changed -= refreshCapture;
            viewer.Changed -= refreshCalls;
            StopFinding();
        };
        if (hooks is not null)
        {
            hooks.NewDevice = NewDevice;
            hooks.Choose = Choose;
            hooks.SearchAgain = StartFinding;
            hooks.Mounts = () => [.. mountRows.Select(r => (r.Camera, r.Label, (FisheyeMount)r.Picker.SelectedIndex))];
            hooks.ChooseMount = (camera, mount) =>
            {
                // The same path as a click on the picker's item.
                ComboBox picker = mountRows.First(r => r.Camera == camera).Picker;
                picker.SelectedIndex = (int)mount;
            };
            hooks.Capture = () => (folderText.Text, viewer.Settings.PictureFormat, shareSafe.IsChecked == true, resetFolder.IsEnabled);
            hooks.ChooseFormat = format => formatPicker.SelectedIndex = (int)format;
            hooks.SetShareSafe = on =>
            {
                shareSafe.IsChecked = on;
                viewer.Settings.ShareSafePictureNames = on;
            };
            hooks.ResetFolder = () => viewer.Settings.PicturesFolder = ViewSettings.DefaultPicturesFolder;
            hooks.Form = () => (name.Text, host.Text, port.Value);
            hooks.Panel = () => (
                finder.Visibility == Visibility.Visible,
                [.. found.Items.OfType<ListViewItem>().Select(i =>
                {
                    var d = (DiscoveredDevice)i.Tag;
                    return (d.Title, d.Detail, i.IsEnabled);
                })],
                findStatus.Text);
        }
        return result;
    }
}

/// <summary>A "Calls open in" choice as an item's tag (<see cref="Value"/> null is Default).</summary>
internal sealed record CallPresentationChoice(CallPresentation? Value);
