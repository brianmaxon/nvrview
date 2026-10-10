// What the window shows, kept on the UI thread: the devices with their
// connection state, and one tile per connected channel (sub stream in the
// grid), with each tile's status, tier, listening state, and detections.
// The render thread reads the tile list through an immutable snapshot
// (Tiles) that is replaced, never changed, when tiles come and go.

using System.Diagnostics;
using NvrView.Interop;
using NvrView.Tiles;
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

/// <summary>One tile of the grid.</summary>
internal sealed class StageTile(ulong id, DeviceEntry device, byte channel, int lens, VideoTile video)
{
    public ulong Id { get; } = id;

    public DeviceEntry Device { get; } = device;

    public byte Channel { get; } = channel;

    /// <summary>0, or 1 for the second lens of a multi-lens camera (shown through the mobile and extern tiers).</summary>
    public int Lens { get; } = lens;

    /// <summary>The camera this tile shows (device key, channel, lens): the key alerts, indicators, and tiers use.</summary>
    public CameraRef Camera { get; } = new(device.Config.Key, channel, lens);

    public VideoTile Video { get; } = video;

    public string Name { get; set; } = lens > 0 ? $"Channel {channel + 1} · lens {lens + 1}" : $"Channel {channel + 1}";

    // UI thread.
    public string? Waiting { get; set; }

    public string? Error { get; set; }

    public bool Audible { get; set; }

    /// <summary>The tier last asked of the core for this tile.</summary>
    public StreamTier RequestedTier { get; set; } = StreamTier.Sub;

    /// <summary>The tier the tile was opened on (a rejected mobile start reopens it on extern).</summary>
    public StreamTier OpenedTier { get; set; } = StreamTier.Sub;

    public string ShownTier { get; set; } = "sub";

    public long TierFailures { get; set; }

    /// <summary>The tile's quality choice (docs/UX.md "Stream quality per tile"); a layout leaf stores it as its "quality" member.</summary>
    public TileQuality Quality { get; set; } = TileQuality.Auto;

    /// <summary>Whether the tile is focused (fills the stage); part of what it asks of its camera's stream.</summary>
    public bool Focused { get; set; }

    /// <summary>The zoom last reported to the tier planner (Auto switches to the main stream past 2x).</summary>
    public float DemandZoom { get; set; } = 1;

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

/// <summary>A clock that reads another one, which can be replaced.</summary>
internal sealed class SwitchableClock : IViewerClock
{
    public IViewerClock Inner { get; set; } = new SystemViewerClock();

    public double Now => Inner.Now;
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
    private int _alertsLogged;

    public Viewer(CoreHost host, ICredentialStore credentials, DeviceStore store, ViewSettings? settings = null)
    {
        _host = host;
        _credentials = credentials;
        _store = store;
        Settings = settings ?? new ViewSettings(null);
        host.SetSink(Apply);
        host.Tiers.HasMiddleTier = HasMiddleTier;
    }

    /// <summary>View preferences (the indicator corner).</summary>
    public ViewSettings Settings { get; }

    /// <summary>The tier planner every window shares (quality, focus, zoom, second lenses).</summary>
    public TierPlanner Planner => _host.Tiers;

    private readonly SwitchableClock _clock = new();

    /// <summary>Time for the indicator hold and the full screen auto-hide; <see cref="UseClock"/> swaps it (the self-test).</summary>
    public IViewerClock Clock => _clock;

    /// <summary>Replaces the clock everything on <see cref="Clock"/> reads.</summary>
    public void UseClock(IViewerClock clock) => _clock.Inner = clock;

    /// <summary>The wall clock the alert header's times come from.</summary>
    public Func<DateTime> WallClock { get; set; } = () => DateTime.Now;

    /// <summary>The core's clock in nanoseconds (a detection's at_ns is on it).</summary>
    public Func<long> CoreNowNs { get; set; } = () => (long)NvCore.ClockNowNs();

    /// <summary>Whether the system's "Show animations" is on; the alert border blinks only then.</summary>
    public Func<bool> AnimationsEnabled { get; set; } = SystemAnimationsEnabled;

    /// <summary>Active detections per camera (sound shown on every lens).</summary>
    public DetectionTracker Detections { get; } = new();

    /// <summary>What every tile and sidebar row shows, with the 2 s hold; read it on the UI thread.</summary>
    public IndicatorBoard Board { get; } = new();

    /// <summary>The session's alerts and their log.</summary>
    public AlertCenter Alerts { get; } = new();

    /// <summary>Raised on the UI thread when what the indicators show changes (events and the slow tick).</summary>
    public event Action? IndicatorsChanged;

    /// <summary>Alert raises, repeats, and dismissals so far (self-test).</summary>
    public int AlertChanges { get; private set; }

    private static Windows.UI.ViewManagement.UISettings? s_uiSettings;

    private static bool SystemAnimationsEnabled()
    {
        try
        {
            return (s_uiSettings ??= new Windows.UI.ViewManagement.UISettings()).AnimationsEnabled;
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or InvalidOperationException)
        {
            return true;
        }
    }

    /// <summary>Whether Balanced is offered: a single-lens channel whose tiers list extern.</summary>
    private bool HasMiddleTier(CameraRef camera) =>
        camera.Lens == 0 && _devices.FirstOrDefault(d => d.Config.Key == camera.Device) is { } d &&
        d.Channels.TryGetValue((byte)camera.Channel, out ChannelInfo? c) && c.Lenses < 2 && c.Tiers.Contains("extern");

    public bool OffersBalanced(StageTile tile) => Planner.OffersBalanced(tile.Camera);

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
                    if (d.State != "connected")
                    {
                        // A lost session says nothing about what is still going on.
                        Detections.Clear(d.Config.Key);
                        Alerts.DeviceLost(d.Config.Key);
                        UpdateBoard();
                    }
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
                    if (Planner.StartFailed(failed.Camera, failed.OpenedTier, e.Reason))
                    {
                        // The device refuses this lens's mobile stream: extern for the rest of the run.
                        AppLog.Info($"second lens of channel {failed.Channel + 1}: mobile refused, reopening on extern");
                        ReopenTile(failed);
                        break;
                    }
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
                        if (Planner.TierFailed(tier.Camera, CoreHost.ParseTier(e.Tier), e.Reason))
                        {
                            ApplyTier(tier.Camera);
                        }
                        else if (e.Reason != "superseded")
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
        var detection = new DetectionEvent(
            device.Config.Key, e.Channel, kind, e.State == "start", e.Source ?? DetectionSource.Device, e.AtNs, e.Lens, e.Type == "alarm");
        Detections.Apply(detection);
        if (Alerts.Apply(detection, HeardAt(e.AtNs)))
        {
            AlertChanges++;
            LogAlerts();
        }
        UpdateBoard();
    }

    /// <summary>When a detection was first heard, in local time (a sound's start arrives seconds after it).</summary>
    private DateTime HeardAt(long atNs)
    {
        DateTime now = WallClock();
        long ago = atNs > 0 ? CoreNowNs() - atNs : 0;
        return ago > 0 && ago < 3_600_000_000_000 ? now - TimeSpan.FromTicks(ago / 100) : now;
    }

    private void LogAlerts()
    {
        for (; _alertsLogged < Alerts.Log.Count; _alertsLogged++)
        {
            AppLog.Info(Alerts.Log[_alertsLogged].Text);
        }
    }

    private void UpdateBoard()
    {
        if (Board.Update(Detections, Clock.Now))
        {
            IndicatorsChanged?.Invoke();
        }
    }

    /// <summary>The user dismissed a camera's alert: gone for every tile of that camera.</summary>
    public void DismissAlert(CameraRef camera)
    {
        if (Alerts.Dismiss(camera, WallClock()) is not null)
        {
            AlertChanges++;
            LogAlerts();
            Changed?.Invoke();
        }
    }

    /// <summary>
    /// The slow tick (a few times a second, never per frame): ends held indicators, and reports
    /// zoom changes to the tier planner (Auto switches to the main stream past 2x).
    /// </summary>
    public void Tick()
    {
        UpdateBoard();
        foreach (StageTile t in _tiles)
        {
            if (t.Zoom != t.DemandZoom)
            {
                UpdateDemand(t);
            }
        }
    }

    private void OpenTilesFor(DeviceEntry device)
    {
        bool added = false;
        foreach (ChannelInfo c in device.Channels.Values.OrderBy(c => c.Channel))
        {
            if (!c.Connected || _tiles.Any(t => t.Device == device && t.Channel == c.Channel && t.Lens == 0))
            {
                continue;
            }
            if (_tiles.Length >= MaxTiles)
            {
                AppLog.Limited("max-tiles", 60, $"more than {MaxTiles} channels; the rest are not shown");
                break;
            }
            if (CreateTile(device, c.Channel, 0, c.Name) is not null)
            {
                added = true;
            }
        }
        if (added)
        {
            Changed?.Invoke();
        }
    }

    /// <summary>
    /// Opens a tile for a camera on the tier it should show now (the low one in a grid: sub, or a
    /// second lens's mobile; never the high one first) and adds it to the grid order. Null when
    /// the core refuses. The caller raises <see cref="Changed"/> (or use <see cref="OpenCameraTile"/>).
    /// </summary>
    private StageTile? CreateTile(DeviceEntry device, byte channel, int lens, string? name)
    {
        var camera = new CameraRef(device.Config.Key, channel, lens);
        StreamTier tier = Planner.WantedTier(camera);
        try
        {
            (ulong id, VideoTile video) = _host.OpenTile(device.Id, channel, CoreHost.ToCore(tier));
            var tile = new StageTile(id, device, channel, lens, video) { RequestedTier = tier, OpenedTier = tier, ShownTier = TierName(tier) };
            if (name is { Length: > 0 } n)
            {
                tile.Name = lens > 0 ? $"{n} · lens {lens + 1}" : n;
            }
            _tilesById[id] = tile;
            // Grid order: device by device (in the order added), channel by channel, lens by lens.
            StageTile[] next = [.. _tiles, tile];
            Array.Sort(next, CompareGridOrder);
            Volatile.Write(ref _tiles, next);
            UpdateDemand(tile);
            return tile;
        }
        catch (NvException ex)
        {
            AppLog.Warn($"opening channel {channel} failed: {ex.Status}");
            return null;
        }
    }

    private int CompareGridOrder(StageTile a, StageTile b)
    {
        int da = _devices.IndexOf(a.Device), db = _devices.IndexOf(b.Device);
        if (da != db)
        {
            return da.CompareTo(db);
        }
        return a.Channel != b.Channel ? a.Channel.CompareTo(b.Channel) : a.Lens.CompareTo(b.Lens);
    }

    /// <summary>Opens a tile for one lens of a channel (the second lens of a multi-lens camera is lens 1).</summary>
    public StageTile? OpenCameraTile(DeviceEntry device, byte channel, int lens)
    {
        StageTile? tile = CreateTile(device, channel, lens, device.Channels.GetValueOrDefault(channel)?.Name);
        if (tile is not null)
        {
            Changed?.Invoke();
        }
        return tile;
    }

    /// <summary>Closes a tile and opens its camera again on the tier it should use now, keeping its choices.</summary>
    private void ReopenTile(StageTile old)
    {
        _host.CloseTile(old.Id);
        _tilesById.Remove(old.Id);
        Planner.ClearDemands(old);
        StageTile[] without = [.. _tiles.Where(t => t != old)];
        Volatile.Write(ref _tiles, without);
        StageTile? neu = CreateTile(old.Device, old.Channel, old.Lens, null);
        if (neu is null)
        {
            old.Error = "Could not reopen the stream";
            Volatile.Write(ref _tiles, [.. without, old]);
            Changed?.Invoke();
            return;
        }
        neu.Name = old.Name;
        neu.Quality = old.Quality;
        neu.Focused = old.Focused;
        neu.Zoom = old.Zoom;
        neu.CenterX = old.CenterX;
        neu.CenterY = old.CenterY;
        UpdateDemand(neu);
        if (old.Audible)
        {
            SetListening(neu, true);
        }
        Changed?.Invoke();
    }

    private static string TierName(StreamTier tier) => tier switch
    {
        StreamTier.Main => "main",
        StreamTier.Mobile => "mobile",
        StreamTier.Extern => "extern",
        _ => "sub",
    };

    /// <summary>Closes one tile (a lens tile opened with <see cref="OpenCameraTile"/>).</summary>
    public void CloseCameraTile(StageTile tile)
    {
        CloseTile(tile);
        Changed?.Invoke();
    }

    private void CloseTile(StageTile tile)
    {
        Volatile.Write(ref _tiles, [.. _tiles.Where(t => t != tile)]);
        _tilesById.Remove(tile.Id);
        _host.CloseTile(tile.Id);
        if (Planner.ClearDemands(tile) is { } camera)
        {
            ApplyTier(camera);
        }
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

    /// <summary>
    /// A tile's quality choice (Auto, Clear, Fluent, Balanced): the tile asks its camera for the
    /// matching stream; the highest ask across every tile of the camera wins. A layout leaf keeps
    /// the choice as its "quality" member (<see cref="TileQualities.ToMember"/>).
    /// </summary>
    public void SetQuality(StageTile tile, TileQuality quality)
    {
        if (tile.Quality == quality)
        {
            return;
        }
        tile.Quality = quality;
        UpdateDemand(tile);
        Changed?.Invoke();
    }

    /// <summary>A tile fills the stage (or stops): Auto asks for the main stream while it does.</summary>
    public void SetFocused(StageTile tile, bool focused)
    {
        if (tile.Focused == focused)
        {
            return;
        }
        tile.Focused = focused;
        UpdateDemand(tile);
    }

    /// <summary>States what the tile asks of its camera (quality, focus, zoom) and carries out any tier switch that follows.</summary>
    public void UpdateDemand(StageTile tile)
    {
        tile.DemandZoom = tile.Zoom;
        foreach (CameraRef camera in Planner.SetDemand(tile, tile.Camera, tile.Quality, tile.Focused, tile.Zoom))
        {
            ApplyTier(camera);
        }
    }

    /// <summary>Switches every open tile of a camera to the tier it should show now (nothing when it already asks for it).</summary>
    private void ApplyTier(CameraRef camera)
    {
        StreamTier wanted = Planner.WantedTier(camera);
        foreach (StageTile tile in _tiles.Where(t => t.Camera == camera && t.RequestedTier != wanted))
        {
            try
            {
                Core.SetTier(tile.Id, CoreHost.ToCore(wanted));
                tile.RequestedTier = wanted;
            }
            catch (NvException e)
            {
                AppLog.Warn($"tier switch on tile {tile.Id} failed: {e.Status}");
            }
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
