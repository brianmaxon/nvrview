// Export in the Recordings window (docs/UX.md "Recordings", "Export"): the
// Export menu beside the transport, the I and O keys, the progress strip with
// Cancel, and the range download sheet. The state and the commands are
// ExportController (NvrView.Shell, tested on Linux); the writers are
// ExportWriters (Media Foundation). This file only draws them.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NvrView.Shell;
using NvrView.Tiles;
using Windows.System;

namespace NvrView.App;

internal sealed partial class RecordingsWindow
{
    /// <summary>Where the export switches and the last folder are kept (null: not kept); set by the app at start.</summary>
    internal static string? ExportSettingsPath { get; set; }

    private readonly DropDownButton _exportButton = new() { Content = "Export" };
    private readonly Grid _exportStrip = new() { Visibility = Visibility.Collapsed, ColumnSpacing = 8, Margin = new Thickness(10, 0, 10, 0) };
    private readonly TextBlock _exportText = new() { VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis };
    private readonly ProgressBar _exportProgress = new() { Minimum = 0, Maximum = 1, VerticalAlignment = VerticalAlignment.Center };
    private readonly Button _exportCancel = new() { Content = "Cancel" };
    private ExportWriters? _exportWriters;

    /// <summary>The window's export writers; the follow and grid views set <see cref="ExportWriters.EditFrames"/> on them.</summary>
    internal ExportWriters Writers => _exportWriters!;

    /// <summary>The window's export, with the platform writers and pickers (the self-test replaces the pickers).</summary>
    private void AttachExport(CoreHost host)
    {
        _exportWriters = new ExportWriters(host);
        ExportController export = Browser.Export;
        export.Writers = _exportWriters;
        export.Dialogs = new ExportDialogs(() => Microsoft.UI.Win32Interop.GetWindowFromWindowId(AppWindow.Id));
        export.Changed += ScheduleRefresh;
    }

    /// <summary>The export menu button and the progress strip, for the transport.</summary>
    private void BuildExport(StackPanel more, StackPanel transport)
    {
        ToolTipService.SetToolTip(_exportButton, "Trim, download a range, or save pictures (I and O set the in and out points)");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_exportButton, "Export");
        var menu = new MenuFlyout();
        menu.Opening += (_, _) => FillExportMenu(menu);
        _exportButton.Flyout = menu;
        more.Children.Add(_exportButton);

        _exportStrip.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        _exportStrip.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(160) });
        _exportStrip.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(_exportProgress, 1);
        Grid.SetColumn(_exportCancel, 2);
        _exportStrip.Children.Add(_exportText);
        _exportStrip.Children.Add(_exportProgress);
        _exportStrip.Children.Add(_exportCancel);
        _exportCancel.Click += (_, _) =>
        {
            if (Browser.Export.Job is { Running: true } job)
            {
                job.Cancel();
            }
            else
            {
                Browser.Export.DismissJob();
            }
        };
        transport.Children.Add(_exportStrip);

        // I and O set the points, except while typing.
        AddPointKey(VirtualKey.I, () => Browser.Export.MarkIn());
        AddPointKey(VirtualKey.O, () => Browser.Export.MarkOut());

        _scrub.PointMoved += (handle, ms) => Browser.Export.MovePoint(handle, ms);
    }

    private void AddPointKey(VirtualKey key, Action action)
    {
        var accelerator = new KeyboardAccelerator { Key = key, ScopeOwner = _root };
        accelerator.Invoked += (_, e) =>
        {
            if (Microsoft.UI.Xaml.Input.FocusManager.GetFocusedElement(_root.XamlRoot) is TextBox or PasswordBox or RichEditBox or AutoSuggestBox)
            {
                return;
            }
            action();
            e.Handled = true;
        };
        _root.KeyboardAccelerators.Add(accelerator);
    }

    private MenuFlyoutItem Item(string text, Func<Task> run, bool enabled = true, string? tip = null)
    {
        var item = new MenuFlyoutItem { Text = text, IsEnabled = enabled };
        if (tip is not null)
        {
            ToolTipService.SetToolTip(item, tip);
        }
        item.Click += async (_, _) =>
        {
            try
            {
                await run();
            }
            catch (Exception e) when (e is IOException or InvalidOperationException or UnauthorizedAccessException)
            {
                AppLog.Warn($"an export command failed: {e.GetType().Name}");
                Browser.Message = "The export failed";
            }
            RefreshNotice();
        };
        return item;
    }

    private MenuFlyoutItem Item(string text, Action run, bool enabled = true, string? tip = null) => Item(text, () =>
    {
        run();
        return Task.CompletedTask;
    }, enabled, tip);

    private void FillExportMenu(MenuFlyout menu)
    {
        ExportController export = Browser.Export;
        menu.Items.Clear();
        bool open = Browser.Player.Playback is not null;
        bool span = export.ScrubSpan is not null;
        bool window = export.PointsWindow is not null;
        bool idle = !export.Busy;
        menu.Items.Add(Item("Set In Point    I", () => export.MarkIn(), span));
        menu.Items.Add(Item("Set Out Point    O", () => export.MarkOut(), span));
        menu.Items.Add(Item("Clear In and Out", () => export.ClearPoints(), !export.Points.IsEmpty));
        menu.Items.Add(new MenuFlyoutSeparator());
        menu.Items.Add(Item("Trim to In and Out", () => export.TrimAsync(null), window && idle, "All the cameras playing, as one file each"));
        var one = new MenuFlyoutSubItem { Text = "Trim One Camera", IsEnabled = window && idle };
        foreach (PlayerCell cell in Browser.Player.Cells)
        {
            RecordingCamera camera = cell.Camera;
            one.Items.Add(Item(Service.CameraName(camera), () => export.TrimAsync([camera])));
        }
        menu.Items.Add(one);
        var exact = new ToggleMenuFlyoutItem { Text = "Exact Trim (re-encodes)", IsChecked = export.TrimExact };
        ToolTipService.SetToolTip(exact, "Start at the picture asked for instead of the keyframe before it; slower, and the video is encoded again");
        exact.Click += (_, _) => export.TrimExact = exact.IsChecked;
        menu.Items.Add(exact);
        if (_exportWriters?.CanExportEdit == true)
        {
            menu.Items.Add(new MenuFlyoutSeparator());
            menu.Items.Add(Item("Export Follow Cut", () => export.ExportEditAsync(null), idle));
            menu.Items.Add(Item("Export 2-Up", () => export.ExportEditAsync("2up"), idle));
            menu.Items.Add(Item("Export 4-Up", () => export.ExportEditAsync("4up"), idle));
            var h265 = new ToggleMenuFlyoutItem { Text = "H.265 for Follow Cut and Grids", IsChecked = export.Codec == ExportCodec.Hevc };
            h265.Click += (_, _) => export.Codec = h265.IsChecked ? ExportCodec.Hevc : ExportCodec.H264;
            menu.Items.Add(h265);
        }
        menu.Items.Add(new MenuFlyoutSeparator());
        menu.Items.Add(Item("Download Range…", async () => await ShowRangeSheetAsync(), span));
        menu.Items.Add(new MenuFlyoutSeparator());
        menu.Items.Add(Item("Save Picture", () => SaveStills(false), open && idle));
        menu.Items.Add(Item("Save a Picture of Each Camera", () => SaveStills(true), open && Browser.Player.Cells.Count > 1 && idle));
        menu.Items.Add(new MenuFlyoutSeparator());
        var safe = new ToggleMenuFlyoutItem { Text = "Share-Safe", IsChecked = export.ShareSafe };
        ToolTipService.SetToolTip(safe, "Neutral camera names, no incident name, and no creation times or other metadata in what is written");
        safe.Click += (_, _) => export.ShareSafe = safe.IsChecked;
        menu.Items.Add(safe);
    }

    private Task SaveStills(bool each)
    {
        Browser.Export.StillFormat = _viewer.Settings.PictureFormat;
        return Browser.Export.SaveStillsAsync(each);
    }

    // ---- refreshing ----

    private void RefreshExport()
    {
        ExportController export = Browser.Export;
        InOutPoints points = export.Points;
        _scrub.SetPoints(points);
        _lanes.SetPoints(points);
        ExportJob? job = export.Job;
        _exportStrip.Visibility = job is null ? Visibility.Collapsed : Visibility.Visible;
        if (job is not null)
        {
            _exportText.Text = job.Running ? job.Title + "…" : job.Result ?? "";
            _exportProgress.Value = job.Fraction;
            _exportProgress.Visibility = job.Running ? Visibility.Visible : Visibility.Collapsed;
            _exportCancel.Content = job.Running ? "Cancel" : "Dismiss";
        }
        _exportButton.IsEnabled = Browser.Player.Playback is not null || !points.IsEmpty;
    }

    // ---- the range sheet ----

    private async Task ShowRangeSheetAsync()
    {
        ExportController export = Browser.Export;
        export.BeginRangeDownload();
        if (export.RangeRequest is not { } request)
        {
            return;
        }
        var body = new StackPanel { Spacing = 8, MinWidth = 380 };
        body.Children.Add(new TextBlock
        {
            Text = $"{TransportState.Clock(request.StartMs)} to {TransportState.Clock(request.EndMs)}",
            FontFamily = new FontFamily("Consolas"),
        });
        var boxes = new Dictionary<RecordingCamera, CheckBox>();
        foreach (RecordingCamera camera in Browser.Cameras)
        {
            var box = new CheckBox { Content = Service.CameraName(camera), IsChecked = request.Cameras.Contains(camera) };
            boxes[camera] = box;
            body.Children.Add(box);
        }
        var name = new TextBox { Header = "Name (optional)", Text = request.Name };
        body.Children.Add(name);
        var folder = new TextBlock { Text = "In the downloads folder", Foreground = new SolidColorBrush(Muted), TextTrimming = TextTrimming.CharacterEllipsis };
        var choose = new Button { Content = "Choose Another Folder…" };
        choose.Click += async (_, _) =>
        {
            if (export.Dialogs is { } dialogs && await dialogs.ChooseFolder("Choose where to save the download", export.Folder) is { } chosen)
            {
                request.Folder = chosen;
                folder.Text = chosen;
            }
        };
        body.Children.Add(choose);
        body.Children.Add(folder);
        var dialog = new ContentDialog
        {
            Title = "Download Range",
            Content = new ScrollViewer { Content = body, MaxHeight = 460 },
            PrimaryButtonText = "Download",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = _root.XamlRoot,
            RequestedTheme = ElementTheme.Dark,
        };
        if (await dialog.ShowAsync() == ContentDialogResult.Primary)
        {
            request.Cameras = [.. boxes.Where(b => b.Value.IsChecked == true).Select(b => b.Key)];
            request.Name = name.Text;
            export.DownloadRange(request);
            Refresh();
        }
        else
        {
            export.RangeRequest = null;
        }
    }

    // ---- self-test ----

    /// <summary>The export strip as drawn (self-test): its text and whether it is shown.</summary>
    public (bool Shown, string Text) ExportStripShown => (_exportStrip.Visibility == Visibility.Visible, _exportText.Text);

    /// <summary>The scrubber's in and out points as drawn (self-test).</summary>
    public (long? In, long? Out) ScrubPointsShown => _scrub.PointsShown;
}
