// The app's state, shared by every window and kept on the UI thread: the
// devices with their connection state and channels, one core tile per camera
// shown anywhere (leased by the windows' layouts, NvrView.Shell.TileLeases),
// the stream tier each camera runs at (one TierPlanner for every window: the
// highest ask wins), the layouts (stored by the core), the camera groups, who
// is listening, the detections with their corner indicators and alerts.
//
// Each window has its own arrangement (ViewerWindow, StageView); its tiles
// (StageTile) point at the shared CameraTile of their camera, so a camera in
// several tiles and windows is streamed once. Cameras are named by the
// device's stable key (the core's, nv_device_key), as layouts name them.

using System.Diagnostics;
using NvrView.Interop;
using NvrView.Shell;
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

    /// <summary>The name layouts, groups, indicators, and tiers give the device's cameras: the core's stable key once known, else the app's.</summary>
    public string Key => Config.CoreKey ?? Config.Key;
}

/// <summary>One core tile: a camera shown in one or more tiles of one or more windows.</summary>
internal sealed class CameraTile(ulong id, DeviceEntry device, CameraRef camera, VideoTile video)
{
    public ulong Id { get; } = id;

    public DeviceEntry Device { get; } = device;

    public CameraRef Camera { get; } = camera;

    public byte Channel => (byte)Camera.Channel;

    public int Lens => Camera.Lens;

    public VideoTile Video { get; } = video;

    // UI thread.
    public string? Waiting { get; set; }

    /// <summary>The generation whose stream <see cref="Waiting"/> is about: a wait ends with its
    /// generation, which a tier switch may replace without a <c>streaming</c> event.</summary>
    public long WaitingGeneration { get; set; }

    public string? Error { get; set; }

    public bool Audible { get; set; }

    /// <summary>The tier last asked of the core for this tile.</summary>
    public StreamTier RequestedTier { get; set; } = StreamTier.Sub;

    /// <summary>The tier the tile was opened on (a rejected mobile start reopens it on extern).</summary>
    public StreamTier OpenedTier { get; set; } = StreamTier.Sub;

    public string ShownTier { get; set; } = "sub";

    public long TierFailures { get; set; }

    public long LastReceived { get; set; }

    public double ReceivedRate { get; set; }

    public long LastStatsAt { get; set; }

    /// <summary>The latest audio_stats event (JSON), for diagnostics.</summary>
    public string? AudioStats { get; set; }
}

/// <summary>
/// One tile of one window: a leaf of its layout, showing a camera (whose
/// shared core tile may be open or not yet) or nothing. Camera state reads
/// through to the shared <see cref="CameraTile"/>; placement, zoom, quality,
/// focus, and draw counters are this tile's own. It is also what the tier
/// planner keys this tile's demand by.
/// </summary>
internal sealed class StageTile(Viewer viewer, NodePath path, CameraRef? camera, TileQuality quality)
{
    public NodePath Path { get; } = path;

    /// <summary>The camera, or null for an empty tile.</summary>
    public CameraRef? CameraOrNone { get; } = camera;

    /// <summary>The camera (an empty tile's is a placeholder no device has).</summary>
    public CameraRef Camera => CameraOrNone ?? new CameraRef("", -1);

    public bool IsEmpty => CameraOrNone is null;

    private CameraTile? _shared;

    /// <summary>The shared core tile, while it is open (set on the UI thread when the leases open or close it; read by the render thread).</summary>
    public CameraTile? Shared
    {
        get => Volatile.Read(ref _shared);
        set => Volatile.Write(ref _shared, value);
    }

    /// <summary>The core tile's ID, 0 while none is open.</summary>
    public ulong Id => Shared?.Id ?? 0;

    public DeviceEntry? Device => CameraOrNone is { } c ? viewer.FindDevice(c.Device) : null;

    public byte Channel => (byte)Math.Max(Camera.Channel, 0);

    public int Lens => Camera.Lens;

    public VideoTile? Video => Shared?.Video;

    public string Name => CameraOrNone is { } c ? viewer.CameraName(c) : "Empty tile";

    public string? Waiting => Shared?.Waiting;

    public string? Error => Shared?.Error;

    public bool Audible => CameraOrNone is { } c && viewer.IsAudible(c);

    public string ShownTier => Shared?.ShownTier ?? "";

    public StreamTier RequestedTier => Shared?.RequestedTier ?? StreamTier.Sub;

    public StreamTier OpenedTier => Shared?.OpenedTier ?? StreamTier.Sub;

    public string? AudioStats => Shared?.AudioStats;

    public double ReceivedRate => Shared?.ReceivedRate ?? 0;

    /// <summary>The tile's quality choice (docs/UX.md "Stream quality per tile"), kept in the layout leaf's "quality" member.</summary>
    public TileQuality Quality { get; set; } = quality;

    /// <summary>Whether the tile is focused (fills the stage); part of what it asks of its camera's stream.</summary>
    public bool Focused { get; set; }

    /// <summary>The zoom last reported to the tier planner (Auto switches to the main stream past 2x).</summary>
    public float DemandZoom { get; set; } = 1;

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

    /// <summary>The tile's saved fisheye view (UI thread; the layout leaf's "fisheye" member, or the view being dragged).</summary>
    public FisheyeTileSetting FisheyeSetting { get; set; } = FisheyeTileSetting.Original;

    private FisheyeChoice? _fisheye;

    /// <summary>What the render thread dewarps with: null draws the picture as it is (Original, or not a fisheye camera).</summary>
    public FisheyeChoice? Fisheye
    {
        get => Volatile.Read(ref _fisheye);
        set => Volatile.Write(ref _fisheye, value);
    }

    // Render thread: the dewarp constants and what they were computed for.
    public object? DewarpKey;
    public DewarpConstants DewarpRows;
}

/// <summary>A dewarped tile's view, mount and aim (immutable; replaced when any changes).</summary>
internal sealed record FisheyeChoice(FisheyeViewMode Mode, FisheyeAim Aim, FisheyeMount Mount);

/// <summary>
/// Each fisheye camera's image circle, found from its first decoded pictures
/// (docs/RENDERING.md "Finding the circle") and kept for the run; shared by
/// every window. The render threads ask for tries and hand the luma over;
/// detection runs on the thread pool. Thread-safe.
/// </summary>
internal sealed class FisheyeCircles
{
    private readonly Lock _lock = new();
    private readonly Dictionary<CameraRef, FisheyeCircleTracker> _trackers = [];
    private readonly HashSet<CameraRef> _busy = [];

    /// <summary>The camera's circle once found (or fallen back to the inscribed one).</summary>
    public FisheyeCircle? Circle(CameraRef camera)
    {
        lock (_lock)
        {
            return _trackers.TryGetValue(camera, out FisheyeCircleTracker? t) ? t.Circle : null;
        }
    }

    /// <summary>Whether the camera's circle came from the fallback (no clear rim found).</summary>
    public bool FellBack(CameraRef camera)
    {
        lock (_lock)
        {
            return _trackers.TryGetValue(camera, out FisheyeCircleTracker? t) && t.FellBack;
        }
    }

    /// <summary>Claims a try at <paramref name="now"/> (seconds) if one is due and none is running.</summary>
    public bool TryBegin(CameraRef camera, double now)
    {
        lock (_lock)
        {
            if (_busy.Contains(camera))
            {
                return false;
            }
            if (!_trackers.TryGetValue(camera, out FisheyeCircleTracker? t))
            {
                t = new FisheyeCircleTracker();
                _trackers[camera] = t;
            }
            if (!t.WantsTry(now))
            {
                return false;
            }
            _busy.Add(camera);
            return true;
        }
    }

    /// <summary>Measures a luma plane on the thread pool and records the outcome of the try claimed with <see cref="TryBegin"/>.</summary>
    public void Measure(CameraRef camera, byte[]? luma, int width, int height, double now)
    {
        if (luma is null || width <= 0 || height <= 0)
        {
            Finish(camera, null, now, Math.Max(width, 1), Math.Max(height, 1));
            return;
        }
        _ = Task.Run(() =>
        {
            FisheyeCircle? found = null;
            try
            {
                found = FisheyeCircleDetector.Detect(width, height, (x, y) => luma[y * width + x]);
            }
            catch (Exception e)
            {
                AppLog.Limited("fisheye", 3, $"fisheye circle detection failed: {e.Message}");
            }
            Finish(camera, found, now, width, height);
        });
    }

    private void Finish(CameraRef camera, FisheyeCircle? found, double now, int width, int height)
    {
        lock (_lock)
        {
            _busy.Remove(camera);
            if (_trackers.TryGetValue(camera, out FisheyeCircleTracker? t))
            {
                bool had = t.Circle is not null;
                t.Record(found, now, width, height);
                if (!had && t.Circle is { } c)
                {
                    AppLog.Info(t.FellBack
                        ? $"fisheye: no clear image circle after {t.Tries} tries; using the inscribed circle"
                        : $"fisheye: image circle at ({c.Cx:F3}, {c.Cy:F3}), radius {c.R:F3} of the width");
                }
            }
        }
    }
}

/// <summary>A clock that reads another one, which can be replaced.</summary>
internal sealed class SwitchableClock : IViewerClock
{
    public IViewerClock Inner { get; set; } = new SystemViewerClock();

    public double Now => Inner.Now;
}

/// <summary>The viewer's state (see the file comment). UI thread only, except Tiles.</summary>
internal sealed class Viewer : ITileBackend<CameraTile>
{
    /// <summary>The most tiles one window shows (a layout's limit).</summary>
    public const int MaxTiles = LayoutNode.MaxLeaves;

    private readonly CoreHost _host;
    private readonly ICredentialStore _credentials;
    private readonly DeviceStore _store;
    private readonly GroupStore _groupStore;
    private readonly List<DeviceEntry> _devices = [];
    private readonly Dictionary<ulong, CameraTile> _tilesById = [];
    private readonly HashSet<CameraRef> _audible = [];
    private CameraTile[] _tiles = [];
    private int _alertsLogged;

    public Viewer(CoreHost host, ICredentialStore credentials, DeviceStore store, ViewSettings? settings = null, GroupStore? groups = null, CallServices? callServices = null)
    {
        _host = host;
        _credentials = credentials;
        _store = store;
        _groupStore = groups ?? new GroupStore(null);
        Settings = settings ?? new ViewSettings(null);
        Leases = new TileLeases<CameraTile>(this);
        Calls = new ViewerCalls(this, callServices ?? CallServices.Real());
        Groups = _groupStore.Load();
        host.SetSink(Apply);
        host.Tiers.HasMiddleTier = HasMiddleTier;
        Settings.Changed += () => Changed?.Invoke();
        ReloadLayouts();
    }

    /// <summary>View preferences (the indicator corner, hiding offline channels).</summary>
    public ViewSettings Settings { get; }

    /// <summary>The tier planner every window shares (quality, focus, zoom, second lenses).</summary>
    public TierPlanner Planner => _host.Tiers;

    /// <summary>One core tile per camera any window shows.</summary>
    public TileLeases<CameraTile> Leases { get; }

    /// <summary>The calls in progress: doorbell presses and the user's conversations (docs/UX.md "Doorbell calls").</summary>
    public ViewerCalls Calls { get; }

    /// <summary>Raised on the UI thread when a call opens, ends, or changes (captions, talk state).</summary>
    public event Action? CallsChanged;

    internal void RaiseCallsChanged() => CallsChanged?.Invoke();

    /// <summary>Allows or forbids talk-back to a camera (camera motion control; off by default): saved, and passed to the core at once.</summary>
    public void SetTalkAllowed(CameraRef camera, bool allowed)
    {
        Settings.SetTalkAllowed(camera, allowed);
        AppLog.Info($"Talk-back {(allowed ? "allowed" : "turned off")}: channel {camera.Channel + 1}");
        ApplyTalkPermission(camera);
    }

    /// <summary>Passes the user's talk-back setting for a camera to the core (it keeps it for this run only).</summary>
    public void ApplyTalkPermission(CameraRef camera)
    {
        if (FindDevice(camera.Device) is not { Id: not 0 } device || camera.Channel is < 0 or > 255)
        {
            return;
        }
        try
        {
            Core.TalkSetPermission(device.Id, (byte)camera.Channel, Settings.TalkAllowed(camera));
        }
        catch (NvException e)
        {
            AppLog.Warn($"talk-back permission for channel {camera.Channel + 1} failed: {e.Status}");
        }
    }

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
        camera.Lens == 0 && ChannelOf(camera) is { } c && c.Lenses < 2 && c.Tiers.Contains("extern");

    public bool OffersBalanced(StageTile tile) => !tile.IsEmpty && Planner.OffersBalanced(tile.Camera);

    /// <summary>The open core tiles; an immutable snapshot, any thread.</summary>
    public CameraTile[] Tiles => Volatile.Read(ref _tiles);

    public IReadOnlyList<DeviceEntry> Devices => _devices;

    public NvCore Core => _host.Core;

    public CoreHost Host => _host;

    /// <summary>Raised on the UI thread when devices, tiles, statuses, or detections change.</summary>
    public event Action? Changed;

    /// <summary>The core's update events (the updater listens).</summary>
    public event Action<NvrView.Shell.UpdateEvent>? UpdateReceived;

    /// <summary>Raised when the stored layouts changed (here or in another window or app).</summary>
    public event Action? LayoutsChanged;

    /// <summary>Raised when the groups changed.</summary>
    public event Action? GroupsChanged;

    /// <summary>Every tile_tier event seen, for the self-test (type, tile, state, generation).</summary>
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

    /// <summary>The sidebar lists only connected channels (a view setting, remembered).</summary>
    public bool HideOfflineChannels
    {
        get => Settings.HideOfflineChannels;
        set => Settings.HideOfflineChannels = value;
    }

    // ---- finding devices (docs/UX.md "Finding devices") ----

    /// <summary>The add-device screen's list of what answered a search.</summary>
    public DiscoveryList Discovery { get; } = new();

    /// <summary>The search request (JSON); null: the core's defaults, every interface. The self-test limits it to loopback.</summary>
    public string? DiscoveryRequest { get; set; }

    /// <summary>
    /// Starts a search of the local network (the core never searches on its
    /// own); one already running restarts. The list empties.
    /// </summary>
    public void StartDiscovery()
    {
        try
        {
            Core.DiscoverStart(DiscoveryRequest);
            Discovery.Requested();
        }
        catch (NvException e)
        {
            AppLog.Warn($"device search refused: {e.Status}");
            Discovery.FailedToStart();
        }
        Changed?.Invoke();
    }

    /// <summary>Stops the search (the screen closed or went away).</summary>
    public void StopDiscovery()
    {
        try
        {
            Core.DiscoverStop();
        }
        catch (NvException)
        {
            // Closing or already closed: nothing runs.
        }
        Discovery.Stopped();
        Changed?.Invoke();
    }

    /// <summary>Whether a found device is one the user already added.</summary>
    public bool IsAdded(DiscoveredDevice found) =>
        found.IsAdded(_devices.Select(d => (d.Config.Host, (int)d.Config.Port)));

    // ---- devices ----

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
        if (id != 0)
        {
            // Layouts and groups name the device by the core's stable key.
            try
            {
                string key = Core.DeviceKey(id);
                if (key.Length > 0 && key != entry.Key)
                {
                    AdoptCoreKey(entry, key);
                }
            }
            catch (NvException e)
            {
                AppLog.Warn($"device key for {config.Key}: {e.Status}");
            }
        }
        Changed?.Invoke();
        return entry;
    }

    /// <summary>
    /// The core names the device differently from what layouts and groups
    /// used so far (a first run with core keys, or a new address): rename its
    /// cameras everywhere the app can.
    /// </summary>
    private void AdoptCoreKey(DeviceEntry device, string key)
    {
        string old = device.Key;
        Leases.CloseTiles(old);
        device.Config.CoreKey = key;
        _store.Save(_devices.Select(x => x.Config));
        if (Groups.Any(g => g.Cameras.Any(c => c.Device == old)))
        {
            SetGroups([.. Groups.Select(g => g with { Cameras = [.. g.Cameras.Select(c => c.Device == old ? c.Rekeyed(key) : c)] })]);
        }
        foreach (CameraRef c in _audible.Where(c => c.Device == old).ToArray())
        {
            _audible.Remove(c);
            _audible.Add(c.Rekeyed(key));
        }
        foreach (SavedLayout l in Layouts.Where(l => l.Root.Cameras.Any(c => c.Device == old)).ToArray())
        {
            SaveLayout(l with { Root = l.Root.Rekeying(old, key) });
        }
    }

    /// <summary>Removes a device from the core and closes its tiles (not from settings).</summary>
    public void Disconnect(DeviceEntry device)
    {
        // Only this entry's tiles: another entry may name the same device
        // (the same address added twice gets the same core key).
        bool twin = _devices.Any(o => o != device && o.Key == device.Key);
        if (!twin)
        {
            Calls.EndDevice(device.Key);
        }
        if (twin)
        {
            foreach (CameraTile t in _tiles.Where(t => t.Device == device).ToArray())
            {
                Leases.TileEnded(t);
            }
        }
        else
        {
            Leases.CloseTiles(device.Key);
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
        if (twin)
        {
            Leases.Reopen(device.Key);
        }
        else
        {
            Detections.Clear(device.Key);
            Alerts.DeviceLost(device.Key);
            UpdateBoard();
        }
        Changed?.Invoke();
    }

    /// <summary>Removes the device from the core and adds it again with its current password.</summary>
    public void Reconnect(DeviceEntry device)
    {
        if (_credentials.Read(device.Config.Key) is not { } password)
        {
            return;
        }
        Disconnect(device);
        Connect(device.Config, password);
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
            // Kept until the core says otherwise (a new address gets a new key).
            config.CoreKey ??= old.Config.CoreKey;
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

    /// <summary>Removes a device from the core, settings, the credential store, and every group.</summary>
    public void ForgetDevice(string key)
    {
        if (_devices.FirstOrDefault(d => d.Config.Key == key) is { } device)
        {
            string cameraKey = device.Key;
            Disconnect(device);
            if (Groups.Any(g => g.Cameras.Any(c => c.Device == cameraKey)))
            {
                SetGroups([.. Groups.Select(g => g with { Cameras = [.. g.Cameras.Where(c => c.Device != cameraKey)] })]);
            }
            _audible.RemoveWhere(c => c.Device == cameraKey);
        }
        _credentials.Delete(key);
        _store.Save(_devices.Select(d => d.Config));
        Changed?.Invoke();
    }

    /// <summary>A device by camera key or configuration key.</summary>
    public DeviceEntry? FindDevice(string key) => _devices.FirstOrDefault(d => d.Key == key || d.Config.Key == key);

    /// <summary>Every fisheye camera's image circle (shared by the windows' render threads).</summary>
    public FisheyeCircles Circles { get; } = new();

    /// <summary>Whether a camera is a fisheye camera (the core's "fisheye"; a first lens only).</summary>
    public bool IsFisheye(CameraRef camera) =>
        camera.Lens == 0 && camera.Channel is >= 0 and <= 255 && FindDevice(camera.Device) is { } d
        && d.Channels.TryGetValue((byte)camera.Channel, out ChannelInfo? c) && c.Fisheye;

    private DeviceEntry? FindDevice(ulong id) => id == 0 ? null : _devices.FirstOrDefault(d => d.Id == id);

    // ---- cameras ----

    public ChannelInfo? ChannelOf(CameraRef c) =>
        FindDevice(c.Device) is { } d && c.Channel is >= 0 and <= 255 ? d.Channels.GetValueOrDefault((byte)c.Channel) : null;

    /// <summary>Generic lens words: "Lens 1", "Lens 2".</summary>
    public static string LensName(int lens) => $"Lens {lens + 1}";

    /// <summary>The channel's name without the lens.</summary>
    public string ChannelName(CameraRef c) => ChannelOf(c)?.Name is { Length: > 0 } n ? n : $"Channel {c.Channel + 1}";

    /// <summary>The channel's name; for a multi-lens camera with the lens ("Channel 3 · Lens 2").</summary>
    public string CameraName(CameraRef c)
    {
        string name = ChannelName(c);
        return (ChannelOf(c)?.Lenses ?? 0) >= 2 || c.Lens > 0 ? $"{name} · {LensName(c.Lens)}" : name;
    }

    /// <summary>The cameras of a channel: one per lens (two for a multi-lens camera).</summary>
    public static IEnumerable<CameraRef> CamerasOf(DeviceEntry d, ChannelInfo c) =>
        Enumerable.Range(0, Math.Clamp(c.Lenses, 1, CameraRef.MaxLens + 1)).Select(lens => new CameraRef(d.Key, c.Channel, lens));

    /// <summary>The channels the sidebar lists for a device.</summary>
    public IEnumerable<ChannelInfo> ListedChannels(DeviceEntry d) =>
        ChannelFilter.Visible(d.Channels.Values.OrderBy(c => c.Channel), c => c.Connected, HideOfflineChannels);

    /// <summary>Every camera of every device, in order (listed channels).</summary>
    public IEnumerable<CameraRef> AllCameras =>
        _devices.SelectMany(d => ListedChannels(d).SelectMany(c => CamerasOf(d, c)));

    /// <summary>The cameras "All Cameras" shows: every online channel of every connected device, in sidebar order, up to the layout limit.</summary>
    public List<CameraRef> OnlineCameras => NvrView.Shell.AllCameras.Select(_devices.Select(AsAllCameras));

    /// <summary>The online cameras of one device, up to the layout limit.</summary>
    public static List<CameraRef> OnlineCamerasOf(DeviceEntry d) => NvrView.Shell.AllCameras.Select([AsAllCameras(d)]);

    private static AllCamerasDevice AsAllCameras(DeviceEntry d) =>
        new(d.Key, d.State == "connected", [.. d.Channels.Values.OrderBy(c => c.Channel).Select(c => new AllCamerasChannel(c.Channel, c.Connected, c.Lenses))]);

    // ---- tiles (ITileBackend: the leases open and close them) ----

    CameraTile? ITileBackend<CameraTile>.Open(CameraRef camera, bool focused)
    {
        if (FindDevice(camera.Device) is not { Id: not 0 } device || device.State != "connected"
            || ChannelOf(camera) is not { Connected: true } || camera.Channel is < 0 or > 255)
        {
            return null;
        }
        // Open on the tier the camera should show now: the low one in a grid
        // (sub, or a second lens's mobile), never the high one first.
        StreamTier tier = Planner.WantedTier(camera);
        try
        {
            (ulong id, VideoTile video) = _host.OpenTile(device.Id, (byte)camera.Channel, CoreHost.ToCore(tier));
            var tile = new CameraTile(id, device, camera, video) { RequestedTier = tier, OpenedTier = tier, ShownTier = TierName(tier) };
            _tilesById[id] = tile;
            Volatile.Write(ref _tiles, [.. _tiles, tile]);
            if (_audible.Contains(camera))
            {
                Core.SetTileAudio(id, true, 1);
                tile.Audible = true;
            }
            return tile;
        }
        catch (NvException ex)
        {
            AppLog.Warn($"opening channel {camera.Channel} failed: {ex.Status}");
            return null;
        }
    }

    void ITileBackend<CameraTile>.Close(CameraRef camera, CameraTile tile)
    {
        Volatile.Write(ref _tiles, [.. _tiles.Where(t => t != tile)]);
        _tilesById.Remove(tile.Id);
        _host.CloseTile(tile.Id);
    }

    // Focus reaches the planner through each tile's demand (SetFocused).
    void ITileBackend<CameraTile>.FocusChanged(CameraRef camera, CameraTile tile, bool focused) => ApplyTier(camera);

    private static string TierName(StreamTier tier) => tier switch
    {
        StreamTier.Main => "main",
        StreamTier.Mobile => "mobile",
        StreamTier.Extern => "extern",
        _ => "sub",
    };

    public CameraTile? FindTile(ulong id) => _tilesById.GetValueOrDefault(id);

    // ---- tiers (one planner for every window) ----

    /// <summary>
    /// A tile's quality choice (Auto, Clear, Fluent, Balanced): the tile asks its camera for the
    /// matching stream; the highest ask across every tile of the camera wins. The window keeps
    /// the choice in the layout leaf (StageView.SetQuality).
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
        if (tile.IsEmpty)
        {
            WithdrawDemand(tile);
            return;
        }
        foreach (CameraRef camera in Planner.SetDemand(tile, tile.Camera, tile.Quality, tile.Focused, tile.Zoom))
        {
            ApplyTier(camera);
        }
    }

    /// <summary>The tile went away: everything it asked for is withdrawn.</summary>
    public void WithdrawDemand(StageTile tile)
    {
        if (Planner.ClearDemands(tile) is { } camera)
        {
            ApplyTier(camera);
        }
    }

    /// <summary>Switches a camera's open tile to the tier it should show now (nothing when it already asks for it).</summary>
    private void ApplyTier(CameraRef camera)
    {
        if (Leases.Tile(camera) is not { } tile)
        {
            return;
        }
        StreamTier wanted = Planner.WantedTier(camera);
        if (tile.RequestedTier == wanted)
        {
            return;
        }
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

    /// <summary>
    /// The Retry button: the core restarts the tile's stream now (or connects now, when its device is
    /// waiting to). The core keeps a 2 s floor and does nothing for a refused login.
    /// </summary>
    public void RetryTile(StageTile tile)
    {
        if (tile.Id == 0)
        {
            return;
        }
        try
        {
            Core.RetryTile(tile.Id);
        }
        catch (NvException e)
        {
            AppLog.Warn($"retry on tile {tile.Id} failed: {e.Status}");
        }
    }

    // ---- listening ----

    public bool IsAudible(CameraRef camera) => _audible.Contains(camera);

    /// <summary>The cameras that play now.</summary>
    public IReadOnlyCollection<CameraRef> AudibleCameras => _audible;

    /// <summary>Makes exactly these cameras audible and mutes the rest.</summary>
    public void SetAudible(IEnumerable<CameraRef> cameras)
    {
        CameraRef[] wanted = [.. cameras];
        foreach (CameraRef other in _audible.Where(c => !wanted.Contains(c)).ToArray())
        {
            SetListening(other, false);
        }
        foreach (CameraRef camera in wanted)
        {
            SetListening(camera, true);
        }
    }

    /// <summary>Makes a camera audible or silent (cameras start silent); unmuting only routes audio the core already has.</summary>
    public void SetListening(CameraRef camera, bool audible)
    {
        if (audible ? !_audible.Add(camera) : !_audible.Remove(camera))
        {
            return;
        }
        if (Leases.Tile(camera) is { } tile)
        {
            try
            {
                Core.SetTileAudio(tile.Id, audible, 1);
                tile.Audible = audible;
            }
            catch (NvException e)
            {
                AppLog.Warn($"listening on tile {tile.Id} failed: {e.Status}");
            }
        }
        Calls.AudibleChanged();
        Changed?.Invoke();
    }

    /// <summary>A speaker click: listen to this camera alone, or also to it when <paramref name="additive"/> (Ctrl); a second click mutes it.</summary>
    public void ToggleListening(CameraRef camera, bool additive)
    {
        if (_audible.Contains(camera))
        {
            SetListening(camera, false);
            return;
        }
        if (!additive)
        {
            foreach (CameraRef other in _audible.ToArray())
            {
                SetListening(other, false);
            }
        }
        SetListening(camera, true);
    }

    // ---- detections, indicators, alerts ----

    private void ApplyDetection(CoreEvent e)
    {
        if (FindDevice(e.Device) is not { } device || e.Channel < 0)
        {
            return;
        }
        string kind = e.Type == "motion" ? "motion" : e.Kind ?? e.Type;
        var detection = new DetectionEvent(
            device.Key, e.Channel, kind, e.State == "start", e.Source ?? DetectionSource.Device, e.AtNs, e.Lens, e.Type == "alarm");
        Detections.Apply(detection);
        if (e.Type == "alarm" && kind == "visitor" && detection.Active)
        {
            // A doorbell press opens a call on the camera's first lens.
            Calls.Ring(new CameraRef(device.Key, e.Channel), HeardAt(e.AtNs));
        }
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

    /// <summary>The slow tick (a few times a second, never per frame): ends held indicators. Each window reports its tiles' zoom changes (StageView.Tick).</summary>
    public void Tick()
    {
        UpdateBoard();
        Calls.Tick(Clock.Now);
    }

    // ---- layouts (stored by the core) ----

    public List<SavedLayout> Layouts { get; private set; } = [];

    public long LayoutsRevision { get; private set; }

    public SavedLayout? Layout(string id) => Layouts.FirstOrDefault(l => l.Id == id);

    /// <summary>A message for the user about the last layout operation, if it failed.</summary>
    public string? Notice { get; set; }

    private void ReloadLayouts()
    {
        try
        {
            Layouts = SavedLayout.List(Core.LayoutsJson(), out long revision);
            LayoutsRevision = revision;
        }
        catch (NvException e)
        {
            AppLog.Warn($"reading the layouts failed: {e.Status}");
        }
        LayoutsChanged?.Invoke();
    }

    /// <summary>Stores a layout (insert or replace); false (with a notice) when the core refuses it.</summary>
    public bool SaveLayout(SavedLayout layout)
    {
        try
        {
            Core.PutLayout(layout.ToJson());
        }
        catch (NvException e)
        {
            Notice = e.Status == NvStatus.Limit ? "Too many layouts; delete some first." : $"The layout could not be saved ({e.Status}).";
            AppLog.Warn($"storing layout {layout.Id} failed: {e.Status}");
            Changed?.Invoke();
            return false;
        }
        ReloadLayouts();
        return true;
    }

    /// <summary>A new layout from a tree; its id, or null.</summary>
    public string? CreateLayout(string name, LayoutNode root)
    {
        var l = new SavedLayout(SavedLayout.NewId(), name, root);
        return SaveLayout(l) ? l.Id : null;
    }

    public void RenameLayout(string id, string name)
    {
        if (Layout(id) is { } l && name.Length > 0 && name != l.Name)
        {
            SaveLayout(l with { Name = name });
        }
    }

    public string? DuplicateLayout(string id) =>
        Layout(id) is { } l ? CreateLayout(UniqueLayoutName(l.Name), l.Root) : null;

    public void DeleteLayout(string id)
    {
        try
        {
            Core.DeleteLayout(id);
        }
        catch (NvException e) when (e.Status == NvStatus.UnknownId)
        {
        }
        ReloadLayouts();
    }

    public string UniqueLayoutName(string name)
    {
        var names = Layouts.Select(l => l.Name).ToHashSet();
        if (!names.Contains(name))
        {
            return name;
        }
        int n = 2;
        while (names.Contains($"{name} {n}"))
        {
            n++;
        }
        return $"{name} {n}";
    }

    // ---- groups ----

    public List<CameraGroup> Groups { get; private set; }

    public CameraGroup? Group(string id) => Groups.FirstOrDefault(g => g.Id == id);

    private void SetGroups(List<CameraGroup> groups)
    {
        Groups = groups;
        try
        {
            _groupStore.Save(groups);
        }
        catch (IOException e)
        {
            AppLog.Warn($"saving the groups failed: {e.Message}");
        }
        GroupsChanged?.Invoke();
        Changed?.Invoke();
    }

    public string CreateGroup(string name, IEnumerable<CameraRef> cameras)
    {
        var g = new CameraGroup(CameraGroup.NewId(), name, [.. cameras.Distinct()]);
        SetGroups([.. Groups, g]);
        return g.Id;
    }

    public void RenameGroup(string id, string name)
    {
        if (name.Length > 0)
        {
            SetGroups([.. Groups.Select(g => g.Id == id ? g with { Name = name } : g)]);
        }
    }

    public void DeleteGroup(string id) => SetGroups([.. Groups.Where(g => g.Id != id)]);

    public void AddToGroup(string id, CameraRef camera)
    {
        if (Group(id) is { } g && !g.Cameras.Contains(camera))
        {
            SetGroups([.. Groups.Select(x => x.Id == id ? x with { Cameras = [.. x.Cameras, camera] } : x)]);
        }
    }

    public void RemoveFromGroup(string id, CameraRef camera) =>
        SetGroups([.. Groups.Select(x => x.Id == id ? x with { Cameras = [.. x.Cameras.Where(c => c != camera)] } : x)]);

    // ---- events ----

    /// <summary>Applies one core event (UI thread).</summary>
    public void Apply(CoreEvent e)
    {
        switch (e.Type)
        {
            case "device_state":
                if (FindDevice(e.Device) is { } d)
                {
                    string before = d.State;
                    d.State = e.State ?? d.State;
                    d.Attempt = e.Attempt;
                    d.LoginStatus = e.Status;
                    d.RetryAt = Stopwatch.GetTimestamp() + e.RetryMs * Stopwatch.Frequency / 1000;
                    if (d.State != "connected")
                    {
                        // A lost session says nothing about what is still going on.
                        Detections.Clear(d.Key);
                        Alerts.DeviceLost(d.Key);
                        UpdateBoard();
                    }
                    if (e.State == "login_failed")
                    {
                        AppLog.Warn($"device {d.Config.Key}: login failed (status {e.Status})");
                        // Terminal: the tiles end; new credentials in Settings add it again.
                        Calls.EndDevice(d.Key);
                        Leases.CloseTiles(d.Key);
                    }
                    else if (d.State == "connected" && before != "connected")
                    {
                        Leases.Reopen(d.Key);
                    }
                }
                break;
            case "device_info":
                if (FindDevice(e.Device) is { } info)
                {
                    info.DeviceName = e.Name;
                    info.Model = e.Model;
                    if (e.Key is { Length: > 0 } key && info.Key != key)
                    {
                        AdoptCoreKey(info, key);
                    }
                }
                break;
            case "channels":
                if (FindDevice(e.Device) is { } dev && e.Channels is { } channels)
                {
                    foreach (ChannelInfo c in channels)
                    {
                        if (dev.Channels.TryGetValue(c.Channel, out ChannelInfo? before))
                        {
                            // "fisheye" comes with the capabilities, as "detects" does.
                            if (c.Detects is null)
                            {
                                c.Fisheye |= before.Fisheye;
                            }
                            // Names, detection kinds, and lenses may come in a later list than the slots.
                            c.Name ??= before.Name;
                            c.Detects ??= before.Detects;
                            if (c.Lenses == 0)
                            {
                                c.Lenses = before.Lenses;
                            }
                        }
                        dev.Channels[c.Channel] = c;
                    }
                    Leases.Reopen(dev.Key);
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
                    waiting.WaitingGeneration = e.Generation;
                }
                break;
            case "tile_error":
                if (FindTile(e.Tile) is { } failed)
                {
                    if (Planner.StartFailed(failed.Camera, failed.OpenedTier, e.Reason))
                    {
                        // The device refuses this lens's mobile stream: extern for the rest of the run.
                        AppLog.Info($"second lens of channel {failed.Channel + 1}: mobile refused, reopening on extern");
                        Leases.Reopen(failed.Camera);
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
                    // Another generation took over, or the waiting one failed: that wait is over.
                    if (tier.Waiting != null && ((e.State == "active" && e.Generation != tier.WaitingGeneration)
                        || (e.State == "failed" && e.Generation == tier.WaitingGeneration)))
                    {
                        tier.Waiting = null;
                    }
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
                    // The core ended it on its own: the lease stays, and the
                    // next channel list (or reconnect) opens it again.
                    AppLog.Info($"tile {e.Tile} closed by the core ({e.Reason})");
                    Leases.TileEnded(closed);
                }
                break;
            case "detection":
            case "motion":
            case "alarm":
                DetectionEvents++;
                ApplyDetection(e);
                break;
            case "update":
                if (e.Update is { } update)
                {
                    UpdateReceived?.Invoke(update);
                }
                return;
            case "talk":
                Calls.OnTalkEvent(e);
                return;
            case "discovery_started":
            case "discovered":
            case "discovery_done":
                if (e.Discovery is { } found)
                {
                    Discovery.Apply(found);
                }
                break;
            case "layouts_changed":
                if (e.Revision != LayoutsRevision)
                {
                    ReloadLayouts();
                }
                return;
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

    /// <summary>Closes every tile and removes every device (shutdown).</summary>
    public void CloseAll()
    {
        Calls.EndAll();
        Leases.CloseAll();
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
