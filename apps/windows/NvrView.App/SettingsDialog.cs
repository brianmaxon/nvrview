// Settings: the devices to connect to (address, port, user, password). The
// list and the form share one dialog; saving reconnects that device. The
// password goes straight to the credential store and is never shown again:
// leaving the box empty when editing keeps the saved one. The corner of the
// tiles' detection symbols is chosen here too.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NvrView.Tiles;

namespace NvrView.App;

internal static class SettingsDialog
{
    public static async Task ShowAsync(XamlRoot root, Viewer viewer, bool development) =>
        await Create(root, viewer, development).ShowAsync();

    /// <summary>The dialog, not shown yet.</summary>
    public static ContentDialog Create(XamlRoot root, Viewer viewer, bool development)
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

        void Fill(DeviceConfig? c)
        {
            editing = c;
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
        add.Click += (_, _) =>
        {
            list.SelectedIndex = -1;
            Fill(null);
        };
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
        return new ContentDialog
        {
            XamlRoot = root,
            Title = "Devices",
            Content = layout,
            CloseButtonText = "Done",
            DefaultButton = ContentDialogButton.Close,
        };
    }
}
