// The Recordings window's right pane and its downloads panel (docs/UX.md
// "Recordings"): the selected incident's name (Rename), time, badges and
// group, Locked, Download, Revert Edits, Split Here at the playhead with what
// it does in words, and each camera with its thumbnail and Remove from
// Incident, plus Add Camera; and the downloads (pause, resume, retry, show in
// Explorer, forget, Recycle Bin).

using Microsoft.UI;
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
    private readonly StackPanel _inspectorBody = new() { Spacing = 10, Margin = new Thickness(12) };
    private readonly Grid _downloadsPanel = new() { Visibility = Visibility.Collapsed };
    private readonly StackPanel _downloadsList = new() { Spacing = 10, Margin = new Thickness(14) };
    private string _inspectorSignature = "";
    private Button? _inspectorSplit;

    private void BuildInspector()
    {
        BuildInspectorPane();
    }

    private string InspectorSignature(Incident? i, int selected) => i is null
        ? $"none/{selected}"
        : $"{i.Key}/{i.Name}/{i.Locked}/{i.Edited}/{i.Start}/{i.End}/{i.Group}/{i.Downloads.Count}/{string.Join(",", i.Cameras.Select(c => c.Camera))}" +
          $"/{string.Join(",", i.CoverKeys.Select(k => Service.Thumbnails[k]?.GetType().Name))}/{string.Join(",", Browser.Addable(i))}";

    private void RefreshInspector()
    {
        Incident? incident = Browser.Selected;
        int selected = Browser.SelectedKeys.Count;
        string signature = InspectorSignature(incident, selected);
        if (signature == _inspectorSignature)
        {
            UpdateInspectorSplit();
            return;
        }
        _inspectorSignature = signature;
        _inspectorBody.Children.Clear();
        _inspectorSplit = null;
        if (incident is null)
        {
            _inspectorBody.Children.Add(new TextBlock
            {
                Text = selected >= 2 ? $"{selected} incidents selected." : "Select an incident to see its details.",
                Foreground = new SolidColorBrush(Muted),
                TextWrapping = TextWrapping.Wrap,
            });
            if (selected >= 2)
            {
                var merge = new Button { Content = "Merge" };
                merge.Click += (_, _) => Browser.Merge();
                _inspectorBody.Children.Add(merge);
            }
            return;
        }

        _inspectorBody.Children.Add(new TextBlock { Text = incident.Title, FontSize = 16, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap });
        var name = new TextBox { Text = incident.Name ?? "", PlaceholderText = "Name (empty: the time and kinds)", Tag = incident.Key };
        void Commit() => Browser.Rename(incident.Key, name.Text);
        name.KeyDown += (_, e) =>
        {
            if (e.Key == VirtualKey.Enter)
            {
                Commit();
                e.Handled = true;
            }
        };
        var rename = new Button { Content = "Rename" };
        rename.Click += (_, _) => Commit();
        var nameRow = new Grid { ColumnSpacing = 6 };
        nameRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        nameRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(rename, 1);
        nameRow.Children.Add(name);
        nameRow.Children.Add(rename);
        _inspectorBody.Children.Add(nameRow);

        _inspectorBody.Children.Add(Caption($"{DeviceTime.Span(incident.Start, incident.End)}  {DeviceTime.LongDay(incident.Start)}"));
        var kinds = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        foreach (string kind in incident.Kinds.Select(KindFilters.Indicator).Distinct().Select(k => incident.Kinds.First(x => KindFilters.Indicator(x) == k)))
        {
            kinds.Children.Add(new TextBlock { Text = KindFilters.Indicator(kind).Label(), FontSize = 12, Foreground = new SolidColorBrush(TimelineColors.Of(kind)) });
        }
        _inspectorBody.Children.Add(kinds);
        if (incident.Group is { } group)
        {
            _inspectorBody.Children.Add(Caption($"Camera group: {group}"));
        }

        var locked = new CheckBox { Content = "Locked", IsChecked = incident.Locked };
        locked.Click += (_, _) => Browser.SetLocked(incident.Key, locked.IsChecked == true);
        ToolTipService.SetToolTip(locked, "Locking keeps it: its video is downloaded and nothing about it expires");
        _inspectorBody.Children.Add(locked);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        var download = new Button { Content = "Download" };
        download.Click += (_, _) => Browser.Download(incident.Key);
        actions.Children.Add(download);
        if (incident.Edited)
        {
            var revert = new Button { Content = "Revert Edits" };
            revert.Click += (_, _) => Browser.Revert(incident.Key);
            actions.Children.Add(revert);
        }
        _inspectorBody.Children.Add(actions);

        _inspectorBody.Children.Add(new TextBlock { Text = "Split", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Margin = new Thickness(0, 6, 0, 0) });
        _inspectorSplit = new Button { Content = "Split Here" };
        _inspectorSplit.Click += (_, _) => Browser.SplitHere(incident);
        _inspectorBody.Children.Add(_inspectorSplit);
        _inspectorBody.Children.Add(Caption(
            "The incident becomes two at the playhead: what happened before stays, what follows becomes a new incident. Revert Edits undoes it."));

        _inspectorBody.Children.Add(new TextBlock { Text = "Cameras", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Margin = new Thickness(0, 6, 0, 0) });
        IReadOnlyList<ThumbnailKey> covers = incident.CoverKeys;
        for (int i = 0; i < incident.Cameras.Count; i++)
        {
            IncidentCamera c = incident.Cameras[i];
            var row = new Grid { ColumnSpacing = 8 };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            FrameworkElement thumb = Thumbnail(covers[i], IncidentRowStrip.ThumbnailWidth, IncidentRowStrip.ThumbnailHeight);
            var label = new TextBlock { Text = Service.CameraName(c.Camera), VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis };
            Grid.SetColumn(label, 1);
            var remove = new Button { Content = new FontIcon { Glyph = "", FontSize = 11 }, IsEnabled = incident.Cameras.Count > 1, Padding = new Thickness(6, 4, 6, 4) };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(remove, "Remove from Incident");
            ToolTipService.SetToolTip(remove, incident.Cameras.Count > 1 ? "Remove from Incident" : "The last camera cannot be removed");
            RecordingCamera camera = c.Camera;
            remove.Click += (_, _) => Browser.Remove(camera, incident.Key);
            Grid.SetColumn(remove, 2);
            row.Children.Add(thumb);
            row.Children.Add(label);
            row.Children.Add(remove);
            _inspectorBody.Children.Add(row);
        }
        List<RecordingCamera> addable = Browser.Addable(incident);
        var add = new DropDownButton { Content = "Add Camera", IsEnabled = addable.Count > 0 };
        var menu = new MenuFlyout();
        foreach (RecordingCamera c in addable)
        {
            var item = new MenuFlyoutItem { Text = Service.CameraName(c) };
            RecordingCamera camera = c;
            item.Click += (_, _) => Browser.Add(camera, incident.Key);
            menu.Items.Add(item);
        }
        add.Flyout = menu;
        _inspectorBody.Children.Add(add);
        UpdateInspectorSplit();
    }

    private void UpdateInspectorSplit()
    {
        if (_inspectorSplit is not null)
        {
            _inspectorSplit.IsEnabled = Browser.Selected is { } i && Browser.SplitSecond(i) is not null;
        }
    }

    private static TextBlock Caption(string text) =>
        new() { Text = text, FontSize = 12, Foreground = new SolidColorBrush(Muted), TextWrapping = TextWrapping.Wrap };

    /// <summary>The inspector's text as drawn (self-test): every text block and button label, top to bottom.</summary>
    public List<string> InspectorTexts()
    {
        var list = new List<string>();
        void Walk(UIElement e)
        {
            switch (e)
            {
                case TextBlock t:
                    list.Add(t.Text);
                    break;
                case Button { Content: string s }:
                    list.Add(s);
                    break;
                case CheckBox { Content: string s }:
                    list.Add(s);
                    break;
                case Panel p:
                    foreach (UIElement child in p.Children)
                    {
                        Walk(child);
                    }
                    break;
            }
        }
        foreach (UIElement e in _inspectorBody.Children)
        {
            Walk(e);
        }
        return list;
    }

    // ---- downloads ----

    private void BuildDownloads()
    {
        _downloadsPanel.Children.Add(new ScrollViewer { Content = _downloadsList, VerticalScrollBarVisibility = ScrollBarVisibility.Auto });
        _centerHost.Children.Add(_downloadsPanel);
    }

    private void RefreshDownloads()
    {
        _downloadsList.Children.Clear();
        IReadOnlyList<DownloadProgress> downloads = Service.Downloads;
        if (downloads.Count == 0)
        {
            _downloadsList.Children.Add(new TextBlock
            {
                Text = "No downloads yet. Lock an incident or choose Download to keep its video.",
                Foreground = new SolidColorBrush(Muted),
                TextWrapping = TextWrapping.Wrap,
            });
            return;
        }
        foreach (DownloadProgress d in downloads)
        {
            _downloadsList.Children.Add(DownloadCard(d));
        }
    }

    private Border DownloadCard(DownloadProgress d)
    {
        var body = new StackPanel { Spacing = 6 };
        var header = new Grid();
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.Children.Add(new TextBlock { Text = d.Title(), FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextTrimming = TextTrimming.CharacterEllipsis });
        var state = new TextBlock { Text = d.StateText(), Foreground = new SolidColorBrush(Muted) };
        Grid.SetColumn(state, 1);
        header.Children.Add(state);
        body.Children.Add(header);
        body.Children.Add(new ProgressBar { Minimum = 0, Maximum = 1, Value = d.Fraction(), IsIndeterminate = false });
        foreach (DownloadCamera c in d.Cameras)
        {
            body.Children.Add(Caption($"{Service.CameraName(c.Camera)}: {c.ChunksDone} of {c.ChunksTotal} parts, {c.State.Replace('_', ' ')}"));
        }
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        string id = d.Download;
        if (d.State is not ("done" or "failed"))
        {
            var pause = new Button { Content = d.Paused ? "Resume" : "Pause" };
            pause.Click += (_, _) => Service.Pause(id, !d.Paused);
            buttons.Children.Add(pause);
        }
        if (d.State == "failed")
        {
            var retry = new Button { Content = "Retry" };
            retry.Click += (_, _) => Service.Retry(id);
            buttons.Children.Add(retry);
        }
        var show = new Button { Content = "Show in Explorer" };
        show.Click += (_, _) => Service.Reveal(d);
        buttons.Children.Add(show);
        var forget = new Button { Content = "Forget", IsEnabled = !d.IsWorking() };
        ToolTipService.SetToolTip(forget, d.IsWorking() ? "Pause the download first" : "Forget the download; its files stay");
        forget.Click += (_, _) => Service.Forget(id);
        buttons.Children.Add(forget);
        var trash = new Button { Content = "Move to Recycle Bin" };
        trash.Click += (_, _) => Service.Trash(id);
        buttons.Children.Add(trash);
        body.Children.Add(buttons);
        return new Border
        {
            Child = body,
            Padding = new Thickness(12),
            CornerRadius = new CornerRadius(6),
            BorderThickness = new Thickness(1),
            BorderBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(0x40, 0x80, 0x80, 0x80)),
            Tag = id,
        };
    }

    /// <summary>The downloads as drawn (self-test): ID and the state line.</summary>
    public List<(string Id, string State)> DownloadCards() =>
        [.. _downloadsList.Children.OfType<Border>().Where(b => b.Tag is string).Select(b =>
            ((string)b.Tag, ((TextBlock)((Grid)((StackPanel)b.Child).Children[0]).Children[1]).Text))];

    /// <summary>The downloads' titles as drawn (self-test): the folder's name, or "Range" and its times.</summary>
    public List<string> DownloadCardTitles() =>
        [.. _downloadsList.Children.OfType<Border>().Where(b => b.Tag is string).Select(b =>
            ((TextBlock)((Grid)((StackPanel)b.Child).Children[0]).Children[0]).Text)];

    /// <summary>The downloads panel is shown instead of the player (self-test).</summary>
    public bool DownloadsShown => _downloadsPanel.Visibility == Visibility.Visible;
}
