// Settings, Updates (docs/UPDATES.md, "Settings and checks in the apps"):
// the source, the trusted keys, automatic checks, Check now, one line of
// status, and the folder this version runs from (the folder-switch habit).

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NvrView.Shell;

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
        string BundledNote() => updater.Settings.SourceIsBundled
            ? $"Bundled default: {updater.Settings.EffectiveSource}. Type another to override it; clear the field to return to it."
            : "";
        var sourceNote = new TextBlock { TextWrapping = TextWrapping.Wrap, MaxWidth = 520, Text = BundledNote() };
        source.LostFocus += (_, _) => sourceNote.Text = updater.SetSource(source.Text) ?? BundledNote();
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
            sourceNote.Text = updater.SetSource(source.Text) ?? BundledNote();
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
            foreach (UpdateSettings.KeyRow keyRow in updater.Settings.KeyRows)
            {
                string key = keyRow.Key;
                var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
                row.Children.Add(new TextBlock { Text = $"Key {Updater.KeyId(key)}", VerticalAlignment = VerticalAlignment.Center });
                if (keyRow.IsBundled)
                {
                    row.Children.Add(new TextBlock { Text = keyRow.Note, Opacity = 0.7, VerticalAlignment = VerticalAlignment.Center });
                    var trust = new CheckBox { Content = "Trust", IsChecked = keyRow.IsOn };
                    trust.Click += (_, _) => updater.SetBundledKey(key, trust.IsChecked == true);
                    row.Children.Add(trust);
                }
                else
                {
                    var remove = new Button { Content = "Remove" };
                    remove.Click += (_, _) => updater.RemoveKey(key);
                    row.Children.Add(remove);
                }
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
