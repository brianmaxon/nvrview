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
// 11b. calls (docs/UX.md "Doorbell calls"), against the mock's talk-capable channel and
//    fake microphone, voice, and recognizer (never real hardware): Start conversation opens a
//    call with no press (audible, a card over the stage, the camera's tapped audio reaching the
//    recognizer, nothing sent by itself); Talk is refused until talk-back is allowed for the
//    camera and then really talks (the core reports the audio it sent); a typed reply is rendered
//    by the voice, pushed in one go and ended with a draining stop; captions reach the card; a
//    doorbell press (an injected alarm) joins the call or opens one with the Unmute control;
//    a call ends a minute after its last activity (manual clock) and gives the audio back; the
//    tile menu offers the conversation and the permission. A call pops out into a call window of
//    its own (its own swap chain and renderer drawing the doorbell's shared tile) and back, with
//    talk, "Keep talking", and the unsent reply untouched and the leases following; the window's
//    close and Esc end the call, it never expires while popped out, and its frame, pin, and the
//    "Open calls in their own window" routing are remembered. Then, on the real computer: the
//    system voice renders a sentence, the desktop recognizer (where Windows has one) hears it,
//    and with --mic-test the microphone opens for a moment (its audio is measured and dropped);
// 11e. camera motion (docs/UX.md "Pan, tilt, and zoom"), against the mock's cameras that move (SelfTest.Ptz.cs):
//    a held pad button renews its move at least every 500 ms and stops on release, an Alt arrow does the same, the
//    app going quiet lets the camera stop by itself (the core's dead-man event), presets recall from the menu and
//    with Alt+2, the lens stays in its range (the core clamps what is sent past it) and a drag sends the first and
//    the last position, and a multi-lens camera's second lens is moved as its own camera;
// 11c. the header and the Settings window (docs/UX.md "Windows header", "Settings window"): the
//    header is the title bar with its quick actions, Settings, the "…" menu, and the layout picker, and
//    hands its controls to the system; in full screen it hides and is revealed at the top edge on a
//    manual clock; the tile menu holds none of the app-wide items; Settings is one window of its own
//    with a pane of six sections, a section taller than the window scrolls to its last control, a
//    second open brings the same window forward, its size is remembered, and it closes with the app;
// 12. with --credential-test, a throwaway credential is written, read back,
//    and deleted in Credential Manager.
//
// The steps are in areas (scripts/windows-selftest-areas.txt; the code is in SelfTest.<Area>.cs files);
// `--areas a,b` runs only those and the startup smoke, so the test result cache can rerun what a change touches.
//
// Every step carries a parity marker (a comment naming a kebab-case ID) above its code, shared with
// the macOS self-test where it checks the same thing (docs/PARITY.md;
// scripts/check-parity.py compares them).
//
// Writes what it measured and PASS or FAIL to the --log file and exits 0 or 1.

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

internal sealed partial class SelfTest(App app, AppOptions options)
{
    private readonly List<string> _problems = [];
    private readonly List<string> _lines = [];
    private readonly Stopwatch _clock = Stopwatch.StartNew();

    /// <summary>
    /// The self-test's areas: scripts/windows-selftest-areas.txt puts every step in exactly one, and
    /// scripts/suite-cache.py fingerprints each. "startup" is the smoke that always runs.
    /// </summary>
    internal static readonly string[] AreaNames =
        ["startup", "viewer", "tiles", "layouts", "header", "settings", "calls", "recordings", "capture", "ptz"];

    /// <summary>True when the run covers the area (without --areas, all of them).</summary>
    private bool Runs(string area) => area == "startup" || options.Areas is null || options.Areas.Contains(area);

    private bool RunsAny(params string[] areas) => areas.Any(Runs);

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
        try
        {
            if (Runs("settings"))
            {
                // parity: updates-notes-scroll
                _problems.AddRange(UpdateNotesSelfTest.Run());
            }
        }
        catch (Exception e)
        {
            _problems.Add($"the update notes step threw: {e}");
        }
        try
        {
            if (Runs("settings"))
            {
                // parity: updates-bundled-defaults
                _problems.AddRange(UpdateDefaultsSelfTest.Run(app.Viewer!, line => Say(line)));
            }
        }
        catch (Exception e)
        {
            _problems.Add($"the bundled update defaults step threw: {e}");
        }
        Finish(_problems);
    }

    private async Task Steps()
    {
        Viewer v = app.Viewer!;
        StageView stage = app.Stage!;
        StageRenderer r = app.Renderer!;
        Say($"GPU: {v.Host.Gpu.Describe()}");
        Say($"areas: {(options.Areas is null ? "all" : string.Join(", ", AreaNames.Where(Runs)))}");

        // parity: credential-store
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

        // parity: grid-plays
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
        // parity: third-party-channel
        if (options.MockChannels >= 2)
        {
            StageTile last = Tiles[^1];
            Say($"third-party channel: waited {v.WaitedTiles.Contains(last.Id)}, now {app.Stage!.StatusText(last) ?? "playing"}");
            if (!v.WaitedTiles.Contains(last.Id))
            {
                _problems.Add("the third-party channel never reported waiting for the camera");
            }
        }

        // parity: render-no-alloc
        // Steady state: the render thread allocates nothing per frame.
        long bytes = r.AllocatedBytes, framesBefore = r.Frames;
        await Until(() => r.Frames - framesBefore >= 30, 10);
        long allocated = r.AllocatedBytes - bytes, framesDrawn = r.Frames - framesBefore;
        Say($"render thread: {allocated} bytes allocated over {framesDrawn} frames");
        if (allocated > 0)
        {
            _problems.Add($"the render thread allocated {allocated} bytes in steady state");
        }

        // parity: wrong-password
        // A wrong password: login_failed, said in words, no retry.
        if (Runs("settings") && app.MockAddress is { } mock)
        {
            DeviceEntry wrong = v.Connect(new DeviceConfig { Name = "wrong password", Host = mock.Host, Port = mock.Port, User = "example-user" }, "not-the-password");
            bool refused = await Until(() => wrong.State == "login_failed", 10);
            Say($"wrong password: {wrong.State}, \"{StageView.DeviceText(wrong)}\"");
            if (!refused)
            {
                _problems.Add("a wrong password did not end in login_failed");
            }
            v.Disconnect(wrong);

            // parity: settings-save-remove
            // What the Settings window does on Save and Remove (in memory in
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

            // parity: settings-dialog-opens
            // The window itself opens and closes. Its device search is real but
            // limited to loopback (nothing answers there); the answers are made up
            // and fed in as the core's events.
            v.DiscoveryRequest = "{\"seconds\":30,\"interfaces\":[\"127.0.0.1\"],\"onvif\":false}";
            var hooks = new SettingsPagesHooks();
            SettingsWindow settings = SettingsWindow.Open(v, development: true, SettingsSection.Devices, hooks: hooks, background: true);
            await Until(() => settings.IsLoaded, 5);
            bool shown = settings.IsLoaded;
            Say($"settings window shown: {shown}");
            if (!shown)
            {
                _problems.Add("the Settings window did not open");
            }
            await FindDevices(v, hooks, mock);
            // parity: discovery-stop-on-close
            settings.Close();
            bool stopped = await Until(() => !v.Discovery.IsSearching, 5);
            Say($"find devices: search stopped when the window closed: {stopped}");
            if (!stopped)
            {
                _problems.Add("closing Settings did not stop the device search");
            }
            v.DiscoveryRequest = null;
        }

        if (RunsAny("header", "settings"))
        {
            await HeaderAndSettings(v);
        }

        // parity: resize
        // 2. Live resize.
        await Resize(r);

        // parity: key-selection-right
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

        if (Runs("viewer"))
        {
            await ViewerSteps(v, stage, r, first);
        }

        // parity: decode-failures
        long failuresBefore = Tiles.Sum(t => (t.Video?.Counters.Failures ?? 0) + (t.Video?.Counters.ConfigureFailures ?? 0));
        Say($"decode failures before the device loss: {failuresBefore}");
        if (failuresBefore > 0)
        {
            _problems.Add($"{failuresBefore} decode failures");
        }
        long memoryAfter = PrivateBytes();

        // parity: device-loss
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

        // A group runs when one of its areas was asked for; a group that holds steps of two areas runs
        // whole and gates the inner steps itself.
        if (RunsAny("tiles", "header", "capture"))
        {
            await TileFeatures(v, stage);
        }
        if (RunsAny("tiles", "capture"))
        {
            await FisheyeViews(v, stage, r);
        }
        if (Runs("layouts"))
        {
            await LayoutsAndWindows(v);
        }
        if (RunsAny("calls", "capture"))
        {
            await Calls(v, stage);
        }
        if (Runs("ptz"))
        {
            await PtzSteps(v, stage);
        }
        if (Runs("recordings"))
        {
            await RecordingsSteps(v);
        }

        if (Runs("tiles"))
        {
            // parity: detection-events
            // Detections from the mock (camera AI) reached the tiles.
            Say($"detection events: {v.DetectionEvents}, shown now: " +
                string.Join(", ", Tiles.SelectMany(t => v.Board.Items(t.Camera).Select(i => i.Description)).Distinct()));
            if (v.DetectionEvents == 0)
            {
                _problems.Add("no detection events arrived");
            }
        }

        // parity: tile-retry
        // The device goes away: tiles say they reconnect, with a countdown. The viewer clock is
        // held from here, at the system clock's reading, so the Retry delay below is measured on
        // one clock from the moment the messages appear.
        var retryClock = new ManualViewerClock(new SystemViewerClock().Now);
        v.UseClock(retryClock);
        app.StopMock();
        // parity: tile-reconnect-text
        bool reconnecting = await Until(() => Tiles.All(t => app.Stage!.StatusText(t) is { } s && s.Contains("onnect", StringComparison.Ordinal)), 15);
        Say($"mock-nvr stopped: {app.Stage!.StatusText(Tiles[0]) ?? "playing"}");
        if (!reconnecting)
        {
            _problems.Add("tiles did not say they were reconnecting after the device went away");
        }

        // After 30 s of that on the viewer clock, a Retry button shows under the message; pressing it
        // reaches the core and takes the button away until another 30 s have passed.
        stage.Tick();
        bool early = Tiles.Any(t => stage.ChromeFor(t)?.RetryShown == true);
        if (early)
        {
            Say("retry button early on: " + string.Join("; ", Tiles.Where(t => stage.ChromeFor(t)?.RetryShown == true)
                .Select(t => $"tile {t.Id} {stage.StatusOf(t).Kind} \"{stage.StatusOf(t).Text}\"")));
        }
        retryClock.Advance(TileRetryRule.OfferAfter + 1);
        stage.Tick();
        bool offered = Tiles.All(t => stage.ChromeFor(t)?.RetryShown == true);
        Say($"retry button: early {early}, after {TileRetryRule.OfferAfter:0} s {offered}");
        if (early || !offered)
        {
            _problems.Add("the Retry button did not follow the 30 s delay");
        }
        stage.ChromeFor(Tiles[0])?.PressRetry();
        if (stage.ChromeFor(Tiles[0])?.RetryShown == true)
        {
            _problems.Add("the Retry button stayed after it was pressed");
        }
        v.UseClock(new SystemViewerClock());

        // parity: core-contract
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

    // The bar colors of the *-colors fixtures (COLOR_BARS in fixtures/synthetic/make-video.sh), left to right, 32 x 64 px each.
    private static readonly (int R, int G, int B)[] ColorBars =
    [
        (0xB4, 0x28, 0x28), (0x28, 0xA0, 0x3C), (0x32, 0x3C, 0xBE), (0xDC, 0xDC, 0xDC), (0x80, 0x80, 0x80), (0x18, 0x18, 0x18),
    ];

    // The decoded JPEG as BGRA rows, with its size.
    private static async Task<(int Width, int Height, byte[] Pixels)?> DecodeJpeg(byte[] jpeg)
    {
        try
        {
            using var stream = new Windows.Storage.Streams.InMemoryRandomAccessStream();
            _ = await stream.WriteAsync(System.Runtime.InteropServices.WindowsRuntime.WindowsRuntimeBufferExtensions.AsBuffer(jpeg));
            stream.Seek(0);
            var decoder = await Windows.Graphics.Imaging.BitmapDecoder.CreateAsync(stream);
            var data = await decoder.GetPixelDataAsync(Windows.Graphics.Imaging.BitmapPixelFormat.Bgra8,
                Windows.Graphics.Imaging.BitmapAlphaMode.Straight, new Windows.Graphics.Imaging.BitmapTransform(),
                Windows.Graphics.Imaging.ExifOrientationMode.IgnoreExifOrientation, Windows.Graphics.Imaging.ColorManagementMode.DoNotColorManage);
            return ((int)decoder.PixelWidth, (int)decoder.PixelHeight, data.DetachPixelData());
        }
        catch (Exception e) when (e is InvalidOperationException or System.Runtime.InteropServices.COMException or ArgumentException)
        {
            return null;
        }
    }




    private static CoreEvent Alarm(DeviceEntry device, int channel, string kind, bool start) => new()
    {
        Type = "alarm",
        Device = device.Id,
        Channel = channel,
        Kind = kind,
        State = start ? "start" : "stop",
    };






    private static bool Near(double a, double b) => Math.Abs(a - b) < 1.5;




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



    /// <summary>The texts of a menu's items (submenus by their own text), top level only.</summary>
    private static List<string> Texts(IList<Microsoft.UI.Xaml.Controls.MenuFlyoutItemBase> items) =>
    [
        .. items.Select(i => i switch
        {
            Microsoft.UI.Xaml.Controls.MenuFlyoutItem m => m.Text,
            Microsoft.UI.Xaml.Controls.MenuFlyoutSubItem sub => sub.Text,
            _ => "",
        }).Where(t => t.Length > 0),
    ];





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
