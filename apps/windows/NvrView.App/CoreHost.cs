// The core and the GPU device the tiles decode on, and the callbacks that
// connect them (docs/FFI.md):
//
// - Actions run on the core's thread for each tile and go straight to that
//   tile's VideoTile, inside the read side of the device gate. Replacing a
//   lost GPU device takes the write side, so no action runs while decoders
//   and pictures move to the new device.
// - Events are parsed on the core's event thread and handed to the UI
//   thread (Viewer.Apply), which owns every piece of viewer state.

using System.Text.Json;
using Microsoft.UI.Dispatching;
using NvrView.Interop;
using NvrView.Shell;
using NvrView.Tiles;
using NvrView.Video;

namespace NvrView.App;

/// <summary>One parsed core event (the fields the viewer uses; the rest are ignored).</summary>
internal sealed class CoreEvent
{
    public string Type = "";
    public ulong Device;
    public ulong Tile;
    public string? State;
    public string? Reason;
    public string? Kind;
    public string? Source;
    public string? Tier;
    public string? Name;
    public string? Model;
    public string? Key;
    public string? What;
    public int Channel = -1;
    public int Lens;
    public int Status;
    public int Attempt;
    public long RetryMs;
    public long SentMs;
    public long Generation = -1;
    public long Ns;
    public long AtNs;
    public double? DeltaMs;
    public double? HeldMs;
    public long Received;
    public long Decoded;
    public long Dropped;
    public long Failures;
    public long Revision;
    public List<ChannelInfo>? Channels;

    /// <summary>A discovery event (discovery_started, discovered, discovery_done), parsed whole.</summary>
    public DiscoveryEvent? Discovery;

    /// <summary>An update event (minor 2), parsed whole.</summary>
    public NvrView.Shell.UpdateEvent? Update;

    /// <summary>The raw JSON of audio_stats events (diagnostics only).</summary>
    public string? Raw;

    // Numbers the core may write as integers or with a fraction.
    private static long Long(JsonElement v) => v.TryGetInt64(out long l) ? l : (long)v.GetDouble();

    public static CoreEvent? Parse(ReadOnlySpan<byte> json)
    {
        using JsonDocument doc = JsonDocument.Parse(json.ToArray());
        JsonElement r = doc.RootElement;
        if (r.ValueKind != JsonValueKind.Object || !r.TryGetProperty("type", out JsonElement t))
        {
            return null;
        }
        var e = new CoreEvent { Type = t.GetString() ?? "" };
        if (e.Type == "update")
        {
            e.Update = NvrView.Shell.UpdateEvent.Parse(json);
            return e.Update is null ? null : e;
        }
        if (e.Type is "discovery_started" or "discovered" or "discovery_done")
        {
            e.Discovery = DiscoveryEvent.Parse(json);
            return e.Discovery is null ? null : e;
        }
        if (e.Type == "audio_stats")
        {
            e.Raw = System.Text.Encoding.UTF8.GetString(json);
        }
        foreach (JsonProperty p in r.EnumerateObject())
        {
            JsonElement v = p.Value;
            switch (p.Name)
            {
                case "device" when v.ValueKind == JsonValueKind.Number: e.Device = v.GetUInt64(); break;
                case "tile" when v.ValueKind == JsonValueKind.Number: e.Tile = v.GetUInt64(); break;
                case "state": e.State = v.GetString(); break;
                case "reason": e.Reason = v.GetString(); break;
                case "kind": e.Kind = v.GetString(); break;
                case "source": e.Source = v.GetString(); break;
                case "tier": e.Tier = v.GetString(); break;
                case "name": e.Name = v.GetString(); break;
                case "model": e.Model = v.GetString(); break;
                case "key": e.Key = v.GetString(); break;
                case "what": e.What = v.GetString(); break;
                case "channel" when v.ValueKind == JsonValueKind.Number: e.Channel = (int)Long(v); break;
                case "lens" when v.ValueKind == JsonValueKind.Number: e.Lens = (int)Long(v); break;
                case "status" when v.ValueKind == JsonValueKind.Number: e.Status = (int)Long(v); break;
                case "attempt" when v.ValueKind == JsonValueKind.Number: e.Attempt = (int)Long(v); break;
                case "retry_ms" when v.ValueKind == JsonValueKind.Number: e.RetryMs = Long(v); break;
                case "sent_ms" when v.ValueKind == JsonValueKind.Number: e.SentMs = Long(v); break;
                case "generation" when v.ValueKind == JsonValueKind.Number: e.Generation = Long(v); break;
                case "at_ns" when v.ValueKind == JsonValueKind.Number: e.AtNs = Long(v); break;
                case "ns" when v.ValueKind == JsonValueKind.Number: e.Ns = Long(v); break;
                case "delta_ms" when v.ValueKind == JsonValueKind.Number: e.DeltaMs = v.GetDouble(); break;
                case "held_ms" when v.ValueKind == JsonValueKind.Number: e.HeldMs = v.GetDouble(); break;
                case "received" when v.ValueKind == JsonValueKind.Number: e.Received = Long(v); break;
                case "decoded" when v.ValueKind == JsonValueKind.Number: e.Decoded = Long(v); break;
                case "dropped" when v.ValueKind == JsonValueKind.Number: e.Dropped = Long(v); break;
                case "failures" when v.ValueKind == JsonValueKind.Number: e.Failures = Long(v); break;
                case "revision" when v.ValueKind == JsonValueKind.Number: e.Revision = Long(v); break;
                case "channels" when v.ValueKind == JsonValueKind.Array:
                    e.Channels = [];
                    foreach (JsonElement c in v.EnumerateArray())
                    {
                        if (ChannelInfo.From(c) is { } info)
                        {
                            e.Channels.Add(info);
                        }
                    }
                    break;
            }
        }
        return e;
    }
}

/// <summary>What a camera's motion hardware has (the `ptz` of the channels event); not what the user allowed.</summary>
internal readonly record struct PtzCaps(bool PanTilt, bool Presets, bool ZoomFocus);

/// <summary>One channel of a device, from a channels event.</summary>
internal sealed class ChannelInfo
{
    public byte Channel;
    public bool Connected;
    public string? Name;
    public string[] Tiers = [];
    public string[]? Detects;
    public int Lenses;

    /// <summary>A fisheye camera (the core's "fisheye"; docs/FFI.md): the app dewarps its circular picture.</summary>
    public bool Fisheye;

    /// <summary>What the camera has (docs/FFI.md "channels"): talk-back, a doorbell, and what moves. Capabilities, not permissions.</summary>
    public bool Talk, Doorbell;

    public PtzCaps Ptz;

    /// <summary>Per lens, for a multi-lens camera (null: the channel's own entry applies to lens 0 only).</summary>
    public bool[]? LensTalk;

    public PtzCaps[]? LensPtz;

    /// <summary>Talk-back of one lens: its own entry when the core listed lenses, else the channel's item.</summary>
    public bool TalkOf(int lens) => LensTalk is { } t && lens < t.Length ? t[lens] : lens == 0 && Talk;

    /// <summary>What one lens can move, by the same rule.</summary>
    public PtzCaps PtzOf(int lens) => LensPtz is { } p && lens < p.Length ? p[lens] : lens == 0 ? Ptz : default;

    private static PtzCaps PtzFrom(JsonElement e)
    {
        static bool Flag(JsonElement o, string name) => o.ValueKind == JsonValueKind.Object && o.TryGetProperty(name, out JsonElement v) && v.ValueKind is JsonValueKind.True;
        return new PtzCaps(Flag(e, "pan_tilt"), Flag(e, "presets"), Flag(e, "zoom_focus"));
    }

    public static ChannelInfo? From(JsonElement c)
    {
        if (!c.TryGetProperty("channel", out JsonElement ch) || !ch.TryGetByte(out byte channel))
        {
            return null;
        }
        var info = new ChannelInfo { Channel = channel };
        if (c.TryGetProperty("connected", out JsonElement conn) && conn.ValueKind is JsonValueKind.True)
        {
            info.Connected = true;
        }
        if (c.TryGetProperty("name", out JsonElement n) && n.ValueKind == JsonValueKind.String)
        {
            info.Name = n.GetString();
        }
        if (c.TryGetProperty("tiers", out JsonElement tiers) && tiers.ValueKind == JsonValueKind.Array)
        {
            info.Tiers = [.. tiers.EnumerateArray().Select(x => x.GetString() ?? "")];
        }
        if (c.TryGetProperty("detects", out JsonElement d) && d.ValueKind == JsonValueKind.Array)
        {
            info.Detects = [.. d.EnumerateArray().Select(x => x.GetString() ?? "")];
        }
        if (c.TryGetProperty("fisheye", out JsonElement fe) && fe.ValueKind is JsonValueKind.True)
        {
            info.Fisheye = true;
        }
        info.Talk = c.TryGetProperty("talk", out JsonElement talk) && talk.ValueKind is JsonValueKind.True;
        info.Doorbell = c.TryGetProperty("doorbell", out JsonElement bell) && bell.ValueKind is JsonValueKind.True;
        if (c.TryGetProperty("ptz", out JsonElement ptz))
        {
            info.Ptz = PtzFrom(ptz);
        }
        if (c.TryGetProperty("lens_talk", out JsonElement lt) && lt.ValueKind == JsonValueKind.Array)
        {
            info.LensTalk = [.. lt.EnumerateArray().Select(x => x.ValueKind is JsonValueKind.True)];
        }
        if (c.TryGetProperty("lens_ptz", out JsonElement lp) && lp.ValueKind == JsonValueKind.Array)
        {
            info.LensPtz = [.. lp.EnumerateArray().Select(PtzFrom)];
        }
        if (c.TryGetProperty("lenses", out JsonElement l) && l.TryGetInt32(out int lenses))
        {
            info.Lenses = lenses;
        }
        return info;
    }
}

/// <summary>The core, the GPU device, and the action path (see the file comment).</summary>
internal sealed class CoreHost : NvCoreCallbacks, IDisposable
{
    private readonly ReaderWriterLockSlim _gate = new(LockRecursionPolicy.NoRecursion);
    private readonly Lock _tilesLock = new();
    private readonly Dictionary<ulong, VideoTile> _byId = [];
    private readonly DispatcherQueue _ui;
    private readonly GpuChoice _gpuChoice;
    private readonly bool _allowHardware;
    private Action<CoreEvent>? _sink;

    public CoreHost(DispatcherQueue ui, NvCoreOptions options, GpuChoice gpu, bool allowHardware)
    {
        _ui = ui;
        _gpuChoice = gpu;
        _allowHardware = allowHardware;
        Gpu = GpuDevice.Create(gpu);
        AppLog.Info($"GPU: {Gpu.Describe()}");
        Core = NvCore.Create(options, this);
    }

    public NvCore Core { get; }

    /// <summary>
    /// Which tier each camera's tiles run at (quality, focus, zoom, second-lens tiers); shared by
    /// every window, so the highest ask for a camera wins across all of them.
    /// </summary>
    public TierPlanner Tiers { get; } = new();

    /// <summary>The core's tier for a planned one.</summary>
    public static NvStreamTier ToCore(StreamTier tier) => tier switch
    {
        StreamTier.Main => NvStreamTier.Main,
        StreamTier.Mobile => NvStreamTier.Mobile,
        StreamTier.Extern => NvStreamTier.Extern,
        _ => NvStreamTier.Sub,
    };

    /// <summary>The planned tier for a tile_tier event's tier name.</summary>
    public static StreamTier? ParseTier(string? name) => name switch
    {
        "main" => StreamTier.Main,
        "sub" => StreamTier.Sub,
        "mobile" => StreamTier.Mobile,
        "extern" => StreamTier.Extern,
        _ => null,
    };

    /// <summary>The current device (replaced after a loss); read it on the render thread or inside the gate.</summary>
    public GpuDevice Gpu { get; private set; }

    /// <summary>
    /// Held (read) by every window's render thread for a frame; replacing
    /// the device takes it (write), so no frame draws on a device being
    /// released.
    /// </summary>
    public ReaderWriterLockSlim FrameGate { get; } = new(LockRecursionPolicy.NoRecursion);

    /// <summary>GPU devices replaced after a loss.</summary>
    public int DeviceReplacements { get; private set; }

    /// <summary>Events go here, on the UI thread.</summary>
    public void SetSink(Action<CoreEvent> sink) => _sink = sink;

    /// <summary>Opens a tile and its VideoTile before any action can look for it.</summary>
    public (ulong Id, VideoTile Video) OpenTile(ulong device, byte channel, NvStreamTier tier)
    {
        _gate.EnterReadLock();
        try
        {
            lock (_tilesLock)
            {
                ulong id = Core.OpenTile(device, channel, tier);
                var video = new VideoTile(Gpu, Core, id, _allowHardware);
                _byId[id] = video;
                return (id, video);
            }
        }
        finally
        {
            _gate.ExitReadLock();
        }
    }

    /// <summary>Opens a playback with its tiles' decoders made before any action can reach them (docs/FFI-RECORDINGS.md section 12).</summary>
    public OpenedPlayback OpenPlayback(string json)
    {
        _gate.EnterReadLock();
        try
        {
            lock (_tilesLock)
            {
                OpenedPlayback opened = Core.PlaybackOpen(json);
                foreach (ulong id in opened.Tiles)
                {
                    _byId[id] = new VideoTile(Gpu, Core, id, _allowHardware);
                }
                return opened;
            }
        }
        finally
        {
            _gate.ExitReadLock();
        }
    }

    /// <summary>Adds a camera to a playback, with its tile's decoder made first.</summary>
    public ulong AddPlaybackCamera(ulong playback, string json)
    {
        _gate.EnterReadLock();
        try
        {
            lock (_tilesLock)
            {
                ulong id = Core.PlaybackAddCamera(playback, json);
                _byId[id] = new VideoTile(Gpu, Core, id, _allowHardware);
                return id;
            }
        }
        finally
        {
            _gate.ExitReadLock();
        }
    }

    /// <summary>Releases the decoders of playback tiles the core has closed (no action for them runs any more).</summary>
    public void ReleaseTiles(IEnumerable<ulong> ids)
    {
        foreach (ulong id in ids)
        {
            VideoTile? video;
            lock (_tilesLock)
            {
                _byId.Remove(id, out video);
            }
            if (video is not null)
            {
                _gate.EnterReadLock();
                try
                {
                    video.Dispose();
                }
                finally
                {
                    _gate.ExitReadLock();
                }
            }
        }
    }

    /// <summary>The decoder and pictures of an open tile, or null.</summary>
    public VideoTile? VideoOf(ulong id)
    {
        lock (_tilesLock)
        {
            return _byId.GetValueOrDefault(id);
        }
    }

    /// <summary>Gets the recordings and playback events (set once by the app; they arrive on the UI thread).</summary>
    public RecordingsService? Recordings { get; set; }

    /// <summary>Closes a tile (no action for it runs after this) and releases its decoders.</summary>
    public void CloseTile(ulong id)
    {
        try
        {
            Core.CloseTile(id);
        }
        catch (NvException e) when (e.Status is NvStatus.UnknownId or NvStatus.Closed)
        {
        }
        VideoTile? video;
        lock (_tilesLock)
        {
            _byId.Remove(id, out video);
        }
        if (video is not null)
        {
            _gate.EnterReadLock();
            try
            {
                video.Dispose();
            }
            finally
            {
                _gate.ExitReadLock();
            }
        }
    }

    /// <summary>
    /// Replaces a lost GPU device (render thread): holds back every action,
    /// creates a new device, moves every tile to it, releases the old one.
    /// The renderer's own objects must be released before and recreated after.
    /// With several windows, each render thread may see the loss: passing the
    /// device it lost makes the replacement happen once.
    /// </summary>
    public void ReplaceGpu(GpuDevice? lost = null)
    {
        FrameGate.EnterWriteLock();
        _gate.EnterWriteLock();
        try
        {
            GpuDevice old = Gpu;
            if (lost is not null && !ReferenceEquals(lost, old))
            {
                // Another window's render thread already replaced it.
                return;
            }
            GpuDevice fresh = GpuDevice.Create(_gpuChoice);
            lock (_tilesLock)
            {
                foreach (VideoTile video in _byId.Values)
                {
                    video.ReplaceDevice(fresh);
                }
            }
            Gpu = fresh;
            DeviceReplacements++;
            old.Dispose();
            AppLog.Warn($"GPU device replaced: {fresh.Describe()}");
        }
        finally
        {
            _gate.ExitWriteLock();
            FrameGate.ExitWriteLock();
        }
    }

    public override void OnAction(NvCore core, ulong tile, in NvAction action)
    {
        _gate.EnterReadLock();
        try
        {
            VideoTile? video;
            lock (_tilesLock)
            {
                video = _byId.GetValueOrDefault(tile);
            }
            if (video is not null)
            {
                video.OnAction(in action);
            }
            else if (action.Kind == NvActionKind.Decode)
            {
                core.DecodeFailed(tile, action.Generation, NvDecodeFailure.NotConfigured);
            }
        }
        finally
        {
            _gate.ExitReadLock();
        }
    }

    /// <summary>Where the tapped audio of a call goes (its transcription), by tile.</summary>
    public TapRouter Taps { get; } = new();

    public override void OnAudioTap(NvCore core, ulong tile, in NvAudioBlock block) => Taps.Deliver(tile, in block);

    /// <summary>Runs <paramref name="action"/> on the UI thread.</summary>
    public void Post(Action action) => _ = _ui.TryEnqueue(() => action());

    public override void OnEvent(NvCore core, ReadOnlySpan<byte> json)
    {
        if (Recordings is { } recordings)
        {
            RecordingsEvent? r = RecordingsEvent.Parse(json);
            PlaybackEvent? p = r is null ? PlaybackEvent.Parse(json) : null;
            if (r is not null)
            {
                _ = _ui.TryEnqueue(() => recordings.Apply(r));
            }
            else if (p is not null)
            {
                _ = _ui.TryEnqueue(() => recordings.Apply(p));
            }
        }
        CoreEvent? e = CoreEvent.Parse(json);
        if (e is null || _sink is not { } sink)
        {
            return;
        }
        _ = _ui.TryEnqueue(() => sink(e));
    }

    public override void OnCallbackException(Exception exception) =>
        AppLog.Limited("callback", 10, $"a core callback threw: {exception.GetType().Name}: {exception.Message}");

    /// <summary>Closes every tile, frees the core, releases the device. Stop audio and rendering first.</summary>
    public void Dispose()
    {
        ulong[] ids;
        lock (_tilesLock)
        {
            ids = [.. _byId.Keys];
        }
        foreach (ulong id in ids)
        {
            CloseTile(id);
        }
        Core.Dispose();
        Gpu.Dispose();
        _gate.Dispose();
        FrameGate.Dispose();
    }
}
