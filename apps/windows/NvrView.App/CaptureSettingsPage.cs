// Settings > Capture > Recordings (docs/UX.md "Recording"): where recordings go, "Record from", and when a
// recording stops by itself (share-safe names are the pictures' box above it); and the "Record calls" setting of Alerts & calls with
// its consent note (docs/UX.md "Recording calls"). Every change is kept in view-settings.json and sent to
// the core at once (CaptureController); it applies to recordings started afterwards.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using NvrView.Shell;

namespace NvrView.App;

internal static class CaptureSettingsPage
{
    /// <summary>The recordings' part of the Capture section's page.</summary>
    /// <param name="viewer">The settings and the capture controller.</param>
    /// <param name="windowHandle">The Settings window's handle, for the folder picker.</param>
    public static StackPanel Recordings(Viewer viewer, Func<nint> windowHandle)
    {
        ViewSettings settings = viewer.Settings;
        var page = new StackPanel { Spacing = 12 };
        page.Children.Add(new TextBlock { Text = "Recordings", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });

        // Save recordings to: Choose… and Reset.
        var folder = new TextBlock { VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis, MaxWidth = 380, Opacity = 0.8 };
        var choose = new Button { Content = "Choose…" };
        var reset = new Button { Content = "Reset" };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(choose, "Choose the recordings folder");
        void ShowFolder()
        {
            folder.Text = settings.RecordingsFolder;
            ToolTipService.SetToolTip(folder, settings.RecordingsFolder);
            reset.IsEnabled = settings.RecordingsFolder != ViewSettings.DefaultRecordingsFolder;
        }
        choose.Click += async (_, _) =>
        {
            if (await PickFolderAsync(windowHandle(), settings.RecordingsFolder) is { } chosen)
            {
                settings.RecordingsFolder = chosen;
                ShowFolder();
            }
        };
        reset.Click += (_, _) =>
        {
            settings.RecordingsFolder = ViewSettings.DefaultRecordingsFolder;
            ShowFolder();
        };
        var where = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        where.Children.Add(new TextBlock { Text = "Save recordings to", VerticalAlignment = VerticalAlignment.Center });
        where.Children.Add(folder);
        where.Children.Add(choose);
        where.Children.Add(reset);
        ShowFolder();
        page.Children.Add(where);

        // Record from: Main stream or Sub-stream.
        var from = new ComboBox { Header = "Record from", MinWidth = 200 };
        foreach (RecordQuality q in Enum.GetValues<RecordQuality>())
        {
            from.Items.Add(new ComboBoxItem { Content = q.Title(), Tag = q });
        }
        from.SelectedIndex = (int)settings.RecordFrom;
        from.SelectionChanged += (_, _) =>
        {
            if (from.SelectedItem is ComboBoxItem { Tag: RecordQuality chosen })
            {
                settings.RecordFrom = chosen;
            }
        };
        page.Children.Add(from);

        // Stop after N minutes, or at N GB.
        var minutes = new NumberBox
        {
            Header = "Stop a recording after (minutes)", Value = settings.MaxRecordingMinutes, Minimum = CaptureSettings.MinMinutes,
            Maximum = CaptureSettings.MaxMinutes, SmallChange = 5, LargeChange = 30, SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Inline, Width = 200,
        };
        var minutesText = new TextBlock { VerticalAlignment = VerticalAlignment.Bottom, Opacity = 0.7 };
        var gigabytes = new NumberBox
        {
            Header = "or when it reaches (GB)", Value = settings.MaxRecordingGB, Minimum = CaptureSettings.MinBytes / 1e9,
            Maximum = CaptureSettings.MaxBytes / 1e9, SmallChange = 0.5, LargeChange = 4, SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Inline, Width = 200,
        };
        var gigabytesText = new TextBlock { VerticalAlignment = VerticalAlignment.Bottom, Opacity = 0.7 };
        void ShowLimits()
        {
            minutesText.Text = RecordingText.Minutes(settings.MaxRecordingMinutes);
            gigabytesText.Text = RecordingText.Size((ulong)Math.Round(settings.MaxRecordingGB * 1e9));
        }
        minutes.ValueChanged += (_, _) =>
        {
            if (double.IsFinite(minutes.Value))
            {
                settings.MaxRecordingMinutes = (int)Math.Round(minutes.Value);
                ShowLimits();
            }
        };
        gigabytes.ValueChanged += (_, _) =>
        {
            if (double.IsFinite(gigabytes.Value))
            {
                settings.MaxRecordingGB = gigabytes.Value;
                ShowLimits();
            }
        };
        ShowLimits();
        var limits = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        limits.Children.Add(minutes);
        limits.Children.Add(minutesText);
        limits.Children.Add(gigabytes);
        limits.Children.Add(gigabytesText);
        page.Children.Add(limits);

        var open = new Button { Content = "Open recordings folder" };
        open.Click += (_, _) => viewer.Capture.OpenFolder();
        page.Children.Add(open);
        page.Children.Add(new TextBlock
        {
            Text = "R, the tile's Record button or its menu records a camera's live stream on this PC as an MP4, from the next keyframe. \"Record from\" picks the stream; a tile's menu can record the other one. A recording keeps going when its tile closes, and stops by itself at these limits or when the disk is almost full. These are this PC's recordings, gaps included, not the recorder's own.",
            TextWrapping = TextWrapping.Wrap, MaxWidth = 560, Opacity = 0.7,
        });
        return page;
    }

    /// <summary>The folder the user picks (null if they cancel or no picker can open).</summary>
    private static async Task<string?> PickFolderAsync(nint hwnd, string current)
    {
        try
        {
            var picker = new Windows.Storage.Pickers.FolderPicker();
            picker.FileTypeFilter.Add("*");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, hwnd);
            Windows.Storage.StorageFolder? chosen = await picker.PickSingleFolderAsync();
            return chosen?.Path;
        }
        catch (Exception e) when (e is InvalidOperationException or System.Runtime.InteropServices.COMException or UnauthorizedAccessException)
        {
            AppLog.Warn($"the folder picker failed ({e.GetType().Name}); the folder stays {current}");
            return null;
        }
    }

    /// <summary>
    /// "Record calls" with its consent note, for Alerts &amp; calls (docs/UX.md "Recording calls"): off by default; the first time it
    /// is turned on, a dialog shows the note and the setting changes only on Record Calls. Also the button to open the folder.
    /// </summary>
    public static FrameworkElement RecordCalls(Viewer viewer)
    {
        ViewSettings settings = viewer.Settings;
        var box = new CheckBox { Content = "Record calls", IsChecked = settings.RecordCalls };
        box.Click += async (_, _) =>
        {
            bool on = box.IsChecked == true;
            if (on && !settings.RecordCallsConsentShown)
            {
                box.IsChecked = false;
                if (!await AskConsentAsync(box.XamlRoot))
                {
                    return;
                }
                settings.RecordCallsConsentShown = true;
                box.IsChecked = true;
            }
            settings.RecordCalls = box.IsChecked == true;
        };
        var open = new Button { Content = "Open call recordings folder" };
        open.Click += (_, _) => viewer.Transcripts.OpenFolder();
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 16 };
        row.Children.Add(box);
        row.Children.Add(open);
        var section = new StackPanel { Spacing = 6 };
        section.Children.Add(row);
        section.Children.Add(new TextBlock
        {
            Text = "A call is recorded from the moment it opens: the doorbell's video and three sound tracks, the conversation, the visitor, and you. The call's Record button (Ctrl+Shift+R) starts or stops a recording at any time, with this on or off. Recordings go beside the transcripts, with the same name (the newest 100, at most 10 GB in all, are kept).",
            TextWrapping = TextWrapping.Wrap, MaxWidth = 560, Opacity = 0.7,
        });
        return section;
    }

    private static async Task<bool> AskConsentAsync(XamlRoot? root)
    {
        if (root is null)
        {
            return false;
        }
        var dialog = new ContentDialog
        {
            XamlRoot = root,
            Title = "Record calls?",
            Content = new TextBlock { Text = RecordCallsText.Consent, TextWrapping = TextWrapping.Wrap, MaxWidth = 420 },
            PrimaryButtonText = "Record Calls",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        try
        {
            return await dialog.ShowAsync() == ContentDialogResult.Primary;
        }
        catch (Exception e) when (e is InvalidOperationException or System.Runtime.InteropServices.COMException)
        {
            // Another dialog is open: nothing changes; the user clicks again.
            return false;
        }
    }
}
