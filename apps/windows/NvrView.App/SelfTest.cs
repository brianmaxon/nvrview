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
            // parity: updates-notes-scroll
            _problems.AddRange(UpdateNotesSelfTest.Run());
        }
        catch (Exception e)
        {
            _problems.Add($"the update notes step threw: {e}");
        }
        try
        {
            // parity: updates-bundled-defaults
            _problems.AddRange(UpdateDefaultsSelfTest.Run(app.Viewer!, line => Say(line)));
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
        await LivePacingStep(r);
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

        await HeaderAndSettings(v);

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

        // parity: focus-main-stream
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

        // parity: audio-level-muted
        // 5. Listening.
        float muted = await MaxLevel(v, first, 0.5);
        Say($"muted level {muted:0.00}");
        if (muted <= 0)
        {
            _problems.Add("no audio level while muted");
        }
        long rendered = app.Audio?.FramesRendered ?? 0;
        // parity: audio-listen
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
        // parity: audio-output-renders
        bool started = app.Audio is not { Problem: null } || await Until(() => AudioStarted(first.AudioStats), 8);
        Say($"playout started after {heard.Elapsed.TotalSeconds:0.00} s: {started}");
        if (!started)
        {
            _problems.Add("the core never started playing the audible tile");
        }
        if (app.Audio is { Problem: null })
        {
            await Until(() => app.Audio.FramesRendered > rendered, 3);
        }
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

        // parity: unfocus-sub-stream
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

        // parity: zoom-digital
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
        await CheckZoomFillsTile();
        await CheckZoomChrome();
        await CheckFillTiles();
        await CheckFillPan();
        await CheckZoomPerTile();
        await CheckPanAndFrames();

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

        await TileFeatures(v, stage);
        await FisheyeViews(v, stage, r);
        await LayoutsAndWindows(v);
        await Calls(v, stage);
        await PtzSteps(v, stage);
        await RecordingsSteps(v);
        await PlaybackPacingStep(v);

        // parity: detection-events
        // Detections from the mock (camera AI) reached the tiles.
        Say($"detection events: {v.DetectionEvents}, shown now: " +
            string.Join(", ", Tiles.SelectMany(t => v.Board.Items(t.Camera).Select(i => i.Description)).Distinct()));
        if (v.DetectionEvents == 0)
        {
            _problems.Add("no detection events arrived");
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

    // 11d. The Recordings window (docs/UX.md "Recordings"). First the entry points on the real service (the header's
    // menu, one window, the playback closed with it); then the steps of the macOS self-test on a model over the fakes,
    // shown in the same window class (the mock-nvr has no playback, and a made-up store makes the numbers exact).
    private async Task RecordingsSteps(Viewer v)
    {
        if (app.Recordings is not { Available: true })
        {
            Say("recordings: the core has none; skipped");
            return;
        }
        List<string> overflow = Texts(app.Windows[0].Header.BuildOverflow().Items);
        Expect(overflow.Contains("Recordings"), "the header's menu lacks Recordings");
        RecordingsWindow? first = app.OpenRecordings();
        bool loaded = first is not null && await Until(() => first.IsLoaded, 5);
        RecordingsWindow? again = app.OpenRecordings();
        Say($"recordings: window {(first is null ? "not opened" : "opened")}, loaded {loaded}, second open the same {ReferenceEquals(first, again)}, created {RecordingsWindow.Created}");
        Expect(loaded && ReferenceEquals(first, again) && RecordingsWindow.Created == 1, "the Recordings window did not open once");
        first?.Close();
        await Until(() => RecordingsWindow.Current is null, 5);

        var ui = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        RecordingsService service = null!;
        var fake = new FakeRecordings(json => service.Route(Encoding.UTF8.GetBytes(json))) { RecordingWorkIsManual = true };
        var playback = new FakePlayback(json => service.Route(Encoding.UTF8.GetBytes(json)), fake) { RecordingWorkIsManual = true };
        var host = new PlainPlaybackHost(playback);
        var trashed = new List<string>();
        var revealed = new List<IReadOnlyList<string>>();
        service = new RecordingsService(fake, host)
        {
            Post = a => _ = ui.TryEnqueue(() => a()),
            Now = () => DateTimeOffset.UtcNow,
            Trasher = trashed.Add,
            Revealer = revealed.Add,
            CameraNamer = c => $"Camera {c.Channel + 1}",
        };
        ThumbnailDecodeQueue thumbnailQueue = App.UseThumbnailDecoder(service);
        service.Start(Path.Combine(Path.GetTempPath(), "nvrview-selftest-incidents-" + Environment.ProcessId));
        Task<bool> Playing(Func<bool> condition, double seconds) => Until(() =>
        {
            playback.CompletePlaybackWork();
            return condition();
        }, seconds);

        // parity: recordings-enable
        RecordingCamera[] cams = [.. Enumerable.Range(0, 3).Select(c => new RecordingCamera("d-selftest", c))];
        foreach (RecordingCamera c in cams)
        {
            service.SetEnabled(c, true);
        }
        Expect(service.EnabledCameras.SequenceEqual(cams), "recordings: the chosen cameras are not the enabled ones");
        RecordingsWindow w = RecordingsWindow.Open(v, v.Host, service, background: true);
        RecordingsBrowser b = w.Browser;
        RecordingsPlayer p = b.Player;
        await Until(() => w.IsLoaded, 5);

        // parity: recordings-hold-today
        bool held = await Until(() => fake.Holds.TryGetValue(b.HoldId, out var h) && h.Count == 3 && b.Lanes.Count == 3, 5);
        Say($"recordings: hold {b.HoldId} on {(fake.Holds.TryGetValue(b.HoldId, out var held3) ? held3.Count : 0)} cameras, {b.Lanes.Count} lanes");
        Expect(held && b.Holding, "recordings: today is not held");

        // parity: recordings-calendar-lanes
        bool shown = await Until(() => b.RecordedDays.Contains(b.Day) && b.Lanes.All(l => l.Spans.Count > 0) && b.DayIncidents.Count == 6 && w.IncidentRows().Count == 6, 5);
        int events = b.Lanes.Sum(l => l.Events.Count);
        Say($"recordings: calendar {b.RecordedDays.Count} days, {events} events, {b.DayIncidents.Count} incidents, {w.IncidentRows().Count} rows");
        Expect(shown && events == 6, "recordings: the calendar, lanes, or incident list are not what the store holds");
        Expect(w.CalendarDays().Any(d => d.Day == b.Day && d.Recorded && d.Enabled), "recordings: the calendar does not mark today");
        if (b.Lanes[0].Events.Count > 0)
        {
            b.Select(b.Lanes[0].Events[0]);
            Expect(b.Selected?.Involves(b.Lanes[0].Camera) == true, "recordings: a timeline click selected nothing");
            b.Selection = new HashSet<string>();
            p.Close();
        }

        // parity: recordings-navigate-day
        long today = b.Day;
        b.PreviousDay();
        Expect(!b.Holding && !fake.Holds.ContainsKey(b.HoldId), "recordings: yesterday is held");
        bool yesterday = await Until(() => b.DayIncidents.Count == 6 && b.RecordedDays.Contains(today - DeviceTime.Day), 5);
        b.NextDay();
        bool heldBack = b.Day == today && fake.Holds.TryGetValue(b.HoldId, out var heldAgain) && heldAgain.Count == 3;
        Say($"recordings: yesterday {(yesterday ? "has" : "lacks")} its incidents; today held again {heldBack}");
        Expect(yesterday && heldBack, "recordings: navigating a day did not ask for it or hold today again");
        await Until(() => b.DayIncidents.Count == 6, 5);

        // parity: recordings-thumbnail
        await RecordingsThumbnails(service, fake, b, w);

        // parity: recordings-lock
        Incident? vehicle = b.DayIncidents.FirstOrDefault(i => i.Kinds.SequenceEqual(["vehicle"]));
        if (vehicle is not null)
        {
            b.SetLocked(vehicle.Key, true);
            bool locked = await Until(() => b.DayIncidents.FirstOrDefault(i => i.Key == vehicle.Key)?.Locked == true && service.Downloads.Count == 1, 5);
            Say($"recordings: locked; {service.Notice ?? "no message"}; downloads {service.Downloads.Count}");
            Expect(locked && service.Notice?.Contains("downloading", StringComparison.Ordinal) == true, "recordings: locking did not queue the download and say so");
            b.LockedOnly = true;
            Expect(b.Incidents.Select(i => i.Key).SequenceEqual([vehicle.Key]), "recordings: the Locked filter shows another list");
            b.LockedOnly = false;
            b.SetLocked(vehicle.Key, false);
            await Until(() => b.DayIncidents.FirstOrDefault(i => i.Key == vehicle.Key)?.Locked == false, 5);
            Expect(service.Downloads.Count == 1, "recordings: unlocking touched the download");
        }
        else
        {
            _problems.Add("recordings: no vehicle incident to lock");
        }

        // parity: recordings-downloads
        if (service.Downloads.Count == 1)
        {
            DownloadProgress queued = service.Downloads[0];
            b.Center = RecordingsCenter.Downloads;
            w.Refresh();
            Expect(w.DownloadsShown && !w.PlayerColumnShown, "recordings: the downloads panel is not shown with its tab");
            service.Pause(queued.Download, true);
            await Until(() => service.Downloads.FirstOrDefault()?.Paused == true, 5);
            fake.CompleteRecordingWork();
            Expect(service.Downloads.FirstOrDefault()?.State != "done", "recordings: a paused download finished");
            // The panel follows the download's events (the core's shape, with null members): Downloading, then Done.
            var seen = new List<string>();
            void Seen() => seen.AddRange(w.DownloadCards().Select(c => c.State));
            service.DownloadsChanged += Seen;
            service.Pause(queued.Download, false);
            await Until(() => service.Downloads.FirstOrDefault()?.Paused == false, 5);
            fake.CompleteRecordingWork();
            bool done = await Until(() => service.Downloads.FirstOrDefault()?.State == "done", 5);
            service.DownloadsChanged -= Seen;
            Say($"recordings: the downloads panel showed {string.Join(", ", seen.Distinct())}");
            Expect(seen.Contains("Downloading") && seen.LastOrDefault() == "Done" && w.DownloadCards().Any(c => c.State == "Done"),
                "recordings: the downloads panel did not show the progress and Done");
            DownloadProgress? finished = service.Downloads.FirstOrDefault();
            if (done && finished is not null)
            {
                service.Reveal(finished);
                Expect(revealed.LastOrDefault()?.SequenceEqual(finished.Outputs()) == true, "recordings: Show in Explorer showed something else");
                service.Forget(finished.Download);
                Incident? other = b.DayIncidents.FirstOrDefault(i => i.Key != vehicle?.Key);
                if (other is not null)
                {
                    b.Download(other.Key);
                    await Until(() => service.Downloads.Count == 1 && service.Downloads[0].Download != finished.Download, 5);
                    string? folder = service.Downloads.FirstOrDefault()?.Folder;
                    if (folder is not null)
                    {
                        service.Trash(service.Downloads[0].Download);
                        bool moved = await Until(() => trashed.Count > 0, 5);
                        Say($"recordings: downloads done, forgotten, trashed {trashed.Count}");
                        Expect(moved && trashed.SequenceEqual([folder]) && service.Downloads.Count == 0, "recordings: the Recycle Bin did not get the folder");
                    }
                }
            }
            else
            {
                _problems.Add("recordings: the download did not finish after the resume");
            }
            b.Center = RecordingsCenter.Player;
            w.Refresh();
        }

        // parity: recordings-calendar-month
        int asked = fake.MonthRequests;
        long shownMonth = b.Month;
        b.ShowMonth(-1);
        long previous = b.Month;
        bool paged = await Until(() => fake.MonthRequests > asked && b.RecordedDays.Count > 0, 5);
        Say($"recordings: previous month asked {fake.MonthRequests - asked} time(s), {b.RecordedDays.Count} days marked");
        Expect(paged && fake.MonthRequests == asked + 1 && b.RecordedDays.All(d => DeviceTime.Month(d).Start == previous), "recordings: paging the calendar did not ask for that month alone");
        b.ShowMonth(1);
        b.ShowMonth(-1);
        Expect(fake.MonthRequests == asked + 1, "recordings: a month already asked was asked again");
        b.ShowMonth(1);
        Expect(b.Month == shownMonth, "recordings: the calendar did not come back to the shown day's month");

        // parity: recordings-camera-names
        List<string> names = [.. b.Lanes.Select(l => l.Name)];
        List<string> listed = [.. b.Cameras.Select(c => $"Camera {c.Channel + 1}")];
        Say($"recordings: camera names {string.Join(", ", names)}");
        Expect(names.SequenceEqual(listed), "recordings: the lanes are not named as the camera list names them");

        // parity: playback-open
        Incident? firstIncident = b.DayIncidents.OrderBy(i => i.Start).FirstOrDefault();
        if (firstIncident is null)
        {
            _problems.Add("recordings: no incident to play");
        }
        else
        {
            b.Click(firstIncident.Key, extend: false);
            bool open = await Playing(() => p.Playback is not null && p.Cells.Count == 1 && p.Cells[0].State == "playing", 5);
            long start = (firstIncident.Start - firstIncident.Margin) * 1000;
            Say($"recordings: playback {p.Playback} of {p.Cells.Count} cameras, {p.State}, opens {p.Opens}");
            Expect(open && p.PositionMs >= start && p.PositionMs <= start + 5_000 && p.Transport.IsPlaying, "recordings: the selected incident did not start playing at its start less the margin");
            // The fake has no incident of three cameras; with nothing selected the player keeps the cameras added to it.
            b.Selection = new HashSet<string>();
            p.Add(cams.First(c => !p.Cameras.Contains(c)));
            p.Add(cams.First(c => !p.Cameras.Contains(c)));
            await Playing(() => p.Cells.Count == 3, 5);
            await Until(() => w.Renderer?.Layout.Tiles.Length == 3, 5);
            Expect(p.Cells.Count == 3 && w.Renderer?.Layout.Tiles.Length == 3, "recordings: the window's renderer does not draw the playback's three cameras");

            // parity: playback-transport
            int opens = p.Opens;
            p.Pause();
            await Playing(() => p.State == NvrView.Interop.NvPlaybackState.Paused, 5);
            long before = p.PositionMs;
            p.Skip(TransportState.SkipMs);
            Expect(p.PositionMs == Math.Min(before + TransportState.SkipMs, p.EndMs), "recordings: +10 s moved somewhere else");
            p.SetRate(4);
            Expect(PlaybackSpeed.Hint(p.Rate) == "Keyframes only, no sound" && p.Cells.All(c => !host.Audio.TryGetValue(c.Tile, out var a) || !a.Audible), "recordings: at 4x the hint or the sound is wrong");
            w.Refresh();
            Expect(w.TransportShown().Hint == "Keyframes only, no sound" && w.TransportShown().Speed == "4×", "recordings: the transport does not show the speed and its hint");
            p.SetRate(1);
            p.Play();
            await Playing(() => p.State == NvrView.Interop.NvPlaybackState.Playing && p.Rate == 1, 5);
            bool[] audible = [.. p.Cells.Select(c => host.Audio.TryGetValue(c.Tile, out var a) && a.Audible)];
            Say($"recordings: paused, skipped 10 s, 4x and back, playing; audible {string.Join(",", audible)}");
            Expect(audible.SequenceEqual([true, false, false]) && p.Opens == opens, "recordings: at 1x the first camera should be audible, with no reopen");

            // parity: playback-grid
            p.Click(1);
            PlayerGridMode emphasis = p.Mode;
            bool[] afterClick = [.. p.Cells.Select(c => host.Audio.TryGetValue(c.Tile, out var a) && a.Audible)];
            p.DoubleClick(1);
            PlayerGridMode single = p.Mode;
            p.DoubleClick(1);
            Expect(emphasis.Equals(new PlayerGridMode.Emphasis(1)) && single.Equals(new PlayerGridMode.Single(1)) && p.Mode.Equals(PlayerGridMode.Tiled), "recordings: the grid did not go large, single, and back");
            Expect(afterClick.SequenceEqual([false, true, false]), "recordings: the large camera's sound did not play");
            p.Muted = true;
            Expect(p.Cells.All(c => !host.Audio.TryGetValue(c.Tile, out var a) || !a.Audible), "recordings: muted, a camera is audible");
            p.Muted = false;
            w.Refresh();
            await Until(() => w.CellRects.Count(r => r is not null) == 3, 5);
            Expect(w.CellRects.Count(r => r is not null) == 3 && w.LabelTexts().Count >= 3, "recordings: the player's cells or labels are missing");

            // parity: playback-scrub
            Incident? playing = b.PlayingIncident;
            TimelineLane? lane = b.Lanes.FirstOrDefault(l => p.Cameras.Contains(l.Camera));
            if (playing is not null && lane is not null)
            {
                long target = playing.End + 2;
                int seeks = p.Seeks;
                b.LaneClick(lane.Camera, target);
                Expect(p.Seeks == seeks + 1 && p.PositionMs == target * 1000 && p.Opens == opens, "recordings: a lane click did not seek");
                // The scrubber seeks the same way (a drag shows keyframes, the release the exact picture).
                w.Refresh();
                (long rs, long re) = w.Scrubber.Range;
                Expect(re > rs, "recordings: the scrubber has no range");
                long point = rs + (re - rs) / 4;
                w.Scrubber.PointAt(point);
                Expect(Math.Abs(p.PositionMs - point) <= 1, "recordings: the scrubber did not seek to the point");
                await Playing(() => p.State == NvrView.Interop.NvPlaybackState.Playing, 5);
            }
            else
            {
                _problems.Add("recordings: no lane of a playing camera");
            }
        }

        await ExportSteps(w, fake, service);

        // parity: recordings-timeline-wheel-zoom
        // The lanes' wheel as the window gets it: a notch zooms around the pointer (its time stays under it), many
        // clamp at 48x, Shift and the wheel pans, and notches the other way go back to the whole day.
        {
            LanesView lanes = w.LanesShown;
            b.Timeline = new TimelineViewport();
            w.Refresh();
            double before = lanes.Viewport.FractionAtPointer(0.25);
            lanes.Wheel(120, horizontal: false, shift: false, 0.25);
            TimelineViewport one = b.Timeline;
            Expect(Math.Abs(one.Zoom - 1.25) < 1e-9 && Math.Abs(one.FractionAtPointer(0.25) - before) < 1e-9,
                $"recordings: a wheel notch made {one}, the time under the pointer moved");
            for (int i = 0; i < 40; i++)
            {
                lanes.Wheel(360, horizontal: false, shift: false, 0.25);
            }
            TimelineViewport zoomed = b.Timeline;
            Expect(zoomed.Zoom == TimelineViewport.MaxZoom, $"recordings: the wheel zoomed to {zoomed.Zoom}x, not 48x");
            lanes.Wheel(-120, horizontal: false, shift: true, 0.25);
            Expect(b.Timeline.Start > zoomed.Start && b.Timeline.Zoom == zoomed.Zoom, "recordings: Shift and the wheel did not pan later");
            for (int i = 0; i < 40; i++)
            {
                lanes.Wheel(-360, horizontal: false, shift: false, 0.25);
            }
            Say($"recordings: wheel zoom {one.Zoom}x, to 48x, panned from {zoomed.Start:0.000}, back to {b.Timeline}");
            Expect(b.Timeline == new TimelineViewport(), $"recordings: wheeling back left {b.Timeline}");
        }

        // parity: recordings-listen-for-sounds
        // A camera's switch reaches nv_sound_watch and its sound_watch state comes back; a fifth camera of one device
        // is refused; switching off takes it off the core's list.
        RecordingCamera[] heard = [.. Enumerable.Range(0, 5).Select(c => new RecordingCamera("d-selftest", c))];
        foreach (RecordingCamera c in heard.Take(4))
        {
            Expect(service.SetListening(c, true), $"recordings: listening to {c} was refused ({service.Notice})");
        }
        Expect(!service.SetListening(heard[4], true), "recordings: a fifth camera of a device listens");
        Expect(fake.Watched.SequenceEqual(heard.Take(4)), $"recordings: the core listens to {fake.Watched.Count} cameras");
        bool watching = await Until(() => service.ListenStates.TryGetValue(heard[0], out var st) && st.State == "watching", 5);
        Expect(watching, "recordings: no sound_watch state for a listening camera");
        foreach (RecordingCamera c in heard.Skip(1))
        {
            service.SetListening(c, false);
        }
        Expect(fake.Watched.SequenceEqual([heard[0]]), "recordings: switching off left other cameras listening");
        Say($"recordings: listening to {service.ListeningCameras.Count} camera, the fifth refused: {service.Notice}");

        // parity: recordings-loudness-lanes
        // A listening camera's lane has loudness (the fake answers a steady level), the others are blank; zooming in
        // asks again with a finer bucket.
        {
            b.Timeline = new TimelineViewport();
            b.ReloadLoudness(force: true);
            long whole = b.LoudnessBucket;
            List<RecordingCamera> loud = [.. b.Lanes.Where(l => b.LoudnessOf(l.Camera)?.Heard == true).Select(l => l.Camera)];
            Say($"recordings: loudness at {whole} s buckets, heard on {string.Join(", ", loud.Select(c => c.Channel))}");
            Expect(loud.SequenceEqual([heard[0]]), "recordings: loudness is not on the listening camera's lane alone");
            Expect(b.LoudnessOf(heard[0])?.Bucket == whole, "recordings: the loudness lane is not at the bucket asked");
            int reads = b.LoudnessReads;
            b.Timeline = new TimelineViewport(48);
            Expect(b.LoudnessReads == reads + 1 && b.LoudnessOf(heard[0])?.Bucket == b.LoudnessBucket && b.LoudnessBucket < whole,
                $"recordings: zooming in did not ask for finer loudness ({b.LoudnessBucket} s)");
            b.Timeline = new TimelineViewport();
            w.Refresh();
        }

        // parity: recordings-sound-labels
        // A label (as the app's classifier would put it) becomes an event from sound on its camera's lane and an
        // incident of its own, which the Sounds filter alone shows, with the sound badge.
        {
            long at = b.Day + 10 * 3600;
            fake.SoundLabelsPut($$"""{"camera":{{heard[0].Json}},"analyzer":"selftest-v1","start":{{at - 60}},"end":{{at + 60}},"labels":[{"kind":"dog_bark","start":{{at}},"end":{{at + 4}},"confidence":0.9,"label":"dog_bark"}]}""");
            bool labeled = await Until(() => b.DayIncidents.Any(i => i.Kinds.SequenceEqual(["dog_bark"])), 5);
            bool onLane = b.Lanes.FirstOrDefault(l => l.Camera == heard[0])?.Events.Any(e => e.Kind == "dog_bark" && e.Source == SoundKinds.LocalAudio) == true;
            b.SetKindFilter(KindFilter.Sound, true);
            List<string> soundsShown = [.. b.Incidents.Select(i => string.Join("+", i.Kinds))];
            IReadOnlyList<string> badge = b.Incidents.Count > 0 ? SoundKinds.Names(b.Incidents[0].Events) : [];
            Say($"recordings: a sound label on the lane {onLane}, the Sounds filter shows {string.Join(", ", soundsShown)}, badge {string.Join(", ", badge)}");
            Expect(labeled && onLane, "recordings: the sound label is not on its camera's lane");
            Expect(soundsShown.SequenceEqual(["dog_bark"]), "recordings: the Sounds filter does not show the sound alone");
            Expect(badge.SequenceEqual(["Dog barking"]), "recordings: the sound badge is wrong");
            b.SetKindFilter(KindFilter.Sound, false);
            service.SetListening(heard[0], false);
        }

        // parity: recordings-analyze-no-audio
        // Analyze Sounds with nothing downloaded for the day: the analysis fails no_audio with its message, the lane
        // shows no progress and no loudness.
        RecordingCamera? analyzed = null;
        if (b.Lanes.Count > 0)
        {
            RecordingCamera lane0 = b.Lanes[0].Camera;
            Expect(service.Downloads.Count == 0, $"recordings: {service.Downloads.Count} downloads before the analysis steps");
            b.AnalyzeSound(lane0);
            Expect(b.AnalysisOf(lane0)?.State == AnalysisState.Queued, "recordings: Analyze Sounds queued nothing on the lane");
            bool failed = await Until(() => { fake.CompleteRecordingWork(); return service.Analysis(lane0, b.Day)?.State == AnalysisState.Failed; }, 5);
            Say($"recordings: analyze with nothing downloaded: {service.Analysis(lane0, b.Day)?.Reason ?? "no result"}, {service.Notice ?? "no message"}");
            Expect(failed && service.Analysis(lane0, b.Day)?.Reason == "no_audio", "recordings: the analysis did not fail no_audio");
            Expect(service.Notice == SoundAnalysis.Failure("no_audio"), $"recordings: the no_audio message was {service.Notice ?? "missing"}");
            Expect(b.AnalysisOf(lane0) is null, "recordings: a failed analysis still shows progress");
            b.ReloadLoudness(force: true);
            Expect(b.LoudnessOf(lane0)?.Heard != true, "recordings: loudness appeared without audio");

            // parity: recordings-analyze-day
            // After a download: progress, then loudness and a sound label on the lane, and the done message.
            Incident? toDownload = b.DayIncidents.FirstOrDefault(i => i.Kinds.SequenceEqual(["vehicle"])) ?? b.DayIncidents.FirstOrDefault(i => i.Cameras.Count > 0);
            if (toDownload is not null)
            {
                analyzed = toDownload.Cameras[0].Camera;
                b.Download(toDownload.Key);
                bool downloaded = await Until(() => { fake.CompleteRecordingWork(); return service.Downloads.Any(d => d.State == "done"); }, 5);
                Expect(downloaded, "recordings: the download for the analysis did not finish");
                b.AnalyzeSound(analyzed);
                Expect(service.IsAnalyzing(analyzed, b.Day), "recordings: Analyze Sounds shows no progress");
                bool done = await Until(() => { fake.CompleteRecordingWork(); return service.Analysis(analyzed, b.Day)?.State == AnalysisState.Done; }, 5);
                AnalysisEntry? entry = service.Analysis(analyzed, b.Day);
                Say($"recordings: analyzed {entry?.Seconds ?? 0} s, {entry?.Labels ?? 0} labels; {service.Notice ?? "no message"}");
                Expect(done && entry is { Seconds: > 0, Labels: 1 }, $"recordings: the analysis found {entry}");
                Expect(b.AnalysisOf(analyzed) is null, "recordings: a finished analysis still shows progress");
                Expect(entry is not null && service.Notice == SoundAnalysis.Outcome(entry), $"recordings: the done message was {service.Notice ?? "missing"}");
                bool laneShown = await Until(() => b.LoudnessOf(analyzed)?.Heard == true
                    && b.Lanes.FirstOrDefault(l => l.Camera == analyzed)?.Events.Any(e => e.Kind == "smoke_alarm" && e.Source == SoundKinds.LocalAudio) == true, 5);
                Expect(laneShown, "recordings: no loudness or sound label on the lane after the analysis");
            }
            else
            {
                _problems.Add("recordings: no incident to download for the analysis");
            }

            // parity: recordings-analyze-cancel
            // Canceling an analysis before it runs: it ends canceled, shows no progress, and does not come back when
            // the core's work is done.
            RecordingCamera? other = b.Lanes.Select(l => l.Camera).FirstOrDefault(c => c != analyzed);
            if (other is { } o)
            {
                b.AnalyzeSound(o);
                Expect(b.AnalysisOf(o) is not null, "recordings: nothing to cancel");
                b.CancelAnalysis(o);
                bool canceled = await Until(() => service.Analysis(o, b.Day)?.State == AnalysisState.Canceled, 5);
                fake.CompleteRecordingWork();
                Say($"recordings: analysis canceled: {service.Notice ?? "no message"}");
                Expect(canceled && service.Analysis(o, b.Day)?.State == AnalysisState.Canceled, "recordings: the analysis did not end canceled");
                Expect(b.AnalysisOf(o) is null, "recordings: a canceled analysis still shows progress");
                Expect(service.Notice == SoundAnalysis.Outcome(new AnalysisEntry(0, AnalysisState.Canceled)), $"recordings: the cancel message was {service.Notice ?? "missing"}");
                b.ReloadLoudness(force: true);
                Expect(b.LoudnessOf(o)?.Heard != true, "recordings: a canceled analysis left loudness");
            }
            foreach (DownloadProgress d in service.Downloads.ToList())
            {
                service.Forget(d.Download);
            }
            w.Refresh();
        }

        await EditSteps(v, w, b, fake, playback, service, cams);

        // parity: recordings-layout
        // The window at its smallest and at a large size: the side panes give way in a narrow window (inspector first)
        // and come back in a wide one.
        WindowRect saved = w.CurrentFrame;
        double scale = w.Window.Content.XamlRoot?.RasterizationScale ?? 1;
        var results = new List<string>();
        (int Width, int Height)[] sizes =
        [
            ((int)Math.Ceiling(RecordingsPanes.WindowMinWidth * scale), (int)Math.Ceiling(RecordingsPanes.WindowMinHeight * scale)),
            ((int)(900 * scale), (int)(640 * scale)),
            ((int)(1400 * scale), (int)(900 * scale)),
        ];
        foreach ((int wide, int tall) in sizes)
        {
            w.ResizeTo(wide, tall);
            await Until(() => Math.Abs(w.ContentWidth - wide / scale) < 40, 5);
            await Until(() => w.ShownPanes == RecordingsPanes.Shown(w.ContentWidth, true, true) && w.ContentWidth > 0, 5);
            (bool sidebar, bool inspector) = RecordingsPanes.Shown(w.ContentWidth, true, true);
            results.Add($"{w.ContentWidth:0}: sidebar {w.ShownPanes.Sidebar}, inspector {w.ShownPanes.Inspector}");
            Expect(w.ShownPanes == (sidebar, inspector), $"recordings: at {w.ContentWidth:0} the panes are {w.ShownPanes}, expected {(sidebar, inspector)}");
        }
        Say($"recordings: layout {string.Join("; ", results)}");
        Expect(w.ContentWidth > RecordingsPanes.SidebarWidth + RecordingsPanes.InspectorWidth + RecordingsPanes.CenterMin && w.ShownPanes == (true, true), "recordings: the wide window does not show both side panes");
        w.ResizeTo((int)saved.Width, (int)saved.Height);

        // parity: recordings-window-close
        ulong hold = b.HoldId;
        ulong? openPlayback = p.Playback;
        w.Close();
        bool closed = await Until(() => w.IsClosed && RecordingsWindow.Current is null, 5);
        Expect(closed && !fake.Holds.ContainsKey(hold), "recordings: closing the window kept its hold");
        // parity: playback-close
        bool stillOpen = false;
        if (openPlayback is { } id)
        {
            try
            {
                _ = playback.PlaybackPosition(id);
                stillOpen = true;
            }
            catch (NvrView.Interop.NvException)
            {
            }
        }
        Expect(p.Playback is null && !stillOpen, "recordings: closing the window left its playback open");
        Say($"recordings: closed; hold released {!fake.Holds.ContainsKey(hold)}, playback closed {!stillOpen}");
        thumbnailQueue.Dispose();
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

    // A cover keyframe through the real decoder (Media Foundation and WIC): the size, the bars' colors, and a row that shows it.
    private async Task RecordingsThumbnails(RecordingsService service, FakeRecordings fake, RecordingsBrowser b, RecordingsWindow w)
    {
        string? exe = options.MockNvr;
        string? root = exe is null ? null : Path.GetDirectoryName(Path.GetDirectoryName(Path.GetDirectoryName(exe)));
        string fixtures = root is null ? "" : Path.Combine(root, "fixtures", "synthetic");
        if (!Directory.Exists(fixtures))
        {
            _problems.Add("recordings: no synthetic fixtures to decode a cover keyframe from");
            return;
        }
        byte[] ReadKeyframe(string file, bool h265) => AnnexBKeyframes.Last(File.ReadAllBytes(Path.Combine(fixtures, file)), h265);
        // Each codec's colors fixture at its own size, and the resize fixture's last keyframe (480 x 270) shrunk to 320 wide.
        foreach ((string codec, bool h265, NvThumbKind kind) in new[] { ("h264", false, NvThumbKind.H264Keyframe), ("h265", true, NvThumbKind.H265Keyframe) })
        {
            byte[] colors = ReadKeyframe($"{codec}-colors.{codec}", h265);
            Nv12Picture? raw = KeyframeImage.Decode(h265 ? NvrView.Interop.NvCodec.H265 : NvrView.Interop.NvCodec.H264, colors, 192, 96, out _);
            (YuvMatrix? vuiMatrix, bool? vuiFull) = h265 ? HevcColor.Read(colors) : (null, null);
            Say($"recordings: {codec} decoder {raw?.Decoder}, matrix {raw?.Matrix}, full range {raw?.FullRange}; stream says {vuiMatrix?.ToString() ?? "nothing"}, {vuiFull?.ToString() ?? "nothing"}");
            RecordedThumbnail? small = KeyframeImage.Thumbnail(new RecordedThumbnail(kind, 192, 96, 0, colors), out string? why);
            if (small is null)
            {
                // The H.265 decoder comes with the HEVC Video Extension; without it that codec is reported, not failed.
                if (h265 && why?.Contains("no H.265 decoder", StringComparison.Ordinal) == true)
                {
                    Say($"recordings: {codec} cover keyframe skipped ({why})");
                    continue;
                }
                _problems.Add($"recordings: the {codec} cover keyframe did not decode: {why}");
                continue;
            }
            var image = await DecodeJpeg(small.Data);
            if (image is not { } decoded || decoded.Width != 192 || decoded.Height != 96 || small.Width != 192 || small.Height != 96)
            {
                _problems.Add($"recordings: the {codec} cover is {image?.Width}x{image?.Height}, expected 192x96");
                continue;
            }
            int worst = 0;
            for (int i = 0; i < ColorBars.Length; i++)
            {
                int o = (24 * decoded.Width + 16 + 32 * i) * 4;
                (int r, int g, int bl) = (decoded.Pixels[o + 2], decoded.Pixels[o + 1], decoded.Pixels[o]);
                worst = Math.Max(worst, Math.Max(Math.Abs(r - ColorBars[i].R), Math.Max(Math.Abs(g - ColorBars[i].G), Math.Abs(bl - ColorBars[i].B))));
            }
            Say($"recordings: {codec} cover {decoded.Width}x{decoded.Height}, {small.Data.Length} bytes, bars within {worst} codes");
            Expect(worst <= 14, $"recordings: the {codec} cover's bar colors are {worst} codes off");

            byte[] resize = ReadKeyframe($"{codec}-resize.{codec}", h265);
            RecordedThumbnail? shrunk = KeyframeImage.Thumbnail(new RecordedThumbnail(kind, 480, 270, 0, resize), out why);
            var shrunkImage = shrunk is null ? null : await DecodeJpeg(shrunk.Data);
            Say($"recordings: {codec} 480x270 keyframe shrunk to {shrunkImage?.Width}x{shrunkImage?.Height}");
            Expect(shrunkImage is { Width: 320, Height: 180 }, $"recordings: the {codec} 480x270 cover is {shrunkImage?.Width}x{shrunkImage?.Height} ({why})");
        }

        // The window: the first incident's covers are keyframes in the core's cache; its rows decode and show them,
        // and the small pictures are put back.
        Incident? incident = b.DayIncidents.OrderBy(i => i.Start).FirstOrDefault();
        if (incident is null)
        {
            _problems.Add("recordings: no incident for a cover");
            return;
        }
        byte[] keyframe = ReadKeyframe("h264-colors.h264", false);
        foreach (ThumbnailKey key in incident.CoverKeys)
        {
            fake.ThumbnailPut(key.Json, new RecordedThumbnail(NvThumbKind.H264Keyframe, 192, 96, 0, keyframe));
        }
        int putBefore = service.ThumbnailsPut;
        int shownBefore = w.ThumbnailsShown;
        service.CancelThumbnails();
        w.Refresh();
        bool shown = await Until(() => w.ThumbnailsShown > shownBefore && service.Thumbnails[incident.CoverKeys[0]] is ThumbnailState.Image, 10);
        ThumbnailKey first = incident.CoverKeys[0];
        RecordedThumbnail? cached = fake.ThumbnailGet(first.Json);
        Say($"recordings: row covers shown {w.ThumbnailsShown - shownBefore}, put back {service.ThumbnailsPut - putBefore}, cached kind {cached?.Kind}");
        Expect(shown && service.ThumbnailsPut > putBefore && cached?.Kind == NvThumbKind.Image,
            "recordings: the incident's row did not show the decoded cover, or it was not put back in the core's cache");
    }

    // 11e. The follow cut and grids of a downloaded incident (docs/UX.md "Recordings", "Follow and Grid"; milestone P,
    // W3), on the fake store's plans over synthetic files written where its download says they are: each camera's
    // pictures are its own color with the frame number in a strip (SyntheticRecording). Frames are drawn offscreen with
    // D3D11 from Media Foundation-decoded files (a second set of the plan's files, so the window's render thread cannot
    // move the pictures meanwhile) and checked at sample times for which camera is shown and which of its frames; then
    // the cuts, the sound following the shown camera, the transport on a simulated clock, the picture on screen, the
    // window's view menu, and the way back to the cameras.
    private async Task EditSteps(
        Viewer v, RecordingsWindow w, RecordingsBrowser b, FakeRecordings fake, FakePlayback playback, RecordingsService service, RecordingCamera[] cams)
    {
        Task<bool> Playing(Func<bool> condition, double seconds) => Until(() =>
        {
            playback.CompletePlaybackWork();
            return condition();
        }, seconds);
        RecordingsPlayer p = b.Player;
        EditPlayer ep = b.EditPlayer;
        Func<double> realClock = ep.Now;

        // parity: edit-download
        Incident? incident = b.DayIncidents.FirstOrDefault(i => i.Kinds.SequenceEqual(["vehicle"]) && i.Cameras[0].Camera == cams[0]);
        if (incident is null)
        {
            _problems.Add("edits: no vehicle incident of the first camera");
            return;
        }
        long s = incident.Start * 1000;
        b.Selection = new HashSet<string> { incident.Key };
        await Playing(() => b.Selected?.Key == incident.Key && p.Playback is not null, 5);
        w.Refresh();
        (string title, List<PlayerView> offered, bool hint) = w.ViewMenuShown();
        Expect(b.Editable is null && offered.SequenceEqual([PlayerView.Cameras]) && hint && title == "Cameras",
            $"edits: an incident not downloaded offers {string.Join(", ", offered)} (hint {hint})");
        b.ShowView(PlayerView.Follow);
        Expect(b.PlayerView == PlayerView.Cameras, "edits: Follow Cut showed without a download");
        b.Message = null;
        // The fake store has one camera per incident; its download of three cameras with video along with this one's stands for an edited incident.
        string request = $$"""{"incident":"{{incident.Key}}","cameras":[{{string.Join(",", cams.Select(c => $$"""{"camera":{{c.Json}}}"""))}}]}""";
        fake.DownloadIncident(request);
        bool downloaded = await Until(() =>
        {
            fake.CompleteRecordingWork();
            return service.FinishedDownload(incident.Key) is not null;
        }, 5);
        DownloadProgress? d = service.FinishedDownload(incident.Key);
        var files = new List<(string File, int Source, long DurationMs)>();
        if (downloaded && d is not null)
        {
            using var raw = System.Text.Json.JsonDocument.Parse(fake.EditCompute($$"""{"kind":"follow","download":"{{d.Download}}"}"""));
            foreach (System.Text.Json.JsonElement src in raw.RootElement.GetProperty("sources").EnumerateArray())
            {
                files.Add((src.GetProperty("file").GetString()!, src.GetProperty("source").GetInt32(), src.GetProperty("duration_ms").GetInt64()));
            }
        }
        string? writeFailure = null;
        try
        {
            await Task.Run(() =>
            {
                foreach ((string file, int source, long ms) in files)
                {
                    SyntheticRecording.Write(new SyntheticRecording.Spec(Frames: (int)(ms / 100) + 1, Color: source), file);
                }
            });
        }
        catch (Exception e) when (e is VideoException or IOException)
        {
            writeFailure = e.Message;
        }
        w.Refresh();
        (title, offered, hint) = w.ViewMenuShown();
        Say($"edits: downloaded {d?.Download}, {files.Count} synthetic files written{(writeFailure is null ? "" : ", failed: " + writeFailure)}; the view menu offers {string.Join(", ", offered)}");
        Expect(writeFailure is null, $"edits: the synthetic files were not written ({writeFailure})");
        Expect(downloaded && files.Count == 3, $"edits: the download has {files.Count} files, expected 3");
        Expect(b.Editable is not null && offered.Count == 4 && !hint, "edits: a finished download does not offer the follow cut and grids");
        if (!downloaded || d is null || files.Count != 3 || writeFailure is not null)
        {
            return;
        }

        // parity: edit-follow-cut
        double now = 1000;
        ep.Now = () => now;
        b.ShowView(PlayerView.Follow);
        bool opened = await Until(() => ep.Files is not null && (w.EditShown?.Frames ?? 0) > 0, 10);
        if (!opened || ep.Plan is not { } plan || ep.Files is not EditFiles live)
        {
            _problems.Add($"edits: the follow cut did not open ({ep.Message ?? b.Message ?? "no message"})");
            ep.Now = realClock;
            b.ShowView(PlayerView.Cameras);
            return;
        }
        Say($"edits: follow cut {string.Join(", ", plan.Segments.Select(g => $"{g.Source}@{g.StartMs - s}"))}, cuts {string.Join(", ", ep.Cuts.Select(c => c - s))}, frames drawn {w.EditShown?.Frames}");
        // The device playback's pause arrives as a core event, and the edit
        // can draw its first frame before it.
        await Until(() => !p.Transport.IsPlaying, 5);
        Expect(b.PlayerView == PlayerView.Follow && !p.Transport.IsPlaying, "edits: the device playback still plays under the follow cut");
        Expect(live.Failures.Count == 0, $"edits: files not opened: {string.Join("; ", live.Failures.Select(f => $"{f.Key}: {f.Value}"))}");
        Expect(ep.Cuts.SequenceEqual([s + 1000, s + 2000]) && plan.Segments.Select(g => g.Source).SequenceEqual([0, 1, 2]),
            $"edits: the follow cut cuts at {string.Join(",", ep.Cuts.Select(c => c - s))} to {string.Join(",", plan.Segments.Select(g => g.Source))}");
        w.Refresh();
        Expect(w.Scrubber.CutTicks.SequenceEqual(ep.Cuts) && w.ViewMenuShown().Title == "Follow Cut", "edits: the scrubber's cut marks or the view menu's title are not those of the follow cut");
        Expect(w.LabelTexts().Any(t => t.StartsWith("Camera 1", StringComparison.Ordinal)), $"edits: the follow cut's label is {string.Join("; ", w.LabelTexts())}");

        // Frames of every camera at sample times, drawn offscreen from files of their own.
        using (var probe = EditFiles.Open(plan, () => v.Host.Gpu))
        using (var renderer = new GridRenderer(v.Host.Gpu, 320, 180))
        {
            const long n = 24;
            long[] samples =
            [
                .. Enumerable.Range(0, (int)n).Select(k => plan.StartMs + k * (plan.EndMs - plan.StartMs) / (n - 1)),
                s + 500, s + 999, s + 1000, s + 1500, s + 1999, s + 2000, s + 2100, s + 700,
            ];
            List<string> bad = EditMismatches(probe, plan, renderer, samples);
            Say($"edits: follow cut rendered {samples.Length} frames, {bad.Count} wrong");
            Expect(bad.Count == 0, $"edits: follow cut pictures: {string.Join("; ", bad.Take(4))}");
        }

        // The transport on the simulated clock, and the sound following the picture.
        ep.Pause();
        ep.SetRate(1);
        ep.Seek(s + 500);
        ep.Play();
        var heard = new List<int?> { ep.PlayingSoundSource };
        now += 1;
        ep.Tick();
        heard.Add(ep.PlayingSoundSource);
        now += 1;
        ep.Tick();
        heard.Add(ep.PlayingSoundSource);
        long position = ep.PositionMs;
        ep.SetRate(4);
        heard.Add(ep.PlayingSoundSource);
        bool keyframes = ep.KeyframesOnly;
        ep.SetRate(1);
        ep.Muted = true;
        heard.Add(ep.PlayingSoundSource);
        ep.Muted = false;
        ep.Pause();
        string heardText = string.Join(", ", heard.Select(h => h?.ToString() ?? "none"));
        Say($"edits: played 2 s on the simulated clock to {position - s} ms; sound from {heardText}");
        Expect(position == s + 2500, $"edits: 2 s of play moved to {position - s} ms, expected 2500");
        Expect(heardText == "0, 1, 2, none, none" && keyframes, $"edits: the sound came from {heardText} (keyframes at 4x {keyframes}), expected 0, 1, 2, none, none");

        // On screen: the third camera's picture at 2.5 s, from the window's own renderer.
        bool shown = await Until(() => live.File(2)?.ShownPositionMs == 5500, 5);
        if (w.Renderer is { } stage)
        {
            StageCapture capture = await stage.CaptureAsync();
            (int r, int g, int bl) = capture.Image.Rgb(capture.Image.Width / 2, capture.Image.Height / 2);
            (int R, int G, int B) want = SyntheticRecording.Palette[2];
            Say($"edits: on screen ({r}, {g}, {bl}), expected {want}; the third camera's file at {live.File(2)?.ShownPositionMs} ms");
            Expect(shown && Math.Abs(r - want.R) <= 40 && Math.Abs(g - want.G) <= 40 && Math.Abs(bl - want.B) <= 40,
                $"edits: the screen shows ({r}, {g}, {bl}) at 2.5 s, expected the third camera's {want}");
        }

        // A lane click inside the edit's range on one of its cameras seeks the edit (and plays); another place brings the cameras back.
        b.LaneClick(cams[1], incident.Start + 4);
        Expect(b.PlayerView == PlayerView.Follow && ep.PositionMs == s + 4000, $"edits: a lane click in the range went to {ep.PositionMs - s} ms in {b.PlayerView}");
        ep.Pause();

        // parity: edit-grid
        b.ShowView(PlayerView.Grid4);
        bool gridOpened = await Until(() => ep.Files is not null && ep.Plan?.Kind == EditKind.Grid && (w.EditShown?.Frames ?? 0) > 0, 10);
        if (!gridOpened || ep.Plan is not { } grid)
        {
            _problems.Add($"edits: the grid did not open ({ep.Message ?? "no message"})");
        }
        else
        {
            Say($"edits: grid {grid.Layout} of {grid.Cells.Count} cells on {grid.Canvas?.Width}x{grid.Canvas?.Height}");
            Expect(grid.Layout == "4up" && grid.Cells.Count == 3 && grid.Canvas == new EditCanvas(1920, 1080),
                $"edits: the 4-up grid is {grid.Layout} with {grid.Cells.Count} cells");
            using (var probe = EditFiles.Open(grid, () => v.Host.Gpu))
            using (var renderer = new GridRenderer(v.Host.Gpu, 640, 360))
            {
                const long n = 16;
                long[] samples =
                [
                    .. Enumerable.Range(0, (int)n).Select(k => grid.StartMs + k * (grid.EndMs - grid.StartMs) / (n - 1)),
                    grid.StartMs + 500, grid.StartMs + 1500,
                ];
                List<string> bad = EditMismatches(probe, grid, renderer, samples);
                Say($"edits: grid rendered {samples.Length} frames, {bad.Count} wrong");
                Expect(bad.Count == 0, $"edits: grid pictures: {string.Join("; ", bad.Take(4))}");
            }
            w.Refresh();
            Expect(w.EditCells.Count == 3 && w.LabelTexts().Count(t => t.StartsWith("Camera ", StringComparison.Ordinal)) >= 3,
                $"edits: the grid's cells or labels are {w.EditCells.Count}, {string.Join("; ", w.LabelTexts())}");
            ep.Seek(grid.StartMs);
            w.Refresh();
            int absent = w.MessageTexts().Count(m => m == "No recording at this time");
            Expect(absent == 2, $"edits: {absent} cells say there is no recording at the start, expected the two later cameras'");
            ep.Seek(grid.StartMs + 6000);
            ep.Play();
            int? first = ep.PlayingSoundSource;
            ep.GridSound = 1;
            int? second = ep.PlayingSoundSource;
            ep.Pause();
            ep.GridSound = 0;
            Expect(first == 0 && second == 1, $"edits: the grid's sound came from {first?.ToString() ?? "none"}, then {second?.ToString() ?? "none"}");
        }

        // parity: edit-back-to-cameras
        long at = ep.PositionMs;
        ep.Now = realClock;
        b.ShowView(PlayerView.Cameras);
        w.Refresh();
        Say($"edits: back to the cameras at {TransportState.Clock(p.PositionMs)}");
        Expect(b.PlayerView == PlayerView.Cameras && ep.Files is null && w.EditShown is null, "edits: the edit did not close");
        if (p.Playback is not null)
        {
            Expect(p.PositionMs == at, $"edits: the cameras continue at {p.PositionMs}, not the edit's {at}");
        }

        // The edit is closed, so its files can be written again: with sound for the export steps, then as they were.
        await ExportEditSteps(w, b, fake, d.Download, s, files);
        try
        {
            if (d.Folder is { } folder)
            {
                Directory.Delete(folder, recursive: true);
            }
        }
        catch (IOException)
        {
        }
    }

    // Renders the samples of a plan offscreen from a second set of its files and returns what did not match.
    private static List<string> EditMismatches(EditFiles files, EditPlan plan, GridRenderer renderer, long[] samples)
    {
        var bounds = new NvrView.Shell.Rect(0, 0, renderer.Width, renderer.Height);
        var bad = new List<string>();
        foreach (long t in samples)
        {
            RenderedImage image = files.Render(t, renderer);
            if (plan.Kind == EditKind.Follow)
            {
                if (EditTimeline.Shot(plan, t) is not { } shot)
                {
                    continue;
                }
                (int? color, int frame) = SyntheticRecording.Read(image, bounds);
                if (color != shot.Source || frame != shot.FilePositionMs / 100)
                {
                    bad.Add($"{t - plan.StartMs} ms: camera {color?.ToString() ?? "none"} frame {frame}, expected {shot.Source} frame {shot.FilePositionMs / 100}");
                }
                continue;
            }
            List<NvrView.Shell.Rect> rects = EditComposition.CellRects(plan, bounds);
            for (int i = 0; i < 4; i++)
            {
                var cell = i < rects.Count ? rects[i] : new NvrView.Shell.Rect(bounds.Width / 2, bounds.Height / 2, bounds.Width / 2, bounds.Height / 2);
                if (i < plan.Cells.Count && EditTimeline.CellPosition(plan, i, t) is { } position)
                {
                    NvrView.Shell.Rect picture = PictureFit.Of(640, 360, cell, fill: false).Destination;
                    (int? color, int frame) = SyntheticRecording.Read(image, picture);
                    if (color != plan.Cells[i].Source || frame != position / 100)
                    {
                        bad.Add($"{t - plan.StartMs} ms cell {i}: camera {color?.ToString() ?? "none"} frame {frame}, expected {plan.Cells[i].Source} frame {position / 100}");
                    }
                }
                else
                {
                    (int r, int g, int bl) = image.Rgb((int)cell.MidX, (int)cell.MidY);
                    if (Math.Max(r, Math.Max(g, bl)) > 40)
                    {
                        bad.Add($"{t - plan.StartMs} ms cell {i}: ({r}, {g}, {bl}), expected black");
                    }
                }
            }
        }
        return bad;
    }


    private static CoreEvent Alarm(DeviceEntry device, int channel, string kind, bool start) => new()
    {
        Type = "alarm",
        Device = device.Id,
        Channel = channel,
        Kind = kind,
        State = start ? "start" : "stop",
    };

    // 11b. Calls.
    private async Task Calls(Viewer v, StageView stage)
    {
        DeviceEntry device = v.Devices[0];
        var camera = new CameraRef(device.Key, 0);
        var mic = new FakeMicrophone();
        var voice = new FakeSpeech { Rate = 22_050 };
        var engines = new List<FakeTranscriber>();
        var prompter = new FakeTalkPrompter();
        v.Calls.Services = new CallServices(mic, voice, () =>
        {
            var engine = new FakeTranscriber();
            engines.Add(engine);
            return engine;
        }, prompter);
        var clock = new ManualViewerClock(3000);
        v.UseClock(clock);
        v.SetAudible([]);
        int leasesBefore = v.Leases.LeaseCount(camera);

        // parity: call-tile-menu
        // The tile menu offers the conversation and the permission.
        StageTile menuTile = Tiles.First(t => t.Camera == camera);
        var menu = stage.BuildTileMenu(menuTile)!;
        List<string> items = Texts(menu.Items);
        bool allowChecked = menu.Items.OfType<Microsoft.UI.Xaml.Controls.ToggleMenuFlyoutItem>().FirstOrDefault(i => i.Text == "Allow talk-back")?.IsChecked == true;
        Say($"calls: tile menu {string.Join(" / ", items)}, allow checked {allowChecked}");
        Expect(items.Contains("Start Conversation") && items.Contains("Allow talk-back") && !allowChecked, "the tile menu does not offer Start Conversation and an unchecked Allow talk-back");

        // parity: call-conversation-start
        // 1. A conversation opens with no press and sends nothing by itself.
        v.Calls.StartConversation(camera);
        Call? call = v.Calls.CallFor(camera);
        Expect(call is { Origin: CallOrigin.Conversation, Presses: 0 }, "Start Conversation did not open a call with no press");
        Expect(v.IsAudible(camera) && call is { Audible: true, ShowsUnmute: false }, "a conversation did not make the camera audible");
        // The call holds a lease, and so does its card in this window (for the picture).
        Expect(v.Leases.LeaseCount(camera) == leasesBefore + 2 && stage.CallCards.LeasesHeld == 1, "the call and its card did not each hold a lease on the camera");
        Expect(call?.Talk.Current == TalkState.Phase.Idle && call.Talk.Outstanding == 0, "a conversation started talk-back by itself");
        stage.CallCards.Sync();
        CallCard? card = stage.CallCards.Cards.FirstOrDefault();
        Say($"calls: conversation card \"{card?.TitleText}\", {stage.CallCards.Cards.Count} shown");
        Expect(stage.CallCards.Cards.Count == 1 && card?.TitleText.StartsWith("Conversation", StringComparison.Ordinal) == true, "no conversation card over the stage");
        if (card is not null)
        {
            await CallCardPicture(v, stage, card, camera);
        }
        // parity: call-talk-conversation
        // The core is told the call is open on the camera's tile (its talk starts skip the per-minute limit).
        CameraTile? conversationTile = v.Calls.ConversationTileFor(camera);
        Say($"calls: core told the call is open on tile {conversationTile?.Id.ToString(System.Globalization.CultureInfo.InvariantCulture) ?? "none"}");
        Expect(conversationTile is not null && conversationTile == v.Leases.Tile(camera), "opening a conversation did not tell the core (no talk-back limit during a call)");

        // parity: call-camera-audio-recognized
        Expect(engines.Count == 1 && engines[0].Source == TranscriptSource.Visitor, "the camera's audio has no recognizer");
        bool tapped = engines.Count > 0 && await Until(() => engines[0].SamplesFed > 16_000, 10);
        Say($"calls: camera audio fed to the recognizer: {engines.FirstOrDefault()?.SamplesFed ?? 0} samples");
        Expect(tapped, "the camera's tapped audio did not reach the recognizer");
        menu = stage.BuildTileMenu(menuTile)!;
        Expect(Texts(menu.Items).Contains("End Conversation"), "the tile menu does not offer End Conversation during a call");

        // parity: call-talk-permission-prompt
        // 2. Without the permission Talk asks first; Cancel sends nothing, the call stays open, and the setting stays off.
        string cameraName = v.CameraName(camera);
        prompter.Answers.Enqueue(false);
        await v.Calls.StartTalkingAsync(camera);
        Say($"calls: talk without permission: asked \"{prompter.Asked.FirstOrDefault()?.Title}\", {call!.Talk.Text}");
        Expect(prompter.Asked.Count == 1 && prompter.Asked[0].Title == $"Allow talk-back for {cameraName}?", "Talk without the permission did not ask to allow talk-back for the camera");
        Expect(call.Talk.Current == TalkState.Phase.Idle && call.Talk.Outstanding == 0 && mic.Starts == 0, "a canceled prompt started talk-back (or opened the microphone)");
        Expect(!v.Settings.TalkAllowed(camera) && v.Calls.CallFor(camera) is not null, "Cancel turned talk-back on or ended the call");
        // A typed reply asks too; Cancel keeps the text (not accepted) and sends nothing.
        prompter.Answers.Enqueue(false);
        bool replyAccepted = await v.Calls.SendReplyAsync("Not yet", camera);
        Expect(!replyAccepted && prompter.Asked.Count == 2, "a typed reply without the permission did not ask and keep its text");
        Expect(voice.Rendered.Count == 0 && call.Talk.Outstanding == 0, "a canceled typed reply was rendered or sent");

        // parity: call-talk-allowed
        // 3. Allow saves the setting and starts the talk without pressing again; the core reports the audio it sent.
        prompter.Answers.Enqueue(true);
        v.Calls.StartTalking(camera);
        Expect(v.Settings.TalkAllowed(camera), "Allow did not keep the permission");
        bool active = await Until(() => call.Talk.IsActive, 10);
        Expect(active && mic.Running, "Talk did not become active with the microphone running");
        for (int i = 0; i < 10; i++)
        {
            mic.Speak(1600);
            await Task.Delay(100);
        }
        bool youListening = engines.Count > 1 && engines[1].Source == TranscriptSource.You;
        // What the call took from the (fake) microphone's echo cancellation, checked below.
        (EchoCancellation? State, string? Note, bool? Gate) echoOn = (v.Calls.EchoFor(camera), v.Calls.NoteFor(camera), v.Calls.EchoGateCancelled(camera));
        v.Calls.StopTalking(camera);
        bool stopped = await Until(() => call.Talk.Current == TalkState.Phase.Stopped, 10);
        Say($"calls: talk with permission: active {active}, then {call.Talk.Text}, {call.Talk.SentMs} ms sent");
        Expect(stopped && !mic.Running && call.Talk.SentMs >= 500, "Talk did not send the microphone's audio");
        Expect(youListening, "the microphone had no recognizer while talking");

        // parity: call-echo-cancellation
        // 3b. Echo cancellation (docs/AUDIO.md "Echo cancellation"): the talk above had the system's
        // canceller on; now a microphone without one. Real echo cannot be heard in a self-test: this
        // checks what the call shows and which caption rule its gate runs (the rules themselves are
        // EchoCancellationTests); the microphone is sent either way.
        Say($"calls: echo cancellation on: {echoOn.State}, note \"{echoOn.Note}\", cancelled gate {echoOn.Gate}");
        Expect(echoOn.State == EchoCancellation.System && echoOn.Gate == true, "the microphone's echo cancellation was not taken up by the call");
        Expect(echoOn.Note == "Echo cancellation on", "the call does not show that echo cancellation is on");
        mic.Echo = EchoCancellation.Gated("self-test: no echo canceller");
        v.Calls.StartTalking(camera);
        bool gatedTalk = await Until(() => call.Talk.IsActive && mic.Running, 10);
        (EchoCancellation? State, string? Note, bool? Gate) echoOff = (v.Calls.EchoFor(camera), v.Calls.NoteFor(camera), v.Calls.EchoGateCancelled(camera));
        for (int i = 0; i < 5; i++)
        {
            mic.Speak(1600);
            await Task.Delay(100);
        }
        v.Calls.StopTalking(camera);
        bool gatedStopped = await Until(() => call.Talk.Current == TalkState.Phase.Stopped, 10);
        mic.Echo = EchoCancellation.System;
        Say($"calls: echo cancellation off: {echoOff.State}, note \"{echoOff.Note}\", cancelled gate {echoOff.Gate}, {call.Talk.SentMs} ms sent");
        Expect(gatedTalk && gatedStopped && call.Talk.SentMs >= 200, "without echo cancellation the microphone was not sent");
        Expect(echoOff.State == EchoCancellation.Gated("self-test: no echo canceller") && echoOff.Gate == false, "without echo cancellation the caption gate did not take over");
        Expect(echoOff.Note == "No echo cancellation", "the call does not show that echo cancellation is off");

        // parity: call-typed-reply
        // 4. A typed reply: rendered by the voice (never played here), pushed in one go, ended by a draining stop.
        await v.Calls.SendReplyAsync("Hello, one moment please", camera);
        // Letting go of Talk above also drained ("finished"), so the reply must start a talk of its own first.
        bool replyStarted = call.Talk.IsTalking;
        bool replied = replyStarted && await Until(() => call.Talk is { Current: TalkState.Phase.Stopped, Reason: "finished" }, 15);
        Say($"calls: typed reply: voice rendered {string.Join("|", voice.Rendered)}, talk ended \"{call.Talk.Reason}\", {call.Talk.SentMs} ms sent");
        Expect(voice.Rendered.SequenceEqual(["Hello, one moment please"]), "the voice did not render exactly the typed text");
        Expect(replied && call.Talk.SentMs >= 300, "the typed reply was not sent and finished");

        // parity: call-caption-echo
        // The camera's microphone hears its speaker play the reply: the visitor caption that repeats it
        // is dropped; other speech is kept.
        engines[0].Say("Hello, one moment", "Hello, one moment please.");
        engines[0].Say("I have a", "I have a delivery for the house");
        bool echoChecked = await Until(() => call.Transcript.Finals.Any(l => l.Text.StartsWith("I have a delivery", StringComparison.Ordinal)), 5);
        Say($"calls: caption echo: {string.Join("|", call.Transcript.Finals.Select(l => l.Text))}, dropped {call.Echo.Dropped}");
        Expect(echoChecked, "other visitor speech was not captioned");
        Expect(!call.Transcript.Finals.Any(l => l.Text.StartsWith("Hello, one moment", StringComparison.Ordinal)), "the reply came back as a visitor caption (echo not dropped)");
        Expect(call.Echo.Dropped == 2, $"expected two dropped echo captions, saw {call.Echo.Dropped}");

        // parity: call-captions
        // 5. Captions reach the call and its card.
        int finalsBefore = call.Transcript.Finals.Count;
        engines[0].Say("hello th", "Hello there.");
        bool captioned = await Until(() => call.Transcript.Finals.Count == finalsBefore + 1, 5);
        stage.CallCards.Sync();
        Say($"calls: captions \"{card?.CaptionText}\"");
        Expect(captioned && card?.CaptionText.Contains("Visitor: Hello there.", StringComparison.Ordinal) == true, "captions did not reach the card");

        if (card is not null)
        {
            await CallOverTheStage(v, stage, card, camera, leasesBefore);
        }

        if (card is not null)
        {
            await CallInsetDrag(v, stage, card, camera);
        }

        // parity: call-press-joins-conversation
        // 6. A doorbell press joins the call.
        v.Apply(Alarm(device, 0, "visitor", true));
        Expect(call.Presses == 1, "a press during the conversation did not join it");
        stage.CallCards.Sync();
        Expect(card?.TitleText.StartsWith("Doorbell", StringComparison.Ordinal) == true, "the card does not say Doorbell after a press");
        v.Apply(Alarm(device, 0, "visitor", false));

        // parity: call-transcript-file
        // With "Save call transcripts" on (a temporary folder here), the call's transcript is written as a
        // text file when it ends (checked after step 7); off, the next call writes none (step 8).
        string transcriptFolder = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-transcripts-{Guid.NewGuid():N}");
        string transcriptsBefore = v.Transcripts.Folder;
        v.Transcripts.Folder = transcriptFolder;
        v.Settings.SaveCallTranscripts = true;

        // parity: call-auto-close
        // 7. A call ends a minute after its last activity and gives the audio back.
        clock.Advance(59);
        v.Tick();
        Expect(v.Calls.CallFor(camera) is not null, "the call ended before a minute of silence");
        clock.Advance(2);
        v.Tick();
        stage.CallCards.Sync();
        Say($"calls: after a minute: call open {v.Calls.CallFor(camera) is not null}, cards {stage.CallCards.Cards.Count}, audible {v.IsAudible(camera)}, leases {v.Leases.LeaseCount(camera)} (were {leasesBefore})");
        Expect(v.Calls.CallFor(camera) is null && stage.CallCards.Cards.Count == 0, "the call did not end a minute after its last activity");
        Expect(!v.IsAudible(camera) && v.Leases.LeaseCount(camera) == leasesBefore && stage.CallCards.LeasesHeld == 0, "ending the call left the camera audible or a lease held");
        bool undrawn = await Until(() => app.Renderer!.Overlays.Length == 0 && stage.CallCards.Overlays.Count == 0, 2);
        Expect(undrawn, "the ended call's picture is still drawn over the grid");
        Expect(engines.All(e => e.Finished), "ending the call left a recognizer running");
        string[] transcriptNames = v.Transcripts.Names();
        string transcriptText = transcriptNames.Length == 1 ? File.ReadAllText(Path.Combine(transcriptFolder, transcriptNames[0])) : "";
        string safeName = TranscriptFiles.Sanitized(v.CameraName(camera));
        Say($"calls: transcript file {string.Join("|", transcriptNames)}, {transcriptText.Split('\n').Length} lines");
        Expect(transcriptNames.Length == 1 && transcriptNames[0].EndsWith($" {safeName}.txt", StringComparison.Ordinal), "ending a call with saving on did not write one transcript file named for the camera");
        Expect(
            transcriptText.StartsWith($"NVRView call transcript\nCamera: {v.CameraName(camera)}\n", StringComparison.Ordinal)
                && transcriptText.Contains("\nCall: doorbell, 1 press\n", StringComparison.Ordinal)
                && transcriptText.Contains(" Visitor: Hello there.\n", StringComparison.Ordinal),
            "the transcript file lacks its header or the visitor's line");
        v.Settings.SaveCallTranscripts = false;

        // parity: call-press-opens-call
        // 8. A press alone opens a silent call with Unmute; Unmute plays it; closing gives it back.
        v.Apply(Alarm(device, 0, "visitor", true));
        Call? ring = v.Calls.CallFor(camera);
        stage.CallCards.Sync();
        Expect(ring is { Origin: CallOrigin.Press, Presses: 1, ShowsUnmute: true, Audible: false } && !v.IsAudible(camera), "a press did not open a silent call with Unmute");
        Expect(stage.CallCards.Cards.FirstOrDefault()?.UnmuteShown == true, "the card does not show Unmute for a silent call");
        // parity: call-unmute
        v.Calls.Unmute(camera);
        Expect(v.IsAudible(camera) && ring is { ShowsUnmute: false }, "Unmute did not play the camera");
        // (Saving is off now: what this call transcribes is not written.)
        engines[^1].Say("hi", "Hi.");
        bool ringCaptioned = await Until(() => ring?.Transcript.Finals.Count == 1, 5);
        // parity: call-close-ends
        v.Calls.End(camera, "closed");
        Expect(v.Calls.CallFor(camera) is null && !v.IsAudible(camera), "closing the call did not end it and mute the camera");
        Expect(ringCaptioned && v.Transcripts.Names().Length == 1, "a call with saving off wrote a transcript file");

        // parity: unmute-on-ring
        // The doorbell's "Unmute on ring even when not listening": with it on, a press plays the doorbell at once
        // (no Unmute control); it is a view setting that reads back from view-settings.json.
        v.Settings.SetUnmuteOnRing(camera, true);
        v.Apply(Alarm(device, 0, "visitor", true));
        Call? loud = v.Calls.CallFor(camera);
        bool loudAudible = loud is { ShowsUnmute: false, Audible: true } && v.IsAudible(camera);
        v.Calls.End(camera, "closed");
        bool loudEnded = v.Calls.CallFor(camera) is null && !v.IsAudible(camera);
        v.Apply(Alarm(device, 0, "visitor", false));
        v.Settings.SetUnmuteOnRing(camera, false);
        v.Apply(Alarm(device, 0, "visitor", true));
        bool quietAgain = v.Calls.CallFor(camera) is { ShowsUnmute: true, Audible: false } && !v.IsAudible(camera);
        v.Calls.End(camera, "closed");
        v.Apply(Alarm(device, 0, "visitor", false));
        string unmuteFile = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-view-settings-{Guid.NewGuid():N}.json");
        bool unmuteSaved;
        try
        {
            new ViewSettings(unmuteFile).SetUnmuteOnRing(camera, true);
            var reread = new ViewSettings(unmuteFile);
            unmuteSaved = reread.UnmuteOnRing(camera) && !reread.UnmuteOnRing(new CameraRef(camera.Device, camera.Channel + 1));
        }
        finally
        {
            File.Delete(unmuteFile);
        }
        Say($"calls: unmute on ring: audible at once {loudAudible}, ended {loudEnded}, silent again when off {quietAgain}, setting reads back {unmuteSaved}");
        Expect(loudAudible, "with Unmute on ring on, a press did not play the doorbell at once");
        Expect(loudEnded, "ending the unmuted call left the doorbell audible");
        Expect(quietAgain, "with Unmute on ring off again, a press did not open a silent call");
        Expect(unmuteSaved, "the unmute-on-ring setting did not read back from view-settings.json");
        try
        {
            Directory.Delete(transcriptFolder, recursive: true);
        }
        catch (IOException)
        {
            // Left for the system's temporary-file cleanup.
        }
        v.Transcripts.Folder = transcriptsBefore;

        await CallWindowSteps(v, stage, camera, device, clock, mic);
        await CallRecordingEndsWithCall(v, camera);

        // parity: call-permission-revoke
        // Revoking the permission is the user's too.
        v.SetTalkAllowed(camera, false);
        Expect(!v.Settings.TalkAllowed(camera), "the permission was not withdrawn");
        v.UseClock(new SystemViewerClock());

        await RealCallEngines();
    }

    private static bool NearFrame(WindowRect a, WindowRect b) =>
        Math.Abs(a.X - b.X) <= 3 && Math.Abs(a.Y - b.Y) <= 3 && Math.Abs(a.Width - b.Width) <= 3 && Math.Abs(a.Height - b.Height) <= 3;

    // Calls in their own windows (docs/UX.md "A call in its own window"), against the doorbell channel:
    // a press shown as a card in two viewer windows pops out into a call window that draws exactly the
    // doorbell's shared tile; talk, "Keep talking", and the unsent reply survive popping in and out; a
    // viewer window opened meanwhile gets its card back on Pop In; closing the window or Esc ends the
    // call, and it never ends by inactivity while popped out; the window's frame is remembered or falls
    // back to the top right; the setting and the doorbell's own choice route a ring and a conversation.
    // The windows stay behind the others and never float or take the focus in a test run.
    private async Task CallWindowSteps(Viewer v, StageView stage, CameraRef camera, DeviceEntry device, ManualViewerClock clock, FakeMicrophone mic)
    {
        CallWindows cws = v.CallWindows!;
        CameraRef other = new(device.Key, 1);
        ViewerWindow second = app.OpenWindow(new WindowContent.Scratch("Second", LayoutNode.Leaf(other)));
        var opened = new List<ViewerWindow> { second };
        StageView[] stages = [stage, second.Stage];
        int baseline = v.Leases.LeaseCount(camera);
        StreamTier baselineTier = v.Planner.WantedTier(camera);
        WindowRect? frameBefore = cws.RememberedFrame;
        CallWindowPlacement.Mode modeBefore = cws.PlacementMode;
        bool pinBefore = cws.Pinned;
        try
        {
            // parity: call-popout-open
            v.Apply(Alarm(device, 0, "visitor", true));
            v.Apply(Alarm(device, 0, "visitor", false));
            Call? call = v.Calls.CallFor(camera);
            bool bothShow = await Until(() => stages.All(s => s.CallCards.Cards.Count == 1 && s.CallCards.LeasesHeld == 1), 3);
            if (call is null || !bothShow || v.Leases.Tile(camera) is not { } tile)
            {
                _problems.Add("a press did not open the call's card in both viewer windows");
                return;
            }
            int insetsBefore = stages.Sum(s => s.CallCards.Cards.Count);
            int leasesBefore = v.Leases.LeaseCount(camera);
            bool popButtons = stages.All(s => s.CallCards.Cards[0] is { PopShown: true, PopTip: "Pop Out Call" });
            long starts = mic.Starts;
            int presses = call.Presses, finals = call.Transcript.Finals.Count;
            cws.PopOut(camera);
            if (cws.WindowFor(camera) is not { } cw)
            {
                _problems.Add($"Pop Out opened no call window: {cws.LastError}");
                return;
            }
            bool gone = stages.All(s => s.CallCards.Cards.Count == 0 && s.CallCards.LeasesHeld == 0);
            // The window's picture is a regular tile: the one tile of its own stage (no overlay), drawn from the shared tile.
            bool drawn = await Until(() => cw.Stage.Tiles.Length == 1 && cw.Renderer.Layout.Tiles.Length == 1 && cw.Renderer.Layout.Tiles[0] == cw.Tile && cw.Tile.Shown > 2 && cw.Tile.PictureWidth > 0, 15);
            bool exact = drawn && cw.Renderer.Overlays.Length == 0 && cw.Tile.Shared == tile && v.Tiles.Length == v.Leases.Cameras.Count;
            bool undrawn = await Until(() => stages.All(s => s.CallCards.Overlays.Count == 0 && s.Renderer.Overlays.Length == 0), 3);
            var engine = v.Calls.VisitorEngine(camera) as FakeTranscriber;
            long fed = engine?.SamplesFed ?? 0;
            bool tapped = engine is not null && await Until(() => engine.SamplesFed > fed + 8000, 8);
            bool same = v.Calls.CallFor(camera) == call && call.Presentation == CallPresentation.Window && call.Presses == presses && call.Transcript.Finals.Count == finals;
            int leases = v.Leases.LeaseCount(camera);
            int wantedLeases = leasesBefore - insetsBefore + 1;
            Say($"calls: pop out: cards {insetsBefore} -> gone {gone} (picture undrawn {undrawn}), window draws the doorbell tile {exact} ({cw.Tile.Shown} pictures), " +
                $"same call {same}, tapped {tapped}, leases {leasesBefore} -> {leases}, main stream {cw.AsksForMainStream}");
            Expect(popButtons, "a call's card did not offer Pop Out Call in its header");
            Expect(gone && undrawn, "Pop Out left the call's card (or its picture or lease) in a viewer window");
            Expect(exact, "the call window does not draw exactly the doorbell's shared tile");
            Expect(same, "Pop Out changed the call");
            Expect(tapped, "Pop Out stopped the doorbell's audio tap");
            Expect(mic.Starts == starts && call.Talk.Outstanding == 0, "Pop Out started talk-back");
            Expect(leases == wantedLeases && v.Leases.Tile(camera) == tile, $"Pop Out leases: {leases}, expected {wantedLeases} on the same tile");
            Expect(cws.LastOpenActivated == true, "an explicit Pop Out did not ask for the focus");
            if (drawn)
            {
                await CheckPictureFrame("call window", cw.Tile, cw.Renderer);
                // The window joins the device-loss recovery with the viewer windows: its swap chain and picture come back.
                int recoveries = cw.Renderer.Recoveries;
                long shown = cw.Tile.Shown;
                cw.Renderer.SimulateDeviceLoss();
                bool recovered = await Until(() => cw.Renderer.Recoveries > recoveries, 10);
                bool back = recovered && await Until(() => cw.Tile.Shown > shown + 5 && cw.Tile.PictureWidth > 0, 15);
                Say($"calls: call window after a device loss: recovered {recovered}, pictures {shown} -> {cw.Tile.Shown}");
                Expect(back, "the call window's picture did not come back after a device loss");
                if (back)
                {
                    await CheckPictureFrame("call window after device loss", cw.Tile, cw.Renderer);
                }
                await CallWindowTile(v, cw, camera, clock);
            }

            // parity: call-popout-talk
            // "Keep talking" and an unsent reply belong to the call: popping in and out stops nothing and starts nothing, and any card built for it shows them.
            v.SetTalkAllowed(camera, true);
            v.Calls.SetLatched(camera, true);
            v.Calls.SetDraft(camera, "Be right there");
            bool talking = await Until(() => call.Talk.IsActive && mic.Running, 10);
            long talkStarts = mic.Starts;
            cws.PopIn(camera);
            bool insetsShow = stages.All(s => s.CallCards.Cards.FirstOrDefault() is { KeepTalkingShown: true } c && c.ReplyText == "Be right there");
            cws.PopOut(camera);
            CallWindow? cw2 = cws.WindowFor(camera);
            bool windowShows = cw2 is { Card: { KeepTalkingShown: true } card2 } && card2.ReplyText == "Be right there";
            await Task.Delay(300);
            bool kept = call.Talk.IsActive && mic.Running && mic.Starts == talkStarts;
            Say($"calls: pop in and out while talking: talking {talking}, talk kept {kept}, latch {call.Latched}, draft \"{call.Draft}\", cards show them {insetsShow}, window shows them {windowShows}");
            Expect(talking, "Keep talking did not start the talk before the pop");
            Expect(kept, "popping in and out stopped or restarted talk-back");
            Expect(call.Latched && call.Draft == "Be right there", "popping lost Keep talking or the draft");
            Expect(insetsShow, "the cards did not show Keep talking and the draft after Pop In");
            Expect(windowShows, "the call window did not show Keep talking and the draft");
            v.Calls.SetLatched(camera, false);
            Expect(await Until(() => !call.Talk.IsTalking, 10), "turning Keep talking off did not end the talk");
            v.SetTalkAllowed(camera, false);
            v.Calls.SetDraft(camera, "");
            if (cw2 is null)
            {
                return;
            }

            // parity: call-popin-new-window
            // A viewer window opened while the call is popped out has no card for it, and shows it after Pop In like the others.
            ViewerWindow third = app.OpenWindow(new WindowContent.Scratch("Third", LayoutNode.Leaf(other)));
            opened.Add(third);
            await Task.Delay(300);
            bool hidden = third.Stage.CallCards.Cards.Count == 0 && third.Stage.CallCards.LeasesHeld == 0 && cws.WindowFor(camera) is not null;
            Say($"calls: new window while popped out: no card {hidden}, leases {v.Leases.LeaseCount(camera)}");
            Expect(hidden, "a viewer window opened while the call was popped out shows its card");
            stages = [.. stages, third.Stage];

            // parity: call-popin
            // Pop In: the window closes, every viewer window shows the card again with a fresh minute, and the leases follow.
            CallWindow popped = cws.WindowFor(camera)!;
            WindowRect size = popped.CurrentFrame;
            popped.Place(new WindowRect(size.X, size.Y, 1100, 800));
            bool wantsMain = await Until(() => popped.AsksForMainStream && v.Planner.WantedTier(camera) == StreamTier.Main, 8);
            int leasesPopped = v.Leases.LeaseCount(camera);
            clock.Advance(30);
            double popInAt = clock.Now;
            cws.PopIn(camera);
            bool back2 = stages.All(s => s.CallCards.Cards.Count == 1 && s.CallCards.Cards[0].Mode == CallCardMode.Inset && s.CallCards.Cards[0].PopTip == "Pop Out Call");
            bool minute = call.LastActivity >= popInAt;
            bool closed = cws.WindowFor(camera) is null && popped.IsReleased;
            int leasesIn = v.Leases.LeaseCount(camera);
            StreamTier demand = v.Planner.WantedTier(camera);
            Say($"calls: pop in: window closed {closed}, cards back {back2}, fresh minute {minute}, leases {leasesPopped} -> {leasesIn}, window asked for main {wantsMain}, now wants {demand}");
            Expect(wantsMain, "a large call window did not ask for the main stream");
            Expect(closed, "Pop In left the call window open");
            Expect(back2, "Pop In did not bring the card back in every viewer window");
            Expect(minute, "Pop In did not give the call a fresh minute");
            Expect(leasesIn == leasesPopped - 1 + stages.Length && call.Presentation == CallPresentation.Inset, $"Pop In leases: {leasesIn}, expected {leasesPopped - 1 + stages.Length}");
            Expect(demand == baselineTier, "the closed call window's stream demand stayed");

            // parity: call-popout-end
            // Closing the call window and Esc end the call, and the window and the viewer windows' cards go with it; a call in its own window never ends by inactivity.
            void Ended(string how)
            {
                bool clean = v.Calls.CallFor(camera) is null && cws.WindowFor(camera) is null && stages.All(s => s.CallCards.Cards.Count == 0)
                    && v.Leases.LeaseCount(camera) == baseline;
                Say($"calls: call window {how}: call ended and window closed {clean}, leases {v.Leases.LeaseCount(camera)} (baseline {baseline})");
                Expect(clean, $"{how} did not end the call, close its window and let its cards and leases go");
            }
            cws.PopOut(camera);
            CallWindow? closing = cws.WindowFor(camera);
            closing?.Window.Close();
            await Until(() => v.Calls.CallFor(camera) is null, 5);
            Ended("closed");
            // Esc while the reply field is not being edited.
            v.Calls.StartConversation(camera);
            cws.PopOut(camera);
            bool escHandled = cws.WindowFor(camera)?.HandleKey(Windows.System.VirtualKey.Escape) == true;
            await Until(() => v.Calls.CallFor(camera) is null, 5);
            Expect(escHandled, "Esc in the call window was not handled");
            Ended("Esc");
            // Ten minutes without activity leave a popped-out call open.
            v.Calls.StartConversation(camera);
            cws.PopOut(camera);
            clock.Advance(10 * Call.IdleTimeout);
            v.Tick();
            bool stayed = cws.WindowFor(camera) is not null && v.Calls.CallFor(camera) is not null;
            Say($"calls: call window after 10 min without activity: still open {stayed}");
            Expect(stayed, "a call in its own window ended by itself without activity");
            // Pop In restarts the minute.
            cws.PopIn(camera);
            clock.Advance(Call.IdleTimeout - 1);
            v.Tick();
            bool fresh = v.Calls.CallFor(camera) is not null;
            clock.Advance(2);
            v.Tick();
            Say($"calls: popped in: open after 59 s {fresh}, ended after 61 s {v.Calls.CallFor(camera) is null}");
            Expect(fresh, "Pop In did not give the call a fresh minute");
            Ended("expiry after Pop In");

            // parity: call-window-frame
            // The remembered frame is reused and follows the user's moves; one whose screen is gone falls back to the top right of
            // the active viewer window's screen; "Top right of the active screen" ignores it; the pin is remembered.
            WindowRect visible = cws.ActiveWorkArea() is { } active ? active : new WindowRect(0, 0, 1024, 728);
            const double W = 520, H = 440;
            WindowRect rememberedFrame = new(visible.X + 40, visible.Y + 60, W, H);
            cws.PlacementMode = CallWindowPlacement.Mode.Remembered;
            cws.RememberedFrame = rememberedFrame;
            v.Calls.StartConversation(camera);
            cws.PopOut(camera);
            WindowRect? reused = cws.WindowFor(camera)?.CurrentFrame;
            if (cws.WindowFor(camera) is { } mover)
            {
                mover.AppWindow.Move(new Windows.Graphics.PointInt32((int)rememberedFrame.X + 30, (int)rememberedFrame.Y + 20));
            }
            var movedTo = new WindowRect(rememberedFrame.X + 30, rememberedFrame.Y + 20, W, H);
            bool follows = await Until(() => cws.RememberedFrame is { } f && NearFrame(f, movedTo), 5);
            v.Calls.End(camera, "closed");
            // A frame on a screen that is gone.
            cws.RememberedFrame = new WindowRect(-100_000, -100_000, W, H);
            v.Calls.StartConversation(camera);
            cws.PopOut(camera);
            WindowRect? fallback = cws.WindowFor(camera)?.CurrentFrame;
            v.Calls.End(camera, "closed");
            var topRight = new WindowRect(visible.X + visible.Width - CallWindowPlacement.Margin - W, visible.Y + CallWindowPlacement.Margin, W, H);
            // The other setting ignores a frame that is still on a screen.
            cws.RememberedFrame = rememberedFrame;
            cws.PlacementMode = CallWindowPlacement.Mode.ActiveScreen;
            v.Calls.StartConversation(camera);
            cws.PopOut(camera);
            WindowRect? atTopRight = cws.WindowFor(camera)?.CurrentFrame;
            v.Calls.End(camera, "closed");
            cws.PlacementMode = CallWindowPlacement.Mode.Remembered;
            // The pin is remembered (a test run never floats the window, so the native flag is not read).
            cws.Pinned = false;
            bool pinSaved = !v.Settings.CallWindowOnTop;
            cws.Pinned = true;
            pinSaved &= v.Settings.CallWindowOnTop;
            // The frame, the placement, and the pin are kept in view-settings.json.
            string settingsFile = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-view-settings-{Guid.NewGuid():N}.json");
            try
            {
                var written = new ViewSettings(settingsFile) { CallWindowFrame = rememberedFrame, CallWindowPlacementMode = CallWindowPlacement.Mode.ActiveScreen, CallWindowOnTop = false };
                var read = new ViewSettings(settingsFile);
                bool persisted = read.CallWindowFrame == rememberedFrame && read.CallWindowPlacementMode == CallWindowPlacement.Mode.ActiveScreen && !read.CallWindowOnTop && written.CallWindowOnTop == false;
                bool reusedOk = reused is { } r1 && NearFrame(r1, rememberedFrame);
                bool fallbackOk = fallback is { } r2 && NearFrame(r2, topRight);
                bool settingOk = atTopRight is { } r3 && NearFrame(r3, topRight);
                Say($"calls: call window frame: remembered reused {reusedOk}, move remembered {follows}, gone screen -> top right {fallbackOk}, " +
                    $"top right setting {settingOk}, pin saved {pinSaved}, settings file reads back {persisted}");
                Expect(reusedOk, $"the call window did not open at the remembered frame: {reused}");
                Expect(follows, $"moving the call window did not remember its frame: {cws.RememberedFrame}");
                Expect(fallbackOk, $"a remembered frame off every screen did not fall back to the top right: {fallback} (wanted {topRight})");
                Expect(settingOk, $"Top right of the active screen did not ignore the remembered frame: {atTopRight} (wanted {topRight})");
                Expect(pinSaved, "the pin was not remembered");
                Expect(persisted, "the call window's frame, placement, and pin did not read back from view-settings.json");
            }
            finally
            {
                File.Delete(settingsFile);
            }

            // parity: call-default-ring
            // "Open calls in their own window": a ring opens the call in a call window that is not brought forward, the viewer windows keep no
            // card for it, a press that joins keeps it there, and the doorbell's own choice (Inset) beats the setting.
            v.Settings.CallsOpenInWindow = true;
            v.Apply(Alarm(device, 0, "visitor", true));
            v.Apply(Alarm(device, 0, "visitor", false));
            bool ringWindow = await Until(() => cws.WindowFor(camera) is not null, 5);
            Call? ringCall = v.Calls.CallFor(camera);
            CallWindow? ringWin = cws.WindowFor(camera);
            bool quiet = cws.LastOpenActivated == false;
            bool noCards = stages.All(s => s.CallCards.Cards.Count == 0 && s.CallCards.LeasesHeld == 0);
            v.Settings.CallsOpenInWindow = false;
            v.Apply(Alarm(device, 0, "visitor", true));
            v.Apply(Alarm(device, 0, "visitor", false));
            bool joined = ringCall?.Presses == 2 && ringCall.Presentation == CallPresentation.Window && cws.WindowFor(camera) == ringWin && stages.All(s => s.CallCards.Cards.Count == 0);
            v.Calls.End(camera, "closed");
            v.Settings.CallsOpenInWindow = true;
            v.Settings.SetCallPresentation(camera, CallPresentation.Inset);
            v.Apply(Alarm(device, 0, "visitor", true));
            v.Apply(Alarm(device, 0, "visitor", false));
            bool inset = await Until(() => stages.All(s => s.CallCards.Cards.Count == 1), 3) && v.Calls.CallFor(camera)?.Presentation == CallPresentation.Inset && cws.WindowFor(camera) is null;
            v.Calls.End(camera, "closed");
            v.Settings.CallsOpenInWindow = false;
            v.Settings.SetCallPresentation(camera, null);
            Say($"calls: ring with calls in windows: window {ringWindow && ringCall?.Presentation == CallPresentation.Window}, opened without focus {quiet}, no cards {noCards}, " +
                $"a joining press kept it {joined}; the doorbell set to Inset: cards in every window {inset}");
            Expect(ringWindow && ringCall?.Presentation == CallPresentation.Window, "with the setting on, a ring did not open a call window");
            Expect(quiet, "a call window opened by a ring asked for the focus");
            Expect(noCards, "a ring into a call window left a card in a viewer window");
            Expect(joined, "a press joining a call in its window moved it");
            Expect(inset, "the doorbell's Inset choice did not win over Open calls in their own window");

            // parity: call-default-conversation
            // The doorbell's Window choice opens Start Conversation in a call window (brought forward: the user asked for it) with the
            // setting off; Default again leaves the choice out of the file.
            v.Settings.SetCallPresentation(camera, CallPresentation.Window);
            v.Calls.StartConversation(camera);
            Call? conversation = v.Calls.CallFor(camera);
            bool conversationWindow = conversation is { Presentation: CallPresentation.Window, Origin: CallOrigin.Conversation } && cws.WindowFor(camera) is not null;
            bool activates = cws.LastOpenActivated == true;
            bool noCard = stages.All(s => s.CallCards.Cards.Count == 0);
            v.Calls.End(camera, "closed");
            v.Settings.SetCallPresentation(camera, null);
            bool cleared = v.Settings.CallPresentationFor(camera) is null;
            string choiceFile = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-view-settings-{Guid.NewGuid():N}.json");
            bool readsBack;
            try
            {
                var choices = new ViewSettings(choiceFile) { CallsOpenInWindow = true };
                choices.SetCallPresentation(camera, CallPresentation.Window);
                var reread = new ViewSettings(choiceFile);
                readsBack = reread.CallsOpenInWindow && reread.CallPresentationFor(camera) == CallPresentation.Window;
                choices.SetCallPresentation(camera, null);
                readsBack &= new ViewSettings(choiceFile).CallPresentationFor(camera) is null;
            }
            finally
            {
                File.Delete(choiceFile);
            }
            Say($"calls: conversation with the doorbell set to Window: call window {conversationWindow}, brought forward {activates}, no card {noCard}, Default clears the choice {cleared}, saved choices read back {readsBack}");
            Expect(conversationWindow, "the doorbell's Window choice did not open Start Conversation in a call window");
            Expect(activates, "a call window opened by Start Conversation was not brought forward");
            Expect(noCard, "Start Conversation into a call window left a card in a viewer window");
            Expect(cleared, "choosing Default did not clear the doorbell's call choice");
            Expect(readsBack, "the setting and the doorbell's call choice did not save and read back");
        }
        finally
        {
            v.Calls.End(camera, "closed");
            v.Settings.CallsOpenInWindow = false;
            v.Settings.SetCallPresentation(camera, null);
            v.SetTalkAllowed(camera, false);
            cws.RememberedFrame = frameBefore;
            cws.PlacementMode = modeBefore;
            cws.Pinned = pinBefore;
            foreach (ViewerWindow w in opened)
            {
                w.Window.Close();
            }
            await Until(() => app.Windows.Count == 1, 5);
        }
    }

    // The call window's picture is a regular tile (docs/CAPTURE.md "The call window is a regular tile"): a pinch zooms it, the
    // zoom map shows, the renderer samples the zoomed region, the zoomed tile asks for the main stream, and the header's Reset
    // Zoom brings it back; its menu is the header model's (Pop In Call, End Conversation, Record Call) with no layout items, and
    // focus and split do nothing there.
    private async Task CallWindowTile(Viewer v, CallWindow cw, CameraRef camera, ManualViewerClock clock)
    {
        StageView stage = cw.Stage;
        StageTile tile = cw.Tile;
        TileChrome? chrome = stage.ChromeFor(tile);
        if (!stage.FixedTile || chrome is null)
        {
            _problems.Add("the call window's picture is not a stage in single fixed tile mode");
            return;
        }
        // parity: call-window-tile-zoom
        stage.Hover(tile);
        stage.PinchDigital(tile, tile.X + tile.Width / 2, tile.Y + tile.Height / 2, 2);
        stage.Tick();
        bool zoomed = Math.Abs(tile.Zoom - 2) < 0.01;
        UnitRect seen = tile.DigitalZoom.Visible(tile.ZoomBase);
        bool cropped = seen.Width < 0.9 || seen.Height < 0.9;
        bool wantsMain = v.Planner.WantedTier(camera) == StreamTier.Main;
        // Switching to the main stream takes the picture away for a while (the header then shows only the title and the more button),
        // so the header is read before the switch happens and the zoom is reset, which sends the tile back to the sub-stream.
        // At the window's real size the header may keep Reset Zoom in its "..." menu: it must be offered either way.
        await Until(() => chrome.MapShown && (chrome.ResetShown || chrome.HeaderLayout?.MoreMenu.Any(e => e.Item == TileHeaderItem.ResetZoom) == true), 5);
        bool map = chrome.MapShown;
        bool resetButton = chrome.ResetShown;
        bool resetShown = resetButton || chrome.HeaderLayout?.MoreMenu.Any(e => e.Item == TileHeaderItem.ResetZoom) == true;
        wantsMain |= await Until(() => v.Planner.WantedTier(camera) == StreamTier.Main, 8);
        if (resetButton)
        {
            chrome.PressReset();
        }
        else
        {
            stage.PerformHeaderItem(tile, TileHeaderItem.ResetZoom, null);
        }
        bool back = await Until(() => tile.Zoom <= 1.0001 && !chrome.ResetShown && !chrome.MapShown, 5);
        Say($"call window tile: status \"{stage.StatusText(tile)}\" ({tile.Shared?.Video.Status}); pinch to {tile.Zoom:0.00}x then reset, zoom map {map}, source cropped {cropped} ({seen.Width:0.00}x{seen.Height:0.00}), main stream {wantsMain}, header Reset Zoom shown {resetShown} and back to 1x {back}");
        Expect(zoomed, "a pinch did not zoom the call window's tile");
        Expect(map, "the call window's zoomed tile showed no zoom map");
        Expect(cropped, "the call window's renderer did not sample the zoomed region");
        Expect(wantsMain, "the zoomed call window tile did not ask for the main stream");
        Expect(resetShown && back, "the call window's header did not offer Reset Zoom or did not reset");

        // parity: call-window-tile-menu
        await Until(() => stage.HeaderTileOf(tile) is { Picture: true }, 30);
        List<string> menu = Texts(stage.BuildTileMenu(tile)!.Items);
        List<string> want = stage.HeaderTileOf(tile) is { } header ? [.. TileHeaderLayout.Menu(header).Select(e => e.Label)] : [];
        bool callItems = new[] { "Pop In Call", "End Conversation", "Record Call", "Save Picture" }.All(menu.Contains);
        string[] layoutItems = [.. new[] { "Focus", "Split Side by Side", "Remove Tile", "Clear Tile", "Show Camera", "Record Main Stream" }.Where(menu.Contains)];
        stage.ToggleFocus(tile);
        stage.Split(SplitAxis.Row);
        bool fixedStill = !stage.IsFocused && stage.State.Tree.LeafCount == 1 && stage.Selected == tile;
        IReadOnlyList<TileHeaderItem> shownButtons = chrome.ShownButtons;
        Say($"call window tile menu: the model's {want.Count} items {menu.Take(want.Count).SequenceEqual(want)}, call items {callItems}, layout items [{string.Join(", ", layoutItems)}], " +
            $"focus and split ignored {fixedStill}, header [{string.Join(", ", shownButtons)}]");
        Expect(menu.SequenceEqual(want) && callItems, $"the call window tile's menu is not the header model's call menu: {string.Join(" / ", menu)}");
        Expect(layoutItems.Length == 0, $"the call window tile's menu offers {string.Join(", ", layoutItems)}");
        Expect(fixedStill, "focus or split changed the call window's single tile");
        // The shared model decides (the speaker never hides before Save Picture; Quality may sit in "..."), and both stay reachable.
        IReadOnlyList<TileHeaderMenuEntry> overflow = chrome.HeaderLayout?.MoreMenu ?? [];
        bool Reachable(TileHeaderItem i) => shownButtons.Contains(i) || overflow.Any(e => e.Item == i);
        Expect(Reachable(TileHeaderItem.Speaker) && Reachable(TileHeaderItem.Quality), "the call window's header has no speaker or quality control, in the header or its \"...\" menu");
        Expect(!shownButtons.Contains(TileHeaderItem.SavePicture) || shownButtons.Contains(TileHeaderItem.Speaker), "the call window's header hides the speaker while Save Picture shows");
        await CallRecordButton(v, cw, camera, tile);
    }

    // The tile header (docs/UX.md "Tile header"): laid out by the shared model on the real chrome at several widths, the "..."
    // and right-click menus built from it, and what a not-yet-built action says.
    private async Task HeaderModel(Viewer v, StageView stage)
    {
        StageTile? tile = Tiles.FirstOrDefault(t => stage.HeaderTileOf(t) is { Picture: true } && stage.ChromeFor(t) is not null);
        if (tile is null || stage.ChromeFor(tile) is not { } chrome || stage.HeaderTileOf(tile) is not { } model)
        {
            _problems.Add("no tile with a picture to check the header on");
            return;
        }
        var clock = new ManualViewerClock(new SystemViewerClock().Now);
        v.UseClock(clock);
        try
        {
            IReadOnlyList<TileHeaderItem> buttons = TileHeaderLayout.Buttons(model);
            TileHeaderMetrics m = TileHeaderMetrics.Standard;
            string placedWhy = "", whyNot = "";
            // Lays the header out at a total width: the buttons shown, and whether they sit inside the tile, apart, and right of the title.
            (IReadOnlyList<TileHeaderItem> Shown, TileHeaderLayout? Layout, bool Placed) At(double width)
            {
                chrome.ForceWidth(width);
                var frames = chrome.ShownButtons.Select(i => chrome.ButtonFrame(i)).Where(f => f is not null).Select(f => f!.Value).OrderBy(f => f.X).ToList();
                bool apart = frames.Zip(frames.Skip(1)).All(p => p.First.X + p.First.Width <= p.Second.X + 0.5);
                bool inside = frames.All(f => f.X >= 0 && f.X + f.Width <= width + 0.5);
                bool clear = frames.Count == 0 || chrome.TitleFrame.X + chrome.TitleFrame.Width <= frames[0].X + 0.5;
                placedWhy = $"{width:0}: apart {apart} inside {inside} clear {clear}; title {chrome.TitleFrame.X:0}+{chrome.TitleFrame.Width:0}; buttons {string.Join(" ", frames.Select(f => $"{f.X:0}+{f.Width:0}"))}";
                if (apart && inside && clear)
                {
                    placedWhy = "";
                }
                else
                {
                    whyNot += placedWhy + " | ";
                }
                return (chrome.ShownButtons, chrome.HeaderLayout, apart && inside && clear);
            }
            // parity: header-priority-narrow
            chrome.ForceWidth(m.Narrow + 100);
            double reserve = chrome.Reserve;
            var narrow = At(110 + reserve);
            // One DIP short of every button: some go into "...".
            double width = reserve + 2 * m.Padding + m.TitleMin + m.Icon * buttons.Count - 1;
            int fit = (int)((width - reserve - 2 * m.Padding - m.TitleMin - m.Icon) / m.Icon);
            var middle = At(width);
            var nudged = At(width + 5);
            var wide = At(1000);
            bool narrowOK = narrow.Shown.SequenceEqual([TileHeaderItem.More]) && narrow.Layout?.Visible.SequenceEqual([TileHeaderItem.Title, TileHeaderItem.More]) == true && narrow.Placed;
            bool middleOK = middle.Shown.SequenceEqual([.. buttons.Take(fit), TileHeaderItem.More]) && middle.Placed
                && middle.Layout?.Visible.Where(i => i != TileHeaderItem.Title).SequenceEqual(middle.Shown) == true;
            bool wideOK = wide.Shown.SequenceEqual(buttons) && wide.Layout?.ShowsMore == false && wide.Placed;
            bool steady = nudged.Shown.SequenceEqual(middle.Shown);
            // The channels event's capabilities reach the model: a doorbell with talk-back, and a second lens that moves.
            using var doc = System.Text.Json.JsonDocument.Parse(
                "{\"channel\":6,\"talk\":true,\"doorbell\":true,\"ptz\":{\"pan_tilt\":false,\"presets\":false,\"zoom_focus\":false},"
                + "\"lens_talk\":[true,false],\"lens_ptz\":[{\"pan_tilt\":false},{\"pan_tilt\":true,\"presets\":true,\"zoom_focus\":true}]}");
            ChannelInfo? caps = ChannelInfo.From(doc.RootElement);
            bool capsOK = caps is { Doorbell: true } && caps.TalkOf(0) && !caps.TalkOf(1) && !caps.PtzOf(0).PanTilt && caps.PtzOf(1) is { PanTilt: true, Presets: true, ZoomFocus: true };
            Say($"header: placed {narrow.Placed}/{middle.Placed}/{wide.Placed}, narrow layout [{string.Join(",", narrow.Layout?.Visible ?? [])}], reserve {reserve:0}, {whyNot}; 110 DIP {string.Join(",", narrow.Shown)}, {width:0} DIP {string.Join(",", middle.Shown)}, 5 DIP wider unchanged {steady}, 1000 DIP {string.Join(",", wide.Shown)}; capabilities parsed {capsOK}");
            Expect(narrowOK, "below 120 DIP the header did not show only the title and the more button");
            Expect(middleOK, $"one DIP short of every button, the header did not show the model's first {fit} buttons and the more button");
            Expect(wideOK, "a wide header did not show every button without the more button");
            Expect(steady, "a 5 DIP width change reflowed the header");
            Expect(capsOK, "the channels event's talk, doorbell and PTZ capabilities were not read");

            // parity: header-overflow-menu
            At(width);
            List<string> more = [.. stage.MoreMenuTexts(tile)];
            List<string> wantMore = chrome.HeaderLayout is { } l ? [.. l.MoreMenu.Select(e => e.Label)] : ["?"];
            List<string> context = Texts(stage.BuildTileMenu(tile)!.Items);
            List<string> wantContext = [.. TileHeaderLayout.Menu(model).Select(e => e.Label)];
            bool menusOK = more.SequenceEqual(wantMore) && context.Take(wantContext.Count).SequenceEqual(wantContext)
                && context.Contains("Split Side by Side") && more.Contains("Entrance") && !more.Contains("Listen");
            Say($"header menus: more [{string.Join(" / ", more)}], right-click starts with the model's {wantContext.Count} items {menusOK}");
            Expect(menusOK, $"the more or right-click menu is not the header model's list ({string.Join(" / ", more)} vs {string.Join(" / ", wantMore)})");
            // Every header action is built: nothing is left to say "coming soon".
        }
        finally
        {
            chrome.ForceWidth(null);
            v.UseClock(new SystemViewerClock());
        }
        await CaptureRecordSteps(v, stage, tile);
    }

    // The call card's picture: one more tile of this window's grid, drawn last from the camera's
    // shared tile (no second decoder), in the card's hole; it comes back after a device loss; the
    // cards stack from the Settings corner, else opposite the indicators.
    private async Task CallCardPicture(Viewer v, StageView stage, CallCard card, CameraRef camera)
    {
        StageRenderer r = app.Renderer!;
        // parity: alert-inset-in-grid
        bool drawn = await Until(() => card.Placed && stage.CallCards.Overlays.Any(o => o.Tile == card.Tile) && card.Tile.Shown > 2 && card.Tile.PictureWidth > 0, 15);
        StageTile? grid = Tiles.FirstOrDefault(t => t.Camera == camera);
        Say($"calls: card picture drawn {drawn}, {card.Tile.Shown} pictures, tile {card.Tile.Id} (grid tile {grid?.Id}), frame {card.Frame}, " +
            $"hole {stage.CallCards.Overlays.FirstOrDefault()?.Rect}, core tiles {v.Tiles.Length} for {v.Leases.Cameras.Count} cameras");
        Expect(drawn, "the call card's picture was not drawn");
        Expect(card.Tile.Id != 0 && (grid is null || grid.Id == card.Tile.Id) && v.Tiles.Length == v.Leases.Cameras.Count, "the call card does not draw the camera's shared tile");
        await CheckPictureFrame("call card", card.Tile, r);

        // parity: call-inset-device-loss
        long shown = card.Tile.Shown;
        int recoveries = r.Recoveries;
        r.SimulateDeviceLoss();
        bool recovered = await Until(() => r.Recoveries > recoveries, 10);
        bool back = recovered && await Until(() => card.Tile.Shown > shown + 5 && card.Tile.PictureWidth > 0, 15);
        Say($"calls: card picture after a device loss: recovered {recovered}, pictures {shown} -> {card.Tile.Shown}");
        Expect(back, "the call card's picture did not come back after a device loss");
        if (back)
        {
            await CheckPictureFrame("call card after device loss", card.Tile, r);
        }

        // parity: call-inset-corner
        double w = stage.ActualWidth, h = stage.ActualHeight;
        ChromeCorner? choice = v.Settings.InsetCornerChoice;
        v.Settings.InsetCornerChoice = ChromeCorner.TopLeft;
        stage.CallCards.UpdateLayout();
        InsetFrame topLeft = card.Frame;
        bool holeMoved = await Until(() => stage.CallCards.Overlays.FirstOrDefault()?.Rect is { } hole && hole.Y < topLeft.Bottom && hole.X < topLeft.Right, 3);
        v.Settings.InsetCornerChoice = null;
        stage.CallCards.UpdateLayout();
        InsetFrame opposite = card.Frame;
        ChromeCorner expected = v.Settings.IndicatorCorner.Opposite();
        Say($"calls: card at {topLeft} with the top left corner chosen (hole followed {holeMoved}); at {opposite} opposite the indicators ({expected}) in {w:0}x{h:0}");
        Expect(Near(topLeft.X, InsetGeometry.Margin) && Near(topLeft.Y, InsetGeometry.Margin), "the call card did not move to the corner chosen in Settings");
        Expect(holeMoved, "the card's picture did not follow the card to its corner");
        Expect(Near(expected.IsLeft() ? opposite.X : w - opposite.Right, InsetGeometry.Margin) &&
            Near(expected.IsTop() ? opposite.Y : h - opposite.Bottom, InsetGeometry.Margin), "the call card is not opposite the indicators by default");
        v.Settings.InsetCornerChoice = choice;
    }

    private static bool Near(double a, double b) => Math.Abs(a - b) < 1.5;

    // ---- moving the call insets (docs/UX.md "Alert insets", "Moving them") ----
    // The pointer is not injected: the steps drive the card's own DragPress/DragMove/DragRelease,
    // which its pointer handlers call with the pointer's place in the stack.

    private static (double X, double Y) MeasuredAnchor(InsetFrame column, double w, double h)
    {
        double m = InsetGeometry.Margin;
        double fx = w - 2 * m - column.Width, fy = h - 2 * m - column.Height;
        return (fx > 0 ? (column.X - m) / fx : 0, fy > 0 ? (column.Y - m) / fy : 0);
    }

    private static bool NearAnchor((double X, double Y) a, InsetAnchor b) => Math.Abs(a.X - b.X) < 0.01 && Math.Abs(a.Y - b.Y) < 0.01;

    // A drag of the card by (dx, dy) from a point on its header or picture, in steps.
    private static void DragCard(CallCard card, double fromX, double fromY, double dx, double dy, bool onHeader)
    {
        card.DragPress(new Windows.Foundation.Point(fromX, fromY), onHeader);
        const int steps = 5;
        for (int i = 1; i <= steps; i++)
        {
            card.DragMove(new Windows.Foundation.Point(fromX + dx * i / steps, fromY + dy * i / steps));
        }
        card.DragRelease();
    }

    // Moves the column so its top left lands on (x, y), grabbing the card's header.
    private static void DragColumnTo(CallStack cards, CallCard grab, double x, double y)
    {
        InsetFrame start = cards.Column!.Value;
        InsetFrame f = grab.Frame;
        DragCard(grab, f.X + 40, f.Y + 8, x - start.X, y - start.Y, onHeader: true);
        cards.UpdateLayout();
    }

    private async Task CallInsetDrag(Viewer v, StageView stage, CallCard card, CameraRef camera)
    {
        CallStack cards = stage.CallCards;
        ViewerWindow window = app.Windows.First(w => w.Stage == stage);
        cards.UpdateLayout();
        double m = InsetGeometry.Margin;
        int changes = 0;
        void Count() => changes++;
        cards.AnchorChanged += Count;
        try
        {
            // parity: inset-drag-snap
            // The header dragged to 10 DIPs from the right edge snaps there and 40% down does not; a
            // small move on the picture is a click and moves nothing; the picture past the slop drags.
            double w = cards.ActualWidth, h = cards.ActualHeight;
            InsetFrame column = cards.Column ?? default;
            bool atDefault = cards.Anchor is null && NearAnchor(MeasuredAnchor(column, w, h), InsetAnchor.FromCorner(v.Settings.InsetCorner));
            InsetFrame f0 = card.Frame;
            DragCard(card, f0.X + 60, f0.Y + 120, 3, 2, onHeader: false);
            bool clickMoved = cards.Anchor is not null || changes != 0;
            double freeX = w - 2 * m - column.Width, freeY = h - 2 * m - column.Height;
            DragColumnTo(cards, card, m + freeX - 10, m + 0.4 * freeY);
            InsetAnchor? anchor = cards.Anchor;
            bool snapped = anchor is { X: 1 } a && Math.Abs(a.Y - 0.4) < 0.01;
            bool placed = cards.Column is { } c1 && Math.Abs(c1.Right - (w - m)) < 0.5;
            bool saved = window.ToSaved() is { InsetX: 1, InsetY: { } sy } && Math.Abs(sy - 0.4) < 0.01 && changes == 1;
            InsetFrame pic = card.Frame;
            DragCard(card, pic.X + 100, pic.Y + 120, -40, 0, onHeader: false);
            cards.UpdateLayout();
            bool pictureDrag = cards.Anchor is { } pa && Math.Abs(pa.X - (freeX - 40) / freeX) < 0.01 && Math.Abs(pa.Y - 0.4) < 0.01;
            Say($"inset drag: default corner {atDefault}, small move on the picture moves nothing {!clickMoved}, header drag snaps x {snapped} (anchor {anchor}, right edge {placed}), " +
                $"saved with the window {saved}, picture drag past the slop {pictureDrag}");
            Expect(atDefault, "the call card column did not start in the default corner");
            Expect(!clickMoved, "a small move on the card's picture moved the column");
            Expect(snapped && placed, "dragging the card's header did not snap to the right edge and keep 40% down");
            Expect(saved, "the moved column was not saved with the window");
            Expect(pictureDrag, "dragging the card's picture past the slop did not move the column");

            // parity: inset-drag-resize
            // Resizing the window keeps the column at the same fraction of the free space.
            InsetAnchor? keep = cards.Anchor;
            SizeInt32 size = window.AppWindow.Size;
            var results = new List<bool>();
            foreach ((int dw, int dh) in new[] { (220, 120), (-120, -60) })
            {
                double before = cards.ActualWidth;
                window.AppWindow.Resize(new SizeInt32(size.Width + dw, size.Height + dh));
                await Until(() => Math.Abs(cards.ActualWidth - before) > 1, 5);
                cards.UpdateLayout();
                results.Add(keep is { } k && cards.Anchor == keep && cards.Column is { } cc && NearAnchor(MeasuredAnchor(cc, cards.ActualWidth, cards.ActualHeight), k));
            }
            double last = cards.ActualWidth;
            window.AppWindow.Resize(size);
            await Until(() => Math.Abs(cards.ActualWidth - last) > 1, 5);
            cards.UpdateLayout();
            Say($"inset drag: the column keeps its place when the window grows {results[0]} and shrinks {results[1]}");
            Expect(results.All(r => r), "resizing the window moved the call column off its fraction");

            // parity: inset-drag-restore
            // The position is saved with the window: the record reads back, a window opened from it puts
            // the column there, and Reset Position returns it to the default corner.
            SavedWindow record = WindowStore.FromJson(System.Text.Json.Nodes.JsonNode.Parse(WindowStore.ToJson([window.ToSaved()])))[0];
            bool readsBack = keep is { } kp && record.InsetX == kp.X && record.InsetY == kp.Y;
            ViewerWindow second = app.OpenWindow(window.State.Content, record);
            bool there = false;
            try
            {
                CallStack other = second.Stage.CallCards;
                other.Sync();
                bool shown = await Until(() => other.Cards.Count == 1 && other.Column is not null, 10);
                other.UpdateLayout();
                there = shown && other.Anchor == keep && other.Column is { } oc && keep is { } ok && NearAnchor(MeasuredAnchor(oc, other.ActualWidth, other.ActualHeight), ok);
            }
            finally
            {
                second.Window.Close();
                await Until(() => app.Windows.Count == 1, 5);
            }
            card.ResetPosition();
            cards.UpdateLayout();
            bool reset = cards.Anchor is null && cards.Column is { } rc && NearAnchor(MeasuredAnchor(rc, cards.ActualWidth, cards.ActualHeight), InsetAnchor.FromCorner(v.Settings.InsetCorner))
                && window.ToSaved() is { InsetX: null, InsetY: null };
            Say($"inset drag: saved record reads back {readsBack}, a window opened from it puts the column there {there}, Reset Position {reset}");
            Expect(readsBack, "the window's saved record lost the call column's position");
            Expect(there, "a window opened from the record did not put its call column there");
            Expect(reset, "Reset Position did not return the call column to the default corner");

            // parity: inset-drag-multi
            // Two cards move as one column: at the bottom the newest is at the bottom and the other stacks
            // upward, at the top the reverse. They need the room for two, so the window is made taller.
            var second2 = new CameraRef(camera.Device, camera.Channel + 1);
            v.Calls.StartConversation(second2);
            cards.Sync();
            SizeInt32 tall = window.AppWindow.Size;
            double heightBefore = cards.ActualHeight;
            window.AppWindow.Resize(new SizeInt32(tall.Width, tall.Height + 600));
            await Until(() => Math.Abs(cards.ActualHeight - heightBefore) > 1, 5);
            bool two = await Until(() => { cards.UpdateLayout(); return cards.Cards.Count == 2 && cards.Cards.All(c => c.Placed); }, 5);
            if (two)
            {
                w = cards.ActualWidth;
                h = cards.ActualHeight;
                CallCard newest = cards.Cards[1], oldest = cards.Cards[0];
                InsetFrame start = cards.Column!.Value;
                DragColumnTo(cards, newest, 15, h - start.Height - 15);
                double gap = InsetGeometry.Gap;
                bool upward = cards.Anchor == new InsetAnchor(0, 1) && Math.Abs(newest.Frame.X - m) < 0.5 && Math.Abs(oldest.Frame.X - m) < 0.5
                    && Math.Abs(newest.Frame.Bottom - (h - m)) < 0.5 && Math.Abs(oldest.Frame.Bottom - (newest.Frame.Y - gap)) < 0.5;
                InsetFrame now = cards.Column!.Value;
                DragColumnTo(cards, newest, m + (w - 2 * m - now.Width) / 3, 20);
                bool downward = cards.Anchor is { Y: 0 } ta && Math.Abs(ta.X - 1.0 / 3) < 0.01 && Math.Abs(newest.Frame.X - oldest.Frame.X) < 0.5
                    && Math.Abs(newest.Frame.Y - m) < 0.5 && Math.Abs(oldest.Frame.Y - (newest.Frame.Bottom + gap)) < 0.5;
                Say($"inset drag with two cards: one column, bottom grows upward {upward}, top grows downward {downward}");
                Expect(upward, $"two cards at the bottom did not stack upward from it in one column: {newest.Frame} {oldest.Frame}");
                Expect(downward, $"two cards at the top did not stack downward from it in one column: {newest.Frame} {oldest.Frame}");
            }
            else
            {
                Say($"inset drag with two cards: both did not fit in {cards.ActualWidth:0}x{cards.ActualHeight:0} ({string.Join(", ", cards.Cards.Select(c => c.Frame))})");
                Expect(false, "two call cards did not fit the stage even after making the window taller");
            }
            v.Calls.End(second2, "closed");
            cards.Sync();
            cards.ResetPosition();
            window.AppWindow.Resize(tall);
            await Until(() => Math.Abs(cards.ActualHeight - heightBefore) < 1, 5);
            cards.UpdateLayout();
        }
        finally
        {
            cards.AnchorChanged -= Count;
        }
    }

    // parity: zoom-fills-tile
    // A letterboxed tile zoomed past the point where its picture fills the
    // tile: the former bar shows picture; back at 1x it is black again.
    private async Task CheckZoomFillsTile()
    {
        StageRenderer r = app.Renderer!;
        StageTile? boxed = Tiles.FirstOrDefault(t => !(t.Fisheye is { } f && f.Mode.Dewarps()) && ZoomBar(t) is not null);
        if (boxed is null || ZoomBar(boxed) is not { } bar)
        {
            _problems.Add("no letterboxed tile to check the zoom on");
            return;
        }
        double? before = await BarLit(r, bar);
        ZoomBase b = boxed.ZoomBase;
        // Past the scale where the picture fills the tile's short side.
        double past = Math.Min(1.25 / Math.Min(b.Width, b.Height), TileZoom.MaxScale);
        StageView.ZoomAround(boxed, 0.5, 0.5, past);
        double? zoomed = await BarLit(r, bar);
        StageView.ResetZoom(boxed);
        double? after = await BarLit(r, bar);
        Say($"zoom fills the tile: tile {boxed.Id} at {past:0.00}x, bar lit {before:0.00} at 1x, {zoomed:0.00} zoomed, {after:0.00} back at 1x");
        Expect(before is < 0.02, "the letterbox bar was not black at 1x");
        Expect(zoomed is >= 0.5, "zoomed past filling the tile, the letterbox bar still shows no picture");
        Expect(after is < 0.02, "back at 1x, the letterbox bar is not black");
    }

    // parity: zoom-map
    // parity: zoom-reset-control
    // parity: zoom-touch-pinch
    // The zoom map and the header's reset control follow a tile's zoom, and a touch screen's pinch zooms
    // the digital zoom around the point between the fingers.
    private async Task CheckZoomChrome()
    {
        StageView stage = app.Stage!;
        StageTile? t = Tiles.FirstOrDefault(x => !(x.Fisheye is { } f && f.Mode.Dewarps()) && x.PictureWidth > 0 && x.ViewWidth > 0 && x.ZoomBase.Width > 0.99);
        TileChrome? chrome = t is null ? null : stage.ChromeFor(t);
        if (t is null || chrome is null)
        {
            _problems.Add("no ordinary tile to check the zoom map on");
            return;
        }
        StageView.ResetZoom(t);
        stage.Interact();
        stage.Tick();
        await Task.Delay(100);
        bool idleHidden = !chrome.MapShown && !chrome.ResetShown;
        // The pinch: spreading zooms in around the point between the fingers; closing zooms back out; junk changes nothing.
        double cx = t.ViewX + t.ViewWidth * 0.25, cy = t.ViewY + t.ViewHeight * 0.25;
        stage.PinchDigital(t, cx, cy, 2);
        bool pinchedIn = Math.Abs(t.Zoom - 2) < 0.01;
        double pointX = t.CenterX;
        stage.PinchDigital(t, cx, cy, double.NaN);
        bool junkIgnored = Math.Abs(t.Zoom - 2) < 0.01;
        for (int i = 0; i < 8; i++)
        {
            stage.PinchDigital(t, cx, cy, 2);
        }
        bool bounded = Math.Abs(t.Zoom - TileZoom.MaxScale) < 0.01;
        for (int i = 0; i < 8; i++)
        {
            stage.PinchDigital(t, cx, cy, 0.5);
        }
        bool pinchedOut = t.Zoom <= 1.0001f;
        Say($"zoom touch pinch: in {pinchedIn} (center x {pointX:0.00}), junk ignored {junkIgnored}, bounded at {TileZoom.MaxScale:0}x {bounded}, back out {pinchedOut}");
        Expect(pinchedIn && pointX < 0.5, $"a pinch spread did not zoom in around the fingers (zoom {t.Zoom:0.00}, center x {pointX:0.00})");
        Expect(junkIgnored, "a pinch step of NaN changed the zoom");
        Expect(bounded, $"pinching past the limit did not stop at the maximum zoom ({t.Zoom:0.00})");
        Expect(pinchedOut, $"pinching closed did not return to 1x ({t.Zoom:0.00})");

        // The map and the reset control, zoomed.
        StageView.ZoomAround(t, 0.25, 0.25, 2);
        stage.Interact();
        stage.Tick();
        bool shown = await Until(() => chrome.MapShown && chrome.ResetShown, 3);
        (UnitRect frame, UnitRect visible) = chrome.MapGeometry;
        TileZoom zoom = t.DigitalZoom;
        UnitRect d = zoom.Destination(t.ZoomBase);
        double left = t.ViewX + d.X * t.ViewWidth, bottom = t.ViewY + (d.Y + d.Height) * t.ViewHeight;
        bool corner = Math.Abs(frame.X - left - ZoomMap.Margin) < 1.5 && Math.Abs(bottom - ZoomMap.Margin - (frame.Y + frame.Height)) < 1.5;
        bool shape = frame.Height > 0 && Math.Abs(frame.Width / frame.Height - t.PictureWidth / (double)t.PictureHeight) < 0.1;
        UnitRect want = zoom.Visible(t.ZoomBase);
        bool region = Math.Abs(visible.X - want.X) < 1e-6 && Math.Abs(visible.Width - want.Width) < 1e-6 && visible.Width < 1;
        Say($"zoom map: shown {shown}, frame {frame.Width:0}x{frame.Height:0}, bottom left corner {corner}, picture shape {shape}, marks the visible region {region}");
        Expect(shown, "zoomed and active, the tile shows no zoom map or no reset control");
        Expect(corner && shape && region, $"the zoom map is misplaced or marks the wrong region: frame {frame}, visible {visible}, wanted corner at {left:0},{bottom:0}");

        // It fades with the chrome (a few idle seconds) and returns with the pointer.
        bool faded = await Until(() =>
        {
            stage.Tick();
            return !chrome.MapShown;
        }, ChromeIdle.HideAfterSeconds + 4);
        stage.Interact();
        stage.Tick();
        bool back = await Until(() => chrome.MapShown, 3);
        Say($"zoom map: fades with the chrome {faded}, returns with the pointer {back}, hidden at 1x before {idleHidden}");
        Expect(faded && back, "the zoom map did not fade with the chrome and return with the pointer");
        Expect(idleHidden, "at 1x the tile showed a zoom map or a reset control");

        // The header's reset control returns the tile to 1x and takes the map and itself away.
        chrome.PressReset();
        bool reset = t.Zoom <= 1.0001f && t.CenterX == 0.5f && t.CenterY == 0.5f;
        bool gone = await Until(() => !chrome.MapShown && !chrome.ResetShown, 3);
        Say($"zoom reset control: back to 1x {reset}, control and map gone {gone}");
        Expect(reset, $"the reset control did not return the tile to 1x ({t.Zoom:0.00})");
        Expect(gone, "after the reset the tile still shows the reset control or the zoom map");
    }

    // parity: fill-tiles
    // Fill Tiles crops the picture to fill its tile (the former letterbox bar shows picture) and
    // turning it off fits it again; a zoom still works from the crop.
    private async Task CheckFillTiles()
    {
        StageView stage = app.Stage!;
        StageRenderer r = app.Renderer!;
        StageTile? boxed = Tiles.FirstOrDefault(t => !(t.Fisheye is { } f && f.Mode.Dewarps()) && ZoomBar(t) is not null);
        if (boxed is null || ZoomBar(boxed) is not { } bar)
        {
            _problems.Add("no letterboxed tile to check Fill Tiles on");
            return;
        }
        double? fitted = await BarLit(r, bar);
        stage.SetFill(true);
        double? filled = await BarLit(r, bar);
        ZoomBase b = boxed.ZoomBase;
        bool crop = boxed.Fill && Math.Max(b.Width, b.Height) > 1.01 && Math.Min(b.Width, b.Height) > 0.999;
        bool tileArea = Near(boxed.X, boxed.ViewX) && Near(boxed.Width, boxed.ViewWidth) && Near(boxed.Y, boxed.ViewY) && Near(boxed.Height, boxed.ViewHeight);
        StageView.ZoomAround(boxed, 0.5, 0.5, 2);
        double? zoomedFilled = await BarLit(r, bar);
        bool reaches = boxed.DigitalZoom.Visible(boxed.ZoomBase).Width < 1;
        StageView.ResetZoom(boxed);
        stage.SetFill(false);
        double? fittedAgain = await BarLit(r, bar);
        Say($"fill tiles: bar lit {fitted:0.00} fitted, {filled:0.00} filled, {zoomedFilled:0.00} filled and zoomed, {fittedAgain:0.00} fitted again; base {b.Width:0.00}x{b.Height:0.00}, tile area {tileArea}");
        Expect(fitted is < 0.02, "the letterbox bar was not black before Fill Tiles");
        Expect(filled is >= 0.5, "with Fill Tiles the former letterbox bar still shows no picture");
        Expect(crop && tileArea, $"Fill Tiles did not crop the picture to the tile (base {b.Width:0.00}x{b.Height:0.00}, tile area {tileArea})");
        Expect(zoomedFilled is >= 0.5 && reaches, "zooming from the filled crop lost the picture");
        Expect(fittedAgain is < 0.02 && !boxed.Fill, "turning Fill Tiles off did not fit the picture again");
    }

    // Fill per tile, the filled crop's pan, the header swap, and the window's record (docs/UX.md "Fill
    // Tiles"): a letterboxed tile gets its own Fill tile (the others stay fitted); a drag on its picture
    // moves the crop and stops at the picture's edge; a drag on its shown header swaps it; the record
    // brings the crop and the single tile's choice back in a new window; Fill Tiles then sets every tile.
    private async Task CheckFillPan()
    {
        StageView stage = app.Stage!;
        ViewerWindow window = app.Windows.First(w => w.Stage == stage);
        StageTile? boxed = Tiles.FirstOrDefault(t => !(t.Fisheye is { } f && f.Mode.Dewarps()) && ZoomBar(t) is not null);
        StageTile? other = boxed is null ? null : Tiles.FirstOrDefault(t => t.CameraOrNone != boxed.CameraOrNone && !(t.Fisheye is { } f && f.Mode.Dewarps()));
        if (boxed?.CameraOrNone is not { } camera || other?.CameraOrNone is not { } otherCamera)
        {
            _problems.Add("no letterboxed tile and second camera for the Fill pan checks");
            return;
        }

        // parity: fill-per-tile
        var menu = stage.BuildTileMenu(boxed);
        var fillItem = menu?.Items.OfType<Microsoft.UI.Xaml.Controls.ToggleMenuFlyoutItem>().FirstOrDefault(i => i.Text == "Fill Tile");
        bool offAtFirst = fillItem is { IsChecked: false };
        stage.SetTileFill(boxed, true);
        bool published = await Until(() => boxed.Fill, 5);
        bool single = stage.Fills(boxed) && !stage.Fills(other) && stage.FillMixed && !other.WantsFill;
        bool checkedNow = stage.BuildTileMenu(boxed)?.Items.OfType<Microsoft.UI.Xaml.Controls.ToggleMenuFlyoutItem>().Any(i => i.Text == "Fill Tile" && i.IsChecked) == true;
        Say($"fill per tile: menu item {(fillItem is null ? "missing" : "present")}, this tile fills {stage.Fills(boxed)} (drawn {published}), another {stage.Fills(other)}, mixed {stage.FillMixed}, checked {checkedNow}");
        Expect(offAtFirst, "the tile menu has no unchecked Fill tile on a fitted tile");
        Expect(published && single, "Fill tile on one tile did not fill only it");
        Expect(checkedNow, "Fill tile is not checked on the tile that fills");

        // parity: fill-pan-drag
        ZoomBase b = boxed.ZoomBase;
        bool wide = b.Width > 1;
        var middle = new Point(boxed.ViewX + boxed.ViewWidth / 2, boxed.ViewY + boxed.ViewHeight / 2 + 20);
        string? kind = stage.DragKind(middle);
        double step = wide ? boxed.ViewWidth / 8 : boxed.ViewHeight / 8;
        stage.SimulateDrag(middle, wide ? new Point(middle.X + step, middle.Y) : new Point(middle.X, middle.Y + step));
        TileZoom want = TileZoom.None.Panned(wide ? 0.125 : 0, wide ? 0 : 0.125, b);
        TileZoom got = boxed.DigitalZoom;
        bool followed = Math.Abs(got.CenterX - want.CenterX) < 1e-4 && Math.Abs(got.CenterY - want.CenterY) < 1e-4 && got.IsPanned;
        stage.SimulateDrag(middle, wide ? new Point(middle.X + step * 40, middle.Y) : new Point(middle.X, middle.Y + step * 40));
        UnitRect edge = boxed.DigitalZoom.Visible(b);
        bool stopped = wide ? Math.Abs(edge.X) < 1e-4 : Math.Abs(edge.Y) < 1e-4;
        bool fittedSwaps = stage.DragKind(new Point(other.ViewX + other.ViewWidth / 2, other.ViewY + other.ViewHeight / 2 + 20)) == "swap";
        bool shared = stage.State.ViewKey(boxed.Path) is { } bk && stage.State.Views.Zoom(bk) == boxed.DigitalZoom;
        Say($"fill pan drag: kind {kind}, crop followed {followed} ({got.CenterX:0.0000},{got.CenterY:0.0000} for {want.CenterX:0.0000},{want.CenterY:0.0000}), stopped at the edge {stopped}, kept for the tile {shared}, a fitted tile swaps {fittedSwaps}");
        Expect(kind == "pan", "a drag on a filled, cropped picture would not pan it");
        Expect(followed, "dragging the filled picture did not move the crop with the drag");
        Expect(stopped, "panning the filled picture did not stop at its edge");
        Expect(shared, "the crop was not kept for the tile");
        Expect(fittedSwaps, "a drag on a fitted tile's picture no longer swaps it");
        // Back from the edge to a position of its own, for the restore check.
        stage.SimulateDrag(middle, wide ? new Point(middle.X - step * 2, middle.Y) : new Point(middle.X, middle.Y - step * 2));
        TileZoom crop = boxed.DigitalZoom;

        // parity: fill-pan-header-swap
        NodePath from = boxed.Path, to = other.Path;
        stage.Hover(boxed);
        var onHeader = new Point(boxed.FrameX + boxed.FrameWidth / 2, boxed.FrameY + 10);
        string? headerKind = stage.DragKind(onHeader);
        stage.SimulateDrag(onHeader, new Point(other.ViewX + other.ViewWidth / 2, other.ViewY + other.ViewHeight / 2));
        bool swapped = stage.State.Tree[from] is LayoutLeaf { Cam: { } a } && a == otherCamera && stage.State.Tree[to] is LayoutLeaf { Cam: { } c2 } && c2 == camera;
        StageTile? moved = stage.Tiles.FirstOrDefault(t => t.CameraOrNone == camera);
        bool kept = moved is not null && moved.DigitalZoom == crop;
        if (swapped && moved is not null)
        {
            await Until(() => moved.Visible && moved.Width > 1, 5);
            StageTile back = stage.Tiles.First(t => t.Path == from);
            await Until(() => back.Visible && back.ViewWidth > 1, 5);
            stage.Hover(moved);
            stage.SimulateDrag(new Point(moved.FrameX + moved.FrameWidth / 2, moved.FrameY + 10), new Point(back.ViewX + back.ViewWidth / 2, back.ViewY + back.ViewHeight / 2));
        }
        bool swappedBack = stage.State.Tree[from] is LayoutLeaf { Cam: { } a2 } && a2 == camera;
        Say($"fill pan header swap: kind {headerKind}, swapped {swapped}, crop kept with the camera {kept}, swapped back {swappedBack}");
        Expect(headerKind == "swap", "a drag on the header of a panning tile would not swap it");
        Expect(swapped, "dragging a tile's header onto another did not swap them");
        Expect(kept, "the crop did not follow the camera through the swap");
        Expect(swappedBack, "dragging the header back did not swap the tiles back");

        // The tiles were made anew by the swaps.
        boxed = stage.Tiles.First(t => t.CameraOrNone == camera);
        other = stage.Tiles.First(t => t.CameraOrNone == otherCamera);
        StageTile b2 = boxed, o2 = other;
        await Until(() => b2.Visible && b2.Fill && o2.Visible, 5);

        // parity: fill-pan-restore
        // The record keeps the crop and the Fill choice under the tile's identity; a window opened from it
        // shows them in that tile.
        SavedWindow record = WindowStore.FromJson(System.Text.Json.Nodes.JsonNode.Parse(WindowStore.ToJson([window.ToSaved()])))[0];
        TileViews<TileViewKey> saved = record.PictureViews;
        TileViewKey? key = stage.State.ViewKey(boxed.Path), otherKey = stage.State.ViewKey(other.Path);
        bool readsBack = key is { } k1 && otherKey is { } k2 && saved.Zoom(k1) == crop && saved.Fills(k1) && !saved.Fills(k2)
            && record.Views?.Entries.Any(e => e.Key.Tile == k1.Tile && e.Key.Layout == stage.State.ViewsKey) == true;
        ViewerWindow second = app.OpenWindow(window.State.Content, record);
        bool same = false;
        try
        {
            NodePath boxedPath = boxed.Path, otherPath = other.Path;
            StageTile? there = null;
            bool drawn = await Until(() => (there = second.Stage.Tiles.FirstOrDefault(t => t.Path == boxedPath && t.CameraOrNone == camera)) is { Fill: true, PictureWidth: > 0 }, 10);
            StageTile? fitted = second.Stage.Tiles.FirstOrDefault(t => t.Path == otherPath);
            same = drawn && there!.DigitalZoom == crop && second.Stage.Fills(there) && fitted is not null && !second.Stage.Fills(fitted) && !fitted.WantsFill;
        }
        finally
        {
            second.Window.Close();
            await Until(() => app.Windows.Count == 1, 5);
        }
        Say($"fill pan restore: record reads back per tile {readsBack}, a window opened from it has the tile's crop and Fill {same}");
        Expect(readsBack, "the window's saved record lost the tile's crop or Fill choice");
        Expect(same, "a window opened from the record did not get the tile's crop and Fill choice back");

        // parity: fill-window-switch
        // The window's switch sets every tile; the crop stays with the tile; Reset position recenters it.
        stage.SetFill(true);
        bool allFill = stage.Fills(boxed) && stage.Fills(other) && !stage.FillMixed;
        stage.SetFill(false);
        bool allFit = !stage.Fills(boxed) && !stage.Fills(other) && !stage.FillMixed;
        stage.SetTileFill(boxed, true);
        bool offered = stage.BuildTileMenu(boxed)?.Items.OfType<Microsoft.UI.Xaml.Controls.MenuFlyoutItem>().Any(i => i.Text == "Reset Position") == true;
        StageView.ResetZoom(boxed);
        bool reset = boxed.DigitalZoom == TileZoom.None && key is { } k3 && stage.State.Views.Zoom(k3) == TileZoom.None;
        stage.SetTileFill(boxed, false);
        Say($"fill per tile: Fill Tiles fills every tile {allFill}, then fits every tile {allFit}; Reset position offered {offered}, recenters {reset}");
        Expect(allFill && allFit, "Fill Tiles did not set every tile of the window");
        Expect(offered && reset, "Reset position was not offered or did not recenter the filled crop");
    }

    // Zoom per tile (docs/UX.md "Digital zoom"): two tiles of one camera are independent views. Show camera
    // puts the camera in a second tile at the default view; each tile zooms on its own, focus keeps a tile's
    // zoom, and resetting one leaves the other. Then a swap moves each view with its camera while the tiles
    // keep their identities, and the layout is put back as it was.
    // A press, a long drag and a release on a zoomed picture through the pointer path the handlers take (the
    // button state each move carries, a mouse drag also starting a manipulation, the hover header appearing
    // under the pointer and capture going away while the button is down): the picture follows the pointer 1:1
    // the whole way, and the tile's frame (outline and header) is the tile's area at 1x, zoomed, and focused.
    // WinUI cannot be fed real pointer events from inside the app without moving the user's pointer, so the
    // handlers' own entry points stand in for them.
    private async Task CheckPanAndFrames()
    {
        StageView stage = app.Stage!;
        if (Tiles.FirstOrDefault(x => !(x.Fisheye is { } f && f.Mode.Dewarps()) && ZoomBar(x) is not null) is not { } t
            || stage.ChromeFor(t) is not { } chrome)
        {
            _problems.Add("no letterboxed tile for the pan and frame checks");
            return;
        }
        bool FrameIsTile() => chrome.Placed is var c && Near(c.X, t.ViewX) && Near(c.Y, t.ViewY) && Near(c.Width, t.ViewWidth) && Near(c.Height, t.ViewHeight);

        // parity: tile-frame-zoom
        bool at1x = await Until(FrameIsTile, 3);
        var picture = (t.X, t.Y, t.Width, t.Height);
        bool letterboxed = picture.Width < t.ViewWidth - 2 || picture.Height < t.ViewHeight - 2;
        StageView.ZoomAround(t, 0.5, 0.5, 4);
        bool drawnZoomed = await Until(() => t.Zoom > 3.9f, 3);
        bool zoomedFrame = drawnZoomed && await Until(FrameIsTile, 3);
        Say($"tile frame zoom: letterboxed {letterboxed}, frame is the tile at 1x {at1x} and at 4x {zoomedFrame} ({chrome.Placed.Width:0}x{chrome.Placed.Height:0} for a tile {t.ViewWidth:0}x{t.ViewHeight:0})");
        Expect(at1x && zoomedFrame, "the tile's frame is not the tile's area (it stays at the fitted picture)");

        // parity: pan-continuous
        TileZoom from = t.DigitalZoom;
        ZoomBase b = t.ZoomBase;
        var start = new Point(t.ViewX + t.ViewWidth / 2, t.ViewY + t.ViewHeight / 2 + 20);
        bool began = stage.DragKind(start) == "pan" && stage.PointerDown(start);
        const int moves = 60;
        double sx = 0.5, sy = 0.25;
        for (int i = 1; i <= moves; i++)
        {
            stage.PointerMove(new Point(start.X + i * sx, start.Y + i * sy), buttonDown: true);
            if (i == 8)
            {
                // A mouse drag past the system's threshold starts a manipulation too.
                stage.ManipulationBegan(touch: false);
            }
            if (i == 20)
            {
                // The hover header appears under the pointer, and capture goes away while the button is down.
                stage.Hover(t);
                stage.PointerCaptureLostWith(buttonDown: true);
            }
        }
        var end = new Point(start.X + moves * sx, start.Y + moves * sy);
        stage.PointerUp(end);
        TileZoom got = t.DigitalZoom;
        // The whole picture's length at this zoom (it overflows the tile), so a change of center is a distance
        // the picture moved (the drawn rectangle itself is clipped to the tile).
        double lengthX = from.Scale * b.Width * t.ViewWidth, lengthY = from.Scale * b.Height * t.ViewHeight;
        double movedX = (from.CenterX - got.CenterX) * lengthX, movedY = (from.CenterY - got.CenterY) * lengthY;
        bool oneToOne = Math.Abs(movedX - moves * sx) < 0.5 && Math.Abs(movedY - moves * sy) < 0.5;
        // After the release, moves without the button do nothing.
        stage.PointerMove(new Point(end.X + 40, end.Y + 40), buttonDown: false);
        bool ended = t.DigitalZoom == got;
        Say($"pan continuous: began {began}, {moves} moves of {moves * sx:0.0},{moves * sy:0.0} in all moved the picture {movedX:0.00},{movedY:0.00}, ended at release {ended}");
        Expect(began && oneToOne, "a long drag did not pan the picture 1:1 with the pointer to its end");
        Expect(ended, "the pan went on after the release");

        // parity: tile-frame-focus
        stage.FocusOn(t);
        double w = stage.ActualWidth, h = stage.ActualHeight;
        bool focusFrame = await Until(() => chrome.Placed is var c && Near(c.X, 0) && Near(c.Y, 0) && Near(c.Width, w) && Near(c.Height, h), FocusAnimation.Seconds + 3);
        bool pictureSmaller = t.Width < w - 2 || t.Height < h - 2 || t.Zoom > 1.0001f;
        stage.Unfocus();
        bool unfocused = await Until(() => app.Renderer!.Focus is null, FocusAnimation.Seconds + 2);
        bool backFrame = unfocused && await Until(FrameIsTile, 3);
        StageView.ResetZoom(t);
        Say($"tile frame focus: frame is the whole stage {focusFrame} ({chrome.Placed.Width:0}x{chrome.Placed.Height:0} of {w:0}x{h:0}, picture {t.Width:0}x{t.Height:0}, zoomed or smaller {pictureSmaller}), back to the tile {backFrame}");
        Expect(focusFrame, "a focused tile's frame hugs the picture instead of the stage");
        Expect(backFrame, "the tile's frame did not come back to its cell after focus");
    }

    private async Task CheckZoomPerTile()
    {
        StageView stage = app.Stage!;
        StageTile? first = Tiles.FirstOrDefault(t => !(t.Fisheye is { } f && f.Mode.Dewarps()));
        StageTile? next = first is null ? null : Tiles.FirstOrDefault(t => t != first && t.CameraOrNone != first.CameraOrNone && !(t.Fisheye is { } f && f.Mode.Dewarps()));
        if (first?.CameraOrNone is not { } camera || next?.CameraOrNone is not { } otherCamera)
        {
            _problems.Add("no two plain tiles for the zoom per tile checks");
            return;
        }
        NodePath path = first.Path, second = next.Path;
        StageTile At(NodePath p) => stage.Tiles.First(t => t.Path == p);
        static bool Scale(StageTile t, double s) => Math.Abs(t.DigitalZoom.Scale - s) < 0.01;

        // parity: zoom-per-tile
        stage.Assign(camera, second);
        bool fresh = At(second).CameraOrNone == camera && At(second).DigitalZoom == TileZoom.None;
        StageView.ZoomAround(At(path), 0.5, 0.5, 2);
        StageView.ZoomAround(At(second), 0.5, 0.5, 3);
        bool independent = Scale(At(path), 2) && Scale(At(second), 3) && stage.State.ViewKey(path) is { } ka && stage.State.ViewKey(second) is { } kb
            && ka != kb && Math.Abs(stage.State.Views.Zoom(ka).Scale - 2) < 0.01 && Math.Abs(stage.State.Views.Zoom(kb).Scale - 3) < 0.01;
        stage.FocusOn(At(second));
        bool focusKeeps = stage.IsFocused && Scale(At(second), 3);
        stage.Unfocus();
        bool unfocused = await Until(() => app.Renderer!.Focus is null, FocusAnimation.Seconds + 2);
        bool unfocusKeeps = unfocused && Scale(At(second), 3) && Scale(At(path), 2);
        StageView.ResetZoom(At(second));
        bool resetOne = At(second).DigitalZoom == TileZoom.None && Scale(At(path), 2);
        Say($"zoom per tile: one camera in two tiles at {At(path).DigitalZoom.Scale:0.00}x and 3x, Show camera starts at 1x {fresh}, independent {independent}, focus keeps {focusKeeps}, unfocus keeps {unfocusKeeps}, reset one tile only {resetOne}");
        Expect(fresh, "Show camera did not start the tile at the default view");
        Expect(independent, "two tiles of one camera did not zoom independently");
        Expect(focusKeeps && unfocusKeeps, "focus lost a tile's own zoom");
        Expect(resetOne, "resetting one tile's zoom changed another tile of the same camera");

        // parity: zoom-follows-swap
        stage.Assign(otherCamera, second);
        bool showReset = At(second).CameraOrNone == otherCamera && At(second).DigitalZoom == TileZoom.None;
        StageView.ZoomAround(At(second), 0.5, 0.5, 3);
        string? idA = stage.State.Tree[path]?.TileId(), idB = stage.State.Tree[second]?.TileId();
        stage.Swap(path, second);
        bool moved = At(second).CameraOrNone == camera && Scale(At(second), 2) && At(path).CameraOrNone == otherCamera && Scale(At(path), 3);
        bool idsStay = idA is not null && stage.State.Tree[path]?.TileId() == idA && stage.State.Tree[second]?.TileId() == idB;
        stage.Swap(second, path);
        bool back = At(path).CameraOrNone == camera && Scale(At(path), 2) && At(second).CameraOrNone == otherCamera && Scale(At(second), 3);
        StageView.ResetZoom(At(path));
        StageView.ResetZoom(At(second));
        bool clean = At(path).DigitalZoom == TileZoom.None && At(second).DigitalZoom == TileZoom.None
            && stage.State.ViewKey(path) is { } kp && stage.State.Views.Zoom(kp) == TileZoom.None;
        Say($"zoom follows swap: Show camera resets {showReset}, views moved with the cameras {moved}, tiles kept their identities {idsStay}, swapped back {back}, reset {clean}");
        Expect(showReset, "Show camera did not reset the tile's view");
        Expect(moved, "a swap did not move each tile's zoom with its camera");
        Expect(idsStay, "a swap moved the tiles' identities");
        Expect(back && clean, "swapping back did not bring the zooms back");
    }

    // One of a tile's bars at 1x (DIPs, a unit in from its edges), or null if the picture fills it.
    private static Rect? ZoomBar(StageTile t)
    {
        if (t.PictureWidth <= 0 || t.ViewWidth <= 0 || t.ViewHeight <= 0)
        {
            return null;
        }
        if (t.Y - t.ViewY >= 4)
        {
            return new Rect(t.ViewX + 2, t.ViewY + 1, t.ViewWidth - 4, t.Y - t.ViewY - 2);
        }
        if (t.X - t.ViewX >= 4)
        {
            return new Rect(t.ViewX + 1, t.ViewY + 2, t.X - t.ViewX - 2, t.ViewHeight - 4);
        }
        return null;
    }

    // The share of a rectangle's pixels (DIPs; sampled) that are not black in a frame begun after
    // the caller's change: the next frame may already be in flight with the old zoom (the render
    // thread reads it at the start of a frame), so that one is skipped and the one after is read.
    private async Task<double?> BarLit(StageRenderer r, Rect bar)
    {
        StageCapture c = null!;
        for (int i = 0; i < 2; i++)
        {
            Task<StageCapture> task = r.CaptureAsync();
            if (await Task.WhenAny(task, Task.Delay(5000)) != task)
            {
                _problems.Add("no frame was drawn for the zoom check");
                return null;
            }
            c = await task;
        }
        int lit = 0, n = 0;
        double step = Math.Max(Math.Min(bar.Width, bar.Height) / 8, 1);
        for (double y = bar.Y; y < bar.Y + bar.Height; y += step)
        {
            for (double x = bar.X; x < bar.X + bar.Width; x += step)
            {
                int px = (int)(x * c.ScaleX), py = (int)(y * c.ScaleY);
                if (px < 0 || py < 0 || px >= c.Image.Width || py >= c.Image.Height)
                {
                    continue;
                }
                n++;
                (int cr, int cg, int cb) = c.Image.Rgb(px, py);
                if (cr + cg + cb > 24)
                {
                    lit++;
                }
            }
        }
        return n > 0 ? lit / (double)n : null;
    }

    // A frame read back: the card's picture is the last tile drawn and is not flat.
    private async Task CheckPictureFrame(string what, StageTile picture, StageRenderer r)
    {
        Task<StageCapture> task = r.CaptureAsync();
        if (await Task.WhenAny(task, Task.Delay(5000)) != task)
        {
            _problems.Add($"no {what} frame was drawn");
            return;
        }
        StageCapture c = await task;
        (StageTile last, float x, float y, float cw, float ch) = c.Rects.Length > 0 ? c.Rects[^1] : default;
        double spread = Spread(c.Image, x, y, cw, ch);
        Say($"{what} frame {c.Image.Width}x{c.Image.Height}: drawn last {last == picture}, {cw:0}x{ch:0} px at {x:0},{y:0}, spread {spread:0.0}");
        Expect(last == picture, $"the {what} picture is not drawn last (on top)");
        Expect(spread >= 8, $"the {what} picture is flat (spread {spread:0.0})");
    }

    // The doorbell fills the stage (focused): its controls show over the stage instead of a card,
    // and the card's lease and picture go; back in the grid, the card returns.
    private async Task CallOverTheStage(Viewer v, StageView stage, CallCard card, CameraRef camera, int leasesBefore)
    {
        StageRenderer r = app.Renderer!;
        // parity: call-stage-controls
        if (Tiles.FirstOrDefault(t => t.Camera == camera) is not { } tile)
        {
            _problems.Add("no tile shows the call's camera to focus");
            return;
        }
        stage.FocusOn(tile);
        stage.CallCards.UpdateLayout();
        bool overStage = card.Mode == CallCardMode.Stage;
        bool undrawn = await Until(() => r.Overlays.Length == 0, 3);
        InsetFrame f = card.Frame;
        double w = stage.ActualWidth, h = stage.ActualHeight;
        Say($"calls: doorbell focused: controls over the stage {overStage} at {f} of {w:0}x{h:0}, picture drawn {!undrawn}, leases {v.Leases.LeaseCount(camera)} (were {leasesBefore})");
        Expect(overStage && Near(f.Bottom, h - InsetGeometry.Margin) && Near(f.MidX, w / 2), "the call's controls did not move over the stage when the doorbell filled it");
        Expect(undrawn && stage.CallCards.LeasesHeld == 0 && v.Leases.LeaseCount(camera) == leasesBefore + 1, "the card kept its picture or lease while the doorbell filled the stage");
        long shown = card.Tile.Shown;
        stage.Unfocus();
        bool unfocused = await Until(() => r.Focus is null, FocusAnimation.Seconds + 2);
        stage.CallCards.UpdateLayout();
        bool returned = await Until(() => card.Mode == CallCardMode.Inset && r.Overlays.Any(o => o.Tile == card.Tile) && card.Tile.Shown > shown + 2, 10);
        Say($"calls: back in the grid: card {card.Mode}, picture drawn {returned}, leases {v.Leases.LeaseCount(camera)}");
        Expect(unfocused && returned && v.Leases.LeaseCount(camera) == leasesBefore + 2, "the card did not come back with its picture after the focus ended");
    }

    // The computer's own voice, recognizer, and microphone, which depend on what Windows has installed:
    // reported, and failing only when something claims to work and does not.
    private async Task RealCallEngines()
    {
        // parity: call-system-voice
        (float[] samples, uint rate) = await new WindowsSpeech().RenderAsync("Hello, one moment please.");
        Say($"calls: system voice rendered {samples.Length} samples at {rate} Hz ({(rate == 0 ? 0 : samples.Length / (double)rate):0.0} s)");
        Expect(rate >= 8000 && samples.Length > rate / 2, "the system voice rendered nothing usable");

        if (samples.Length > 0)
        {
            // parity: call-desktop-recognizer
            using var sapi = new SapiTranscriber();
            var statuses = new List<string>();
            var texts = new List<string>();
            var guesses = new List<string>();
            sapi.OnStatus = s => statuses.Add(s.Phase + (s.Why is null ? "" : $" ({s.Why})"));
            sapi.OnFinal = t => texts.Add(t);
            sapi.OnPartial = t => guesses.Add(t);
            sapi.Start(TranscriptSource.Visitor);
            // 16 kHz by linear interpolation (the voice's own rate is higher), fed in 100 ms blocks.
            float[] pcm = new float[(int)(samples.Length * 16_000L / rate)];
            for (int i = 0; i < pcm.Length; i++)
            {
                double at = i * (double)rate / 16_000;
                int j = (int)at;
                double f = at - j;
                pcm[i] = (float)(samples[j] * (1 - f) + samples[Math.Min(j + 1, samples.Length - 1)] * f);
            }
            for (int at = 0; at < pcm.Length; at += 1600)
            {
                sapi.Feed(pcm.AsSpan(at, Math.Min(1600, pcm.Length - at)));
            }
            sapi.Feed(new float[16_000]);
            await Until(() => texts.Count > 0 || statuses.Any(s => s.StartsWith("Unavailable", StringComparison.Ordinal)), 10);
            sapi.Finish();
            await Task.Delay(1500);
            Say($"calls: desktop recognizer: {string.Join(", ", statuses)}; heard \"{string.Join(" ", texts)}\", guesses \"{string.Join(" | ", guesses.TakeLast(3))}\"; {sapi.Counts}");
            Expect(statuses.Any(x => x.StartsWith("Unavailable", StringComparison.Ordinal)) ||
                string.Join(" ", texts).Contains("moment", StringComparison.OrdinalIgnoreCase),
                "the desktop recognizer is available but did not hear the system voice's sentence");
        }

        // parity: call-microphone
        if (options.MicrophoneTest)
        {
            var mic = new WasapiMicrophone(() => app.Audio?.DeviceId);
            int blocks = 0, samplesSeen = 0;
            float peak = 0;
            string? problem = mic.Start(block =>
            {
                Interlocked.Increment(ref blocks);
                Interlocked.Add(ref samplesSeen, block.Length);
                foreach (float x in block)
                {
                    peak = Math.Max(peak, Math.Abs(x));
                }
            });
            if (problem is null)
            {
                await Until(() => Volatile.Read(ref blocks) >= 5, 3);
            }
            mic.Stop();
            Say($"calls: microphone {(problem is null ? $"opened: {blocks} blocks, {samplesSeen} samples at 16 kHz, peak {peak:0.000}" : $"not available: {problem}")}");
            // What Windows reports about its echo canceller for this microphone and the output that plays.
            Say($"calls: microphone echo cancellation: {(mic.Echo is { } echo ? echo.Active ? "on" : $"off ({echo.Why})" : "not reported")}, playback device {(app.Audio?.DeviceId is null ? "none" : "known")}");
            Expect(problem is not null || mic.Echo is not null, "the microphone opened but did not report on echo cancellation");
            Expect(problem is not null || blocks > 10, "the microphone opened but delivered no audio");
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

    // 11c. The header, the trimmed tile menu, and the Settings window (docs/UX.md "Windows header", "Settings window").
    private async Task HeaderAndSettings(Viewer v)
    {
        ViewerWindow w = app.Windows[0];
        StageView stage = w.Stage;
        HeaderBar header = w.Header;
        IViewerClock realClock = w.RevealClock;

        // parity: header-present
        // The header is the title bar: it draws into it, keeps room for the caption buttons, hands the system its
        // controls, and shows the quick actions, Settings, and the reserved recording spot.
        var buttons = header.Buttons();
        Say($"header: {string.Join(" / ", buttons.Select(b => b.Name))}; layout \"{header.LayoutLabel}\"; recording spot reserved {header.HasRecordingSpot}, shown {header.RecordingSpotVisible}");
        Expect(w.ExtendsIntoTitleBar, "the window does not extend its content into the title bar");
        HeaderAction[] expected = [HeaderAction.FindDevices, HeaderAction.EditLayout, HeaderAction.FillTiles, HeaderAction.FullScreen, HeaderAction.Settings];
        Expect(expected.All(a => buttons.Any(b => b.Action == a && b.Enabled && b.Visible && b.Name.Length > 0)), "the header lacks a quick action or Settings");
        Expect(header.LayoutLabel == w.ContentTitle && w.DockedHeaderShown, "the header does not show the current layout");
        Expect(header.HasRecordingSpot && !header.RecordingSpotVisible, "the recording spot is not reserved, or shows before anything records");
        Expect(Math.Abs(header.Root.ActualHeight - header.Root.Height) < 1 && header.Root.ActualHeight >= 32, "the header does not have the title bar's height");
        bool regions = await Until(() => w.PassthroughRegions >= buttons.Count, 5);
        Say($"header: {w.PassthroughRegions} interactive regions handed to the system");
        Expect(regions, "the header's controls were not given to the system as interactive regions");

        // parity: header-actions
        // The actions do what the menu items did.
        header.Press(HeaderAction.EditLayout);
        bool editing = stage.State.Editing;
        string editName = header.Buttons().First(b => b.Action == HeaderAction.EditLayout).Name;
        header.Press(HeaderAction.EditLayout);
        Expect(editing && editName == "Done editing" && !stage.State.Editing, "Edit layout in the header did not toggle editing");
        header.Press(HeaderAction.FillTiles);
        bool filled = stage.State.Fill;
        header.Press(HeaderAction.FillTiles);
        Expect(filled && !stage.State.Fill, "Fill tiles in the header did not toggle");
        stage.FullScreenOverride = true;
        header.Update();
        string fullName = header.Buttons().First(b => b.Action == HeaderAction.FullScreen).Name;
        stage.FullScreenOverride = null;
        header.Update();
        Expect(fullName == "Leave full screen" && header.Buttons().First(b => b.Action == HeaderAction.FullScreen).Name == "Full screen", "the Full screen button does not say what it does");
        List<string> overflow = Texts(header.BuildOverflow().Items);
        Say($"header: overflow {string.Join(" / ", overflow)}");
        string[] rest = ["New window", "Recordings", w.SidebarDocked ? "Hide sidebar" : "Show sidebar", "Indicator corner", "Show diagnostics", "Keyboard shortcuts"];
        Expect(rest.All(overflow.Contains) && overflow.Any(t => t == "Check for updates…" || t.StartsWith("Update to ", StringComparison.Ordinal) || t.StartsWith("Switch to ", StringComparison.Ordinal)),
            "the overflow menu lacks an app-wide item that left the tile menu");
        // The same arrangement saved as a layout, so no camera loses its lease while the window switches.
        WindowContent before = w.State.Content;
        string? layoutId = v.CreateLayout("Header test", w.State.Tree);
        List<string> picker = Texts(header.BuildLayoutMenu().Items);
        Say($"header: layout picker {string.Join(" / ", picker)}");
        Expect(layoutId is not null && picker.Contains("All Cameras") && picker.Contains("Header test") && picker.Contains("New layout…")
            && picker.Contains("New layout from template") && picker.Contains("Save as layout…"), "the layout picker lacks a layout or a layout action");
        // parity: header-layout-picker
        // Choosing a layout in the picker shows it in the window, and the header follows.
        if (layoutId is not null)
        {
            w.Show(new WindowContent.Layout(layoutId));
            Expect(header.LayoutLabel == "Header test" && Texts(header.BuildLayoutMenu().Items).Contains("Header test")
                && !Texts(header.BuildLayoutMenu().Items).Contains("Save as layout…"), "the header did not follow the layout shown");
            w.Show(before);
            v.DeleteLayout(layoutId);
        }
        Expect(header.LayoutLabel == w.ContentTitle, "the header did not return to the arrangement shown");

        // parity: header-fullscreen-reveal
        // In full screen the header is hidden and slides in from the top edge on the sidebar's timing (manual clock).
        var clock = new ManualViewerClock(5000);
        w.RevealClock = clock;
        w.SimulateHeaderFullScreen(true);
        Expect(!w.HeaderVisible && !w.DockedHeaderShown && !w.HeaderOverlayInteractive, "the header is not hidden in full screen");
        w.HeaderPointerAt(inEdge: true, inHeader: false);
        clock.Advance(0.1);
        w.TickHeader();
        Expect(!w.HeaderVisible, "a pass over the top edge revealed the header");
        clock.Advance(0.06);
        w.TickHeader();
        Expect(w.HeaderVisible && w.HeaderOverlayShown && w.HeaderOverlayInteractive, "resting at the top edge did not reveal the header");
        w.HeaderPointerAt(inEdge: false, inHeader: true);
        clock.Advance(5);
        w.TickHeader();
        Expect(w.HeaderVisible, "the header slid away while the pointer was on it");
        w.HeaderPointerAt(inEdge: false, inHeader: false);
        clock.Advance(0.49);
        w.TickHeader();
        bool stillThere = w.HeaderVisible;
        clock.Advance(0.02);
        w.TickHeader();
        Say($"header: full screen reveal: hidden {!w.HeaderOverlayShown}, held until half a second after the pointer left {stillThere}");
        Expect(stillThere && !w.HeaderVisible && !w.HeaderOverlayInteractive, "the header did not slide away half a second after the pointer left");
        w.SimulateHeaderFullScreen(null);
        w.RevealClock = realClock;
        Expect(w.HeaderVisible && w.DockedHeaderShown, "leaving full screen did not dock the header again");

        // parity: tile-menu-no-app-wide
        // The right-click menu holds the tile's own actions and none of the app-wide ones.
        StageTile tile = Tiles[0];
        Microsoft.UI.Xaml.Controls.MenuFlyout? tileMenu = stage.BuildTileMenu(tile);
        List<string> tileItems = tileMenu is null ? [] : Texts(tileMenu.Items);
        Say($"tile menu: {string.Join(" / ", tileItems)}");
        string[] own = ["Focus", "Listen", "Start Conversation", "Allow talk-back", "Quality"];
        Expect(own.All(tileItems.Contains), "the tile menu lost one of the tile's own actions");
        Expect(!tileItems.Any(HeaderActions.IsAppWideTitle), $"the tile menu still holds an app-wide item: {string.Join(", ", tileItems.Where(HeaderActions.IsAppWideTitle))}");
        stage.SetEditing(true);
        List<string> editItems = Texts(stage.BuildTileMenu(tile)!.Items);
        stage.SetEditing(false);
        Expect(editItems.Contains("Split Side by Side") && editItems.Contains("Show Camera") && !editItems.Any(HeaderActions.IsAppWideTitle),
            "the tile menu while editing lacks a layout action or holds an app-wide item");

        await SettingsWindowSteps(v, w);
    }

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
        await Task.Delay(500);
        bool searching = await Until(() => v.Discovery.IsSearching, 5);
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
        await Task.Delay(300);
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

    // 10. The tile features.
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

    private async Task TileFeatures(Viewer v, StageView stage)
    {
        await HeaderModel(v, stage);
        await SavePictures(v, stage);

        // parity: corner-indicators
        // Camera AI detections from the mock reach the corner indicators.
        bool shown = await Until(() => Tiles.Any(t => stage.ChromeFor(t) is { Indicators.Count: > 0 }), 15);
        StageTile? withSymbols = Tiles.FirstOrDefault(t => stage.ChromeFor(t) is { Indicators.Count: > 0 });
        Say($"corner indicators: {(shown ? string.Join(", ", stage.ChromeFor(withSymbols!)!.Indicators.Select(i => i.Description)) : "none")}, " +
            $"corner {v.Settings.IndicatorCorner}");
        Expect(shown, "no tile showed a corner indicator from the mock's detections");
        Expect(v.Settings.IndicatorCorner == ChromeCorner.TopRight, "the indicator corner is not top right by default");
        if (withSymbols is not null)
        {
            // parity: corner-setting
            v.Settings.IndicatorCorner = ChromeCorner.BottomLeft;
            stage.SyncOverlays();
            Expect(stage.ChromeFor(withSymbols)!.Corner == ChromeCorner.BottomLeft, "the indicator corner setting did not move the symbols");
            v.Settings.IndicatorCorner = ChromeCorner.TopRight;
        }
        // parity: indicator-strip-render
        await RenderStrip(stage);

        // Alerts, auto-hide, and the hold, on a clock that moves only when told to.
        var clock = new ManualViewerClock(1000);
        v.UseClock(clock);
        StageTile lens0 = Tiles.First(t => t.Channel == 1 && t.Lens == 0);
        DeviceEntry? device = lens0.Device;
        // parity: second-lens
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

        // parity: alert-raise
        v.Apply(Detect(lens0, "smoke_alarm", "local_audio", true));
        stage.Tick();
        bool both = c0.AlertShown && c1.AlertShown && !cOther.AlertShown;
        Say($"alert raised: lens 0 {c0.AlertShown}, lens 1 {c1.AlertShown}, other camera {cOther.AlertShown}; " +
            $"border {(c0.AlertBlinking ? "blinking" : "steady")} (animations {(v.AnimationsEnabled() ? "on" : "off")}); " +
            $"\"{c0.AlertText.Replace('\n', '|')}\"");
        Expect(both, "a sound alarm did not raise the alert on both lens tiles of its camera only");
        // parity: alert-reduce-motion
        Expect(c0.AlertBlinking == v.AnimationsEnabled(), "the alert border does not follow the animation setting");
        Expect(c0.AlertText.Contains("Smoke alarm (sound)", StringComparison.Ordinal) && c0.AlertText.Contains("Detected 1 time", StringComparison.Ordinal) &&
            c0.AlertText.Contains("Sounding now", StringComparison.Ordinal), "the alert header lacks the reason, count, or state");
        Expect(c0.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm) && c1.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm),
            "the sound alarm is not an indicator on both lenses");
        Expect(!cOther.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm), "the sound alarm showed on another camera");

        // parity: fullscreen-chrome-autohide
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

        // parity: alert-survives-stop
        // parity: indicator-hold
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

        // parity: alert-dismiss-enter
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
        // parity: quality-menu-choices
        Expect(v.OffersBalanced(q), "Balanced is not offered for a single-lens channel that lists extern");
        Expect(!v.OffersBalanced(lens0), "Balanced is offered for a two-lens camera");
        Expect(QualityMenu.Choices(true).SequenceEqual(["Auto", "Clear", "Fluent", "Balanced"]) && QualityMenu.Choices(false).SequenceEqual(["Auto", "Clear", "Fluent"]),
            "the Quality menu lists the wrong choices");
        // parity: quality-clear-auto
        v.SetQuality(q, TileQuality.Clear);
        bool main = await Until(() => q.ShownTier == "main" && q.PictureWidth > 0, 10);
        Say($"quality Clear: showing {q.ShownTier} {q.PictureWidth}x{q.PictureHeight}, header \"{StageView.TitleOf(q)}\"");
        Expect(main, "Clear did not switch the tile to the main stream");
        Expect(StageView.TitleOf(q).Contains("Clear", StringComparison.Ordinal), "the header does not name the quality");
        // parity: quality-balanced
        v.SetQuality(q, TileQuality.Balanced);
        bool mid = await Until(() => q.ShownTier == "extern" && q.PictureWidth > 0, 10);
        Say($"quality Balanced: showing {q.ShownTier}");
        Expect(mid, "Balanced did not switch the tile to the extern stream");
        v.SetQuality(q, TileQuality.Auto);
        bool sub = await Until(() => q.ShownTier == "sub", 10);
        Say($"quality Auto: showing {q.ShownTier}");
        Expect(sub, "Auto did not return the tile to the sub stream");
        // parity: zoom-auto-quality
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

        // parity: second-lens-refused
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
        // parity: fisheye-detected
        bool known = await Until(() => v.IsFisheye(f.Camera), 10);
        Expect(known, "the mock's fisheye channel was not reported as a fisheye camera");
        Expect(!v.IsFisheye(Tiles.First(t => t.Channel == 0).Camera), "an ordinary channel was reported as a fisheye camera");
        if (!known)
        {
            return;
        }
        stage.SetFisheyeMode(f, FisheyeViewMode.Panorama);
        // parity: fisheye-circle
        bool found = await Until(() => v.Circles.Circle(f.Camera) is not null, 10);
        FisheyeCircle circle = v.Circles.Circle(f.Camera) ?? FisheyeCircle.Inscribed(384, 384);
        Say($"fisheye circle: ({circle.Cx:F4}, {circle.Cy:F4}) r {circle.R:F4} (synthetic: 0.49, 0.505, 0.46), fell back {v.Circles.FellBack(f.Camera)}");
        Expect(found && !v.Circles.FellBack(f.Camera), "the fisheye circle was not found");
        Expect(Math.Abs(circle.Cx - 0.49) < 0.01 && Math.Abs(circle.Cy - 0.505) < 0.01 && Math.Abs(circle.R - 0.46) < 0.01,
            "the fisheye circle is more than 1% off the synthetic one");

        // parity: fisheye-view-pixels
        foreach (FisheyeViewMode mode in new[] { FisheyeViewMode.Panorama, FisheyeViewMode.DoublePanorama, FisheyeViewMode.Quad, FisheyeViewMode.Ptz })
        {
            stage.SetFisheyeMode(f, mode);
            long before = r.DewarpDraws;
            bool drawn = await Until(() => r.DewarpDraws > before + 3, 5);
            Expect(drawn, $"fisheye {mode.Title()}: the dewarp shader did not draw");
            Expect(StageView.TitleOf(f).Contains(mode.Title(), StringComparison.Ordinal), $"fisheye {mode.Title()}: the header does not name the view");
            // parity: fisheye-view-saved
            string? member = stage.State.Tree.Member(f.Path, FisheyeTileSetting.Member);
            Expect(FisheyeTileSetting.FromJson(member).Mode == mode, $"fisheye {mode.Title()}: the view was not kept in the layout leaf ({member})");
            await CheckColors(mode.Title(), f, r, circle);
        }

        await FisheyePictures(v, stage, r, f, circle);
        stage.SetFisheyeMode(f, FisheyeViewMode.Ptz);

        // parity: fisheye-ptz-zoom
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
        // parity: fisheye-reset-key
        stage.Select(f.Path);
        stage.HandleKey(Windows.System.VirtualKey.Number0, false, false);
        Expect(f.Fisheye?.Aim == FisheyeAim.Default(FisheyeMount.Ceiling), "0 did not reset the virtual camera");

        // parity: fisheye-mount-settings
        // Settings > fisheye mount (the picker, not the menu), then a synthetic pinch on the Virtual PTZ view.
        var mountHooks = new SettingsPagesHooks();
        SettingsWindow mountWindow = SettingsWindow.Open(v, development: true, SettingsSection.Cameras, hooks: mountHooks, background: true);
        await Until(() => mountWindow.IsLoaded, 5);
        var pickers = mountHooks.Mounts();
        Expect(pickers.Any(m => m.Camera == f.Camera.ChannelKey && m.Mount == FisheyeMount.Ceiling), "Settings does not list the fisheye camera with the default ceiling mount");
        Expect(pickers.All(m => v.IsFisheye(m.Camera)), "Settings lists a camera that is not a fisheye camera");
        mountHooks.ChooseMount(f.Camera.ChannelKey, FisheyeMount.Wall);
        Expect(v.Settings.Mount(f.Camera) == FisheyeMount.Wall, "choosing Wall in Settings did not set the mount");
        Expect(f.Fisheye is { Mount: FisheyeMount.Wall } onWall && onWall.Aim == FisheyeAim.Default(FisheyeMount.Wall), "the tile did not follow the mount chosen in Settings");
        Expect(mountHooks.Mounts().Any(m => m.Camera == f.Camera.ChannelKey && m.Mount == FisheyeMount.Wall), "the picker does not show the saved mount");
        // parity: fisheye-pinch
        var pinchClock = new ManualViewerClock(3000);
        v.UseClock(pinchClock);
        double pinchFrom = f.Fisheye?.Aim.Fov ?? 0;
        stage.PinchFisheye(f, 1.5);
        pinchClock.Advance(2);
        stage.StepFisheyeZoom(pinchClock.Now);
        double pinched = f.Fisheye?.Aim.Fov ?? 0;
        stage.PinchFisheye(f, 2);
        stage.PinchFisheye(f, 2);
        pinchClock.Advance(2);
        stage.StepFisheyeZoom(pinchClock.Now);
        double limit = f.Fisheye?.Aim.Fov ?? 0;
        stage.PinchFisheye(f, double.NaN);
        pinchClock.Advance(2);
        stage.StepFisheyeZoom(pinchClock.Now);
        v.UseClock(new SystemViewerClock());
        FisheyeTileSetting pinchSaved = FisheyeTileSetting.FromJson(stage.State.Tree.Member(f.Path, FisheyeTileSetting.Member));
        Say($"fisheye pinch (wall mount): {pinchFrom:0.#}° → {pinched:0.#}° (x1.5) → {limit:0.#}° (two x2), saved {pinchSaved.Aim?.Fov:0.#}°");
        Expect(Math.Abs(pinched - pinchFrom / 1.5) < 1e-9, "a pinch of 1.5 did not divide the field of view by 1.5");
        Expect(Math.Abs(limit - FisheyeAim.MinFov) < 1e-9 && (f.Fisheye?.Aim.Fov ?? 0) == limit, "pinching in did not stop at the narrowest view, or an unusable scale moved it");
        Expect(pinchSaved.Aim is { } pa && Math.Abs(pa.Fov - FisheyeAim.MinFov) < 1e-9, "the pinched view was not saved with the layout");
        mountHooks.ChooseMount(f.Camera.ChannelKey, FisheyeMount.Ceiling);
        Expect(v.Settings.Mount(f.Camera) == FisheyeMount.Ceiling, "choosing Ceiling in Settings did not restore the default mount");
        mountWindow.Close();
        stage.Select(f.Path);
        stage.HandleKey(Windows.System.VirtualKey.Number0, false, false);
        Expect(f.Fisheye?.Aim == FisheyeAim.Default(FisheyeMount.Ceiling), "0 did not reset the virtual camera after the pinch");

        // parity: fisheye-original
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

        // parity: layout-save
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
            // parity: layout-designer-edits
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
            // parity: layout-quality-saved
            // A tile's quality is saved in its leaf ("quality"), and Auto removes it.
            stage.SetQuality(stage.Tiles[0], TileQuality.Fluent);
            string? member = v.Layout(id)?.Root.Member(stage.Tiles[0].Path, "quality");
            stage.SetQuality(stage.Tiles[0], TileQuality.Auto);
            Expect(member == "\"fluent\"" && v.Layout(id)?.Root.Member(stage.Tiles[0].Path, "quality") is null,
                "a tile's quality was not kept in its layout leaf");
            stage.SetEditing(false);
            // parity: layout-roundtrip
            // The stored JSON reads back to the same layout.
            Expect(SavedLayout.Parse(v.Layout(id)!.ToJson()) == v.Layout(id), "the stored layout does not round-trip");

            bool playing = await Until(() => Tiles.All(t => (t.Video?.Counters.Decoded ?? 0) > 3 && t.PictureWidth > 0), 15);
            Expect(playing, $"tiles did not play after the edits: {string.Join("; ", Tiles.Select(Describe))}");

            // parity: focus-content-change
            // Changing the content while a tile is focused ends the focus only: the renderer keeps presenting.
            StageTile focusTile = Tiles[0];
            stage.FocusOn(focusTile);
            long focusFrames = r.Frames;
            await Until(() => r.Frames - focusFrames >= 5, 5);
            int chains = r.SwapChains;
            long frames = r.Frames;
            w1.Show(new WindowContent.Layout(id));
            await Until(() => r.Frames - frames > 12, 5);
            Say($"content changed while focused: focus {(r.Focus is null ? "ended" : "kept")}, swap chains {chains} -> {r.SwapChains}, frames +{r.Frames - frames}");
            Expect(r.Focus is null && !focusTile.Focused, "changing the content did not end the focus");
            Expect(r.SwapChains == chains && r.Frames - frames > 10, "changing the content while focused disturbed the renderer");
            await CheckFrame("after a content change while focused", Tiles);

            // parity: second-window
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

            // parity: second-window-follows
            // An edit in one window reaches the other (layouts_changed), not during a drag.
            stage.Edit(t => t.SettingRatio(0.3, NodePath.Root));
            bool followed = await Until(() => w2.State.Tree == w1.State.Tree, 5);
            Say($"second window followed the edit: {followed}");
            Expect(followed, "the second window did not follow the stored layout");

            // parity: second-window-close
            // Closing it gives its leases back; the first window keeps playing.
            int before = v.Leases.LeaseCount(sample);
            w2.Window.Close();
            await Until(() => app.Windows.Count == 1, 3);
            Say($"second window closed: windows {app.Windows.Count}, leases of {sample} {before} -> {v.Leases.LeaseCount(sample)}");
            Expect(app.Windows.Count == 1 && v.Leases.LeaseCount(sample) == before - t2.Count(t => t.Camera == sample), "closing a window did not give its leases back");

            // parity: sidebar-reveal
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
            await Until(() => r.Frames - frames2 > 5, 5);
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
            // parity: window-restore
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
