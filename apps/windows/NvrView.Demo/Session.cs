// What the self-test and the live mode share: mock-nvr processes, one core
// whose tiles are carried out by NvrView.Video, and a render thread that
// draws the grid offscreen at a fixed rate and reads frames back on request.

using System.Diagnostics;
using System.Text;
using System.Text.Json;
using NvrView.Interop;
using NvrView.Video;

namespace NvrView.Demo;

/// <summary>A spawned tools/mock-nvr.</summary>
internal sealed class MockNvr : IDisposable
{
    private readonly Process _process;

    private MockNvr(Process process, string host, ushort port)
    {
        _process = process;
        Host = host;
        Port = port;
    }

    public string Host { get; }

    public ushort Port { get; }

    public Process Process => _process;

    /// <summary>Starts mock-nvr on a free port with <paramref name="args"/>.</summary>
    public static MockNvr Start(string exe, string password, params string[] args)
    {
        var psi = new ProcessStartInfo(exe)
        {
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            UseShellExecute = false,
        };
        psi.ArgumentList.Add("--port");
        psi.ArgumentList.Add("0");
        foreach (string a in args)
        {
            psi.ArgumentList.Add(a);
        }
        psi.Environment["MOCK_NVR_PASSWORD"] = password;
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
        return new MockNvr(p, addr[..colon], ushort.Parse(addr[(colon + 1)..]));
    }

    public void Dispose()
    {
        try
        {
            if (!_process.HasExited)
            {
                _process.Kill(entireProcessTree: true);
                _process.WaitForExit(5000);
            }
        }
        catch (InvalidOperationException)
        {
        }
        _process.Dispose();
    }
}

/// <summary>The core's latest tile_stats for one tile.</summary>
internal readonly record struct CoreTileStats(long Received, long Decoded, long Gated, long Dropped, long Failures, long Resets);

/// <summary>One tile of the demo.</summary>
internal sealed class DemoTile(string name, ulong device, ulong id, VideoTile video)
{
    public string Name { get; } = name;

    public ulong Device { get; } = device;

    public ulong Id { get; } = id;

    public VideoTile Video { get; } = video;

    /// <summary>Distinct pictures the renderer drew (written by the render thread).</summary>
    public long Shown;

    public ulong LastSequence;
}

/// <summary>A frame read back, with what the renderer drew it from.</summary>
internal sealed record Capture(RenderedImage Image, NvTileLayout[] Layout, NvSize[] Sizes, long[] Decoded, double AtSeconds);

/// <summary>A core with demo tiles, rendered offscreen (see the file comment).</summary>
internal sealed class Session : NvCoreCallbacks, IDisposable
{
    private readonly Lock _tilesLock = new();
    private readonly Dictionary<ulong, DemoTile> _byId = [];
    private readonly List<DemoTile> _tiles = [];
    private readonly Dictionary<ulong, CoreTileStats> _stats = [];
    private readonly Dictionary<ulong, string> _deviceStates = [];
    private readonly List<string> _errors = [];
    private readonly bool _allowHardware;
    private readonly string _password;
    private readonly Stopwatch _clock = Stopwatch.StartNew();
    private readonly string _temp;
    private Thread? _renderThread;
    private volatile bool _stopRender;
    private readonly double _renderInterval;
    private TaskCompletionSource<Capture>? _captureRequest;
    private VideoTile?[] _renderTiles = [];
    private long _renderTicks;
    private long _renderFrames;

    public Session(GpuDevice gpu, uint width, uint height, bool allowHardware, string password, double fps = 30)
    {
        Gpu = gpu;
        _allowHardware = allowHardware;
        _password = password;
        _renderInterval = 1.0 / fps;
        Renderer = new GridRenderer(gpu, width, height);
        _temp = Path.Combine(Path.GetTempPath(), "nvrview-demo-" + Environment.ProcessId);
        Directory.CreateDirectory(_temp);
        Core = NvCore.Create(new NvCoreOptions
        {
            LogDir = Path.Combine(_temp, "logs"),
            LogMaxFiles = 2,
            LogMaxTotalBytes = 4 << 20,
        }, this);
    }

    public GpuDevice Gpu { get; }

    public NvCore Core { get; }

    public GridRenderer Renderer { get; }

    public double Elapsed => _clock.Elapsed.TotalSeconds;

    public IReadOnlyList<DemoTile> Tiles => _tiles;

    /// <summary>Average time to draw one frame (CPU side, including the lock), ms.</summary>
    public double RenderMs => _renderFrames == 0 ? 0 : _renderTicks * 1000.0 / Stopwatch.Frequency / _renderFrames;

    public long RenderFrames => Interlocked.Read(ref _renderFrames);

    public List<string> Errors
    {
        get
        {
            lock (_tilesLock)
            {
                return [.. _errors];
            }
        }
    }

    public CoreTileStats StatsOf(ulong tile)
    {
        lock (_tilesLock)
        {
            return _stats.GetValueOrDefault(tile);
        }
    }

    /// <summary>Adds a device and waits until it is connected.</summary>
    public ulong AddDevice(string host, ushort port)
    {
        ulong device = Core.AddDevice(host, port, "example-user", _password);
        var deadline = Stopwatch.StartNew();
        while (deadline.ElapsedMilliseconds < 15_000)
        {
            lock (_tilesLock)
            {
                if (_deviceStates.GetValueOrDefault(device) == "connected")
                {
                    return device;
                }
            }
            Thread.Sleep(20);
        }
        throw new InvalidOperationException($"device {device} did not connect within 15 s");
    }

    /// <summary>Opens a tile and gives it a VideoTile before any action can look for it.</summary>
    public DemoTile OpenTile(string name, ulong device, byte channel, NvStreamTier tier)
    {
        lock (_tilesLock)
        {
            ulong id = Core.OpenTile(device, channel, tier);
            var tile = new DemoTile(name, device, id, new VideoTile(Gpu, Core, id, _allowHardware));
            _byId[id] = tile;
            _tiles.Add(tile);
            return tile;
        }
    }

    public void StartRendering()
    {
        _renderTiles = [.. _tiles.Select(t => (VideoTile?)t.Video)];
        _renderThread = new Thread(RenderLoop) { Name = "render", IsBackground = true };
        _renderThread.Start();
    }

    /// <summary>Reads back the next frame the render thread draws.</summary>
    public Capture CaptureNext()
    {
        var request = new TaskCompletionSource<Capture>(TaskCreationOptions.RunContinuationsAsynchronously);
        Volatile.Write(ref _captureRequest, request);
        if (!request.Task.Wait(TimeSpan.FromSeconds(10)))
        {
            throw new InvalidOperationException("no frame was drawn within 10 s");
        }
        return request.Task.Result;
    }

    private void RenderLoop()
    {
        ulong[] sequences = new ulong[_renderTiles.Length];
        long period = (long)(_renderInterval * Stopwatch.Frequency);
        long next = Stopwatch.GetTimestamp();
        while (!_stopRender)
        {
            long start = Stopwatch.GetTimestamp();
            Renderer.Render(_renderTiles, sequences);
            for (int i = 0; i < sequences.Length; i++)
            {
                DemoTile t = _tiles[i];
                if (sequences[i] != 0 && sequences[i] != t.LastSequence)
                {
                    t.Shown++;
                }
                t.LastSequence = sequences[i];
            }
            _renderTicks += Stopwatch.GetTimestamp() - start;
            Interlocked.Increment(ref _renderFrames);
            TaskCompletionSource<Capture>? request = Interlocked.Exchange(ref _captureRequest, null);
            if (request is not null)
            {
                try
                {
                    RenderedImage image = Renderer.ReadBack();
                    request.SetResult(new Capture(
                        image, Renderer.Layout.ToArray(), Renderer.PictureSizes.ToArray(),
                        [.. _tiles.Select(t => t.Video.Counters.Decoded)], Elapsed));
                }
                catch (Exception e)
                {
                    request.SetException(e);
                }
            }
            next += period;
            long wait = next - Stopwatch.GetTimestamp();
            if (wait > 0)
            {
                Thread.Sleep(TimeSpan.FromTicks(wait * TimeSpan.TicksPerSecond / Stopwatch.Frequency));
            }
            else
            {
                next = Stopwatch.GetTimestamp();
            }
        }
    }

    public override void OnAction(NvCore core, ulong tile, in NvAction action)
    {
        DemoTile? t;
        lock (_tilesLock)
        {
            t = _byId.GetValueOrDefault(tile);
        }
        if (t is not null)
        {
            t.Video.OnAction(in action);
        }
        else if (action.Kind == NvActionKind.Decode)
        {
            core.DecodeFailed(tile, action.Generation, NvDecodeFailure.NotConfigured);
        }
    }

    public override void OnEvent(NvCore core, ReadOnlySpan<byte> json)
    {
        using JsonDocument doc = JsonDocument.Parse(json.ToArray());
        JsonElement root = doc.RootElement;
        string? type = root.TryGetProperty("type", out JsonElement t) ? t.GetString() : null;
        switch (type)
        {
            case "device_state":
                lock (_tilesLock)
                {
                    _deviceStates[root.GetProperty("device").GetUInt64()] = root.GetProperty("state").GetString() ?? "";
                }
                break;
            case "tile_stats":
                var stats = new CoreTileStats(
                    Long(root, "received"), Long(root, "decoded"), Long(root, "gated"),
                    Long(root, "dropped"), Long(root, "failures"), Long(root, "resets"));
                lock (_tilesLock)
                {
                    _stats[root.GetProperty("tile").GetUInt64()] = stats;
                }
                break;
            case "tile_error":
            case "tile_closed" when root.GetProperty("reason").GetString() != "closed":
                lock (_tilesLock)
                {
                    _errors.Add(Encoding.UTF8.GetString(json));
                }
                break;
            case "tile_tier" when root.GetProperty("state").GetString() == "failed":
                lock (_tilesLock)
                {
                    _errors.Add(Encoding.UTF8.GetString(json));
                }
                break;
        }
    }

    private static long Long(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.TryGetInt64(out long n) ? n : 0;

    public override void OnCallbackException(Exception exception)
    {
        lock (_tilesLock)
        {
            _errors.Add($"callback threw: {exception}");
        }
    }

    /// <summary>Stops rendering, closes every tile and device, frees the core.</summary>
    public void Dispose()
    {
        _stopRender = true;
        _renderThread?.Join();
        foreach (DemoTile t in _tiles)
        {
            try
            {
                Core.CloseTile(t.Id);
            }
            catch (NvException)
            {
            }
            t.Video.Dispose();
        }
        foreach (ulong device in _tiles.Select(t => t.Device).Distinct())
        {
            try
            {
                Core.RemoveDevice(device);
            }
            catch (NvException)
            {
            }
        }
        Core.Dispose();
        Renderer.Dispose();
        try
        {
            Directory.Delete(_temp, recursive: true);
        }
        catch (IOException)
        {
        }
        catch (UnauthorizedAccessException)
        {
        }
    }
}
