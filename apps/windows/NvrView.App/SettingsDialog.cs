// Settings: the devices to connect to (address, port, user, password). The
// list and the form share one dialog; saving reconnects that device. The
// password goes straight to the credential store and is never shown again:
// leaving the box empty when editing keeps the saved one. The corner of the
// tiles' detection symbols is chosen here too. The new-device form opens
// with "Find Devices on This Network": a search of the local network starts
// when the form is shown and stops when it goes away (docs/UX.md, "Finding
// devices"); choosing a row fills the form's address and port.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NvrView.Shell;
using NvrView.Tiles;

namespace NvrView.App;

/// <summary>What the self-test drives and reads: the same actions a click does.</summary>
internal sealed class SettingsDialogHooks
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
}

internal static class SettingsDialog
{
    public static async Task ShowAsync(XamlRoot root, Viewer viewer, bool development) =>
        await Create(root, viewer, development).ShowAsync();

    /// <summary>The dialog, not shown yet.</summary>
    public static ContentDialog Create(XamlRoot root, Viewer viewer, bool development, SettingsDialogHooks? hooks = null)
    {
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
            if (opened)
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

        var layout = new Grid { ColumnSpacing = 16, RowSpacing = 12 };
        layout.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        layout.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        layout.RowDefinitions.Add(new RowDefinition());
        layout.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        layout.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        layout.Children.Add(left);
        Grid.SetColumn(form, 1);
        layout.Children.Add(form);
        Grid.SetRow(corner, 1);
        Grid.SetColumnSpan(corner, 2);
        layout.Children.Add(corner);
        Grid.SetRow(note, 2);
        Grid.SetColumnSpan(note, 2);
        layout.Children.Add(note);

        Reload(viewer.Devices.FirstOrDefault()?.Config.Key);
        var dialog = new ContentDialog
        {
            XamlRoot = root,
            Title = "Devices",
            Content = layout,
            CloseButtonText = "Done",
            DefaultButton = ContentDialogButton.Close,
        };
        // The search starts when the form with no device selected is on
        // screen, and ends when the dialog closes.
        dialog.Opened += (_, _) =>
        {
            opened = true;
            RefreshFinder();
            if (editing is null)
            {
                StartFinding();
            }
        };
        dialog.Closed += (_, _) =>
        {
            opened = false;
            viewer.Changed -= refresh;
            StopFinding();
        };
        if (hooks is not null)
        {
            hooks.NewDevice = NewDevice;
            hooks.Choose = Choose;
            hooks.SearchAgain = StartFinding;
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
        return dialog;
    }
}
