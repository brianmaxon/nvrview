// The Export tab in the Recordings window's right pane (docs/UX.md "Recordings", "Export tab"): what to export (the
// incident or the in and out range, with editable times), the cameras, the kind, the stream, the options, and the
// Export button with its progress. The decisions are NvrView.Shell's ExportPanel rules and ExportController (tested on
// Linux, the same vectors as the Mac app's); this file only draws them. The controls are built once and updated, so a
// refresh never takes the focus from a box being typed in.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using NvrView.Shell;
using Windows.System;

namespace NvrView.App;

internal sealed partial class RecordingsWindow
{
    private readonly ToggleButton _tabIncident = new() { Content = "Incident" };
    private readonly ToggleButton _tabExport = new() { Content = "Export" };
    private readonly ScrollViewer _incidentScroll = new() { VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled };
    private readonly ScrollViewer _exportScroll = new() { VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled, Visibility = Visibility.Collapsed };

    private readonly StackPanel _tabBody = new() { Spacing = 14, Margin = new Thickness(12) };
    private readonly StackPanel _tabEmpty = new() { Spacing = 6 };
    private readonly StackPanel _tabContent = new() { Spacing = 14 };
    private readonly TextBlock _tabWhat = new() { FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap };
    private readonly StackPanel _tabTimes = new() { Spacing = 6 };
    private readonly TextBox _tabStart = new() { Header = "Start", FontFamily = new FontFamily("Consolas"), MinWidth = 110 };
    private readonly TextBox _tabEnd = new() { Header = "End", FontFamily = new FontFamily("Consolas"), MinWidth = 110 };
    private readonly HyperlinkButton _tabWhole = new() { Content = "Whole Range", Padding = new Thickness(0) };
    private readonly TextBlock _tabSummary = new() { Foreground = new SolidColorBrush(Muted), TextWrapping = TextWrapping.Wrap };
    private readonly StackPanel _tabCamerasBox = new() { Spacing = 4 };
    private readonly StackPanel _tabCameras = new() { Spacing = 2 };
    private readonly StackPanel _tabKinds = new() { Spacing = 2 };
    private readonly TextBlock _tabKindNote = new() { FontSize = 12, Foreground = new SolidColorBrush(Muted), TextWrapping = TextWrapping.Wrap };
    private readonly StackPanel _tabQualityBox = new() { Spacing = 4 };
    private readonly RadioButton _tabMain = new() { Content = "Main Stream", GroupName = "exportquality" };
    private readonly RadioButton _tabSub = new() { Content = "Sub-stream", GroupName = "exportquality" };
    private readonly TextBlock _tabQualityNote = new() { FontSize = 12, Foreground = new SolidColorBrush(Muted), TextWrapping = TextWrapping.Wrap };
    private readonly CheckBox _tabShareSafe = new() { Content = "Share-safe (no names, times or metadata)" };
    private readonly CheckBox _tabSound = new() { Content = "Sound" };
    private readonly CheckBox _tabH265 = new() { Content = "H.265 video (needs the computer's encoder)" };
    private readonly TextBlock _tabFolder = new() { FontSize = 12, Foreground = new SolidColorBrush(Muted), TextTrimming = TextTrimming.CharacterEllipsis, VerticalAlignment = VerticalAlignment.Center };
    private readonly Button _tabChoose = new() { Content = "Choose…" };
    private readonly Button _tabAsk = new() { Content = "Ask Each Time" };
    private readonly Button _tabGo = new() { Content = "Export" };
    private readonly TextBlock _tabProblem = new() { FontSize = 12, Foreground = new SolidColorBrush(Muted), TextWrapping = TextWrapping.Wrap };
    private readonly StackPanel _tabJob = new() { Spacing = 4, Visibility = Visibility.Collapsed };
    private readonly TextBlock _tabJobText = new() { FontSize = 12, TextWrapping = TextWrapping.Wrap };
    private readonly ProgressBar _tabJobProgress = new() { Minimum = 0, Maximum = 1 };
    private readonly StackPanel _tabJobButtons = new() { Orientation = Orientation.Horizontal, Spacing = 8 };
    private readonly Button _tabJobCancel = new() { Content = "Cancel" };
    private readonly Button _tabJobShow = new() { Content = "Show in Explorer" };

    private bool _syncingTab;
    private string _tabCamerasSignature = "";
    private string _tabKindsSignature = "";

    /// <summary>The right pane: the Incident and Export tabs over their content.</summary>
    private void BuildInspectorPane()
    {
        _incidentScroll.Content = _inspectorBody;
        BuildExportTab();

        var tabs = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, Margin = new Thickness(12, 8, 12, 8) };
        tabs.Children.Add(_tabIncident);
        tabs.Children.Add(_tabExport);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_tabIncident, "Incident tab");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_tabExport, "Export tab");
        ToolTipService.SetToolTip(_tabExport, "Export (Ctrl+E)");
        _tabIncident.Click += (_, _) => ShowInspectorTab(InspectorTab.Incident);
        _tabExport.Click += (_, _) => ShowInspectorTab(InspectorTab.Export);

        var pane = new Grid();
        pane.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        pane.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        Grid.SetRow(_incidentScroll, 1);
        Grid.SetRow(_exportScroll, 1);
        pane.Children.Add(tabs);
        pane.Children.Add(_incidentScroll);
        pane.Children.Add(_exportScroll);
        _inspectorHost.Child = pane;
        RefreshInspectorTabs();
    }

    private void ShowInspectorTab(InspectorTab tab)
    {
        Browser.InspectorTab = tab;
        RefreshInspectorTabs();
        RefreshExportTab();
    }

    /// <summary>Ctrl+E: shows the right pane on its Export tab (the window grows when it is too narrow for it).</summary>
    public void OpenExportTab()
    {
        bool wasShown = Browser.ShowInspector;
        Browser.OpenExportTab();
        if (!wasShown)
        {
            double need = RecordingsPanes.CenterMin + (Browser.ShowSidebar ? RecordingsPanes.SidebarWidth : 0) + RecordingsPanes.InspectorWidth;
            if (need > _width)
            {
                Grow(need);
            }
        }
        ApplyPanes();
        RefreshInspectorTabs();
        RefreshExportTab();
    }

    private void RefreshInspectorTabs()
    {
        bool export = Browser.InspectorTab == InspectorTab.Export;
        _tabIncident.IsChecked = !export;
        _tabExport.IsChecked = export;
        _incidentScroll.Visibility = export ? Visibility.Collapsed : Visibility.Visible;
        _exportScroll.Visibility = export ? Visibility.Visible : Visibility.Collapsed;
    }

    private static TextBlock TabHeading(string text) =>
        new() { Text = text.ToUpperInvariant(), FontSize = 11, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Foreground = new SolidColorBrush(Muted) };

    private void BuildExportTab()
    {
        ExportController export = Browser.Export;
        _exportScroll.Content = _tabBody;

        _tabEmpty.Children.Add(new TextBlock { Text = "Nothing to Export", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
        _tabEmpty.Children.Add(new TextBlock
        {
            Text = "Play an incident or a camera, then set the range with I and O.",
            Foreground = new SolidColorBrush(Muted),
            TextWrapping = TextWrapping.Wrap,
        });

        // What
        var what = new StackPanel { Spacing = 6 };
        what.Children.Add(TabHeading("What"));
        what.Children.Add(_tabWhat);
        var times = new Grid { ColumnSpacing = 8 };
        times.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        times.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(_tabEnd, 1);
        times.Children.Add(_tabStart);
        times.Children.Add(_tabEnd);
        _tabTimes.Children.Add(times);
        _tabTimes.Children.Add(_tabWhole);
        ToolTipService.SetToolTip(_tabStart, "Edit the times, or set them with I and O or the flags on the scrubber");
        ToolTipService.SetToolTip(_tabEnd, "Edit the times, or set them with I and O or the flags on the scrubber");
        what.Children.Add(_tabTimes);
        what.Children.Add(_tabSummary);
        _tabStart.KeyDown += (_, e) => CommitTime(e, start: true);
        _tabEnd.KeyDown += (_, e) => CommitTime(e, start: false);
        _tabStart.LostFocus += (_, _) => CommitTime(null, start: true);
        _tabEnd.LostFocus += (_, _) => CommitTime(null, start: false);
        _tabWhole.Click += (_, _) => export.ClearPoints();

        // Cameras
        _tabCamerasBox.Children.Add(TabHeading("Cameras"));
        _tabCamerasBox.Children.Add(_tabCameras);

        // Kind
        var kind = new StackPanel { Spacing = 4 };
        kind.Children.Add(TabHeading("Kind"));
        kind.Children.Add(_tabKinds);
        kind.Children.Add(_tabKindNote);

        // Quality
        _tabQualityBox.Children.Add(TabHeading("Quality"));
        _tabQualityBox.Children.Add(_tabMain);
        _tabQualityBox.Children.Add(_tabSub);
        _tabQualityBox.Children.Add(_tabQualityNote);
        _tabMain.Checked += (_, _) => ChooseQuality(ExportQuality.Main);
        _tabSub.Checked += (_, _) => ChooseQuality(ExportQuality.Sub);

        // Options
        var options = new StackPanel { Spacing = 6 };
        options.Children.Add(TabHeading("Options"));
        ToolTipService.SetToolTip(_tabShareSafe, "Neutral camera names, no incident name, and no creation times or other metadata in what is written");
        _tabShareSafe.Click += (_, _) => export.ShareSafe = _tabShareSafe.IsChecked == true;
        _tabSound.Click += (_, _) => export.Sound = _tabSound.IsChecked == true;
        _tabH265.Click += (_, _) => export.Codec = _tabH265.IsChecked == true ? ExportCodec.Hevc : ExportCodec.H264;
        options.Children.Add(_tabShareSafe);
        options.Children.Add(_tabSound);
        options.Children.Add(_tabH265);
        var folder = new Grid { ColumnSpacing = 8 };
        folder.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        folder.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        folder.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(_tabChoose, 1);
        Grid.SetColumn(_tabAsk, 2);
        folder.Children.Add(_tabFolder);
        folder.Children.Add(_tabChoose);
        folder.Children.Add(_tabAsk);
        options.Children.Add(folder);
        _tabChoose.Click += async (_, _) =>
        {
            if (export.Dialogs is { } dialogs && await dialogs.ChooseFolder("Choose where exports are saved", export.Folder) is { } chosen)
            {
                export.ChooseFolder(chosen);
            }
        };
        _tabAsk.Click += (_, _) => export.AskEachTime();

        // The button and the job
        var go = new StackPanel { Spacing = 4 };
        go.Children.Add(_tabGo);
        go.Children.Add(_tabProblem);
        _tabGo.Click += async (_, _) =>
        {
            try
            {
                export.StillFormat = _viewer.Settings.PictureFormat;
                await export.StartAsync();
            }
            catch (Exception e) when (e is IOException or InvalidOperationException or UnauthorizedAccessException)
            {
                AppLog.Warn($"an export command failed: {e.GetType().Name}");
                Browser.Message = "The export failed";
            }
            RefreshNotice();
        };
        _tabJob.Children.Add(_tabJobText);
        _tabJob.Children.Add(_tabJobProgress);
        _tabJobButtons.Children.Add(_tabJobCancel);
        _tabJobButtons.Children.Add(_tabJobShow);
        _tabJob.Children.Add(_tabJobButtons);
        _tabJobCancel.Click += (_, _) =>
        {
            if (export.Job is { Running: true } job)
            {
                job.Cancel();
            }
            else
            {
                export.DismissJob();
            }
        };
        _tabJobShow.Click += (_, _) =>
        {
            if (export.Job is { Running: false, Files.Count: > 0 } job)
            {
                SystemActions.ShowInExplorer(job.Files);
            }
        };
        var downloads = new HyperlinkButton { Content = "Downloads…", Padding = new Thickness(0) };
        ToolTipService.SetToolTip(downloads, "The history of downloads, with their progress");
        downloads.Click += (_, _) =>
        {
            Browser.Center = RecordingsCenter.Downloads;
            Refresh();
        };

        _tabContent.Children.Add(what);
        _tabContent.Children.Add(_tabCamerasBox);
        _tabContent.Children.Add(kind);
        _tabContent.Children.Add(_tabQualityBox);
        _tabContent.Children.Add(options);
        _tabContent.Children.Add(go);
        _tabBody.Children.Add(_tabEmpty);
        _tabBody.Children.Add(_tabContent);
        _tabBody.Children.Add(_tabJob);
        _tabBody.Children.Add(downloads);
    }

    private void ChooseQuality(ExportQuality quality)
    {
        if (!_syncingTab)
        {
            Browser.Export.QualityChoice = quality;
        }
    }

    // A time typed in a box: Enter or leaving the box sets the point; text that is not a time inside the range goes back.
    private void CommitTime(KeyRoutedEventArgs? key, bool start)
    {
        if (_syncingTab || (key is not null && key.Key != VirtualKey.Enter))
        {
            return;
        }
        ExportController export = Browser.Export;
        TextBox box = start ? _tabStart : _tabEnd;
        string current = start ? export.StartText : export.EndText;
        if (box.Text != current && !(start ? export.SetStart(box.Text) : export.SetEnd(box.Text)))
        {
            box.Text = current;
        }
        if (key is not null)
        {
            key.Handled = true;
        }
    }

    /// <summary>Draws the Export tab from the controller's state (a refresh; also called by the self-test).</summary>
    private void RefreshExportTab()
    {
        ExportController export = Browser.Export;
        bool shown = Browser.InspectorTab == InspectorTab.Export;
        if (!shown)
        {
            return;
        }
        _syncingTab = true;
        try
        {
            IReadOnlyList<ExportKind> kinds = export.Kinds;
            ExportKind? kind = export.Kind;
            bool any = kind is not null;
            _tabEmpty.Visibility = any ? Visibility.Collapsed : Visibility.Visible;
            _tabContent.Visibility = any ? Visibility.Visible : Visibility.Collapsed;
            if (kind is { } k)
            {
                _tabWhat.Text = Browser.PlayingIncident?.Title ?? "The range playing";
                _tabTimes.Visibility = k.IsStill() ? Visibility.Collapsed : Visibility.Visible;
                SetText(_tabStart, export.StartText);
                SetText(_tabEnd, export.EndText);
                _tabWhole.Visibility = export.PointsWindow is null ? Visibility.Collapsed : Visibility.Visible;
                _tabSummary.Text = export.Summary;

                bool cameras = ExportPanel.UsesCameras(k);
                _tabCamerasBox.Visibility = cameras ? Visibility.Visible : Visibility.Collapsed;
                FillCameras(export);
                FillKinds(kinds, k);
                _tabKindNote.Text = k switch
                {
                    ExportKind.TrimExact => "Starts at the exact picture; the video is encoded again, so it takes longer.",
                    ExportKind.Download => "The device's files for the range, as they are.",
                    _ => "",
                };
                _tabKindNote.Visibility = _tabKindNote.Text.Length > 0 ? Visibility.Visible : Visibility.Collapsed;

                bool quality = ExportPanel.UsesQuality(k, Browser.Editable is not null);
                _tabQualityBox.Visibility = quality ? Visibility.Visible : Visibility.Collapsed;
                _tabMain.IsChecked = export.Quality == ExportQuality.Main;
                _tabSub.IsChecked = export.Quality == ExportQuality.Sub;
                _tabQualityNote.Text = export.Quality.Detail();

                _tabShareSafe.IsChecked = export.ShareSafe;
                _tabSound.Visibility = ExportPanel.UsesSound(k) ? Visibility.Visible : Visibility.Collapsed;
                _tabH265.Visibility = _tabSound.Visibility;
                _tabSound.IsChecked = export.Sound;
                _tabH265.IsChecked = export.Codec == ExportCodec.Hevc;
                _tabFolder.Text = export.DirectFolder ?? "Ask where to save each time";
                ToolTipService.SetToolTip(_tabFolder, export.DirectFolder);
                _tabAsk.Visibility = export.Direct ? Visibility.Visible : Visibility.Collapsed;

                _tabGo.Content = k.ButtonTitle();
                _tabGo.IsEnabled = !export.Busy;
                string? problem = export.Busy ? null : export.Problem;
                _tabProblem.Text = problem ?? "";
                _tabProblem.Visibility = problem is null ? Visibility.Collapsed : Visibility.Visible;
            }
            RefreshExportJob();
        }
        finally
        {
            _syncingTab = false;
        }
    }

    private static void SetText(TextBox box, string text)
    {
        if (box.FocusState == FocusState.Unfocused && box.Text != text)
        {
            box.Text = text;
        }
    }

    // The camera checklist is rebuilt only when the cameras change; the checks are set in place.
    private void FillCameras(ExportController export)
    {
        IReadOnlyList<RecordingCamera> source = export.SourceCameras;
        string signature = string.Join("|", source.Select(c => $"{c.Device}/{c.Channel}/{c.Lens}"));
        HashSet<RecordingCamera> chosen = export.Checked;
        if (signature != _tabCamerasSignature)
        {
            _tabCamerasSignature = signature;
            _tabCameras.Children.Clear();
            foreach (RecordingCamera camera in source)
            {
                RecordingCamera c = camera;
                var box = new CheckBox { Content = Service.CameraName(c), Tag = c };
                box.Click += (_, _) => Browser.Export.SetCamera(c, box.IsChecked == true);
                _tabCameras.Children.Add(box);
            }
        }
        foreach (CheckBox box in _tabCameras.Children.OfType<CheckBox>())
        {
            box.IsChecked = box.Tag is RecordingCamera c && chosen.Contains(c);
        }
    }

    // The kind list is rebuilt only when the kinds that apply change; the selection is set in place.
    private void FillKinds(IReadOnlyList<ExportKind> kinds, ExportKind selected)
    {
        string signature = string.Join("|", kinds.Select(k => k.Id()));
        if (signature != _tabKindsSignature)
        {
            _tabKindsSignature = signature;
            _tabKinds.Children.Clear();
            foreach (ExportKind kind in kinds)
            {
                ExportKind k = kind;
                var radio = new RadioButton { Content = k.Title(), GroupName = "exportkind", Tag = k };
                radio.Checked += (_, _) =>
                {
                    if (!_syncingTab)
                    {
                        Browser.Export.KindChoice = k;
                    }
                };
                _tabKinds.Children.Add(radio);
            }
        }
        foreach (RadioButton radio in _tabKinds.Children.OfType<RadioButton>())
        {
            radio.IsChecked = radio.Tag is ExportKind k && k == selected;
        }
    }

    // The running or just-ended export, in place: progress and Cancel, then what was written with Show in Explorer.
    private void RefreshExportJob()
    {
        ExportJob? job = Browser.Export.Job;
        _tabJob.Visibility = job is null ? Visibility.Collapsed : Visibility.Visible;
        if (job is null)
        {
            return;
        }
        _tabJobText.Text = job.Running ? job.Title + "…" : job.Result ?? "";
        _tabJobProgress.Value = job.Fraction;
        _tabJobProgress.Visibility = job.Running ? Visibility.Visible : Visibility.Collapsed;
        _tabJobCancel.Content = job.Running ? "Cancel" : "Dismiss";
        _tabJobShow.Visibility = !job.Running && job.Files.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    // ---- self-test ----

    /// <summary>Whether the Export tab is the one shown, in a pane that is shown (self-test).</summary>
    public bool ExportTabShown => _shown.Inspector && _exportScroll.Visibility == Visibility.Visible;

    /// <summary>Whether Ctrl+E is registered as the Export tab's shortcut (self-test).</summary>
    public bool ExportShortcutRegistered => _root.KeyboardAccelerators.Any(a => a.Key == VirtualKey.E && a.Modifiers == VirtualKeyModifiers.Control);

    /// <summary>The kinds the tab lists, by id, and the time boxes as drawn (self-test).</summary>
    public (List<string> Kinds, string Start, string End, string Summary) ExportTabDrawn =>
        ([.. _tabKinds.Children.OfType<RadioButton>().Select(r => r.Tag is ExportKind k ? k.Id() : "")], _tabStart.Text, _tabEnd.Text, _tabSummary.Text);
}
