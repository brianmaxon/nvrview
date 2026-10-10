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
// 10. with --credential-test, a throwaway credential is written, read back,
//    and deleted in Credential Manager.
//
// Writes what it measured and PASS or FAIL to the --log file and exits 0 or 1.

using System.Diagnostics;
using System.Text;
using Microsoft.UI.Windowing;
using NvrView.Video;
using Windows.Graphics;

namespace NvrView.App;

internal sealed class SelfTest(App app, AppOptions options)
{
    private readonly List<string> _problems = [];
    private readonly List<string> _lines = [];
    private readonly Stopwatch _clock = Stopwatch.StartNew();

    public void Start()
    {
        _ = RunAsync();
        // A hard limit for the whole run.
        _ = Watchdog();
    }

    private async Task Watchdog()
    {
        await Task.Delay(TimeSpan.FromSeconds(options.SelfTestSeconds + 90));
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

        // 1. Tiles play.
        bool playing = await Until(() => v.Tiles.Length >= options.MockChannels &&
            v.Tiles.All(t => t.Video.Counters.Decoded > 5 && t.PictureWidth > 0 && t.Shown > 2), 30);
        Say($"devices {v.Devices.Count} ({string.Join(", ", v.Devices.Select(d => d.State))}), tiles {v.Tiles.Length}, " +
            $"first picture {Ms(v.Timings.GetValueOrDefault("first_picture"))}");
        if (!playing)
        {
            _problems.Add($"tiles did not all play: {string.Join("; ", v.Tiles.Select(Describe))}");
            return;
        }
        await Task.Delay(TimeSpan.FromSeconds(Math.Max(1, options.SelfTestSeconds * 0.2)));
        long memoryBefore = PrivateBytes();
        await CheckFrame("grid", v.Tiles);
        if (options.MockChannels >= 2)
        {
            StageTile last = v.Tiles[^1];
            Say($"third-party channel: waited {v.WaitedTiles.Contains(last.Id)}, now {StageView.StatusText(last) ?? "playing"}");
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
            int tilesWithSecond = v.Tiles.Length;
            v.ForgetDevice(saved.Key);
            Say($"settings save: second device connected {connected}, {tilesWithSecond} tiles; removed: {v.Devices.All(d => d.Config.Key != saved.Key)}, {v.Tiles.Length} tiles");
            if (!connected || v.Devices.Any(d => d.Config.Key == saved.Key))
            {
                _problems.Add("saving and removing a device did not connect and remove it");
            }

            // The dialog itself opens and closes.
            var dialog = SettingsDialog.Create(app.Stage!.XamlRoot, v, development: true);
            _ = dialog.ShowAsync();
            await Task.Delay(500);
            bool shown = dialog.IsLoaded;
            dialog.Hide();
            Say($"settings dialog shown: {shown}");
            if (!shown)
            {
                _problems.Add("the Settings dialog did not open");
            }
        }

        // 2. Live resize.
        await Resize(r);

        // 3. Spatial selection.
        StageTile first = v.Tiles[0];
        if (v.Tiles.Length >= 2)
        {
            StageTile? right = stage.Neighbor(first, Windows.System.VirtualKey.Right);
            Say($"right of tile 0: {(right is null ? "none" : $"tile {Array.IndexOf(v.Tiles, right)}")}");
            if (right is null)
            {
                _problems.Add("arrow-key selection found no tile to the right of the first");
            }
        }

        // 4. Focus: animation, then the main stream after the aligned switch.
        int tierEventsBefore = v.TierEvents.Count;
        stage.FocusOn(first);
        bool main = await Until(() => first.Video.ShownGeneration == 1 && first.ShownTier == "main", FocusAnimation.Seconds + 8);
        float stageW = (float)stage.ActualWidth, stageH = (float)stage.ActualHeight;
        Say($"focused tile 0: showing generation {first.Video.ShownGeneration} ({first.ShownTier}), picture {first.PictureWidth}x{first.PictureHeight}, " +
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
        v.ToggleListening(first, additive: false);
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
        v.SetListening(first, false);

        // 6. Leave focus.
        tierEventsBefore = v.TierEvents.Count;
        stage.Unfocus();
        bool sub = await Until(() => first.Video.ShownGeneration == 2 && first.ShownTier == "sub" && r.Focus is null, FocusAnimation.Seconds + 8);
        Say($"after focus: showing generation {first.Video.ShownGeneration} ({first.ShownTier})");
        if (!sub)
        {
            _problems.Add("leaving focus did not return to the sub stream");
        }
        CheckTierEvents(v, first.Id, tierEventsBefore, 2);

        // 7. Digital zoom.
        StageTile zoomed = v.Tiles[^1];
        StageView.ZoomAround(zoomed, 0.25, 0.25, 2);
        await CheckFrame("zoomed", [zoomed]);
        Say($"zoom {zoomed.Zoom:0.0} around {zoomed.CenterX:0.00},{zoomed.CenterY:0.00}");
        if (Math.Abs(zoomed.Zoom - 2) > 0.01)
        {
            _problems.Add("zoom was not applied");
        }
        StageView.ResetZoom(zoomed);

        long failuresBefore = v.Tiles.Sum(t => t.Video.Counters.Failures + t.Video.Counters.ConfigureFailures);
        Say($"decode failures before the device loss: {failuresBefore}");
        if (failuresBefore > 0)
        {
            _problems.Add($"{failuresBefore} decode failures");
        }
        long memoryAfter = PrivateBytes();

        // 8. Device loss.
        long[] decodedBefore = [.. v.Tiles.Select(t => t.Video.Counters.Decoded)];
        r.SimulateDeviceLoss();
        bool recovered = await Until(() => r.Recoveries >= 1, 10);
        bool playingAgain = recovered && await Until(() =>
            v.Tiles.Select((t, i) => t.Video.Counters.Decoded > decodedBefore[i] + 10 && t.PictureWidth > 0).All(x => x), 15);
        Say($"device loss: recoveries {r.Recoveries}, device replacements {v.Host.DeviceReplacements}, " +
            $"decoders rebuilt {v.Tiles.Sum(t => t.Video.Rebuilds)}, failures answered {v.Tiles.Sum(t => t.Video.Counters.Failures) - failuresBefore}");
        if (!recovered || !playingAgain)
        {
            _problems.Add($"tiles did not play again after the device loss: {string.Join("; ", v.Tiles.Select(Describe))}");
        }
        else
        {
            await Task.Delay(500);
            await CheckFrame("after device loss", v.Tiles);
        }

        // Detections from the mock (camera AI) reached the tiles as badges.
        Say($"detection events: {v.DetectionEvents}, active now: " +
            string.Join(", ", v.Tiles.SelectMany(t => t.Detections.Select(TileChrome.BadgeText)).Distinct()));
        if (v.DetectionEvents == 0)
        {
            _problems.Add("no detection events arrived");
        }

        // The device goes away: tiles say they reconnect, with a countdown.
        app.StopMock();
        bool reconnecting = await Until(() => v.Tiles.All(t => StageView.StatusText(t) is { } s && s.Contains("onnect", StringComparison.Ordinal)), 15);
        Say($"mock-nvr stopped: {StageView.StatusText(v.Tiles[0]) ?? "playing"}");
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
        foreach (StageTile t in v.Tiles)
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

    private static string Describe(StageTile t)
    {
        TileCounters c = t.Video.Counters;
        return $"tile {t.Id} ch{t.Channel}: decoded {c.Decoded}, shown {t.Shown}, failures {c.Failures}, no picture {c.NoPictures}, " +
            $"generations {c.Generations}, resets {c.Resets}, picture {t.PictureWidth}x{t.PictureHeight}, " +
            $"decoder {t.Video.DecoderKind} ({t.Video.DecoderName}), status {StageView.StatusText(t) ?? "playing"}";
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

    private async Task CheckFrame(string what, IReadOnlyList<StageTile> tiles)
    {
        StageRenderer r = app.Renderer!;
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
            (ulong _, float x, float y, float w, float h) = c.Rects.FirstOrDefault(rect => rect.Tile == t.Id);
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

    private static async Task<bool> Until(Func<bool> condition, double seconds)
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
