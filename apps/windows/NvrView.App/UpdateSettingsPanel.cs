// Settings, Updates (docs/UPDATES.md, "Settings and checks in the apps"):
// the source, the trusted keys, automatic checks, Check now, one line of
// status, and the folder this version runs from (the folder-switch habit).

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace NvrView.App;

internal static class UpdateSettingsPanel
{
    public static FrameworkElement Build(Updater updater)
    {
        var panel = new StackPanel { Spacing = 8 };
        panel.Children.Add(new TextBlock { Text = "Updates", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });

        var source = new TextBox
        {
            Header = "Update source (a folder or network share, https://, or http:// on your local network)",
            PlaceholderText = @"\\server\share\nvrview or https://…",
            Text = updater.Settings.Source,
            MinWidth = 420,
        };
        var sourceNote = new TextBlock { TextWrapping = TextWrapping.Wrap, MaxWidth = 520 };
        source.LostFocus += (_, _) => sourceNote.Text = updater.SetSource(source.Text) ?? "";
        panel.Children.Add(source);
        panel.Children.Add(sourceNote);

        var keys = new StackPanel { Spacing = 4 };
        var keyText = new TextBox { Header = "Trusted keys", PlaceholderText = "nvrview-ed25519-pub …", MinWidth = 420 };
        var keyNote = new TextBlock { TextWrapping = TextWrapping.Wrap, MaxWidth = 520, Text = "Updates are installed only when signed by one of these keys." };
        var addKey = new Button { Content = "Add key" };
        addKey.Click += (_, _) =>
        {
            string? problem = updater.AddKey(keyText.Text);
            keyNote.Text = problem ?? "Updates are installed only when signed by one of these keys.";
            if (problem is null)
            {
                keyText.Text = "";
            }
        };
        var keyRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        keyRow.Children.Add(keyText);
        keyRow.Children.Add(addKey);
        panel.Children.Add(keyRow);
        panel.Children.Add(keys);
        panel.Children.Add(keyNote);

        var automatic = new CheckBox { Content = "Check automatically", IsChecked = updater.Settings.Automatic };
        automatic.Checked += (_, _) => updater.SetAutomatic(true);
        automatic.Unchecked += (_, _) => updater.SetAutomatic(false);
        panel.Children.Add(automatic);

        var status = new TextBlock { TextWrapping = TextWrapping.Wrap, MaxWidth = 520 };
        var check = new Button { Content = "Check now" };
        check.Click += (_, _) =>
        {
            sourceNote.Text = updater.SetSource(source.Text) ?? "";
            updater.Check(userAsked: true);
        };
        var statusRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        statusRow.Children.Add(check);
        statusRow.Children.Add(status);
        panel.Children.Add(statusRow);
        panel.Children.Add(new TextBlock
        {
            Text = $"This version runs from {updater.AppDir}",
            TextWrapping = TextWrapping.Wrap,
            MaxWidth = 520,
            Opacity = 0.7,
        });

        void Refresh()
        {
            keys.Children.Clear();
            foreach (string key in updater.Settings.Keys.ToArray())
            {
                var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
                row.Children.Add(new TextBlock { Text = $"Key {Updater.KeyId(key)}", VerticalAlignment = VerticalAlignment.Center });
                var remove = new Button { Content = "Remove" };
                remove.Click += (_, _) => updater.RemoveKey(key);
                row.Children.Add(remove);
                keys.Children.Add(row);
            }
            status.Text = updater.Status.Line(updater.Current);
            check.IsEnabled = updater.Settings.CanCheck || source.Text.Trim().Length > 0;
        }
        Refresh();
        updater.Changed += Refresh;
        panel.Unloaded += (_, _) => updater.Changed -= Refresh;
        return panel;
    }
}
