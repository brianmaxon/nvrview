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

    private readonly Grid _exportStrip = new() { Visibility = Visibility.Collapsed, ColumnSpacing = 8, Margin = new Thickness(10, 0, 10, 0) };
    private readonly TextBlock _exportText = new() { VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis };
    private readonly ProgressBar _exportProgress = new() { Minimum = 0, Maximum = 1, VerticalAlignment = VerticalAlignment.Center };
    private readonly Button _exportCancel = new() { Content = "Cancel" };
    private ExportWriters? _exportWriters;

    /// <summary>The window's export writers, with the follow cut's and grids' frames (<see cref="ExportWriters.EditFrames"/>).</summary>
    internal ExportWriters Writers => _exportWriters!;

    /// <summary>The window's export, with the platform writers and pickers (the self-test replaces the pickers).</summary>
    private void AttachExport(CoreHost host)
    {
        _exportWriters = new ExportWriters(host)
        {
            EditFrames = json => ExportWriters.OpenEditFrames(json, () => host.Gpu),
            GridSoundCell = () => Browser.EditPlayer.GridSound,
        };
        ExportController export = Browser.Export;
        export.Writers = _exportWriters;
        export.Dialogs = new ExportDialogs(() => Microsoft.UI.Win32Interop.GetWindowFromWindowId(AppWindow.Id));
        export.DefaultQuality = () => _viewer.Settings.ExportQuality;
        export.Changed += ScheduleRefresh;
    }

    /// <summary>The progress strip for the transport (shown while the Export tab is not), and the keys of the Export tab.</summary>
    private void BuildExport(StackPanel more, StackPanel transport)
    {
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

        // Ctrl+E opens the Export tab (it works while typing too, like any shortcut with a modifier).
        var openTab = new KeyboardAccelerator { Key = VirtualKey.E, Modifiers = VirtualKeyModifiers.Control, ScopeOwner = _root };
        openTab.Invoked += (_, e) =>
        {
            OpenExportTab();
            e.Handled = true;
        };
        _root.KeyboardAccelerators.Add(openTab);

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

    // ---- refreshing ----

    private void RefreshExport()
    {
        ExportController export = Browser.Export;
        InOutPoints points = export.Points;
        _scrub.SetPoints(points);
        _lanes.SetPoints(points);
        ExportJob? job = export.Job;
        bool inTab = _shown.Inspector && Browser.InspectorTab == InspectorTab.Export;
        _exportStrip.Visibility = job is null || inTab ? Visibility.Collapsed : Visibility.Visible;
        RefreshExportJob();
        if (job is not null)
        {
            _exportText.Text = job.Running ? job.Title + "…" : job.Result ?? "";
            _exportProgress.Value = job.Fraction;
            _exportProgress.Visibility = job.Running ? Visibility.Visible : Visibility.Collapsed;
            _exportCancel.Content = job.Running ? "Cancel" : "Dismiss";
        }
    }

    // ---- self-test ----

    /// <summary>The export strip as drawn (self-test): its text and whether it is shown.</summary>
    public (bool Shown, string Text) ExportStripShown => (_exportStrip.Visibility == Visibility.Visible, _exportText.Text);

    /// <summary>The scrubber's in and out points as drawn (self-test).</summary>
    public (long? In, long? Out) ScrubPointsShown => _scrub.PointsShown;
}
