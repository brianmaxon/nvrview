// What the window shows, kept on the UI thread: the devices with their
// connection state, and one tile per connected channel (sub stream in the
// grid), with each tile's status, tier, listening state, and detections.
// The render thread reads the tile list through an immutable snapshot
// (Tiles) that is replaced, never changed, when tiles come and go.

using System.Diagnostics;
using NvrView.Interop;
using NvrView.Video;

namespace NvrView.App;

/// <summary>A device in the core.</summary>
internal sealed class DeviceEntry(DeviceConfig config, ulong id)
{
    public DeviceConfig Config { get; } = config;

    public ulong Id { get; } = id;

    /// <summary>connecting, connected, backoff, login_failed, closed.</summary>
    public string State { get; set; } = "connecting";

    public int Attempt { get; set; }

    /// <summary>Stopwatch timestamp of the next attempt while in backoff.</summary>
    public long RetryAt { get; set; }

    public int LoginStatus { get; set; }

    public string? Model { get; set; }

    public string? DeviceName { get; set; }

    public Dictionary<byte, ChannelInfo> Channels { get; } = [];

    public string DisplayName => Config.Name.Length > 0 ? Config.Name : DeviceName ?? "Device";
}

/// <summary>An active detection on a tile.</summary>
internal readonly record struct Detection(string Kind, string Source, int Lens);

/// <summary>One tile of the grid.</summary>
internal sealed class StageTile(ulong id, DeviceEntry device, byte channel, VideoTile video)
{
    public ulong Id { get; } = id;

    public DeviceEntry Device { get; } = device;

    public byte Channel { get; } = channel;

    public VideoTile Video { get; } = video;

    public string Name { get; set; } = $"Channel {channel + 1}";

    // UI thread.
    public string? Waiting { get; set; }

    public string? Error { get; set; }

    public bool Audible { get; set; }

    public string RequestedTier { get; set; } = "sub";

    public string ShownTier { get; set; } = "sub";

    public long TierFailures { get; set; }

    public List<Detection> Detections { get; } = [];

    public long LastReceived { get; set; }

    public double ReceivedRate { get; set; }

    public long LastStatsAt { get; set; }

    /// <summary>The latest audio_stats event (JSON), for diagnostics.</summary>
    public string? AudioStats { get; set; }

    // Digital zoom: written by the UI thread, read by the render thread.
    // Zoom 1..8; the center in visible-picture coordinates 0..1, so it is
    // the same region whatever the stream's resolution.
    public volatile float Zoom = 1;
    public volatile float CenterX = 0.5f;
    public volatile float CenterY = 0.5f;

    // Written by the render thread each frame, read by the UI thread: where
    // the tile is drawn, in panel units (DIPs), and its picture size.
    public volatile float X;
    public volatile float Y;
    public volatile float Width;
    public volatile float Height;
    public volatile bool Visible;
    public volatile int PictureWidth;
    public volatile int PictureHeight;

    // Render thread: distinct pictures drawn.
    public long Shown;
    public ulong LastSequence;
}

/// <summary>The viewer's state (see the file comment). UI thread only, except Tiles.</summary>
internal sealed class Viewer
{
    /// <summary>The most tiles one window shows (W2 has no layouts yet).</summary>
    public const int MaxTiles = 36;

    private readonly CoreHost _host;
    private readonly ICredentialStore _credentials;
    private readonly DeviceStore _store;
    private readonly List<DeviceEntry> _devices = [];
    private readonly Dictionary<ulong, StageTile> _tilesById = [];
    private StageTile[] _tiles = [];

    public Viewer(CoreHost host, ICredentialStore credentials, DeviceStore store)
    {
        _host = host;
        _credentials = credentials;
        _store = store;
        host.SetSink(Apply);
    }

    /// <summary>The tiles in grid order; an immutable snapshot, any thread.</summary>
    public StageTile[] Tiles => Volatile.Read(ref _tiles);

    public IReadOnlyList<DeviceEntry> Devices => _devices;

    public NvCore Core => _host.Core;

    public CoreHost Host => _host;

    /// <summary>Raised on the UI thread when tiles, statuses, or detections change.</summary>
    public event Action? Changed;

    /// <summary>Every event seen, for the self-test (type, tile, state, generation).</summary>
    public List<(string Type, ulong Tile, string? State, long Generation)> TierEvents { get; } = [];

    public List<string> Problems { get; } = [];

    /// <summary>Detection, motion, and alarm events seen (self-test).</summary>
    public int DetectionEvents { get; private set; }

    /// <summary>Tiles that reported tile_waiting (self-test).</summary>
    public HashSet<ulong> WaitedTiles { get; } = [];

    public Dictionary<string, long> Timings { get; } = [];

    /// <summary>The last tier switch's alignment (timing event): new picture minus old, ms.</summary>
    public double? TierSwitchDeltaMs { get; private set; }

    /// <summary>How long the last tier switch waited for alignment, ms.</summary>
    public double? TierSwitchHeldMs { get; private set; }

    public ICredentialStore Credentials => _credentials;

    /// <summary>Adds every device saved in settings, with its password from the credential store.</summary>
    public void LoadSavedDevices()
    {
        foreach (DeviceConfig config in _store.Load())
        {
            string? password = _credentials.Read(config.Key);
            if (password is null)
            {
                AppLog.Warn($"no saved password for device {config.Key}; open Settings to enter it");
                _devices.Add(new DeviceEntry(config, 0) { State = "no_password" });
                continue;
            }
            Connect(config, password);
        }
        Changed?.Invoke();
    }

    public IEnumerable<DeviceConfig> SavedConfigs => _devices.Select(d => d.Config);

    /// <summary>Adds a device to the core (connecting starts in the background).</summary>
    public DeviceEntry Connect(DeviceConfig config, string password)
    {
        ulong id = 0;
        string state = "connecting";
        try
        {
            id = Core.AddDevice(config.Host, config.Port, config.User, password);
        }
        catch (NvException e)
        {
            AppLog.Warn($"adding device {config.Key} failed: {e.Status}");
            state = "invalid";
        }
        var entry = new DeviceEntry(config, id) { State = state };
        int existing = _devices.FindIndex(d => d.Config.Key == config.Key);
        if (existing >= 0)
        {
            _devices[existing] = entry;
        }
        else
        {
            _devices.Add(entry);
        }
        return entry;
    }

    /// <summary>Removes a device from the core and closes its tiles (not from settings).</summary>
    public void Disconnect(DeviceEntry device)
    {
        foreach (StageTile t in _tiles.Where(t => t.Device == device).ToArray())
        {
            CloseTile(t);
        }
        if (device.Id != 0)
        {
            try
            {
                Core.RemoveDevice(device.Id);
            }
            catch (NvException)
            {
            }
        }
        _devices.Remove(device);
        Changed?.Invoke();
    }

    /// <summary>Saves, connects, or reconnects a device from the Settings dialog.</summary>
    public void SaveDevice(DeviceConfig config, string? newPassword)
    {
        DeviceEntry? old = _devices.FirstOrDefault(d => d.Config.Key == config.Key);
        if (newPassword is { Length: > 0 })
        {
            _credentials.Write(config.Key, config.User, newPassword);
        }
        string? password = newPassword is { Length: > 0 } ? newPassword : _credentials.Read(config.Key);
        if (old is not null)
        {
            Disconnect(old);
        }
        if (password is null)
        {
            _devices.Add(new DeviceEntry(config, 0) { State = "no_password" });
        }
        else
        {
            Connect(config, password);
        }
        _store.Save(_devices.Select(d => d.Config));
        Changed?.Invoke();
    }

    /// <summary>Removes a device from the core, settings, and the credential store.</summary>
    public void ForgetDevice(string key)
    {
        if (_devices.FirstOrDefault(d => d.Config.Key == key) is { } device)
        {
            Disconnect(device);
        }
        _credentials.Delete(key);
        _store.Save(_devices.Select(d => d.Config));
        Changed?.Invoke();
    }

    /// <summary>Applies one core event (UI thread).</summary>
    public void Apply(CoreEvent e)
    {
        switch (e.Type)
        {
            case "device_state":
                if (FindDevice(e.Device) is { } d)
                {
                    d.State = e.State ?? d.State;
                    d.Attempt = e.Attempt;
                    d.LoginStatus = e.Status;
                    d.RetryAt = Stopwatch.GetTimestamp() + e.RetryMs * Stopwatch.Frequency / 1000;
                    if (e.State == "login_failed")
                    {
                        AppLog.Warn($"device {d.Config.Key}: login failed (status {e.Status})");
                    }
                }
                break;
            case "device_info":
                if (FindDevice(e.Device) is { } info)
                {
                    info.DeviceName = e.Name;
                    info.Model = e.Model;
                    if (e.Key is { } key && info.Config.CoreKey != key)
                    {
                        info.Config.CoreKey = key;
                        _store.Save(_devices.Select(x => x.Config));
                    }
                }
                break;
            case "channels":
                if (FindDevice(e.Device) is { } dev && e.Channels is { } channels)
                {
                    foreach (ChannelInfo c in channels)
                    {
                        if (dev.Channels.TryGetValue(c.Channel, out ChannelInfo? before) && c.Name is null)
                        {
                            c.Name = before.Name;
                        }
                        dev.Channels[c.Channel] = c;
                    }
                    OpenTilesFor(dev);
                    foreach (StageTile t in _tiles.Where(t => t.Device == dev))
                    {
                        if (dev.Channels.TryGetValue(t.Channel, out ChannelInfo? ci) && ci.Name is { Length: > 0 } n)
                        {
                            t.Name = n;
                        }
                    }
                }
                break;
            case "tile_waiting":
                if (e.State == "waiting")
                {
                    WaitedTiles.Add(e.Tile);
                }
                if (FindTile(e.Tile) is { } waiting)
                {
                    waiting.Waiting = e.State == "waiting" ? "Waiting for the camera" : null;
                }
                break;
            case "tile_error":
                if (FindTile(e.Tile) is { } failed)
                {
                    failed.Waiting = null;
                    failed.Error = e.Reason == "start_rejected"
                        ? $"The device refused this stream (status {e.Status})"
                        : $"Stream error: {e.Reason}";
                    Problems.Add($"tile_error tile {e.Tile}: {e.Reason} {e.Status}");
                }
                break;
            case "tile_tier":
                TierEvents.Add((e.Type, e.Tile, e.State, e.Generation));
                if (FindTile(e.Tile) is { } tier)
                {
                    if (e.State == "active")
                    {
                        tier.ShownTier = e.Tier ?? tier.ShownTier;
                    }
                    else if (e.State == "failed")
                    {
                        tier.TierFailures++;
                        if (e.Reason != "superseded")
                        {
                            Problems.Add($"tile_tier failed tile {e.Tile}: {e.Reason}");
                        }
                    }
                }
                break;
            case "tile_stats":
                if (FindTile(e.Tile) is { } stats)
                {
                    long now = Stopwatch.GetTimestamp();
                    if (stats.LastStatsAt != 0 && e.Received >= stats.LastReceived)
                    {
                        double dt = (now - stats.LastStatsAt) / (double)Stopwatch.Frequency;
                        stats.ReceivedRate = dt > 0 ? (e.Received - stats.LastReceived) / dt : 0;
                    }
                    stats.LastReceived = e.Received;
                    stats.LastStatsAt = now;
                }
                return; // no visible change worth a refresh
            case "audio_stats":
                if (FindTile(e.Tile) is { } audio)
                {
                    audio.AudioStats = e.Raw;
                }
                return;
            case "tile_closed":
                if (e.Reason != "closed" && FindTile(e.Tile) is { } closed)
                {
                    closed.Error = $"Closed ({e.Reason})";
                }
                break;
            case "detection":
            case "motion":
            case "alarm":
                DetectionEvents++;
                ApplyDetection(e);
                break;
            case "timing":
                if (e.What is { } what)
                {
                    Timings[what] = e.Ns;
                    if (what == "tier_switch")
                    {
                        TierSwitchDeltaMs = e.DeltaMs;
                        TierSwitchHeldMs = e.HeldMs;
                    }
                }
                return;
            default:
                return;
        }
        Changed?.Invoke();
    }

    private void ApplyDetection(CoreEvent e)
    {
        DeviceEntry? device = FindDevice(e.Device);
        if (device is null || e.Channel < 0)
        {
            return;
        }
        string kind = e.Type == "motion" ? "motion" : e.Kind ?? e.Type;
        string source = e.Source ?? "device";
        foreach (StageTile t in _tiles)
        {
            if (t.Device != device || t.Channel != e.Channel)
            {
                continue;
            }
            t.Detections.RemoveAll(x => x.Kind == kind && x.Source == source && x.Lens == e.Lens);
            if (e.State == "start")
            {
                t.Detections.Add(new Detection(kind, source, e.Lens));
            }
        }
    }

    private void OpenTilesFor(DeviceEntry device)
    {
        bool added = false;
        foreach (ChannelInfo c in device.Channels.Values.OrderBy(c => c.Channel))
        {
            if (!c.Connected || _tiles.Any(t => t.Device == device && t.Channel == c.Channel))
            {
                continue;
            }
            if (_tiles.Length >= MaxTiles)
            {
                AppLog.Limited("max-tiles", 60, $"more than {MaxTiles} channels; the rest are not shown");
                break;
            }
            try
            {
                (ulong id, VideoTile video) = _host.OpenTile(device.Id, c.Channel, NvStreamTier.Sub);
                var tile = new StageTile(id, device, c.Channel, video);
                if (c.Name is { Length: > 0 } n)
                {
                    tile.Name = n;
                }
                _tilesById[id] = tile;
                // Grid order: device by device (in the order added), channel by channel.
                StageTile[] next = [.. _tiles, tile];
                Array.Sort(next, (a, b) =>
                {
                    int da = _devices.IndexOf(a.Device), db = _devices.IndexOf(b.Device);
                    return da != db ? da.CompareTo(db) : a.Channel.CompareTo(b.Channel);
                });
                Volatile.Write(ref _tiles, next);
                added = true;
            }
            catch (NvException ex)
            {
                AppLog.Warn($"opening channel {c.Channel} failed: {ex.Status}");
            }
        }
        if (added)
        {
            Changed?.Invoke();
        }
    }

    private void CloseTile(StageTile tile)
    {
        Volatile.Write(ref _tiles, [.. _tiles.Where(t => t != tile)]);
        _tilesById.Remove(tile.Id);
        _host.CloseTile(tile.Id);
    }

    /// <summary>Makes a tile audible or silent; tiles start silent.</summary>
    public void SetListening(StageTile tile, bool audible)
    {
        if (tile.Audible == audible)
        {
            return;
        }
        try
        {
            Core.SetTileAudio(tile.Id, audible, 1);
            tile.Audible = audible;
        }
        catch (NvException e)
        {
            AppLog.Warn($"listening on tile {tile.Id} failed: {e.Status}");
        }
        Changed?.Invoke();
    }

    /// <summary>
    /// Click on a speaker: listen to this tile alone, or also to it when
    /// <paramref name="additive"/> (Ctrl); a second click mutes it.
    /// </summary>
    public void ToggleListening(StageTile tile, bool additive)
    {
        if (tile.Audible)
        {
            SetListening(tile, false);
            return;
        }
        if (!additive)
        {
            foreach (StageTile other in _tiles)
            {
                if (other != tile && other.Audible)
                {
                    SetListening(other, false);
                }
            }
        }
        SetListening(tile, true);
    }

    /// <summary>Asks a tile for a stream tier (main when focused, sub in the grid).</summary>
    public void RequestTier(StageTile tile, NvStreamTier tier)
    {
        string name = tier == NvStreamTier.Main ? "main" : "sub";
        if (tile.RequestedTier == name)
        {
            return;
        }
        try
        {
            Core.SetTier(tile.Id, tier);
            tile.RequestedTier = name;
        }
        catch (NvException e)
        {
            AppLog.Warn($"tier switch on tile {tile.Id} failed: {e.Status}");
        }
    }

    public StageTile? FindTile(ulong id) => _tilesById.GetValueOrDefault(id);

    private DeviceEntry? FindDevice(ulong id) => id == 0 ? null : _devices.FirstOrDefault(d => d.Id == id);

    /// <summary>Closes every tile and removes every device (shutdown).</summary>
    public void CloseAll()
    {
        foreach (StageTile t in _tiles)
        {
            _host.CloseTile(t.Id);
        }
        Volatile.Write(ref _tiles, []);
        _tilesById.Clear();
        foreach (DeviceEntry d in _devices)
        {
            if (d.Id != 0)
            {
                try
                {
                    Core.RemoveDevice(d.Id);
                }
                catch (NvException)
                {
                }
            }
        }
    }
}
