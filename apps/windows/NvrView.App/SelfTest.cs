// --selftest SECONDS: runs the real app against a mock-nvr it started (its
// window behind the others, never activated), then checks, in order:
//
// 1. every channel of the mock plays as a tile with pictures, and the grid
//    read back from the swap chain is not flat inside each tile;
// 2. resizing the window on every refresh for half a second keeps the grid
//    presenting while the buffers are resized only now and then, and they
//    settle at the panel's exact pixel size;
// 3. arrow-key selection finds the neighbor to the right;
// 4. focusing a tile animates it to the whole stage and then switches it to
//    the main stream (generation 1 shown after the old generation's final
//    Reset; the core reports the switch's alignment), still showing a picture;
// 5. a muted tile reads an audio level (the hover meter), and listening
//    makes it audible with a level while the WASAPI stream renders;
// 6. leaving focus switches it back to the sub stream (generation 2);
// 7. digital zoom changes only the source rectangle and still draws;
// 8. a simulated GPU device loss replaces the device, rebuilds every
//    decoder, and every tile plays again;
// 9. no callback threw, no tile reported an error or a failed tier switch,
//    and no Decode failed before the device loss;
// 10. the tile features (docs/UX.md): detections from the mock reach a tile's
//    corner indicators, which sit in the configured corner and render; a
//    sound alarm injected as a core event raises an alert on every lens tile
//    of its camera (red border, pinned header with reason, count, times),
//    which survives the stop and the full screen auto-hide on a manual clock
//    (chrome and pointer hide after 3 s idle, the alert and indicators stay,
//    the next interaction brings the chrome back), is dismissed with Enter,
//    and raises a fresh one on the next detection; the indicator hold ends 2 s
//    after a stop; the Quality menu choices Clear, Balanced, and Auto switch
//    the stream (main, extern, sub) and Auto follows zoom past 2x and back
//    below 1.6x; a second-lens tile opens on mobile, and where the device
//    refuses mobile it is reopened on extern for the run;
// 10b. fisheye (docs/UX.md "Fisheye cameras"): the mock's fisheye channel is
//    reported as one, its image circle is found from the decoded picture,
//    every dewarped view draws through the dewarp shader and its pixels show
//    the synthetic sphere's colors where the shared mapping says they should,
//    the view is saved in the layout leaf, the Virtual PTZ zoom eases on a
//    manual clock and ends on its target, and 0 resets it;
// 11. layouts and windows: the arrangement saved as a layout (device keys),
//    a split, a camera picked into the new tile, a swap, and a tile's
//    quality are each stored; changing the content while a tile is focused
//    ends only the focus (the renderer keeps its swap chain and presents);
//    the layout opened in a second window draws with its own render thread
//    on the same core tiles and follows an edit made in the first; closing it
//    gives its leases back; collapsing the sidebar and revealing it at the
//    edge (on a manual clock) does not resize the stage's buffers; the
//    window's restore record says what it shows and that the sidebar is
//    collapsed;
// 12. with --credential-test, a throwaway credential is written, read back,
//    and deleted in Credential Manager.
//
// Writes what it measured and PASS or FAIL to the --log file and exits 0 or 1.

using System.Diagnostics;
using System.Text;
using Microsoft.UI.Windowing;
using NvrView.Shell;
using NvrView.Tiles;
using NvrView.Video;
using Windows.Graphics;

namespace NvrView.App;

internal sealed class SelfTest(App app, AppOptions options)
{
    private readonly List<string> _problems = [];
    private readonly List<string> _lines = [];
    private readonly Stopwatch _clock = Stopwatch.StartNew();

    /// <summary>The first window's tiles that show a camera.</summary>
    private StageTile[] Tiles => [.. app.Stage!.Tiles.Where(t => !t.IsEmpty)];

    public void Start()
    {
        _ = RunAsync();
        // A hard limit for the whole run.
        _ = Watchdog();
    }

    private async Task Watchdog()
    {
        await Task.Delay(TimeSpan.FromSeconds(options.SelfTestSeconds + 150));
        Finish(["timed out"]);
    }

    private void Say(string line) => _lines.Add($"[{_clock.Elapsed.TotalSeconds,6:0.00}] {line}");

    private async Task RunAsync()
    {
        try
        {
            await Steps();
        }
        catch (Exception e)
        {
            _problems.Add($"self-test threw: {e}");
        }
        Finish(_problems);
    }

    private async Task Steps()
    {
        Viewer v = app.Viewer!;
        StageView stage = app.Stage!;
        StageRenderer r = app.Renderer!;
        Say($"GPU: {v.Host.Gpu.Describe()}");

        if (options.CredentialTest)
        {
            string? problem = WindowsCredentials.RoundTrip();
            Say($"credential manager round trip: {problem ?? "ok"}");
            if (problem is not null)
            {
                _problems.Add($"credentials: {problem}");
            }
        }

        // The first window shows the mock's channels (first lenses) as an even grid.
        bool listed = await Until(() => v.Devices.FirstOrDefault() is { State: "connected" } d && d.Channels.Count >= options.MockChannels, 30);
        if (!listed)
        {
            _problems.Add("the mock's channels were not listed");
            return;
        }
        DeviceEntry mockDevice = v.Devices[0];
        app.Windows[0].Show(new WindowContent.Scratch("Self-test", LayoutNode.Balanced(
            mockDevice.Channels.Keys.Order().Select(c => new CameraRef(mockDevice.Key, c)))));

        // 1. Tiles play.
        bool playing = await Until(() => Tiles.Length >= options.MockChannels &&
            Tiles.All(t => (t.Video?.Counters.Decoded ?? 0) > 5 && t.PictureWidth > 0 && t.Shown > 2), 30);
        Say($"devices {v.Devices.Count} ({string.Join(", ", v.Devices.Select(d => d.State))}), tiles {Tiles.Length}, " +
            $"first picture {Ms(v.Timings.GetValueOrDefault("first_picture"))}");
        if (!playing)
        {
            _problems.Add($"tiles did not all play: {string.Join("; ", Tiles.Select(Describe))}");
            return;
        }
        await Task.Delay(TimeSpan.FromSeconds(Math.Max(1, options.SelfTestSeconds * 0.2)));
        long memoryBefore = PrivateBytes();
        await CheckFrame("grid", Tiles);
        if (options.MockChannels >= 2)
        {
            StageTile last = Tiles[^1];
            Say($"third-party channel: waited {v.WaitedTiles.Contains(last.Id)}, now {app.Stage!.StatusText(last) ?? "playing"}");
            if (!v.WaitedTiles.Contains(last.Id))
            {
                _problems.Add("the third-party channel never reported waiting for the camera");
            }
        }

        // Steady state: the render thread allocates nothing per frame.
        long bytes = r.AllocatedBytes, framesBefore = r.Frames;
        await Task.Delay(1000);
        long allocated = r.AllocatedBytes - bytes, framesDrawn = r.Frames - framesBefore;
        Say($"render thread: {allocated} bytes allocated over {framesDrawn} frames");
        if (allocated > 0)
        {
            _problems.Add($"the render thread allocated {allocated} bytes in steady state");
        }

        // A wrong password: login_failed, said in words, no retry.
        if (app.MockAddress is { } mock)
        {
            DeviceEntry wrong = v.Connect(new DeviceConfig { Name = "wrong password", Host = mock.Host, Port = mock.Port, User = "example-user" }, "not-the-password");
            bool refused = await Until(() => wrong.State == "login_failed", 10);
            Say($"wrong password: {wrong.State}, \"{StageView.DeviceText(wrong)}\"");
            if (!refused)
            {
                _problems.Add("a wrong password did not end in login_failed");
            }
            v.Disconnect(wrong);

            // What the Settings dialog does on Save and Remove (in memory in
            // this mode): a second device connects, then goes away.
            var saved = new DeviceConfig { Name = "second", Host = mock.Host, Port = mock.Port, User = "example-user" };
            v.SaveDevice(saved, Environment.GetEnvironmentVariable("NVRVIEW_TEST_PASSWORD") ?? "example-password");
            bool connected = await Until(() => v.Devices.Any(d => d.Config.Key == saved.Key && d.State == "connected"), 10);
            int tilesWithSecond = Tiles.Length;
            v.ForgetDevice(saved.Key);
            Say($"settings save: second device connected {connected}, {tilesWithSecond} tiles; removed: {v.Devices.All(d => d.Config.Key != saved.Key)}, {Tiles.Length} tiles");
            if (!connected || v.Devices.Any(d => d.Config.Key == saved.Key))
            {
                _problems.Add("saving and removing a device did not connect and remove it");
            }

            // The dialog itself opens and closes. Its device search is real but
            // limited to loopback (nothing answers there); the answers are made up
            // and fed in as the core's events.
            v.DiscoveryRequest = "{\"seconds\":30,\"interfaces\":[\"127.0.0.1\"],\"onvif\":false}";
            var hooks = new SettingsDialogHooks();
            var dialog = SettingsDialog.Create(app.Stage!.XamlRoot, v, development: true, hooks);
            _ = dialog.ShowAsync();
            await Task.Delay(500);
            bool shown = dialog.IsLoaded;
            Say($"settings dialog shown: {shown}");
            if (!shown)
            {
                _problems.Add("the Settings dialog did not open");
            }
            await FindDevices(v, hooks, mock);
            dialog.Hide();
            bool stopped = await Until(() => !v.Discovery.IsSearching, 5);
            Say($"find devices: search stopped when the dialog closed: {stopped}");
            if (!stopped)
            {
                _problems.Add("closing Settings did not stop the device search");
            }
            v.DiscoveryRequest = null;
        }

        // 2. Live resize.
        await Resize(r);

        // 3. Spatial selection.
        StageTile first = Tiles[0];
        if (Tiles.Length >= 2)
        {
            StageTile? right = stage.Neighbor(first, Windows.System.VirtualKey.Right);
            Say($"right of tile 0: {(right is null ? "none" : $"tile {Array.IndexOf(Tiles, right)}")}");
            if (right is null)
            {
                _problems.Add("arrow-key selection found no tile to the right of the first");
            }
        }

        // 4. Focus: animation, then the main stream after the aligned switch.
        int tierEventsBefore = v.TierEvents.Count;
        stage.FocusOn(first);
        bool main = await Until(() => first.Video?.ShownGeneration == 1 && first.ShownTier == "main", FocusAnimation.Seconds + 8);
        float stageW = (float)stage.ActualWidth, stageH = (float)stage.ActualHeight;
        Say($"focused tile 0: showing generation {first.Video?.ShownGeneration} ({first.ShownTier}), picture {first.PictureWidth}x{first.PictureHeight}, " +
            $"rect {first.X:0},{first.Y:0} {first.Width:0}x{first.Height:0} of {stageW:0}x{stageH:0}, " +
            $"tier switch {Ms(v.Timings.GetValueOrDefault("tier_switch"))}, delta {v.TierSwitchDeltaMs?.ToString() ?? "n/a"} ms, held {v.TierSwitchHeldMs?.ToString() ?? "n/a"} ms");
        if (!main)
        {
            _problems.Add("focus did not switch to the main stream");
        }
        if (Math.Abs(first.Width - stageW) > 2 && Math.Abs(first.Height - stageH) > 2)
        {
            _problems.Add("the focused tile does not fill the stage");
        }
        CheckTierEvents(v, first.Id, tierEventsBefore, 1);
        await CheckFrame("focused", [first]);

        // 5. Listening.
        float muted = await MaxLevel(v, first, 0.5);
        Say($"muted level {muted:0.00}");
        if (muted <= 0)
        {
            _problems.Add("no audio level while muted");
        }
        long rendered = app.Audio?.FramesRendered ?? 0;
        v.ToggleListening(first.Camera, additive: false);
        var heard = Stopwatch.StartNew();
        float listening = 0;
        while (heard.Elapsed.TotalSeconds < 6 && listening <= 0)
        {
            listening = await MaxLevel(v, first, 0.1);
        }
        Say($"listening: first level after {heard.Elapsed.TotalSeconds:0.00} s");
        // The core starts playout once its delay target is known; then the
        // WASAPI stream carries the tile (audio_stats: starts, no underruns).
        bool started = app.Audio is not { Problem: null } || await Until(() => AudioStarted(first.AudioStats), 8);
        Say($"playout started after {heard.Elapsed.TotalSeconds:0.00} s: {started}");
        if (!started)
        {
            _problems.Add("the core never started playing the audible tile");
        }
        await Task.Delay(300);
        long renderedNow = app.Audio?.FramesRendered ?? 0;
        string audio = app.Audio is null ? "off (--no-audio)" :
            app.Audio.Problem is { } why ? $"no output ({why})" : $"{app.Audio.SampleRate} Hz, {renderedNow - rendered} frames rendered";
        Say($"listening: audible {first.Audible}, level {listening:0.00}, audio output {audio}, lead {app.Audio?.LeadMs:0.0} ms");
        Say($"audio stats: {first.AudioStats ?? "none"}");
        if (!first.Audible)
        {
            _problems.Add("the tile did not become audible");
        }
        if (listening <= 0)
        {
            _problems.Add("no audio level while listening");
        }
        if (app.Audio is { Problem: null } && renderedNow <= rendered)
        {
            _problems.Add("the audio output rendered nothing");
        }
        v.SetListening(first.Camera, false);

        // 6. Leave focus.
        tierEventsBefore = v.TierEvents.Count;
        stage.Unfocus();
        bool sub = await Until(() => first.Video?.ShownGeneration == 2 && first.ShownTier == "sub" && r.Focus is null, FocusAnimation.Seconds + 8);
        Say($"after focus: showing generation {first.Video?.ShownGeneration} ({first.ShownTier})");
        if (!sub)
        {
            _problems.Add("leaving focus did not return to the sub stream");
        }
        CheckTierEvents(v, first.Id, tierEventsBefore, 2);

        // 7. Digital zoom.
        StageTile zoomed = Tiles[^1];
        StageView.ZoomAround(zoomed, 0.25, 0.25, 2);
        await CheckFrame("zoomed", [zoomed]);
        Say($"zoom {zoomed.Zoom:0.0} around {zoomed.CenterX:0.00},{zoomed.CenterY:0.00}");
        if (Math.Abs(zoomed.Zoom - 2) > 0.01)
        {
            _problems.Add("zoom was not applied");
        }
        StageView.ResetZoom(zoomed);

        long failuresBefore = Tiles.Sum(t => (t.Video?.Counters.Failures ?? 0) + (t.Video?.Counters.ConfigureFailures ?? 0));
        Say($"decode failures before the device loss: {failuresBefore}");
        if (failuresBefore > 0)
        {
            _problems.Add($"{failuresBefore} decode failures");
        }
        long memoryAfter = PrivateBytes();

        // 8. Device loss.
        long[] decodedBefore = [.. Tiles.Select(t => (t.Video?.Counters.Decoded ?? 0))];
        r.SimulateDeviceLoss();
        bool recovered = await Until(() => r.Recoveries >= 1, 10);
        bool playingAgain = recovered && await Until(() =>
            Tiles.Select((t, i) => (t.Video?.Counters.Decoded ?? 0) > decodedBefore[i] + 10 && t.PictureWidth > 0).All(x => x), 15);
        Say($"device loss: recoveries {r.Recoveries}, device replacements {v.Host.DeviceReplacements}, " +
            $"decoders rebuilt {Tiles.Sum(t => (t.Video?.Rebuilds ?? 0))}, failures answered {Tiles.Sum(t => (t.Video?.Counters.Failures ?? 0)) - failuresBefore}");
        if (!recovered || !playingAgain)
        {
            _problems.Add($"tiles did not play again after the device loss: {string.Join("; ", Tiles.Select(Describe))}");
        }
        else
        {
            await Task.Delay(500);
            await CheckFrame("after device loss", Tiles);
        }

        await TileFeatures(v, stage);
        await FisheyeViews(v, stage, r);
        await LayoutsAndWindows(v);

        // Detections from the mock (camera AI) reached the tiles.
        Say($"detection events: {v.DetectionEvents}, shown now: " +
            string.Join(", ", Tiles.SelectMany(t => v.Board.Items(t.Camera).Select(i => i.Description)).Distinct()));
        if (v.DetectionEvents == 0)
        {
            _problems.Add("no detection events arrived");
        }

        // The device goes away: tiles say they reconnect, with a countdown.
        app.StopMock();
        bool reconnecting = await Until(() => Tiles.All(t => app.Stage!.StatusText(t) is { } s && s.Contains("onnect", StringComparison.Ordinal)), 15);
        Say($"mock-nvr stopped: {app.Stage!.StatusText(Tiles[0]) ?? "playing"}");
        if (!reconnecting)
        {
            _problems.Add("tiles did not say they were reconnecting after the device went away");
        }

        // 9. Contract and summary.
        if (v.Core.CallbackExceptionCount > 0)
        {
            _problems.Add($"callbacks threw {v.Core.CallbackExceptionCount} times: {v.Core.FirstCallbackException}");
        }
        _problems.AddRange(v.Problems.Select(p => $"core: {p}"));
        foreach (StageTile t in Tiles)
        {
            Say(Describe(t));
        }
        Say($"render: {r.Frames} frames, {r.TotalResizes} resizes, {r.Timeouts} wait timeouts, chrome frames {stage.ChromeFrames}");
        Say($"memory: private {memoryBefore >> 20} MB playing, {memoryAfter >> 20} MB after focus and zoom, {PrivateBytes() >> 20} MB at the end; " +
            $"decoder samples setting {DecoderSettings.OutputSamples}, limit coded size {DecoderSettings.LimitCodedSize}");
        foreach ((string what, long ns) in v.Timings.OrderBy(x => x.Key))
        {
            Say($"timing {what}: {Ms(ns)}");
        }
    }

    private static CoreEvent Detect(StageTile tile, string kind, string source, bool start) => new()
    {
        Type = "detection",
        Device = tile.Device!.Id,
        Channel = tile.Channel,
        Kind = kind,
        Source = source,
        State = start ? "start" : "stop",
    };

    private void Expect(bool ok, string what)
    {
        if (!ok)
        {
            _problems.Add(what);
        }
    }

    // 10. The tile features.
    /// <summary>
    /// The add-device screen's "Find Devices on This Network" list (docs/UX.md,
    /// "Finding devices"): it searches when shown, fills from the core's
    /// events, marks added devices, and a click fills the form.
    /// </summary>
    private async Task FindDevices(Viewer v, SettingsDialogHooks hooks, (string Host, ushort Port) mock)
    {
        static CoreEvent Event(string json) =>
            new() { Type = "discovered", Discovery = DiscoveryEvent.Parse(System.Text.Encoding.UTF8.GetBytes(json)) };
        static string Found(ulong search, string key, string rest) =>
            $$"""{"type":"discovered","search":{{search}},"key":"{{key}}",{{rest}}}""";

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

    private async Task TileFeatures(Viewer v, StageView stage)
    {
        // Camera AI detections from the mock reach the corner indicators.
        bool shown = await Until(() => Tiles.Any(t => stage.ChromeFor(t) is { Indicators.Count: > 0 }), 15);
        StageTile? withSymbols = Tiles.FirstOrDefault(t => stage.ChromeFor(t) is { Indicators.Count: > 0 });
        Say($"corner indicators: {(shown ? string.Join(", ", stage.ChromeFor(withSymbols!)!.Indicators.Select(i => i.Description)) : "none")}, " +
            $"corner {v.Settings.IndicatorCorner}");
        Expect(shown, "no tile showed a corner indicator from the mock's detections");
        Expect(v.Settings.IndicatorCorner == ChromeCorner.TopRight, "the indicator corner is not top right by default");
        if (withSymbols is not null)
        {
            v.Settings.IndicatorCorner = ChromeCorner.BottomLeft;
            stage.SyncOverlays();
            Expect(stage.ChromeFor(withSymbols)!.Corner == ChromeCorner.BottomLeft, "the indicator corner setting did not move the symbols");
            v.Settings.IndicatorCorner = ChromeCorner.TopRight;
        }
        await RenderStrip(stage);

        // Alerts, auto-hide, and the hold, on a clock that moves only when told to.
        var clock = new ManualViewerClock(1000);
        v.UseClock(clock);
        StageTile lens0 = Tiles.First(t => t.Channel == 1 && t.Lens == 0);
        DeviceEntry? device = lens0.Device;
        StageTile? lens1 = AddCamera(stage, new CameraRef(device!.Key, 1, 1));
        Expect(lens1 is not null, "the second-lens tile did not open");
        if (lens1 is null)
        {
            return;
        }
        bool lensPlays = await Until(() => (lens1.Video?.Counters.Decoded ?? 0) > 5 && lens1.PictureWidth > 0, 15);
        Say($"second lens: opened on {lens1.OpenedTier}, showing {lens1.ShownTier}, decoded {(lens1.Video?.Counters.Decoded ?? 0)}");
        Expect(lens1.OpenedTier == StreamTier.Mobile, "a second-lens tile did not open on mobile");
        Expect(lensPlays, "the second-lens tile did not play");
        TileChrome c0 = stage.ChromeFor(lens0)!, c1 = stage.ChromeFor(lens1)!;
        StageTile other = Tiles.First(t => t.Channel == 0);
        TileChrome cOther = stage.ChromeFor(other)!;

        v.Apply(Detect(lens0, "smoke_alarm", "local_audio", true));
        stage.Tick();
        bool both = c0.AlertShown && c1.AlertShown && !cOther.AlertShown;
        Say($"alert raised: lens 0 {c0.AlertShown}, lens 1 {c1.AlertShown}, other camera {cOther.AlertShown}; " +
            $"border {(c0.AlertBlinking ? "blinking" : "steady")} (animations {(v.AnimationsEnabled() ? "on" : "off")}); " +
            $"\"{c0.AlertText.Replace('\n', '|')}\"");
        Expect(both, "a sound alarm did not raise the alert on both lens tiles of its camera only");
        Expect(c0.AlertBlinking == v.AnimationsEnabled(), "the alert border does not follow the animation setting");
        Expect(c0.AlertText.Contains("Smoke alarm (sound)", StringComparison.Ordinal) && c0.AlertText.Contains("Detected 1 time", StringComparison.Ordinal) &&
            c0.AlertText.Contains("Sounding now", StringComparison.Ordinal), "the alert header lacks the reason, count, or state");
        Expect(c0.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm) && c1.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm),
            "the sound alarm is not an indicator on both lenses");
        Expect(!cOther.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm), "the sound alarm showed on another camera");

        // Full screen: the chrome and pointer hide after 3 s idle; the alert and indicators stay.
        stage.FullScreenOverride = true;
        stage.Interact();
        c0.SetHover(true, true);
        clock.Advance(2.9);
        stage.Tick();
        Expect(stage.ChromeShown && !stage.CursorHidden, "the chrome hid before 3 s");
        clock.Advance(0.2);
        stage.Tick();
        Say($"full screen idle 3.1 s: chrome shown {stage.ChromeShown}, pointer hidden {stage.CursorHidden}, header {c0.HeaderShown}, alert {c0.AlertShown}, symbols {c0.Indicators.Count}");
        Expect(!stage.ChromeShown && stage.CursorHidden && !c0.HeaderShown, "the chrome and pointer did not hide after 3 s idle in full screen");
        Expect(c0.AlertShown && c0.Indicators.Count > 0, "the alert or the indicators hid with the chrome");
        stage.Interact();
        stage.Tick();
        Expect(stage.ChromeShown && !stage.CursorHidden, "an interaction did not bring the chrome and pointer back");
        clock.Advance(4);
        stage.Tick();
        Expect(stage.CursorHidden, "the pointer did not hide again");
        stage.FullScreenOverride = false;
        stage.Interact();
        clock.Advance(4);
        stage.Tick();
        Expect(!stage.CursorHidden, "the pointer hid outside full screen");
        stage.FullScreenOverride = null;

        // A stop does not clear the alert; the symbol is held 2 s.
        v.Apply(Detect(lens0, "smoke_alarm", "local_audio", false));
        stage.Tick();
        Expect(c0.AlertShown && c0.AlertText.Contains("Not sounding now", StringComparison.Ordinal), "the stop cleared the alert or did not say it stopped");
        Expect(c0.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm), "the symbol went the moment the sound stopped");
        clock.Advance(1.9);
        stage.Tick();
        bool held = c0.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm);
        clock.Advance(0.2);
        stage.Tick();
        bool gone = !c0.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm);
        Say($"indicator hold: after 1.9 s {(held ? "held" : "gone")}, after 2.1 s {(gone ? "gone" : "held")}");
        Expect(held && gone, "the symbol was not held for 2 s and then removed");

        // Again counts; Enter dismisses for every tile of the camera; a new one starts fresh.
        v.Apply(Detect(lens0, "smoke_alarm", "local_audio", true));
        stage.Tick();
        Expect(c1.AlertText.Contains("Detected 2 times", StringComparison.Ordinal), "a second detection was not counted");
        stage.ApplyAction(TileKeyMap.Map(TileKey.Enter, false, false, false, v.Alerts.IsActive(lens1.Camera)), lens1);
        stage.Tick();
        Say($"dismissed with Enter: alert on lens 0 {c0.AlertShown}, lens 1 {c1.AlertShown}, log: {string.Join(" / ", v.Alerts.Log.Select(l => l.Text))}");
        Expect(!c0.AlertShown && !c1.AlertShown && !v.Alerts.IsActive(lens0.Camera), "Enter did not dismiss the alert on every tile of the camera");
        Expect(v.Alerts.Log.Count >= 3, "the session log lacks the alert lines");
        v.Apply(Detect(lens0, "smoke_alarm", "local_audio", true));
        stage.Tick();
        Expect(c0.AlertShown && c0.AlertText.Contains("Detected 1 time", StringComparison.Ordinal), "a detection after the dismissal did not raise a fresh alert");
        v.Apply(Detect(lens0, "smoke_alarm", "local_audio", false));
        v.DismissAlert(lens0.Camera);
        stage.Tick();
        Expect(!c1.AlertShown, "the Dismiss path did not clear the alert");
        v.UseClock(new SystemViewerClock());

        // Quality: Clear, Balanced, and Auto switch the stream; zoom moves Auto.
        StageTile q = other;
        Expect(v.OffersBalanced(q), "Balanced is not offered for a single-lens channel that lists extern");
        Expect(!v.OffersBalanced(lens0), "Balanced is offered for a two-lens camera");
        Expect(QualityMenu.Choices(true).SequenceEqual(["Auto", "Clear", "Fluent", "Balanced"]) && QualityMenu.Choices(false).SequenceEqual(["Auto", "Clear", "Fluent"]),
            "the Quality menu lists the wrong choices");
        v.SetQuality(q, TileQuality.Clear);
        bool main = await Until(() => q.ShownTier == "main" && q.PictureWidth > 0, 10);
        Say($"quality Clear: showing {q.ShownTier} {q.PictureWidth}x{q.PictureHeight}, header \"{StageView.TitleOf(q)}\"");
        Expect(main, "Clear did not switch the tile to the main stream");
        Expect(StageView.TitleOf(q).Contains("Clear", StringComparison.Ordinal), "the header does not name the quality");
        v.SetQuality(q, TileQuality.Balanced);
        bool mid = await Until(() => q.ShownTier == "extern" && q.PictureWidth > 0, 10);
        Say($"quality Balanced: showing {q.ShownTier}");
        Expect(mid, "Balanced did not switch the tile to the extern stream");
        v.SetQuality(q, TileQuality.Auto);
        bool sub = await Until(() => q.ShownTier == "sub", 10);
        Say($"quality Auto: showing {q.ShownTier}");
        Expect(sub, "Auto did not return the tile to the sub stream");
        q.Zoom = 2.5f;
        v.Tick();
        bool zoomedMain = await Until(() => q.ShownTier == "main", 10);
        q.Zoom = 1.7f;
        v.Tick();
        await Task.Delay(1500);
        bool stays = q.ShownTier == "main" && q.RequestedTier == StreamTier.Main;
        q.Zoom = 1.5f;
        v.Tick();
        bool back = await Until(() => q.ShownTier == "sub", 10);
        Say($"auto zoom: 2.5x {(zoomedMain ? "main" : q.ShownTier)}, 1.7x {(stays ? "still main" : "changed")}, 1.5x {(back ? "sub" : q.ShownTier)}");
        Expect(zoomedMain && stays && back, "Auto did not follow the zoom (main past 2x, still main at 1.7x, sub below 1.6x)");
        StageView.ResetZoom(q);
        v.Tick();
        await Until(() => q.ShownTier == "sub", 10);

        // A camera whose device refuses the second lens's mobile stream: reopened on extern. The core ends
        // a refused start with tile_error start_rejected only after its 60 s wait (docs/FFI.md), so the event is
        // injected, as a fake core would.
        StageTile onCh2 = Tiles.First(t => t.Channel == 2 && t.Lens == 0);
        CameraRef camera = new(onCh2.Device!.Key, 2, 1);
        StageTile? refused = AddCamera(stage, camera);
        Expect(refused is { OpenedTier: StreamTier.Mobile }, "the refused second lens did not open on mobile first");
        ulong firstId = refused?.Id ?? 0;
        if (refused is not null)
        {
            v.Apply(new CoreEvent { Type = "tile_error", Tile = refused.Id, Reason = "start_rejected", Status = 400 });
        }
        StageTile? now = Tiles.FirstOrDefault(t => t.Camera == camera);
        bool reopened = now is { OpenedTier: StreamTier.Extern } && now.Id != firstId && v.Planner.NoMobile(camera);
        Say($"second lens refused on mobile: reopened on {now?.OpenedTier.ToString() ?? "n/a"} as a new tile {now is not null && now.Id != firstId} (no mobile for the run: {v.Planner.NoMobile(camera)}), " +
            $"a first lens is left alone: {!v.Planner.StartFailed(onCh2.Camera, StreamTier.Sub, "start_rejected")}");
        Expect(reopened, "a second lens whose mobile start was refused was not reopened on extern");
        while (Tiles.FirstOrDefault(t => t.Lens > 0) is { } t)
        {
            // The mock refuses extern too on this channel; that refusal is expected.
            v.Problems.RemoveAll(p => p.StartsWith($"tile_error tile {t.Id}:", StringComparison.Ordinal));
            RemoveTile(stage, t);
        }
        Expect(Tiles.All(t => t.Lens == 0), "a second-lens tile stayed open");
    }

    // 10b. Fisheye views (docs/UX.md "Fisheye cameras", docs/RENDERING.md "Fisheye dewarping").
    private async Task FisheyeViews(Viewer v, StageView stage, StageRenderer r)
    {
        StageTile? f = Tiles.FirstOrDefault(t => t.Channel == 2 && t.Lens == 0);
        if (f is null || options.MockChannels < 3)
        {
            Say("fisheye: skipped (needs 3 mock channels)");
            return;
        }
        bool known = await Until(() => v.IsFisheye(f.Camera), 10);
        Expect(known, "the mock's fisheye channel was not reported as a fisheye camera");
        Expect(!v.IsFisheye(Tiles.First(t => t.Channel == 0).Camera), "an ordinary channel was reported as a fisheye camera");
        if (!known)
        {
            return;
        }
        stage.SetFisheyeMode(f, FisheyeViewMode.Panorama);
        bool found = await Until(() => v.Circles.Circle(f.Camera) is not null, 10);
        FisheyeCircle circle = v.Circles.Circle(f.Camera) ?? FisheyeCircle.Inscribed(384, 384);
        Say($"fisheye circle: ({circle.Cx:F4}, {circle.Cy:F4}) r {circle.R:F4} (synthetic: 0.49, 0.505, 0.46), fell back {v.Circles.FellBack(f.Camera)}");
        Expect(found && !v.Circles.FellBack(f.Camera), "the fisheye circle was not found");
        Expect(Math.Abs(circle.Cx - 0.49) < 0.01 && Math.Abs(circle.Cy - 0.505) < 0.01 && Math.Abs(circle.R - 0.46) < 0.01,
            "the fisheye circle is more than 1% off the synthetic one");

        foreach (FisheyeViewMode mode in new[] { FisheyeViewMode.Panorama, FisheyeViewMode.DoublePanorama, FisheyeViewMode.Quad, FisheyeViewMode.Ptz })
        {
            stage.SetFisheyeMode(f, mode);
            long before = r.DewarpDraws;
            bool drawn = await Until(() => r.DewarpDraws > before + 3, 5);
            Expect(drawn, $"fisheye {mode.Title()}: the dewarp shader did not draw");
            Expect(StageView.TitleOf(f).Contains(mode.Title(), StringComparison.Ordinal), $"fisheye {mode.Title()}: the header does not name the view");
            string? member = stage.State.Tree.Member(f.Path, FisheyeTileSetting.Member);
            Expect(FisheyeTileSetting.FromJson(member).Mode == mode, $"fisheye {mode.Title()}: the view was not kept in the layout leaf ({member})");
            await CheckColors(mode.Title(), f, r, circle);
        }

        // Virtual PTZ zoom: eased on the viewer's clock, ends on the target, saved; 0 resets.
        var clock = new ManualViewerClock(2000);
        v.UseClock(clock);
        double fov = f.Fisheye?.Aim.Fov ?? 0;
        stage.ZoomFisheye(f, 2);
        clock.Advance(1.0 / 60);
        stage.StepFisheyeZoom(clock.Now);
        double mid = f.Fisheye?.Aim.Fov ?? 0;
        clock.Advance(2);
        stage.StepFisheyeZoom(clock.Now);
        double end = f.Fisheye?.Aim.Fov ?? 0;
        v.UseClock(new SystemViewerClock());
        FisheyeTileSetting saved = FisheyeTileSetting.FromJson(stage.State.Tree.Member(f.Path, FisheyeTileSetting.Member));
        Say($"fisheye PTZ zoom: {fov:0.#}° → {mid:0.#}° after one refresh → {end:0.#}°, saved {saved.Aim?.Fov:0.#}°");
        Expect(mid < fov && mid > fov / 2 && Math.Abs(end - fov / 2) < 1e-9 && !stage.IsEasingFisheyeZoom, "the Virtual PTZ zoom did not ease to its target");
        Expect(saved.Aim is { } a && Math.Abs(a.Fov - fov / 2) < 1e-9, "the Virtual PTZ zoom was not saved with the layout");
        await CheckColors("Virtual PTZ zoomed", f, r, circle);
        stage.Select(f.Path);
        stage.HandleKey(Windows.System.VirtualKey.Number0, false, false);
        Expect(f.Fisheye?.Aim == FisheyeAim.Default(FisheyeMount.Ceiling), "0 did not reset the virtual camera");

        // Back to the camera's own picture.
        stage.SetFisheyeMode(f, FisheyeViewMode.Original);
        Expect(f.Fisheye is null && stage.State.Tree.Member(f.Path, FisheyeTileSetting.Member) is null, "Original did not remove the leaf member");
    }

    // Samples the dewarped tile where the shared mapping says the synthetic sphere
    // (fixtures/synthetic/make-fisheye.sh) has a known color: red, green, blue, or
    // yellow by azimuth quadrant, away from the checkerboard's edges and the moving dot.
    private async Task CheckColors(string what, StageTile f, StageRenderer r, FisheyeCircle circle)
    {
        if (f.Fisheye is not { } fe)
        {
            _problems.Add($"fisheye {what}: the tile is not dewarped");
            return;
        }
        await Task.Delay(200);
        Task<StageCapture> task = r.CaptureAsync();
        if (await Task.WhenAny(task, Task.Delay(5000)) != task)
        {
            _problems.Add($"fisheye {what}: no frame was drawn");
            return;
        }
        StageCapture c = await task;
        (StageTile _, float x0, float y0, float w, float h) = c.Rects.FirstOrDefault(rect => rect.Tile == f);
        int pw = Math.Max(f.PictureWidth, 1), ph = Math.Max(f.PictureHeight, 1);
        FisheyeDewarp d = FisheyeDewarp.Of(new FisheyeLens(circle, pw / (double)ph), fe.Mount, fe.Mode, fe.Aim,
            Math.Max((int)Math.Round(w), 1), Math.Max((int)Math.Round(h), 1));
        int tried = 0, right = 0;
        string? firstWrong = null;
        for (int j = 0; j < 32; j++)
        {
            for (int i = 0; i < 32; i++)
            {
                var p = new UnitPoint((i + 0.5) / 32, (j + 0.5) / 32);
                if (d.Direction(p) is not { } dir)
                {
                    continue;
                }
                (double pan, double tilt) = Fisheye.PanTilt(dir);
                double psi = ((pan % 360) + 360) % 360;
                static double Off(double a) => Math.Abs(a - Math.Round(a / 15) * 15);
                if (tilt < 5 || tilt > 80 || Off(psi) < 3 || Off(tilt) < 3 || Math.Abs(tilt - 20) < 8)
                {
                    continue;
                }
                int sx = (int)(x0 + p.X * w), sy = (int)(y0 + p.Y * h);
                if (sx < 0 || sy < 0 || sx >= c.Image.Width || sy >= c.Image.Height)
                {
                    continue;
                }
                int want = (int)(psi / 90) % 4;
                (int cr, int cg, int cb) = c.Image.Rgb(sx, sy);
                int got = cr > cb + 40 && cg > cb + 40 && cg > cr * 0.6 ? 3
                    : cr > cg + 25 && cr > cb + 25 ? 0
                    : cg > cr + 25 && cg > cb + 25 ? 1
                    : cb > cr + 25 && cb > cg + 25 ? 2
                    : -1;
                tried++;
                if (got == want)
                {
                    right++;
                }
                else
                {
                    firstWrong ??= $"at ({p.X:F2}, {p.Y:F2}) psi {psi:0} tau {tilt:0}: ({cr}, {cg}, {cb}), wanted quadrant {want}";
                }
            }
        }
        Say($"fisheye {what}: {right} of {tried} sampled pixels show the expected color{(firstWrong is null ? "" : $"; first miss {firstWrong}")}");
        Expect(tried >= 20 && right >= tried * 9 / 10, $"fisheye {what}: dewarped pixels do not match the synthetic scene ({right} of {tried})");
    }

    // 11. Layouts, windows, and the sidebar (docs/UX.md "Layouts", "Sidebar").
    private async Task LayoutsAndWindows(Viewer v)
    {
        ViewerWindow w1 = app.Windows[0];
        StageView stage = w1.Stage;
        StageRenderer r = w1.Renderer;

        // Save the arrangement as a stored layout; the window now shows it.
        int leaves = stage.Tiles.Length;
        string? id = w1.SaveAsLayout(v.UniqueLayoutName("Self-test layout"));
        SavedLayout? stored = id is null ? null : v.Layout(id);
        Say($"layout saved: {id ?? "refused"}, {stored?.Root.LeafCount ?? 0} tiles, window shows it: {w1.State.Content is WindowContent.Layout(string shown) && shown == id}");
        Expect(stored is not null && stored.Root == w1.State.Tree, "Save as layout did not store the window's tree");
        if (id is null || stored is null)
        {
            return;
        }
        Expect(v.Devices[0].Config.CoreKey is { } coreKey && stored.Root.Cameras.All(c => c.Device == coreKey),
            "the layout does not name cameras by the core's device key");
        try
        {
            // Split the first tile, put a camera in the new tile, swap two tiles: each edit is stored.
            stage.SetEditing(true);
            NodePath first = stage.Tiles[0].Path;
            stage.Select(first);
            stage.Split(SplitAxis.Row);
            NodePath? added = w1.State.Selected;
            Expect(v.Layout(id)?.Root.LeafCount == leaves + 1, "splitting a tile was not stored");
            CameraRef extra = new(v.Devices[0].Key, 0);
            w1.CameraPicked(extra);
            Expect(added is { } p && v.Layout(id)?.Root[p]?.Camera == extra, "a camera picked in the sidebar did not fill the selected tile");
            NodePath a = stage.Tiles[0].Path, b = stage.Tiles[^1].Path;
            CameraRef? ca = stage.Tiles[0].CameraOrNone, cb = stage.Tiles[^1].CameraOrNone;
            stage.Swap(a, b);
            LayoutNode? after = v.Layout(id)?.Root;
            Say($"designer: split to {after?.LeafCount} tiles, swapped {ca} and {cb}: stored {after?[a]?.Camera} and {after?[b]?.Camera}");
            Expect(after?[a]?.Camera == cb && after?[b]?.Camera == ca, "swapping two tiles was not stored");
            // A tile's quality is saved in its leaf ("quality"), and Auto removes it.
            stage.SetQuality(stage.Tiles[0], TileQuality.Fluent);
            string? member = v.Layout(id)?.Root.Member(stage.Tiles[0].Path, "quality");
            stage.SetQuality(stage.Tiles[0], TileQuality.Auto);
            Expect(member == "\"fluent\"" && v.Layout(id)?.Root.Member(stage.Tiles[0].Path, "quality") is null,
                "a tile's quality was not kept in its layout leaf");
            stage.SetEditing(false);
            // The stored JSON reads back to the same layout.
            Expect(SavedLayout.Parse(v.Layout(id)!.ToJson()) == v.Layout(id), "the stored layout does not round-trip");

            bool playing = await Until(() => Tiles.All(t => (t.Video?.Counters.Decoded ?? 0) > 3 && t.PictureWidth > 0), 15);
            Expect(playing, $"tiles did not play after the edits: {string.Join("; ", Tiles.Select(Describe))}");

            // Changing the content while a tile is focused ends the focus only: the renderer keeps presenting.
            StageTile focusTile = Tiles[0];
            stage.FocusOn(focusTile);
            await Task.Delay(400);
            int chains = r.SwapChains;
            long frames = r.Frames;
            w1.Show(new WindowContent.Layout(id));
            await Task.Delay(500);
            Say($"content changed while focused: focus {(r.Focus is null ? "ended" : "kept")}, swap chains {chains} -> {r.SwapChains}, frames +{r.Frames - frames}");
            Expect(r.Focus is null && !focusTile.Focused, "changing the content did not end the focus");
            Expect(r.SwapChains == chains && r.Frames - frames > 10, "changing the content while focused disturbed the renderer");
            await CheckFrame("after a content change while focused", Tiles);

            // The same layout in a second window: its own render thread, shared core tiles.
            ViewerWindow w2 = app.OpenWindow(new WindowContent.Layout(id));
            bool drawn = await Until(() => w2.Renderer.Frames > 30 && w2.Stage.Tiles.Where(t => !t.IsEmpty).All(t => t.PictureWidth > 0), 15);
            StageTile[] t2 = [.. w2.Stage.Tiles.Where(t => !t.IsEmpty)];
            bool shared = t2.All(t => Tiles.FirstOrDefault(o => o.CameraOrNone == t.CameraOrNone) is { } o && o.Id == t.Id && t.Id != 0);
            CameraRef sample = t2[0].Camera;
            Say($"second window: {w2.Renderer.Frames} frames, {t2.Length} tiles, same core tiles {shared}, leases of {sample} {v.Leases.LeaseCount(sample)}, core tiles {v.Tiles.Length}");
            Expect(drawn, "the second window did not draw its tiles");
            Expect(shared, "the second window did not share the first window's core tiles");
            Expect(v.Tiles.Length == v.Leases.Cameras.Count, "a camera has more than one core tile");
            if (drawn)
            {
                await CheckFrame("second window", t2, w2.Renderer);
            }

            // An edit in one window reaches the other (layouts_changed), not during a drag.
            stage.Edit(t => t.SettingRatio(0.3, NodePath.Root));
            bool followed = await Until(() => w2.State.Tree == w1.State.Tree, 5);
            Say($"second window followed the edit: {followed}");
            Expect(followed, "the second window did not follow the stored layout");

            // Closing it gives its leases back; the first window keeps playing.
            int before = v.Leases.LeaseCount(sample);
            w2.Window.Close();
            await Task.Delay(300);
            Say($"second window closed: windows {app.Windows.Count}, leases of {sample} {before} -> {v.Leases.LeaseCount(sample)}");
            Expect(app.Windows.Count == 1 && v.Leases.LeaseCount(sample) == before - t2.Count(t => t.Camera == sample), "closing a window did not give its leases back");

            // The sidebar: collapse it (the stage grows), then reveal it at the
            // edge on a manual clock: the overlay slides over the stage, whose
            // buffers are not resized.
            w1.SetSidebarDocked(false);
            await Task.Delay(700);
            (uint bw, uint bh) = r.BufferSize;
            long resizes = r.TotalResizes;
            var clock = new ManualViewerClock(1000);
            w1.RevealClock = clock;
            w1.PointerAt(inEdge: true, inOverlay: false);
            clock.Advance(0.1);
            w1.TickReveal();
            bool early = w1.RevealShown;
            clock.Advance(0.05);
            w1.TickReveal();
            bool shownNow = w1.RevealShown;
            long frames2 = r.Frames;
            await Task.Delay(600);
            Say($"reveal: after 0.10 s {early}, after 0.15 s {shownNow}; buffers {bw}x{bh} -> {r.BufferSize.Width}x{r.BufferSize.Height}, resizes +{r.TotalResizes - resizes}, frames +{r.Frames - frames2}");
            Expect(!early && shownNow, "the edge reveal did not wait about 150 ms and then show");
            Expect(r.TotalResizes == resizes && r.BufferSize == (bw, bh) && !w1.SidebarDocked, "revealing the sidebar resized the stage");
            w1.PointerAt(inEdge: false, inOverlay: true);
            w1.PointerAt(inEdge: false, inOverlay: false);
            clock.Advance(0.49);
            w1.TickReveal();
            bool stays = w1.RevealShown;
            clock.Advance(0.02);
            w1.TickReveal();
            Expect(stays && !w1.RevealShown, "the overlay did not leave half a second after the pointer");
            // The window remembers the collapsed sidebar (window restore).
            List<SavedWindow> saved = WindowStore.FromJson(System.Text.Json.Nodes.JsonNode.Parse(WindowStore.ToJson(app.Windows.Select(w => w.ToSaved()))));
            Say($"window restore: {saved.Count} window(s), content {saved.FirstOrDefault()?.Content}, sidebar collapsed {saved.FirstOrDefault()?.SidebarCollapsed}, frame {saved.FirstOrDefault()?.Frame}");
            Expect(saved.Count == 1 && saved[0].Content == new WindowContent.Layout(id) && saved[0].SidebarCollapsed && saved[0].Frame.Width >= ViewerWindow.MinWidth,
                "the window's restore record is wrong");
            w1.RevealClock = SystemClockInstance;
            w1.SetSidebarDocked(true);
            await Task.Delay(300);
        }
        finally
        {
            // The store lives on in the self-test's data directory.
            v.DeleteLayout(id);
        }
        Expect(v.Layout(id) is null && w1.State.Content is WindowContent.Scratch, "deleting the layout left the window showing it");
    }

    private static readonly NvrView.Tiles.IViewerClock SystemClockInstance = new SystemViewerClock();

    // Shows a camera in a new tile of the window (a split of its last tile), as the sidebar or a drop would.
    private static StageTile? AddCamera(StageView stage, CameraRef camera)
    {
        NodePath last = stage.Tiles[^1].Path;
        stage.Edit(t => t.Splitting(last, SplitAxis.Row, camera)?.Tree);
        return stage.Tiles.FirstOrDefault(t => t.CameraOrNone == camera);
    }

    private static void RemoveTile(StageView stage, StageTile tile)
    {
        stage.Select(tile.Path);
        stage.RemoveSelected();
    }

    // Renders every indicator symbol once on a gray ground, checks that pixels were drawn, and saves the
    // picture next to the log so a person can look at it.
    private async Task RenderStrip(StageView stage)
    {
        var strip = new IndicatorStrip(24);
        strip.SetItems([.. IndicatorKinds.All.Select(k => new IndicatorItem(k, "camera AI"))]);
        var ground = new Microsoft.UI.Xaml.Controls.Border
        {
            Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0x55, 0x55, 0x55)),
            Padding = new Microsoft.UI.Xaml.Thickness(8),
            Child = strip,
        };
        var rtb = new Microsoft.UI.Xaml.Media.Imaging.RenderTargetBitmap();
        stage.HostTemporarily(ground, true);
        await Task.Delay(300);
        await rtb.RenderAsync(ground);
        stage.HostTemporarily(ground, false);
        byte[] pixels = System.Runtime.InteropServices.WindowsRuntime.WindowsRuntimeBufferExtensions.ToArray(await rtb.GetPixelsAsync());
        int bright = 0;
        for (int i = 0; i + 3 < pixels.Length; i += 4)
        {
            if (pixels[i] > 0xB0 && pixels[i + 1] > 0xB0 && pixels[i + 2] > 0xB0)
            {
                bright++;
            }
        }
        Say($"indicator strip render: {rtb.PixelWidth}x{rtb.PixelHeight}, {bright} bright pixels for {strip.Items.Count} symbols");
        Expect(bright > 200, "the indicator symbols did not draw");
        try
        {
            using var stream = new Windows.Storage.Streams.InMemoryRandomAccessStream();
            Windows.Graphics.Imaging.BitmapEncoder encoder = await Windows.Graphics.Imaging.BitmapEncoder.CreateAsync(Windows.Graphics.Imaging.BitmapEncoder.PngEncoderId, stream);
            encoder.SetPixelData(Windows.Graphics.Imaging.BitmapPixelFormat.Bgra8, Windows.Graphics.Imaging.BitmapAlphaMode.Premultiplied,
                (uint)rtb.PixelWidth, (uint)rtb.PixelHeight, 96, 96, pixels);
            await encoder.FlushAsync();
            stream.Seek(0);
            var png = new byte[stream.Size];
            _ = await stream.ReadAsync(System.Runtime.InteropServices.WindowsRuntime.WindowsRuntimeBufferExtensions.AsBuffer(png), (uint)stream.Size, Windows.Storage.Streams.InputStreamOptions.None);
            File.WriteAllBytes(Path.Combine(Path.GetDirectoryName(options.Log!)!, "indicators.png"), png);
        }
        catch (Exception e) when (e is IOException or InvalidOperationException or System.Runtime.InteropServices.COMException)
        {
            Say($"could not save the indicator picture: {e.Message}");
        }
    }

    private static bool AudioStarted(string? stats)
    {
        if (stats is null)
        {
            return false;
        }
        using var doc = System.Text.Json.JsonDocument.Parse(stats);
        System.Text.Json.JsonElement r = doc.RootElement;
        return r.TryGetProperty("audible", out var a) && a.ValueKind == System.Text.Json.JsonValueKind.True &&
            r.TryGetProperty("starts", out var st) && st.TryGetInt64(out long n) && n >= 1 &&
            r.TryGetProperty("buffered_ms", out var b) && b.GetDouble() > 0;
    }

    private static string Ms(long ns) => ns == 0 ? "n/a" : $"{ns / 1e6:0.0} ms";

    private static long PrivateBytes()
    {
        using var p = Process.GetCurrentProcess();
        return p.PrivateMemorySize64;
    }

    private string Describe(StageTile t)
    {
        TileCounters c = t.Video?.Counters ?? default;
        return $"tile {t.Id} ch{t.Channel}: decoded {c.Decoded}, shown {t.Shown}, failures {c.Failures}, no picture {c.NoPictures}, " +
            $"generations {c.Generations}, resets {c.Resets}, picture {t.PictureWidth}x{t.PictureHeight}, " +
            $"decoder {t.Video?.DecoderKind} ({t.Video?.DecoderName}), status {app.Stage!.StatusText(t) ?? "playing"}";
    }

    private void CheckTierEvents(Viewer v, ulong tile, int from, long generation)
    {
        var events = v.TierEvents.Skip(from).Where(e => e.Tile == tile).ToList();
        bool starting = events.Any(e => e.State == "starting" && e.Generation == generation);
        bool active = events.Any(e => e.State == "active" && e.Generation == generation);
        if (!starting || !active)
        {
            _problems.Add($"tier events for generation {generation}: {string.Join(", ", events.Select(e => $"{e.State}/{e.Generation}"))}");
        }
    }

    private static async Task<float> MaxLevel(Viewer v, StageTile t, double seconds)
    {
        float max = 0;
        var sw = Stopwatch.StartNew();
        while (sw.Elapsed.TotalSeconds < seconds)
        {
            max = Math.Max(max, v.Core.TileAudioLevel(t.Id).Peak);
            await Task.Delay(10);
        }
        return max;
    }

    private async Task Resize(StageRenderer r)
    {
        AppWindow w = app.MainWindow!.AppWindow;
        SizeInt32 start = w.Size;
        long resizes = r.TotalResizes, frames = r.Frames;
        var sw = Stopwatch.StartNew();
        const int steps = 30;
        for (int i = 1; i <= steps; i++)
        {
            double t = i / (double)steps;
            w.Resize(new SizeInt32(start.Width + (int)(240 * t), start.Height + (int)(135 * t)));
            await Task.Delay(16);
        }
        await Task.Delay(400);
        double elapsed = sw.Elapsed.TotalSeconds;
        long made = r.Frames - frames, resized = r.TotalResizes - resizes;
        (uint bw, uint bh) = r.BufferSize;
        (uint ww, uint wh) = r.WantedSize;
        Say($"resize: {steps} steps in {elapsed:0.00} s, {resized} buffer resizes, {made} frames presented, buffers {bw}x{bh} for {ww}x{wh}");
        if (resized > elapsed / 0.1 + 2)
        {
            _problems.Add($"the buffers were resized {resized} times in {elapsed:0.00} s");
        }
        if (bw != ww || bh != wh)
        {
            _problems.Add("the buffers did not settle at the panel's size");
        }
        if (made < elapsed * 20)
        {
            _problems.Add($"the grid stopped presenting during the resize ({made} frames in {elapsed:0.00} s)");
        }
        w.Resize(start);
        await Task.Delay(400);
    }

    private async Task CheckFrame(string what, IReadOnlyList<StageTile> tiles, StageRenderer? renderer = null)
    {
        StageRenderer r = renderer ?? app.Renderer!;
        Task<StageCapture> task = r.CaptureAsync();
        if (await Task.WhenAny(task, Task.Delay(5000)) != task)
        {
            _problems.Add($"no {what} frame was drawn");
            return;
        }
        StageCapture c = await task;
        var parts = new List<string>();
        foreach (StageTile t in tiles)
        {
            (StageTile _, float x, float y, float w, float h) = c.Rects.FirstOrDefault(rect => rect.Tile == t);
            double spread = Spread(c.Image, x, y, w, h);
            parts.Add($"tile {t.Id} {spread:0.0}");
            if (spread < 8)
            {
                _problems.Add($"the {what} picture of tile {t.Id} is flat (spread {spread:0.0})");
            }
        }
        Say($"{what} frame {c.Image.Width}x{c.Image.Height}, spread: {string.Join(", ", parts)}");
    }

    // Standard deviation of brightness inside a rectangle, every 4th pixel.
    private static double Spread(RenderedImage image, float x0, float y0, float w, float h)
    {
        double sum = 0, sumSq = 0, n = 0;
        int x1 = (int)Math.Min(x0 + w, image.Width), y1 = (int)Math.Min(y0 + h, image.Height);
        for (int y = Math.Max(0, (int)y0); y < y1; y += 4)
        {
            for (int x = Math.Max(0, (int)x0); x < x1; x += 4)
            {
                (int cr, int cg, int cb) = image.Rgb(x, y);
                double value = (cr + cg + cb) / 3.0;
                sum += value;
                sumSq += value * value;
                n++;
            }
        }
        if (n == 0)
        {
            return 0;
        }
        double mean = sum / n;
        return Math.Sqrt(Math.Max(sumSq / n - mean * mean, 0));
    }

    internal static async Task<bool> Until(Func<bool> condition, double seconds)
    {
        var sw = Stopwatch.StartNew();
        while (sw.Elapsed.TotalSeconds < seconds)
        {
            if (condition())
            {
                return true;
            }
            await Task.Delay(50);
        }
        return condition();
    }

    private bool _finished;

    private void Finish(List<string> problems)
    {
        if (_finished)
        {
            return;
        }
        _finished = true;
        WriteLog(options.Log!, problems, _lines);
        app.Quit(problems.Count == 0 ? 0 : 1);
    }

    /// <summary>Writes the measurements and the verdict.</summary>
    public static void WriteLog(string path, IReadOnlyList<string> problems, IReadOnlyList<string> lines)
    {
        var sb = new StringBuilder();
        foreach (string l in lines)
        {
            sb.AppendLine(l);
        }
        foreach (string p in problems)
        {
            sb.AppendLine($"  {p}");
        }
        sb.AppendLine(problems.Count == 0 ? "selftest: PASS" : "selftest: FAIL");
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        File.WriteAllText(path, sb.ToString());
    }
}
