// A C# client of the nvrview core through NvrView.Interop: compares the
// bindings with the C header (layout and functions), spawns or connects to
// tools/mock-nvr, opens one tile and plays it with a fake decoder (answering
// generation 0 inline and later generations from a decoder thread), switches
// it to main and back, renders and taps its audio, reads its level, round-
// trips layouts and device keys (also across a second core on the same data
// directory), goes through the recordings calls (minor 5) against the mock's
// recordings, and checks the guarantees of docs/FFI.md along the way.
//
// usage: NvrView.Smoke [--mock-nvr EXE --fixtures DIR | --host H --port N]
//                      [--layout FILE] [--header FILE] [--lib PATH]
// The password comes from NVRVIEW_TEST_PASSWORD (default example-password);
// --port may also come from NVRVIEW_MOCK_PORT, --lib from NVRVIEW_LIB.

using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Threading.Channels;
using NvrView.Interop;

namespace NvrView.Smoke;

internal static class Program
{
    private static int Main(string[] args)
    {
        Options options;
        try
        {
            options = Options.Parse(args);
        }
        catch (ArgumentException e)
        {
            Console.Error.WriteLine($"csharp-smoke: {e.Message}");
            Console.Error.WriteLine(Options.Usage);
            return 2;
        }
        if (options.Lib is { } lib)
        {
            NativeLibrary.SetDllImportResolver(typeof(NvNative).Assembly, (name, _, _) =>
                name == NvNative.Library ? NativeLibrary.Load(lib) : 0);
        }
        // CI must never hang on a stuck core.
        var watchdog = new Timer(_ =>
        {
            Console.Error.WriteLine("csharp-smoke: timed out after 120 s");
            Environment.Exit(1);
        }, null, TimeSpan.FromSeconds(120), Timeout.InfiniteTimeSpan);
        try
        {
            return new Smoke(options).Run();
        }
        finally
        {
            watchdog.Dispose();
        }
    }
}

internal sealed class Options
{
    public const string Usage =
        "usage: NvrView.Smoke [--mock-nvr EXE --fixtures DIR | --host H --port N]\n" +
        "                     [--layout FILE] [--header FILE] [--lib PATH]";

    public string Host { get; private set; } = "127.0.0.1";
    public ushort Port { get; private set; }
    public string? MockNvr { get; private set; }
    public string? Fixtures { get; private set; }
    public string? Layout { get; private set; }
    public string? Header { get; private set; }
    public string? Lib { get; private set; } = Environment.GetEnvironmentVariable("NVRVIEW_LIB");

    public static Options Parse(string[] args)
    {
        var o = new Options();
        if (ushort.TryParse(Environment.GetEnvironmentVariable("NVRVIEW_MOCK_PORT"), out ushort envPort))
        {
            o.Port = envPort;
        }
        for (int i = 0; i < args.Length; i++)
        {
            string Value() => i + 1 < args.Length ? args[++i] : throw new ArgumentException($"{args[i]} needs a value");
            switch (args[i])
            {
                case "--host": o.Host = Value(); break;
                case "--port":
                    o.Port = ushort.TryParse(Value(), out ushort p) && p != 0
                        ? p : throw new ArgumentException("bad --port");
                    break;
                case "--mock-nvr": o.MockNvr = Path.GetFullPath(Value()); break;
                case "--fixtures": o.Fixtures = Path.GetFullPath(Value()); break;
                case "--layout": o.Layout = Value(); break;
                case "--header": o.Header = Value(); break;
                case "--lib": o.Lib = Value(); break;
                default: throw new ArgumentException($"unknown argument {args[i]}");
            }
        }
        // A relative library path would go through the platform's search
        // rules instead of the working directory.
        if (o.Lib is not null)
        {
            o.Lib = Path.GetFullPath(o.Lib);
        }
        if (o.MockNvr is null && o.Port == 0)
        {
            throw new ArgumentException("give --mock-nvr EXE or a port (--port or NVRVIEW_MOCK_PORT)");
        }
        if (o.MockNvr is not null && o.Fixtures is null)
        {
            throw new ArgumentException("--mock-nvr needs --fixtures DIR");
        }
        return o;
    }
}

/// <summary>What the fake decoder knows about one generation of the tile.</summary>
internal sealed class Generation
{
    public int Configures;
    public int Decodes;
    public int Answered;
    public int Resets;
    public int Flushes;
    public bool Configured;
    public bool AwaitKeyframe;
    public bool Ended; // its final Reset arrived
}

/// <summary>The app side: a fake platform decoder and an event recorder.</summary>
internal sealed class FakeApp : NvCoreCallbacks
{
    private const int MaxGenerations = 8;
    private readonly object _gate = new();
    private readonly Generation[] _gens = Enumerable.Range(0, MaxGenerations).Select(_ => new Generation()).ToArray();
    private readonly List<string> _problems = [];
    private readonly HashSet<ulong> _actionTiles = [];
    private readonly Dictionary<string, int> _eventTypes = [];
    private readonly List<(string Tier, string State, uint Generation)> _tierEvents = [];
    private readonly List<ulong> _layoutRevisions = [];
    private readonly Channel<(ulong Tile, uint Gen, long Pts)> _answers =
        Channel.CreateBounded<(ulong, uint, long)>(new BoundedChannelOptions(256) { SingleReader = true });
    private readonly Thread _decoderThread;
    private NvCore? _core;
    private int _highestGeneration;
    private int _threwOnce;

    public FakeApp()
    {
        _decoderThread = new Thread(DecoderLoop) { IsBackground = true, Name = "fake decoder" };
        _decoderThread.Start();
    }

    public ulong Device;
    /// <summary>The live tile; set after nv_tile_open returns.</summary>
    public ulong MainTile;
    /// <summary>Set before a playback opens: tiles other than the live one are its tiles.</summary>
    public volatile bool PlaybackPhase;
    public long PlaybackDecodes;
    public long PlaybackTilesClosed;
    public volatile bool CloseReturned;
    public volatile bool TileClosedSeen;
    public volatile bool Connected;
    public volatile bool DeviceClosedSeen;
    public string? TileClosedReason;
    public string? KeyFromEvent;
    private readonly List<(ulong Search, bool Stopped)> _discoveryDone = [];

    /// <summary>The discovery_done events seen so far (search ID, stopped).</summary>
    public (ulong Search, bool Stopped)[] DiscoveryDone()
    {
        lock (_gate)
        {
            return [.. _discoveryDone];
        }
    }
    public string? LastAudioStats;
    public long Callbacks;
    public long Actions;
    public long Events;
    public long TapBlocks;
    public int AnswerErrors;
    public Exception? ReportedException;

    public void Problem(string what)
    {
        Console.Error.WriteLine($"csharp-smoke: {what}");
        lock (_problems)
        {
            _problems.Add(what);
        }
    }

    public int ProblemCount
    {
        get
        {
            lock (_problems)
            {
                return _problems.Count;
            }
        }
    }

    public Generation Gen(uint g)
    {
        lock (_gate)
        {
            return _gens[Math.Min(g, MaxGenerations - 1)];
        }
    }

    public T Locked<T>(Func<T> read)
    {
        lock (_gate)
        {
            return read();
        }
    }

    public IReadOnlyList<(string Tier, string State, uint Generation)> TierEvents => Locked(() => _tierEvents.ToList());

    public IReadOnlyList<ulong> LayoutRevisions => Locked(() => _layoutRevisions.ToList());

    public IReadOnlyCollection<ulong> ActionTiles => Locked(() => _actionTiles.ToList());

    public int EventCount(string type) => Locked(() => _eventTypes.GetValueOrDefault(type));

    public void Attach(NvCore core) => _core = core;

    public override void OnAction(NvCore core, ulong tile, in NvAction action)
    {
        Interlocked.Increment(ref Callbacks);
        Interlocked.Increment(ref Actions);
        if (PlaybackPhase && tile != Volatile.Read(ref MainTile))
        {
            // A playback tile (the only other tiles): recorded video
            // arrives like live video and is answered the same way.
            if (action.Kind == NvActionKind.Decode && core.Decoded(tile, action.Generation, action.Decode.PtsUs) == NvStatus.Ok)
            {
                Interlocked.Increment(ref PlaybackDecodes);
            }
            return;
        }
        if (CloseReturned)
        {
            Problem("on_action after nv_tile_close returned");
        }
        if (TileClosedSeen)
        {
            Problem("on_action after tile_closed");
        }
        uint g = action.Generation;
        bool answerInline = false;
        lock (_gate)
        {
            _actionTiles.Add(tile);
            if (g >= MaxGenerations)
            {
                Problem($"unexpected generation {g}");
                return;
            }
            Generation gen = _gens[g];
            if (gen.Ended)
            {
                Problem($"action {action.Kind} for generation {g} after its final Reset");
            }
            if ((int)g < _highestGeneration && gen.Resets == 0 && action.Kind == NvActionKind.Configure)
            {
                Problem($"Configure of old generation {g}");
            }
            _highestGeneration = Math.Max(_highestGeneration, (int)g);
            switch (action.Kind)
            {
                case NvActionKind.Configure:
                    gen.Configures++;
                    gen.Configured = true;
                    gen.AwaitKeyframe = true;
                    CheckConfigure(in action.Configure);
                    break;
                case NvActionKind.Decode:
                    gen.Decodes++;
                    if (!gen.Configured)
                    {
                        Problem($"Decode before Configure in generation {g}");
                    }
                    if (gen.AwaitKeyframe && !action.Decode.Keyframe)
                    {
                        Problem($"first Decode after Configure is not a keyframe (generation {g})");
                    }
                    gen.AwaitKeyframe = false;
                    CheckAccessUnit(in action.Decode);
                    answerInline = g == 0;
                    break;
                case NvActionKind.Flush:
                    gen.Flushes++;
                    break;
                case NvActionKind.Reset:
                    gen.Resets++;
                    gen.Configured = false;
                    // A Reset of a generation older than the newest is its
                    // final one: nothing of it follows (docs/FFI.md).
                    if ((int)g < _highestGeneration)
                    {
                        gen.Ended = true;
                    }
                    break;
                case NvActionKind.Clear:
                case NvActionKind.Status:
                    break;
                default:
                    // Unknown kinds (a newer core) need no answer.
                    break;
            }
        }
        if (action.Kind != NvActionKind.Decode)
        {
            return;
        }
        // Generation 0 answers from inside the callback (allowed); later
        // generations answer from a decoder thread, as a platform decoder
        // does. The queue is bounded; when it is full, answer inline.
        if (answerInline || !_answers.Writer.TryWrite((tile, g, action.Decode.PtsUs)))
        {
            Answer(core, tile, g, action.Decode.PtsUs);
        }
    }

    private void Answer(NvCore core, ulong tile, uint g, long pts)
    {
        NvStatus s = core.Decoded(tile, g, pts);
        // Unknown tile (closed meanwhile) or closed core are fine; the
        // answer is still given exactly once.
        if (s is not (NvStatus.Ok or NvStatus.UnknownId or NvStatus.Closed))
        {
            Interlocked.Increment(ref AnswerErrors);
            Problem($"nv_tile_decoded returned {s}");
        }
        lock (_gate)
        {
            _gens[g].Answered++;
        }
    }

    private void DecoderLoop()
    {
        ChannelReader<(ulong Tile, uint Gen, long Pts)> reader = _answers.Reader;
        while (reader.WaitToReadAsync().AsTask().GetAwaiter().GetResult())
        {
            while (reader.TryRead(out var item))
            {
                Answer(_core!, item.Tile, item.Gen, item.Pts);
            }
        }
    }

    /// <summary>Stops the decoder thread after it has answered everything queued.</summary>
    public void DrainDecoder()
    {
        _answers.Writer.TryComplete();
        if (!_decoderThread.Join(TimeSpan.FromSeconds(5)))
        {
            Problem("the decoder thread did not finish");
        }
    }

    private unsafe void CheckConfigure(in NvConfigure c)
    {
        ReadOnlySpan<NvBytes> sets = c.ParameterSetSpan;
        if (sets.Length < 2 || c.CodedSize.Width == 0 || c.CodedSize.Height == 0 ||
            c.VisibleRect.Width == 0 || c.VisibleRect.Height == 0 ||
            c.VisibleRect.X + c.VisibleRect.Width > c.CodedSize.Width ||
            c.VisibleRect.Y + c.VisibleRect.Height > c.CodedSize.Height)
        {
            Problem("incomplete Configure");
        }
        foreach (NvBytes b in sets)
        {
            if (b.Data == null || b.Len == 0)
            {
                Problem("empty parameter set");
            }
        }
        if (c.Codec is not (NvCodec.H264 or NvCodec.H265) ||
            c.Color.Matrix is not (NvColorMatrix.Bt601 or NvColorMatrix.Bt709 or NvColorMatrix.Bt2020))
        {
            Problem($"Configure with codec {c.Codec}, matrix {c.Color.Matrix}");
        }
        NvColor color = c.Color;
        for (int i = 0; i < 12; i++)
        {
            if (!float.IsFinite(color.YcbcrToRgb[i]))
            {
                Problem("color matrix is not finite");
            }
        }
        // Row 0 of a YCbCr-to-RGB matrix weighs Y by about 1 (more for
        // limited range): a wrong offset would read something else here.
        if (color.YcbcrToRgb[0] < 0.9f || color.YcbcrToRgb[0] > 1.3f)
        {
            Problem($"color matrix Y weight {color.YcbcrToRgb[0]}");
        }
    }

    private void CheckAccessUnit(in NvDecode d)
    {
        ReadOnlySpan<byte> au = d.AccessUnit.AsSpan();
        // A platform decoder copies the access unit; so does this one.
        byte[] copy = au.ToArray();
        if (copy.Length < 4 || copy[0] != 0 || copy[1] != 0)
        {
            Problem("access unit is missing or not Annex-B");
        }
    }

    public override void OnEvent(NvCore core, ReadOnlySpan<byte> json)
    {
        Interlocked.Increment(ref Callbacks);
        Interlocked.Increment(ref Events);
        if (json.Length < 10 || json[0] != (byte)'{' || json[^1] != (byte)'}')
        {
            Problem("event is not a JSON object");
            return;
        }
        using JsonDocument doc = JsonDocument.Parse(json.ToArray());
        JsonElement e = doc.RootElement;
        string type = e.GetProperty("type").GetString() ?? "";
        lock (_gate)
        {
            _eventTypes[type] = _eventTypes.GetValueOrDefault(type) + 1;
        }
        switch (type)
        {
            case "device_state":
                if (e.GetProperty("device").GetUInt64() != Device)
                {
                    Problem("device_state for a device whose ID nv_device_add had not returned");
                }
                string? state = e.GetProperty("state").GetString();
                if (state == "connected")
                {
                    Connected = true;
                }
                else if (state == "closed")
                {
                    DeviceClosedSeen = true;
                }
                else if (state == "login_failed")
                {
                    Problem("login failed");
                }
                break;
            case "device_info":
                if (e.TryGetProperty("key", out JsonElement key))
                {
                    Volatile.Write(ref KeyFromEvent, key.GetString());
                }
                if (!e.TryGetProperty("recordings", out JsonElement rec) || !rec.GetProperty("search").GetBoolean() ||
                    !rec.GetProperty("download").GetBoolean() || !rec.GetProperty("thumbnails").GetBoolean())
                {
                    Problem("device_info without the recordings flags");
                }
                break;
            case "tile_tier":
                lock (_gate)
                {
                    _tierEvents.Add((e.GetProperty("tier").GetString()!, e.GetProperty("state").GetString()!,
                        e.GetProperty("generation").GetUInt32()));
                }
                break;
            case "discovery_done":
                lock (_gate)
                {
                    _discoveryDone.Add((e.GetProperty("search").GetUInt64(), e.GetProperty("stopped").GetBoolean()));
                }
                break;
            case "layouts_changed":
                lock (_gate)
                {
                    _layoutRevisions.Add(e.GetProperty("revision").GetUInt64());
                }
                break;
            case "tile_closed" when PlaybackPhase && e.GetProperty("tile").GetUInt64() != Volatile.Read(ref MainTile):
                Interlocked.Increment(ref PlaybackTilesClosed);
                break;
            case "tile_closed":
                if (TileClosedSeen)
                {
                    Problem("a second tile_closed");
                }
                Volatile.Write(ref TileClosedReason, e.GetProperty("reason").GetString());
                TileClosedSeen = true;
                break;
            case "audio_stats":
                LastAudioStats = Encoding.UTF8.GetString(json);
                break;
            case "tile_stats":
                // Checks that an exception in a callback is caught at the
                // boundary (and reported) instead of crossing into the core.
                if (Interlocked.Exchange(ref _threwOnce, 1) == 0)
                {
                    throw new InvalidOperationException("deliberate exception from on_event");
                }
                break;
        }
    }

    public override void OnAudioTap(NvCore core, ulong tile, in NvAudioBlock block)
    {
        Interlocked.Increment(ref Callbacks);
        Interlocked.Increment(ref TapBlocks);
        ReadOnlySpan<float> s = block.SampleSpan;
        if (block.SampleRate != 16000 || s.IsEmpty)
        {
            Problem("bad tap block");
            return;
        }
        foreach (float v in s)
        {
            if (!float.IsFinite(v) || v > 1f || v < -1f)
            {
                Problem("tap sample out of range");
                break;
            }
        }
    }

    public override void OnCallbackException(Exception exception) =>
        Interlocked.CompareExchange(ref ReportedException, exception, null);
}

internal sealed class Smoke(Options options)
{
    private readonly FakeApp _app = new();
    private readonly string _password = Environment.GetEnvironmentVariable("NVRVIEW_TEST_PASSWORD") ?? "example-password";
    private volatile bool _stopRender;
    private Thread? _renderer;
    private long _renders;
    private long _loudRenders;

    private void Problem(string what) => _app.Problem(what);

    private static bool WaitUntil(Func<bool> condition, int timeoutMs)
    {
        var sw = Stopwatch.StartNew();
        while (!condition())
        {
            if (sw.ElapsedMilliseconds > timeoutMs)
            {
                return false;
            }
            Thread.Sleep(10);
        }
        return true;
    }

    private static NvStatus StatusOf(Action call)
    {
        try
        {
            call();
            return NvStatus.Ok;
        }
        catch (NvException e)
        {
            return e.Status;
        }
    }

    public int Run()
    {
        var total = Stopwatch.StartNew();
        string layoutSummary = CheckBindings();
        Process? mock = null;
        string dataDir = Directory.CreateTempSubdirectory("nvrview-csharp-smoke-").FullName;
        try
        {
            string host = options.Host;
            ushort port = options.Port;
            if (options.MockNvr is { } exe)
            {
                (mock, host, port) = StartMock(exe, options.Fixtures!);
            }
            int result;
            string summary;
            try
            {
                result = Play(host, port, dataDir, out summary);
            }
            catch (Exception e)
            {
                Problem($"unexpected exception: {e}");
                return 1;
            }
            Console.WriteLine($"csharp-smoke: {layoutSummary} {summary} seconds={total.Elapsed.TotalSeconds:F1}");
            if (result != 0 || _app.ProblemCount != 0)
            {
                return 1;
            }
            Console.WriteLine("csharp-smoke: ok");
            return 0;
        }
        finally
        {
            if (mock is not null)
            {
                try
                {
                    mock.Kill(entireProcessTree: true);
                    mock.WaitForExit(5000);
                }
                catch (InvalidOperationException)
                {
                    // Already exited.
                }
                mock.Dispose();
            }
            try
            {
                Directory.Delete(dataDir, recursive: true);
            }
            catch (IOException)
            {
                // Best effort; it is a temporary directory.
            }
        }
    }

    private string CheckBindings()
    {
        var parts = new List<string>();
        if (options.Layout is { } layoutFile)
        {
            (List<string> mismatches, int compared) = NvLayoutCheck.CompareLayout(File.ReadAllLines(layoutFile));
            mismatches.ForEach(m => Problem($"layout: {m}"));
            parts.Add($"layout_facts={compared}");
        }
        if (options.Header is { } headerFile)
        {
            (List<string> mismatches, int compared) = NvLayoutCheck.CompareFunctions(File.ReadAllText(headerFile));
            mismatches.ForEach(m => Problem($"functions: {m}"));
            parts.Add($"functions={compared}");
        }
        return string.Join(' ', parts);
    }

    private (Process, string, ushort) StartMock(string exe, string fixtures)
    {
        var psi = new ProcessStartInfo(exe)
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
        };
        foreach (string a in new[]
        {
            "--port", "0", "--channels", "2", "--recordings", "2",
            "--fixture", Path.Combine(fixtures, "h264-basic.h264"),
            "--audio", Path.Combine(fixtures, "aac-tone.aac"),
        })
        {
            psi.ArgumentList.Add(a);
        }
        psi.Environment["MOCK_NVR_PASSWORD"] = _password;
        Process p = Process.Start(psi) ?? throw new InvalidOperationException("cannot start mock-nvr");
        var stderr = new StringBuilder();
        p.ErrorDataReceived += (_, e) =>
        {
            lock (stderr)
            {
                stderr.AppendLine(e.Data);
            }
        };
        p.BeginErrorReadLine();
        Task<string?> line = p.StandardOutput.ReadLineAsync();
        if (!line.Wait(TimeSpan.FromSeconds(15)) || line.Result is not { } text ||
            !text.StartsWith("mock-nvr listening on ", StringComparison.Ordinal))
        {
            p.Kill(entireProcessTree: true);
            lock (stderr)
            {
                throw new InvalidOperationException($"mock-nvr did not start: {stderr}");
            }
        }
        string addr = text["mock-nvr listening on ".Length..].Trim();
        int colon = addr.LastIndexOf(':');
        return (p, addr[..colon], ushort.Parse(addr[(colon + 1)..]));
    }

    private int Play(string host, ushort port, string dataDir, out string summary)
    {
        summary = "";
        var opts = new NvCoreOptions
        {
            LogDir = Path.Combine(dataDir, "logs"),
            LogMaxFiles = 2,
            LogMaxTotalBytes = 4 << 20,
            DataDir = dataDir,
            AudioTap = true,
        };
        NvCore core = NvCore.Create(opts, _app);
        _app.Attach(core);
        try
        {
            return Play(core, host, port, dataDir, out summary);
        }
        finally
        {
            // Every exit path stops rendering, then frees the core (which
            // closes the session); after a normal run both already happened.
            _stopRender = true;
            _renderer?.Join();
            core.Dispose();
        }
    }

    private int Play(NvCore core, string host, ushort port, string dataDir, out string summary)
    {
        summary = "";

        // Device and tile.
        ulong device = core.AddDevice(host, port, "example-user", _password);
        _app.Device = device;
        if (device == 0)
        {
            Problem("device ID 0");
        }
        if (!WaitUntil(() => _app.Connected, 10_000))
        {
            Problem("the device did not connect");
            return 1;
        }
        ulong tile = core.OpenTile(device, 0, NvStreamTier.Sub);
        Volatile.Write(ref _app.MainTile, tile);
        if (tile == 0)
        {
            Problem("tile ID 0");
        }

        // Audio: tapped while silent first (the level is measured as audio
        // is decoded), then audible and rendered.
        core.SetAudioOutput(48000, 2);
        core.SetTileAudioTap(tile, true);
        if (!WaitUntil(() => _app.Gen(0).Answered >= 50, 10_000))
        {
            Problem("fewer than 50 decodes of generation 0 in 10 s");
        }
        float silentPeak = 0;
        WaitUntil(() => (silentPeak = core.TileAudioLevel(tile).Peak) > 0.01f, 3_000);
        if (silentPeak <= 0.01f)
        {
            Problem($"no audio level for a silent tile with audio (peak {silentPeak})");
        }
        core.SetTileAudio(tile, true, 1.0f);
        if (core.RenderAudio(new float[16], 1, NvCore.ClockNowNs()) != NvStatus.InvalidArgument)
        {
            Problem("a channel count other than the output's was accepted");
        }
        _renderer = new Thread(() => RenderLoop(core)) { Name = "audio render", IsBackground = true };
        _renderer.Start();

        // Focus: main as generation 1, then back to sub as generation 2.
        SwitchTier(core, tile, NvStreamTier.Main, "main", 1);
        // Audio restarts with the new stream; wait for it to play.
        WaitUntil(() => core.TileAudioLevel(tile).Peak > 0.01f && Interlocked.Read(ref _loudRenders) > 0, 5_000);
        (float peak, float rms) = core.TileAudioLevel(tile);
        if (peak < 0 || peak > 1 || rms < 0 || rms > peak + 1e-6f || peak <= 0.01f)
        {
            Problem($"nv_tile_audio_level while audible: peak {peak}, rms {rms}; last {_app.LastAudioStats}");
        }
        SwitchTier(core, tile, NvStreamTier.Sub, "sub", 2);

        string key = CheckLayouts(core, device, dataDir);
        CheckRecordings(core, key, dataDir);
        CheckPlayback(core, key);
        CheckDiscovery(core);
        CheckUpdates(core);
        CheckGrid();

        // Close: nothing for the tile after nv_tile_close returns, and one
        // tile_closed.
        core.CloseTile(tile);
        _app.CloseReturned = true;
        long actionsAtClose = Interlocked.Read(ref _app.Actions);
        if (!WaitUntil(() => _app.TileClosedSeen, 2_000) || _app.TileClosedReason != "closed")
        {
            Problem($"tile_closed with reason closed did not arrive (reason {_app.TileClosedReason})");
        }
        Thread.Sleep(200);
        if (Interlocked.Read(ref _app.Actions) != actionsAtClose)
        {
            Problem("on_action after nv_tile_close returned");
        }
        if (StatusOf(() => core.CloseTile(tile)) != NvStatus.UnknownId)
        {
            Problem("second close was not NV_ERR_UNKNOWN_ID");
        }
        if (StatusOf(() => core.OpenTile(device, 0, (NvStreamTier)3)) != NvStatus.InvalidArgument)
        {
            Problem("tier 3 was accepted");
        }
        if (core.Decoded(tile, 2, 0) != NvStatus.UnknownId)
        {
            Problem("nv_tile_decoded for a closed tile was not NV_ERR_UNKNOWN_ID");
        }
        core.RemoveDevice(device);
        if (!WaitUntil(() => _app.DeviceClosedSeen, 5_000))
        {
            Problem("no device_state closed after nv_device_remove");
        }

        // Rendering stops before the core is freed.
        _stopRender = true;
        _renderer.Join();
        _app.DrainDecoder();

        // Shutdown completes, and no callback runs after it.
        var free = Stopwatch.StartNew();
        core.Dispose();
        long freeMs = free.ElapsedMilliseconds;
        long callbacksAfterFree = Interlocked.Read(ref _app.Callbacks);
        Thread.Sleep(200);
        if (Interlocked.Read(ref _app.Callbacks) != callbacksAfterFree)
        {
            Problem("callback after nv_core_free returned");
        }
        if (core.Decoded(tile, 2, 0) != NvStatus.Closed)
        {
            Problem("Decoded after Dispose was not NV_ERR_CLOSED");
        }
        if (StatusOf(() => core.AddDevice(host, port, "example-user", _password)) != NvStatus.Closed)
        {
            Problem("a call after Dispose did not report NV_ERR_CLOSED");
        }
        core.Dispose(); // a second Dispose is a no-op
        if (core.CallbackExceptionCount != 1)
        {
            Problem($"{core.CallbackExceptionCount} callback exceptions (expected the one deliberate)");
        }

        CheckSecondRun(host, port, dataDir, key);
        CheckFinalCounts();

        Generation[] g = [_app.Gen(0), _app.Gen(1), _app.Gen(2)];
        int configures = g.Sum(x => x.Configures);
        int decodes = g.Sum(x => x.Decodes);
        int answered = g.Sum(x => x.Answered);
        summary = $"configures={configures} decodes={decodes} answered={answered} " +
            $"gen_decodes={g[0].Decodes}/{g[1].Decodes}/{g[2].Decodes} events={_app.Events} " +
            $"tile_closed={_app.EventCount("tile_closed")} renders={_renders} audible_renders={_loudRenders} " +
            $"tap_blocks={_app.TapBlocks} peak={peak:F2} free_ms={freeMs}";
        return 0;
    }

    private void SwitchTier(NvCore core, ulong tile, NvStreamTier tier, string name, uint generation)
    {
        Generation old = _app.Gen(generation - 1);
        try
        {
            core.RetryTile(tile + 100);
            Problem("nv_tile_retry accepted an unknown tile");
        }
        catch (NvException e) when (e.Status == NvStatus.UnknownId)
        {
        }
        try
        {
            core.TalkSetConversation(tile + 100, true);
            Problem("nv_talk_set_conversation accepted an unknown tile");
        }
        catch (NvException e) when (e.Status == NvStatus.UnknownId)
        {
        }
        core.TalkSetConversation(tile, true);
        core.TalkSetConversation(tile, false);
        core.SetTier(tile, tier);
        bool done = WaitUntil(() =>
            _app.Gen(generation).Answered >= 20 &&
            _app.Locked(() => old.Resets) == 1 &&
            _app.TierEvents.Contains((name, "active", generation)), 10_000);
        if (!done)
        {
            Problem($"the switch to {name} (generation {generation}) did not complete: " +
                $"answered {_app.Gen(generation).Answered}, old resets {old.Resets}");
            return;
        }
        var events = _app.TierEvents.Where(e => e.Generation == generation).ToList();
        if (events.Count < 2 || events[0] != (name, "starting", generation) || events[1] != (name, "active", generation))
        {
            Problem($"tile_tier events for generation {generation}: {string.Join(", ", events)}");
        }
        if (_app.Gen(generation).Configures != 1)
        {
            Problem($"generation {generation} has {_app.Gen(generation).Configures} Configures");
        }
    }

    private void RenderLoop(NvCore core)
    {
        // An audio device's real-time thread: 10 ms of 48 kHz stereo per
        // callback, contiguous on the device's timeline (host time advances
        // by exactly the frames rendered), each callback about 10 ms ahead
        // of when it is heard.
        float[] buffer = new float[480 * 2];
        ulong start = NvCore.ClockNowNs() + 20_000_000;
        ulong frames = 0;
        while (!_stopRender)
        {
            ulong host = start + frames * 1_000_000_000 / 48_000;
            frames += 480;
            NvStatus s = core.RenderAudio(buffer, 2, host);
            if (s != NvStatus.Ok)
            {
                Problem($"nv_audio_render returned {s}");
            }
            bool loud = false;
            foreach (float v in buffer)
            {
                if (!float.IsFinite(v) || v > 1f || v < -1f)
                {
                    Problem("rendered sample out of range");
                    break;
                }
                loud |= MathF.Abs(v) > 0.1f;
            }
            Interlocked.Increment(ref _renders);
            if (loud)
            {
                Interlocked.Increment(ref _loudRenders);
            }
            while (!_stopRender && NvCore.ClockNowNs() + 10_000_000 < host)
            {
                Thread.Sleep(1);
            }
        }
    }

    // Discovery (minor 1): a bad request is refused, and a search on loopback
    // only (nothing answers there) starts, stops, and reports its end.
    // Updates (minor 2): the pure helpers, a refused request, and a check
    // with no trusted key (it starts and reports the failure as an event;
    // nothing is fetched), then cancel.
    private void CheckUpdates(NvCore core)
    {
        if (NvCore.UpdateParseKey("not a key") is not null)
        {
            Problem("nv_update_parse_key accepted text that is not a key");
        }
        string? parsed = NvCore.UpdateParseKey("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c");
        if (parsed is null || !parsed.Contains("\"id\":", StringComparison.Ordinal))
        {
            Problem($"nv_update_parse_key: {parsed}");
        }
        if (!NvCore.UpdateParseSource("http://updates.example.com/").Contains("insecure_source", StringComparison.Ordinal))
        {
            Problem("nv_update_parse_source accepted http:// to a public host");
        }
        try
        {
            core.UpdateCheck("{");
            Problem("nv_update_check accepted invalid JSON");
        }
        catch (NvException e) when (e.Status == NvStatus.InvalidArgument)
        {
        }
        core.UpdateCancel();
        core.UpdateCheck("{\"source\":\"https://updates.example.com/u\",\"keys\":[],\"current\":\"0.3.0\"}");
        core.UpdateCancel();
    }

    private void CheckDiscovery(NvCore core)
    {
        if (NvCore.AbiMinor() != NvNative.AbiMinor)
        {
            Problem($"nv_abi_minor {NvCore.AbiMinor()}, bindings {NvNative.AbiMinor}");
        }
        try
        {
            core.DiscoverStart("{\"seconds\":0}");
            Problem("nv_discover_start accepted seconds 0");
        }
        catch (NvException e) when (e.Status == NvStatus.InvalidArgument)
        {
        }
        core.DiscoverStop();
        core.DiscoverStart("{\"seconds\":30,\"interfaces\":[\"127.0.0.1\"],\"onvif\":false}");
        core.DiscoverStop();
        if (!WaitUntil(() => _app.DiscoveryDone().Length == 1, 5_000) ||
            _app.DiscoveryDone()[0] is not { Search: 1, Stopped: true })
        {
            Problem($"discovery_done after nv_discover_stop: {string.Join(",", _app.DiscoveryDone())}");
        }
    }

    private string CheckLayouts(NvCore core, ulong device, string dataDir)
    {
        // A 4-byte first buffer forces the NV_ERR_BUFFER_TOO_SMALL path.
        string key = core.DeviceKey(device, initialCapacity: 4);
        if (key.Length == 0 || key != core.DeviceKey(device))
        {
            Problem($"nv_device_key: '{key}'");
        }
        if (!WaitUntil(() => Volatile.Read(ref _app.KeyFromEvent) is not null, 2_000) ||
            _app.KeyFromEvent != key)
        {
            Problem("device_info key differs from nv_device_key");
        }

        string layout = JsonSerializer.Serialize(new Dictionary<string, object?>
        {
            ["id"] = "csharp-smoke",
            ["name"] = "Smoke",
            ["x-app"] = new Dictionary<string, object> { ["fill"] = true },
            ["root"] = new Dictionary<string, object?>
            {
                ["split"] = "row",
                ["ratio"] = 0.5,
                ["a"] = new Dictionary<string, object> { ["camera"] = new Dictionary<string, object> { ["device"] = key, ["channel"] = 0 } },
                ["b"] = new Dictionary<string, object?> { ["camera"] = null },
            },
        });
        core.PutLayout(layout);
        (ulong revision, JsonElement? stored) = ReadLayout(core, "csharp-smoke", initialCapacity: 16);
        if (stored is not { } s1 || s1.GetProperty("version").GetInt32() != 1 ||
            !s1.TryGetProperty("x-app", out JsonElement app) || !app.GetProperty("fill").GetBoolean() ||
            s1.GetProperty("root").GetProperty("a").GetProperty("camera").GetProperty("device").GetString() != key)
        {
            Problem("the stored layout differs from what was put");
        }
        if (!WaitUntil(() => _app.LayoutRevisions.Contains(revision), 2_000))
        {
            Problem($"no layouts_changed for revision {revision}");
        }
        core.PutLayout(layout);
        (ulong revision2, JsonElement? stored2) = ReadLayout(core, "csharp-smoke", 4096);
        if (revision2 <= revision || stored2?.GetProperty("version").GetInt32() != 2)
        {
            Problem("replacing a layout did not bump its version and the revision");
        }
        if (!File.Exists(Path.Combine(dataDir, "layouts.json")) || !File.Exists(Path.Combine(dataDir, "devices.json")))
        {
            Problem("layouts.json or devices.json is missing from the data directory");
        }
        if (StatusOf(() => core.PutLayout("{\"id\":\"bad\"}")) != NvStatus.InvalidArgument)
        {
            Problem("an invalid layout was accepted");
        }
        core.DeleteLayout("csharp-smoke");
        if (StatusOf(() => core.DeleteLayout("csharp-smoke")) != NvStatus.UnknownId)
        {
            Problem("deleting a missing layout was not NV_ERR_UNKNOWN_ID");
        }
        if (ReadLayout(core, "csharp-smoke", 4096).Layout is not null)
        {
            Problem("the deleted layout is still listed");
        }
        return key;
    }

    // Recordings (minor 5) through the bindings, against mock-nvr's two days
    // of recordings: configuration, yesterday's index, an incident edited and
    // locked, a thumbnail from pending to ready, a download to the end, and
    // the Recycle Bin hand-off, with the events of each step.
    private void CheckRecordings(NvCore core, string key, string dataDir)
    {
        IRecordings rec = core;
        string downloads = Path.Combine(dataDir, "downloads");
        string channels = """{"channels":[{"channel":0},{"channel":1}]}""";
        if (StatusOf(() => rec.RecordingsEnable(key, channels)) != NvStatus.NotConfigured)
        {
            Problem("nv_recordings_enable before nv_recordings_configure");
        }
        rec.RecordingsConfigure(JsonSerializer.Serialize(new Dictionary<string, object> { ["downloads_dir"] = downloads }));
        rec.RecordingsEnable(key, channels);
        rec.SoundConfigure("""{"alarms":true}""");

        long day = (DateTimeOffset.UtcNow.ToUnixTimeSeconds() / 86400 - 1) * 86400;
        string cams = $$"""[{"device":"{{key}}","channel":0},{"device":"{{key}}","channel":1}]""";
        rec.RecordingsWant($$"""{"cameras":{{cams}},"days":[{{day}}]}""");
        string query = $$"""{"start":{{day}},"end":{{day + 86399}}}""";
        JsonElement firstEvent = default;
        if (!WaitUntil(() =>
            {
                using JsonDocument doc = JsonDocument.Parse(rec.RecordingsEvents(query));
                JsonElement list = doc.RootElement.GetProperty("events");
                if (list.GetArrayLength() == 0 || _app.EventCount("recordings_changed") == 0)
                {
                    return false;
                }
                firstEvent = list[0].Clone();
                return true;
            }, 20_000))
        {
            Problem("no events indexed, or no recordings_changed");
            return;
        }
        if (!rec.RecordingsCalendar(query).Contains("\"days\"", StringComparison.Ordinal) ||
            !rec.RecordingsSpans(query).Contains("\"causes\"", StringComparison.Ordinal) ||
            !rec.RecordingsStatus().Contains(key, StringComparison.Ordinal))
        {
            Problem("calendar, spans, or status");
        }
        rec.RecordingsHoldToday(1, $$"""{"cameras":{{cams}}}""");
        rec.RecordingsHoldToday(1, null);

        string incident;
        using (JsonDocument doc = JsonDocument.Parse(rec.IncidentsGet(query)))
        {
            incident = doc.RootElement.GetProperty("incidents")[0].GetProperty("key").GetString()!;
        }
        if (StatusOf(() => rec.IncidentGet("i-unknown")) != NvStatus.UnknownId ||
            !rec.IncidentEdit($$"""{"op":"rename","key":"{{incident}}","name":"Delivery"}""").Contains(incident, StringComparison.Ordinal))
        {
            Problem("nv_incident_get or nv_incident_edit");
        }
        rec.CameraGroupsPut("""{"groups":[]}""");
        _ = rec.CameraGroupsGet();

        // A thumbnail of the first event: pending, then ready.
        string thumb = $$"""{"camera":{{firstEvent.GetProperty("camera").GetRawText()}},"tier":"sub","start":{{firstEvent.GetProperty("start").GetInt64()}},"end":{{firstEvent.GetProperty("end").GetInt64()}}}""";
        if (rec.ThumbnailGet(thumb) is not null)
        {
            Problem("a thumbnail not cached yet was not pending");
        }
        RecordedThumbnail? t = null;
        if (!WaitUntil(() => _app.EventCount("thumbnail") > 0 && (t = rec.ThumbnailGet(thumb)) is not null, 20_000) ||
            t!.Kind != NvThumbKind.H264Keyframe || t.Data.Length < 5 || t.Data[0] != 0 || t.Data[1] != 0)
        {
            Problem("thumbnail from pending to ready");
        }
        var image = new RecordedThumbnail(NvThumbKind.Image, 64, 36, 0, [0xff, 0xd8, 0xff, 0xd9]);
        rec.ThumbnailPut(thumb, image);
        if (rec.ThumbnailGet(thumb) is not { Kind: NvThumbKind.Image } back || !back.Data.SequenceEqual(image.Data))
        {
            Problem("nv_thumbnail_put");
        }
        rec.ThumbnailCancel(null);

        // A download to the end, a lock that returns it, and the Recycle Bin.
        string id;
        using (JsonDocument doc = JsonDocument.Parse(rec.DownloadIncident($$"""{"incident":"{{incident}}"}""")))
        {
            id = doc.RootElement.GetProperty("download").GetString()!;
        }
        if (!WaitUntil(() =>
            {
                using JsonDocument doc = JsonDocument.Parse(rec.DownloadsGet());
                return doc.RootElement.GetProperty("downloads").EnumerateArray()
                    .Any(d => d.GetProperty("download").GetString() == id && d.GetProperty("state").GetString() == "done");
            }, 30_000))
        {
            Problem("the download did not finish");
        }
        string locked = rec.IncidentLock(incident, true);
        if (!locked.Contains("\"existing\"", StringComparison.Ordinal) || !locked.Contains(id, StringComparison.Ordinal))
        {
            Problem($"nv_incident_lock: {locked}");
        }
        if (StatusOf(() => rec.DownloadPause("no-such-download", null, true)) != NvStatus.UnknownId)
        {
            Problem("nv_download_pause accepted an unknown download");
        }
        rec.DownloadRetry(id, null);
        if (!rec.DownloadTrash(id).Contains("\"folder\"", StringComparison.Ordinal) ||
            !WaitUntil(() => _app.EventCount("download_removed") > 0, 10_000) ||
            StatusOf(() => rec.DownloadForget(id, false)) != NvStatus.UnknownId)
        {
            Problem("nv_download_trash and download_removed");
        }
        if (_app.EventCount("download") == 0 || _app.EventCount("incidents_changed") == 0)
        {
            Problem("no download or incidents_changed events");
        }
    }

    // Recorded playback (minor 6): two cameras of yesterday's recordings
    // (indexed by CheckRecordings) beside the live tile: open, play, seek,
    // 4x, status, close, with the events and status codes.
    private void CheckPlayback(NvCore core, string key)
    {
        IPlayback pb = core;
        long day = (DateTimeOffset.UtcNow.ToUnixTimeSeconds() / 86400 - 1) * 86400;
        long start = (day + 10 * 3600) * 1000, p0 = start + 30_000;
        string request = $$"""{"start_ms":{{start}},"end_ms":{{start + 600_000}},"position_ms":{{p0}},"cameras":[{"camera":{"device":"{{key}}","channel":0},"tier":"sub"},{"camera":{"device":"{{key}}","channel":1},"tier":"sub"}]}""";
        _app.PlaybackPhase = true;
        if (StatusOf(() => pb.PlaybackOpen("{\"start_ms\":0}")) != NvStatus.InvalidArgument ||
            StatusOf(() => pb.PlaybackPlay(999_999)) != NvStatus.UnknownId)
        {
            Problem("playback refusals");
        }
        OpenedPlayback opened = pb.PlaybackOpen(request);
        if (opened.Tiles.Count != 2 || opened.Playback == 0)
        {
            Problem("nv_playback_open");
            return;
        }
        if (!WaitUntil(() => _app.EventCount("playback_seek") >= 2 && Interlocked.Read(ref _app.PlaybackDecodes) > 0, 20_000))
        {
            Problem("the opening seek did not show pictures");
        }
        if (pb.PlaybackPosition(opened.Playback) != (p0, NvPlaybackState.Paused))
        {
            Problem("not paused at the opening position");
        }
        if (StatusOf(() => core.CloseTile(opened.Tiles[0])) != NvStatus.InvalidArgument ||
            StatusOf(() => pb.PlaybackSetRate(opened.Playback, 3)) != NvStatus.InvalidArgument ||
            StatusOf(() => pb.PlaybackSeek(opened.Playback, p0, (NvSeekMode)9)) != NvStatus.InvalidArgument)
        {
            Problem("playback tile refusals");
        }
        pb.PlaybackPlay(opened.Playback);
        if (!WaitUntil(() => pb.PlaybackPosition(opened.Playback) is { State: NvPlaybackState.Playing } p && p.PositionMs > p0 + 500, 10_000))
        {
            Problem("the playback did not play");
        }
        int seeks = _app.EventCount("playback_seek");
        pb.PlaybackSeek(opened.Playback, p0 + 30_000, NvSeekMode.Exact);
        if (!WaitUntil(() => _app.EventCount("playback_seek") >= seeks + 2, 20_000) ||
            pb.PlaybackPosition(opened.Playback).PositionMs < p0 + 30_000)
        {
            Problem("nv_playback_seek");
        }
        pb.PlaybackSetRate(opened.Playback, 4);
        pb.PlaybackSetOffset(opened.Playback, opened.Tiles[1], 0);
        if (!WaitUntil(() => pb.PlaybackStatus(opened.Playback).Contains("\"rate\":4.0", StringComparison.Ordinal), 5_000))
        {
            Problem("nv_playback_set_rate or nv_playback_status");
        }
        pb.PlaybackPause(opened.Playback);
        pb.PlaybackClose(opened.Playback);
        if (!WaitUntil(() => _app.EventCount("playback_closed") == 1 && Interlocked.Read(ref _app.PlaybackTilesClosed) == 2, 5_000) ||
            StatusOf(() => pb.PlaybackClose(opened.Playback)) != NvStatus.UnknownId)
        {
            Problem("nv_playback_close");
        }
        _app.PlaybackPhase = false;
    }

    private static (ulong Revision, JsonElement? Layout) ReadLayout(NvCore core, string id, int initialCapacity)
    {
        using JsonDocument doc = JsonDocument.Parse(core.LayoutsJson(initialCapacity));
        ulong revision = doc.RootElement.GetProperty("revision").GetUInt64();
        foreach (JsonElement l in doc.RootElement.GetProperty("layouts").EnumerateArray())
        {
            if (l.GetProperty("id").GetString() == id)
            {
                return (revision, l.Clone());
            }
        }
        return (revision, null);
    }

    private void CheckGrid()
    {
        Span<NvTileLayout> layout = stackalloc NvTileLayout[2];
        NvSize[] sizes = [new(1920, 1080), new(0, 0)];
        NvCore.GridLayout(new NvSize(800, 300), 2, sizes, false, layout);
        // Two cells side by side; the 16:9 picture letterboxed in its cell,
        // nothing sampled for the tile without a picture.
        if (layout[0].Dest.Width == 0 || layout[0].Cell.Width != layout[1].Cell.Width ||
            layout[1].Cell.X <= layout[0].Cell.X || layout[0].Source.Width != 1920 ||
            layout[1].Source.Width != 0 || layout[0].Dest.Width * 9 > layout[0].Dest.Height * 16 + 16)
        {
            Problem("nv_grid_layout");
        }
    }

    private void CheckSecondRun(string host, ushort port, string dataDir, string key)
    {
        // Device keys persist in the data directory across runs.
        var app2 = new SecondRunApp();
        using NvCore core = NvCore.Create(new NvCoreOptions { DataDir = dataDir }, app2);
        ulong device = core.AddDevice(host, port, "example-user", _password);
        if (core.DeviceKey(device) != key)
        {
            Problem("the device key changed across runs");
        }
        core.RemoveDevice(device);
        if (core.CallbackExceptionCount != 0)
        {
            Problem($"second run: callback exception {core.FirstCallbackException}");
        }
    }

    private void CheckFinalCounts()
    {
        for (uint g = 0; g < 3; g++)
        {
            Generation gen = _app.Gen(g);
            if (gen.Answered != gen.Decodes)
            {
                Problem($"generation {g}: {gen.Decodes} Decodes, {gen.Answered} answers");
            }
            if (gen.Configures != 1)
            {
                Problem($"generation {g}: {gen.Configures} Configures");
            }
        }
        if (_app.Gen(0).Resets != 1 || _app.Gen(1).Resets != 1)
        {
            Problem("each replaced generation must end with exactly one Reset");
        }
        if (_app.Gen(3).Configures != 0)
        {
            Problem("a fourth generation appeared");
        }
        if (_app.ActionTiles.Count != 1)
        {
            Problem($"actions for {_app.ActionTiles.Count} tiles");
        }
        // The playback's two tiles closed with it (CheckPlayback).
        if (_app.EventCount("tile_closed") - Interlocked.Read(ref _app.PlaybackTilesClosed) != 1)
        {
            Problem("expected exactly one tile_closed for the live tile");
        }
        if (_app.EventCount("tile_stats") == 0)
        {
            Problem("no tile_stats event");
        }
        if (_app.TapBlocks == 0)
        {
            Problem("no audio tap blocks");
        }
        if (Interlocked.Read(ref _renders) < 20)
        {
            Problem("the render thread barely ran");
        }
        if (Interlocked.Read(ref _loudRenders) == 0)
        {
            Problem("rendered audio was never audible");
        }
        if (_app.ReportedException is not InvalidOperationException { Message: "deliberate exception from on_event" })
        {
            Problem($"the deliberate callback exception was not reported ({_app.ReportedException})");
        }
    }

    private sealed class SecondRunApp : NvCoreCallbacks
    {
        public override void OnAction(NvCore core, ulong tile, in NvAction action)
        {
        }

        public override void OnEvent(NvCore core, ReadOnlySpan<byte> json)
        {
        }
    }
}
