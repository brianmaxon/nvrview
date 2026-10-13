// Self-test area "settings": the Settings window, device search, and update notes and defaults.

using System.Diagnostics;
using System.Text;
using Microsoft.UI.Windowing;
using NvrView.Shell;
using NvrView.Tiles;
using NvrView.Video;
using Windows.Graphics;
using NvThumbKind = NvrView.Interop.NvThumbKind;
using Point = Windows.Foundation.Point;
using RecordedThumbnail = NvrView.Interop.RecordedThumbnail;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    private async Task SettingsWindowSteps(Viewer v, ViewerWindow w)
    {
        v.DiscoveryRequest = "{\"seconds\":30,\"interfaces\":[\"127.0.0.1\"],\"onvif\":false}";

        // parity: settings-window-sections
        // Settings opens as a window of its own with a pane of sections, from the header (Find devices opens it on Devices).
        int made = SettingsWindow.Created;
        w.Header.Press(HeaderAction.FindDevices);
        SettingsWindow? sw = SettingsWindow.Current;
        Expect(sw is not null && SettingsWindow.Created == made + 1, "Find devices in the header did not open the Settings window");
        if (sw is null)
        {
            return;
        }
        bool searching = await Until(() => sw.IsLoaded && v.Discovery.IsSearching, 5);
        Say($"settings window: sections {string.Join(" / ", sw.SectionTitles)}; on {sw.Section.Title()}, searching {searching}");
        Expect(sw.IsLoaded && sw.Section == SettingsSection.Devices && searching, "Find devices did not show the Devices section and start the search");
        Expect(sw.SectionTitles.SequenceEqual(SettingsSections.All.Select(s => s.Title())), "the Settings pane does not list every section");
        foreach (SettingsSection section in SettingsSections.All)
        {
            sw.Show(section);
            Expect(sw.Section == section && ReferenceEquals(sw.ShownScroller, sw.ScrollerOf(section)), $"the {section.Title()} section was not shown in its scroller");
        }
        sw.Show(SettingsSection.Cameras);
        bool stopped = await Until(() => !v.Discovery.IsSearching, 5);
        Expect(stopped, "leaving the Devices section did not stop the search");

        // parity: settings-window-scrolls
        // A section taller than the window scrolls to its last control.
        sw.ResizeTo(520, 240);
        sw.Show(SettingsSection.AlertsAndCalls);
        Microsoft.UI.Xaml.Controls.ScrollViewer sc = sw.ScrollerOf(SettingsSection.AlertsAndCalls);
        Microsoft.UI.Xaml.FrameworkElement last = sw.LastElementOf(SettingsSection.AlertsAndCalls);
        bool tall = await Until(() => sc.ViewportHeight > 0 && sc.ViewportHeight < 240 && sc.ExtentHeight > sc.ViewportHeight, 5);
        double Bottom() => last.TransformToVisual(sc).TransformBounds(new Windows.Foundation.Rect(0, 0, last.ActualWidth, last.ActualHeight)).Bottom;
        double before = Bottom();
        double viewport = sc.ViewportHeight;
        sc.ChangeView(null, sc.ScrollableHeight, null, true);
        bool scrolled = await Until(() => Math.Abs(sc.VerticalOffset - sc.ScrollableHeight) < 1, 5);
        double after = Bottom();
        Say($"settings window: Alerts & calls extent {sc.ExtentHeight:0} in a viewport of {viewport:0}; last control bottom {before:0} -> {after:0}");
        Expect(tall && before > viewport + 1, "the Alerts & calls section is not taller than the small window, or its last control is already visible");
        Expect(scrolled && after <= sc.ViewportHeight + 1, "scrolling did not bring the section's last control into view");

        // parity: settings-window-single-instance
        // Opening Settings again (the header, Ctrl+,) brings the same window to the front on the section asked for.
        SettingsWindow again = w.OpenSettings(SettingsSection.Updates);
        w.Header.Press(HeaderAction.Settings);
        w.Stage.HandleKey((Windows.System.VirtualKey)188, true, false);
        Say($"settings window: windows made {SettingsWindow.Created - made}, on {again.Section.Title()}");
        Expect(ReferenceEquals(again, sw) && ReferenceEquals(SettingsWindow.Current, sw) && SettingsWindow.Created == made + 1, "opening Settings again made a second window");
        Expect(again.Section == SettingsSection.Updates, "the open Settings window did not keep or take a section");

        // parity: settings-window-frame-remembered
        // The window comes back at the size the user left it.
        sw.AppWindow.Resize(new Windows.Graphics.SizeInt32(700, 480));
        bool pending = await Until(() => sw.FramePending, 5);
        sw.FlushFrame();
        WindowRect? saved = v.Settings.SettingsWindowFrame;
        Say($"settings window: remembered {saved?.Width:0}x{saved?.Height:0}");
        Expect(pending && saved is { } sf && Math.Abs(sf.Width - 700) <= 2 && Math.Abs(sf.Height - 480) <= 2, "the Settings window's size was not remembered");
        sw.Close();
        bool closed = await Until(() => SettingsWindow.Current is null, 5);
        SettingsWindow reopened = SettingsWindow.Open(v, development: true, background: true);
        await Until(() => reopened.IsLoaded, 5);
        WindowRect frame = reopened.CurrentFrame;
        Say($"settings window: reopened at {frame.Width:0}x{frame.Height:0}");
        Expect(closed && Math.Abs(frame.Width - 700) <= 2 && Math.Abs(frame.Height - 480) <= 2, "the Settings window did not reopen at its remembered size");

        // parity: settings-window-closes-with-app
        // Closing the app closes it (Shutdown), and the device search with it.
        SettingsWindow.CloseCurrent();
        bool gone = await Until(() => SettingsWindow.Current is null, 5);
        Expect(gone && !v.Discovery.IsSearching, "the Settings window stayed open when the app closed it");
        v.DiscoveryRequest = null;
    }

    /// <summary>
    /// The add-device screen's "Find Devices on This Network" list (docs/UX.md,
    /// "Finding devices"): it searches when shown, fills from the core's
    /// events, marks added devices, and a click fills the form.
    /// </summary>
    private async Task FindDevices(Viewer v, SettingsPagesHooks hooks, (string Host, ushort Port) mock)
    {
        static CoreEvent Event(string json) =>
            new() { Type = "discovered", Discovery = DiscoveryEvent.Parse(System.Text.Encoding.UTF8.GetBytes(json)) };
        static string Found(ulong search, string key, string rest) =>
            $$"""{"type":"discovered","search":{{search}},"key":"{{key}}",{{rest}}}""";

        // parity: discovery-list
        hooks.NewDevice();
        bool started = await Until(() => v.Discovery.IsSearching && v.Discovery.CurrentSearch is not null, 5);
        if (!started)
        {
            _problems.Add("the add-device screen did not start a search");
            return;
        }
        ulong search = v.Discovery.CurrentSearch!.Value;
        v.Apply(Event(Found(search, "d1", """" "kind":"nvr","name":"NVR 3","address":"192.0.2.12","port":9000,"channels":16,"services":[{"kind":"native","port":9000}]"""")));
        v.Apply(Event(Found(search, "d2", $$"""
            "kind":"nvr","name":"Mock","address":"{{mock.Host}}","port":{{mock.Port}},"channels":2,"services":[{"kind":"native","port":{{mock.Port}}}]
            """)));
        v.Apply(Event(Found(search, "d3", """" "kind":"camera","address":"192.0.2.30","battery_percent":80,"services":[{"kind":"native"}]"""")));
        v.Apply(Event(Found(search, "d4", """" "kind":"camera","name":"Camera 2","model":"EXAMPLE-CAM","address":"192.0.2.40","services":[{"kind":"onvif","port":8000}]"""")));
        // The same device again with more: one row.
        v.Apply(Event(Found(search, "d1", """" "kind":"nvr","name":"NVR 3","model":"EXAMPLE-MODEL","address":"192.0.2.12","port":9000,"channels":16,"services":[{"kind":"native","port":9000}]"""")));
        var panel = hooks.Panel();
        Say($"find devices: {v.Discovery.Devices.Count} found, {panel.Rows.Count} rows, \"{panel.Status}\"");
        if (!panel.Visible || panel.Rows.Count != 4 || v.Discovery.Devices.Count != 4 || !v.Discovery.IsSearching)
        {
            _problems.Add($"the found-devices list did not fill: {panel.Rows.Count} rows, visible {panel.Visible}");
            return;
        }
        if (panel.Rows[0].Detail != "EXAMPLE-MODEL \u00B7 192.0.2.12:9000")
        {
            _problems.Add($"a repeated answer did not update its row: \"{panel.Rows[0].Detail}\"");
        }
        // The mock device is already added; the ONVIF camera cannot be.
        if (!panel.Rows[0].Enabled || panel.Rows[1].Enabled || !panel.Rows[2].Enabled || panel.Rows[3].Enabled)
        {
            _problems.Add($"rows enabled {string.Join(",", panel.Rows.Select(r => r.Enabled))}, expected True,False,True,False");
        }
        // parity: discovery-choose
        // Clicks.
        hooks.Choose(v.Discovery.Devices[0]);
        var form = hooks.Form();
        Say($"find devices: chosen {form.Host}:{form.Port} \"{form.Name}\"");
        if (form.Host != "192.0.2.12" || form.Port != 9000 || form.Name != "NVR 3")
        {
            _problems.Add($"choosing a device did not fill the form: {form}");
        }
        hooks.Choose(v.Discovery.Devices[2]);
        hooks.Choose(v.Discovery.Devices[3]);
        hooks.Choose(v.Discovery.Devices[1]);
        form = hooks.Form();
        if (form.Host != "192.0.2.30" || form.Port != 9000 || form.Name != "NVR 3")
        {
            _problems.Add($"the second choice went wrong, or an unavailable device was chosen: {form}");
        }
        // The search's time is up; the list stays.
        v.Apply(new CoreEvent
        {
            Type = "discovery_done",
            Discovery = DiscoveryEvent.Parse(System.Text.Encoding.UTF8.GetBytes($$"""{"type":"discovery_done","search":{{search}},"count":4,"stopped":false,"errors":[]}""")),
        });
        if (v.Discovery.IsSearching || v.Discovery.Devices.Count != 4 || v.Discovery.FoundNothing || hooks.Panel().Rows.Count != 4)
        {
            _problems.Add("the list changed when the search ended");
        }
        // parity: discovery-search-again
        // Search again twice in a row: the list empties and shows only the last search.
        hooks.SearchAgain();
        hooks.SearchAgain();
        bool again = await Until(() => v.Discovery.IsSearching && v.Discovery.CurrentSearch > search + 1, 5);
        Say($"find devices: after Search again twice: search {v.Discovery.CurrentSearch}, {v.Discovery.Devices.Count} found");
        if (!again || v.Discovery.Devices.Count != 0 || hooks.Panel().Rows.Count != 0)
        {
            _problems.Add("a restarted search did not start from an empty list");
        }
    }
}
