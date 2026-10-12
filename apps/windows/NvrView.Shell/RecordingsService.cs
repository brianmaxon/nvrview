// The recordings side of the app (docs/UX.md "Recordings"), shared by every
// Recordings window: configuration, which cameras are searched, the core's
// recordings events, cover thumbnails, downloads, camera groups, and the
// routing of playback events to the windows' players. The same model as the
// macOS app's RecordingsModel. Plain .NET over IRecordings and
// IPlaybackHost, so the fakes drive it in tests on Linux; the app gives it
// the real core, a thumbnail decoder, the Recycle Bin, and Explorer.
//
// UI thread only. Every call it makes answers from the core's memory or
// queues device work that reports by event (docs/FFI-RECORDINGS.md section
// 9), so nothing here waits on a device.

using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>
/// Which cameras of a device the user lets the recordings screens search, and which listen for sounds
/// (<c>nv_sound_watch</c>), saved between launches.
/// </summary>
public sealed record RecordingsEnablement(string Device, IReadOnlyList<RecordingCamera> Cameras, IReadOnlyList<RecordingCamera>? Listen = null);

/// <summary>Keeps the searched cameras in <c>recordings-cameras.json</c>; the core does not keep them.</summary>
public sealed class RecordingsEnablementStore(string path)
{
    /// <summary>The saved choices (none when the file is missing or unreadable).</summary>
    public List<RecordingsEnablement> Load()
    {
        var list = new List<RecordingsEnablement>();
        if (JsonFile.Read(path) is not JsonArray array)
        {
            return list;
        }
        foreach (JsonNode? n in array)
        {
            if (n is JsonObject o && o["device"] is JsonValue d && d.TryGetValue(out string? device) && o["cameras"] is JsonArray cams)
            {
                RecordingCamera[] cameras = [.. cams.Select(c => c is null ? null : CameraOf(c)).OfType<RecordingCamera>()];
                RecordingCamera[]? listen = o["listen"] is JsonArray heard
                    ? [.. heard.Select(c => c is null ? null : CameraOf(c)).OfType<RecordingCamera>()]
                    : null;
                list.Add(new RecordingsEnablement(device, cameras, listen));
            }
        }
        return list;
    }

    private static RecordingCamera? CameraOf(JsonNode node)
    {
        using JsonDocument doc = JsonDocument.Parse(node.ToJsonString());
        return RecordingCamera.From(doc.RootElement);
    }

    /// <summary>Writes the choices whole.</summary>
    public void Save(IEnumerable<RecordingsEnablement> all) =>
        JsonFile.Write(path, new JsonArray([.. all.Select(e =>
        {
            var o = new JsonObject
            {
                ["device"] = e.Device,
                ["cameras"] = new JsonArray([.. e.Cameras.Select(c => JsonNode.Parse(c.Json))]),
            };
            if (e.Listen is { Count: > 0 } listen)
            {
                o["listen"] = new JsonArray([.. listen.Select(c => JsonNode.Parse(c.Json))]);
            }
            return (JsonNode?)o;
        })]).ToJsonString(JsonFile.Pretty));
}

/// <summary>
/// How the player gets its tiles. The app's implementation makes the
/// decoder and picture objects of a playback's tiles before any action can
/// reach them (as it does for live tiles); the plain one only calls the core.
/// </summary>
public interface IPlaybackHost
{
    /// <summary>The playback calls.</summary>
    IPlayback Playback { get; }

    /// <summary><c>nv_playback_open</c>, with the tiles' objects made before it returns.</summary>
    OpenedPlayback Open(string json);

    /// <summary><c>nv_playback_add_camera</c>, with the tile's objects made before it returns.</summary>
    ulong AddCamera(ulong playback, string json);

    /// <summary>Tiles that are gone (closed, removed, or their playback ended): release their objects.</summary>
    void Released(IReadOnlyList<ulong> tiles);

    /// <summary>A tile's sound through the core's tile audio path.</summary>
    void SetAudio(ulong tile, bool audible, float volume);
}

/// <summary>An <see cref="IPlaybackHost"/> with no tile objects (the fake, tests).</summary>
public sealed class PlainPlaybackHost(IPlayback playback) : IPlaybackHost
{
    /// <summary>Tiles whose sound was set, with whether it is audible (tests).</summary>
    public Dictionary<ulong, (bool Audible, float Volume)> Audio { get; } = [];

    /// <inheritdoc/>
    public IPlayback Playback { get; } = playback;

    /// <inheritdoc/>
    public OpenedPlayback Open(string json) => Playback.PlaybackOpen(json);

    /// <inheritdoc/>
    public ulong AddCamera(ulong playback, string json) => Playback.PlaybackAddCamera(playback, json);

    /// <inheritdoc/>
    public void Released(IReadOnlyList<ulong> tiles)
    {
    }

    /// <inheritdoc/>
    public void SetAudio(ulong tile, bool audible, float volume) => Audio[tile] = (audible, volume);
}

/// <summary>The recordings model (see the file comment).</summary>
public sealed class RecordingsService
{
    private readonly IRecordings _core;
    private readonly RecordingsEnablementStore? _store;
    private readonly DownloadBoard _board = new();
    private readonly Dictionary<ulong, WeakReference<RecordingsPlayer>> _players = [];
    private readonly Dictionary<Guid, Action> _listeners = [];
    private ulong _nextHold = 1;
    private bool _changeScheduled;
    private bool _publishScheduled;
    private bool _closed;

    /// <param name="core">The recordings calls.</param>
    /// <param name="playback">The playback host.</param>
    /// <param name="store">Where the searched cameras are kept (null: not kept).</param>
    public RecordingsService(IRecordings core, IPlaybackHost playback, RecordingsEnablementStore? store = null)
    {
        _core = core;
        Playback = playback;
        _store = store;
    }

    /// <summary>Raised on the UI thread when the downloads list or a download's state or progress changed.</summary>
    public event Action? DownloadsChanged;

    /// <summary>The playback host (the players use it).</summary>
    public IPlaybackHost Playback { get; }

    /// <summary>False until the core accepted the configuration (a core without recordings never does; the screens stay hidden).</summary>
    public bool Available { get; private set; }

    /// <summary>Where downloads go.</summary>
    public string? DownloadsFolder { get; private set; }

    /// <summary>The searched cameras, by device key.</summary>
    public Dictionary<string, List<RecordingCamera>> Enabled { get; } = [];

    /// <summary>The cameras that listen for sounds, by device key.</summary>
    public Dictionary<string, List<RecordingCamera>> Listening { get; } = [];

    /// <summary>Each listening camera's latest <c>sound_watch</c> state and reason.</summary>
    public Dictionary<RecordingCamera, (string State, string? Reason)> ListenStates { get; } = [];

    /// <summary>Each enabled device's search lane: idle, busy, waiting_device, ended.</summary>
    public Dictionary<string, (string State, int PendingDays)> IndexStates { get; } = [];

    /// <summary>Grows with every <c>recordings_changed</c> (and enablement change).</summary>
    public int IndexRevision { get; private set; }

    /// <summary>Grows with every <c>incidents_changed</c>.</summary>
    public int IncidentsRevision { get; private set; }

    /// <summary>Camera-days the device could not search (not retried by the core).</summary>
    public Dictionary<RecordingCamera, HashSet<long>> FailedDays { get; } = [];

    /// <summary>The camera groups for incidents.</summary>
    public IncidentGroups Groups { get; private set; } = IncidentGroups.Empty;

    /// <summary>Every known download, in the order they appeared.</summary>
    public IReadOnlyList<DownloadProgress> Downloads { get; private set; } = [];

    /// <summary>Cover thumbnails the screens show.</summary>
    public ThumbnailBoard Thumbnails { get; } = new();

    /// <summary>How many keyframes were decoded into small images and put back.</summary>
    public int ThumbnailsPut { get; private set; }

    /// <summary>A message for the user (shown at the bottom of a recordings window).</summary>
    public string? Notice { get; set; }

    /// <summary>The computer's time (fixed by tests).</summary>
    public Func<DateTimeOffset> Now { get; set; } = () => DateTimeOffset.Now;

    /// <summary>Runs an action on the UI thread, later (the app's dispatcher); the default runs it at once.</summary>
    public Action<Action> Post { get; set; } = a => a();

    /// <summary>Moves a download's folder to the Recycle Bin (the app's; the self-test records it instead).</summary>
    public Action<string> Trasher { get; set; } = _ => { };

    /// <summary>Shows files in Explorer (the app's; the self-test records it instead).</summary>
    public Action<IReadOnlyList<string>> Revealer { get; set; } = _ => { };

    /// <summary>
    /// Turns a cached keyframe into the app's small image and calls back with it
    /// (null: could not be decoded), from any thread. Null: no decoder, so
    /// keyframes show as failed.
    /// </summary>
    public Action<RecordedThumbnail, Action<RecordedThumbnail?>>? ThumbnailDecoder { get; set; }

    /// <summary>
    /// Drops the decoder's waiting requests (the window moved on); each is answered with
    /// <see cref="ThumbnailDecodeQueue.Canceled"/>, which makes its picture be asked for again later.
    /// </summary>
    public Action? ThumbnailDecoderCancel { get; set; }

    /// <summary>The app names a camera as its camera list does (null: "Channel N").</summary>
    public Func<RecordingCamera, string?>? CameraNamer { get; set; }

    /// <summary>The device's offset from UTC: the computer's (FFI-RECORDINGS.md, decision 6, until the core reads the device's own zone).</summary>
    public int UtcOffset => (int)Now().Offset.TotalSeconds;

    /// <summary>Today on the devices.</summary>
    public long Today => DeviceTime.Today(Now(), UtcOffset);

    private bool Usable => Available && !_closed;

    // ---- start and stop ----

    /// <summary>
    /// Configures recordings with a downloads folder, enables the saved cameras,
    /// and reads the groups and downloads. Without recordings in the core,
    /// nothing is shown.
    /// </summary>
    public void Start(string downloads)
    {
        if (_closed)
        {
            return;
        }
        try
        {
            Directory.CreateDirectory(downloads);
            _core.RecordingsConfigure(new JsonObject { ["downloads_dir"] = downloads }.ToJsonString());
        }
        catch (NvException)
        {
            return;
        }
        catch (IOException)
        {
            return;
        }
        Available = true;
        DownloadsFolder = downloads;
        foreach (RecordingsEnablement e in _store?.Load() ?? [])
        {
            if (e.Cameras.Count > 0)
            {
                Enabled[e.Device] = [.. e.Cameras];
                ApplyEnablement(e.Device);
            }
            foreach (RecordingCamera c in e.Listen ?? [])
            {
                SetListening(c, true, save: false);
            }
        }
        ReloadGroups();
        try
        {
            if (DownloadProgress.ParseList(_core.DownloadsGet()) is { } list)
            {
                _board.Load(list);
                Downloads = _board.Downloads;
            }
        }
        catch (NvException)
        {
        }
    }

    /// <summary>The app quits: every open playback closes first (its plays stop on the device), then nothing more is asked of the core.</summary>
    public void Shutdown()
    {
        foreach (RecordingsPlayer p in _players.Values.Select(w => w.TryGetTarget(out RecordingsPlayer? t) ? t : null).OfType<RecordingsPlayer>().ToList())
        {
            p.Close();
        }
        _players.Clear();
        _closed = true;
    }

    // ---- windows ----

    /// <summary>Calls <paramref name="body"/> (once per run of changes) when the index, the incidents, the groups, or the enabled cameras change. Keep the token.</summary>
    public Guid Subscribe(Action body)
    {
        Guid token = Guid.NewGuid();
        _listeners[token] = body;
        return token;
    }

    /// <summary>Stops calling a subscriber.</summary>
    public void Unsubscribe(Guid token) => _listeners.Remove(token);

    private void Changed()
    {
        if (_changeScheduled)
        {
            return;
        }
        _changeScheduled = true;
        Post(() =>
        {
            _changeScheduled = false;
            foreach (Action body in _listeners.Values.ToList())
            {
                body();
            }
        });
    }

    /// <summary>A hold number for a window (<c>nv_recordings_hold_today</c>).</summary>
    public ulong NewHold() => _nextHold++;

    // ---- enabled cameras ----

    /// <summary>Every camera the recordings screens search, in device and channel order.</summary>
    public List<RecordingCamera> EnabledCameras => [.. Enabled.Keys.Order(StringComparer.Ordinal).SelectMany(k => Enabled[k])];

    /// <summary>Whether a camera is searched.</summary>
    public bool IsEnabled(RecordingCamera camera) => Enabled.TryGetValue(camera.Device, out List<RecordingCamera>? l) && l.Contains(camera);

    /// <summary>Turns searching one camera on or off.</summary>
    public void SetEnabled(RecordingCamera camera, bool on)
    {
        List<RecordingCamera> list = Enabled.TryGetValue(camera.Device, out List<RecordingCamera>? l) ? l : [];
        if (on && !list.Contains(camera))
        {
            list.Add(camera);
            list.Sort((a, b) => (a.Channel, a.Lens).CompareTo((b.Channel, b.Lens)));
        }
        if (!on)
        {
            list.Remove(camera);
        }
        SetEnabled(camera.Device, list);
    }

    /// <summary>Every camera of a device on (the list), or all off (empty).</summary>
    public void SetEnabled(string device, IReadOnlyList<RecordingCamera> cameras)
    {
        if (cameras.Count == 0)
        {
            Enabled.Remove(device);
        }
        else
        {
            Enabled[device] = [.. cameras];
        }
        ApplyEnablement(device);
        SaveEnablement();
        IndexRevision++;
        Changed();
    }

    private void ApplyEnablement(string device)
    {
        if (!Usable)
        {
            return;
        }
        string? json = Enabled.TryGetValue(device, out List<RecordingCamera>? l) ? RecordingsRequest.Enable(l, UtcOffset) : null;
        try
        {
            _core.RecordingsEnable(device, json);
        }
        catch (NvException e)
        {
            Notice = e.Status == NvStatus.Unsupported
                ? "This device has no recordings to search"
                : $"Could not search this device (status {(int)e.Status})";
        }
    }

    private void SaveEnablement() =>
        _store?.Save(Enabled.Keys.Union(Listening.Keys).Order(StringComparer.Ordinal).Select(k => new RecordingsEnablement(
            k, Enabled.TryGetValue(k, out List<RecordingCamera>? on) ? on : [],
            Listening.TryGetValue(k, out List<RecordingCamera>? heard) && heard.Count > 0 ? heard : null)));

    // ---- analyzing a day's sound ----

    /// <summary>Each camera-day's latest sound analysis (<c>nv_sound_analyze_day</c>), by camera as asked and the day's start.</summary>
    public Dictionary<(RecordingCamera Camera, long Day), AnalysisEntry> SoundAnalyses { get; } = [];

    /// <summary>The analysis of a camera's day, if one was asked for.</summary>
    public AnalysisEntry? Analysis(RecordingCamera camera, long day) =>
        SoundAnalyses.GetValueOrDefault((camera, DeviceTime.DayStart(day)));

    /// <summary>Whether a camera's day is waiting for or being analyzed.</summary>
    public bool IsAnalyzing(RecordingCamera camera, long day) => Analysis(camera, day) is { Active: true };

    /// <summary>
    /// Asks for the sound of these cameras' day to be analyzed from the downloaded video (<c>nv_sound_analyze_day</c>);
    /// progress and the outcome come back as <c>sound_analyze</c> events. Sets <see cref="Notice"/> when nothing can be asked.
    /// </summary>
    public void AnalyzeSound(IEnumerable<RecordingCamera> cameras, long day)
    {
        if (!Usable || _core is not IEditSound sound)
        {
            return;
        }
        if (DownloadsFolder is null)
        {
            Notice = SoundAnalysis.Failure("no_source");
            return;
        }
        Notice = null;
        foreach (RecordingCamera camera in cameras)
        {
            if (IsAnalyzing(camera, day))
            {
                continue;
            }
            try
            {
                string answer = sound.SoundAnalyzeDay(SoundAnalysis.Query(camera, day));
                var key = (camera, DeviceTime.DayStart(day));
                if (!SoundAnalyses.TryGetValue(key, out AnalysisEntry? now) || !now.Active)
                {
                    using JsonDocument doc = JsonDocument.Parse(answer);
                    if (doc.RootElement.TryGetProperty("request", out JsonElement r) && r.TryGetUInt64(out ulong id))
                    {
                        SoundAnalyses[key] = new AnalysisEntry(id, AnalysisState.Queued);
                    }
                }
            }
            catch (NvException e)
            {
                Notice = SoundAnalysis.Refusal(e.Status);
            }
        }
        Changed();
    }

    /// <summary>Cancels a camera's analysis (<c>nv_sound_analyze_cancel</c>).</summary>
    public void CancelAnalysis(RecordingCamera camera, long day)
    {
        if (!Usable || _core is not IEditSound sound || Analysis(camera, day) is not { Active: true } e)
        {
            return;
        }
        try
        {
            sound.SoundAnalyzeCancel(e.Request);
        }
        catch (NvException x)
        {
            // Already over: its own event says how.
            if (x.Status != NvStatus.UnknownId)
            {
                Notice = $"Could not cancel {StatusText(x)}";
            }
        }
    }

    private void ApplyAnalysis(RecordingsEvent.SoundAnalyze a)
    {
        if (a.Day is not { } day || SoundAnalysis.ParseState(a.State) is not { } state)
        {
            return;
        }
        var key = (a.Camera, DeviceTime.DayStart(day));
        SoundAnalyses.TryGetValue(key, out AnalysisEntry? old);
        AnalysisEntry entry = SoundAnalysis.Fold(old, (ulong)a.Request, state, a.Reason, a.Seconds, a.Labels);
        if (entry == old)
        {
            return;
        }
        SoundAnalyses[key] = entry;
        if (!entry.Active && SoundAnalysis.Outcome(entry) is { } message)
        {
            Notice = message;
        }
        // What the analysis stored (loudness, sound labels) is read again.
        if (state == AnalysisState.Done)
        {
            IndexRevision++;
        }
        Changed();
    }

    // ---- listening for sounds ----

    /// <summary>Whether a camera listens for sounds.</summary>
    public bool IsListening(RecordingCamera camera) => Listening.TryGetValue(camera.Device, out List<RecordingCamera>? l) && l.Contains(camera);

    /// <summary>Every camera that listens for sounds.</summary>
    public List<RecordingCamera> ListeningCameras => [.. Listening.Keys.Order(StringComparer.Ordinal).SelectMany(k => Listening[k])];

    /// <summary>
    /// Turns listening for sounds on or off for a camera (<c>nv_sound_watch</c>: its sub-stream stays open with no
    /// tile; at most four per device). Returns false (with <see cref="Notice"/> set) when it was refused.
    /// </summary>
    public bool SetListening(RecordingCamera camera, bool on, bool save = true)
    {
        if (on && !SoundListening.CanAdd(camera, ListeningCameras))
        {
            Notice = $"At most {SoundListening.MaxPerDevice} cameras of a device listen for sounds";
            return false;
        }
        // The list changes before the call: the core's sound_watch event may arrive during it (the fake sends it
        // at once) and is kept only for a camera on the list.
        bool had = IsListening(camera);
        SetListed(camera, on);
        if (Usable && _core is IEditSound sound)
        {
            try
            {
                sound.SoundWatch(camera.Json, on);
            }
            catch (NvException e) when (on)
            {
                SetListed(camera, had);
                Notice = e.Status == NvStatus.Limit
                    ? "Too many cameras listen for sounds"
                    : $"Could not listen to this camera {StatusText(e)}";
                return false;
            }
            catch (NvException)
            {
            }
        }
        if (save)
        {
            SaveEnablement();
        }
        Changed();
        return true;
    }

    private void SetListed(RecordingCamera camera, bool on)
    {
        List<RecordingCamera> list = Listening.TryGetValue(camera.Device, out List<RecordingCamera>? l) ? l : [];
        if (on && !list.Contains(camera))
        {
            list.Add(camera);
        }
        if (!on)
        {
            list.Remove(camera);
            ListenStates.Remove(camera);
        }
        if (list.Count == 0)
        {
            Listening.Remove(camera.Device);
        }
        else
        {
            Listening[camera.Device] = list;
        }
    }

    /// <summary>Loudness lanes for <paramref name="cameras"/> over [start, end] (memory only).</summary>
    public LoudnessAnswer? Loudness(long start, long end, long bucket, IReadOnlyList<RecordingCamera> cameras) =>
        cameras.Count == 0 || _core is not IEditSound sound ? null
            : Query(() => sound.RecordingsLoudness(LoudnessScale.Query(start, end, bucket, cameras)), LoudnessAnswer.Parse);

    /// <summary>An incident's finished download (the newest), whose files a follow cut or a grid is made from.</summary>
    public DownloadProgress? FinishedDownload(string incident) =>
        Downloads.LastOrDefault(d => d.Incident == incident && d.State == "done");

    /// <summary><c>nv_edit_compute</c>: the plan, or why there is none (the words the player shows).</summary>
    public (EditPlan? Plan, string? Refusal) ComputeEdit(string json)
    {
        if (!Usable || _core is not IEditSound sound)
        {
            return (null, "Recordings are not available");
        }
        try
        {
            return EditPlan.Parse(sound.EditCompute(json)) is { } plan
                ? (plan, null)
                : (null, "The core's edit list could not be read");
        }
        catch (NvException e)
        {
            return (null, e.Status switch
            {
                NvStatus.UnknownId => "The download or its incident is gone",
                NvStatus.NotConfigured => "There is no downloads folder",
                NvStatus.InvalidArgument => "No camera of this download has video where the incident happened",
                _ => $"No edit could be made (status {(int)e.Status})",
            });
        }
    }

    /// <summary>The core named a device differently (first connection): its cameras follow.</summary>
    public void Rekey(string old, string next)
    {
        if (old != next && Listening.TryGetValue(old, out List<RecordingCamera>? heard))
        {
            foreach (RecordingCamera c in heard.ToList())
            {
                SetListening(c, false, save: false);
                SetListening(c with { Device = next }, true, save: false);
            }
            SaveEnablement();
        }
        if (old == next || !Enabled.Remove(old, out List<RecordingCamera>? cams))
        {
            return;
        }
        if (Usable)
        {
            try
            {
                _core.RecordingsEnable(old, null);
            }
            catch (NvException)
            {
            }
        }
        Enabled[next] = [.. cams.Select(c => c with { Device = next })];
        ApplyEnablement(next);
        SaveEnablement();
        Changed();
    }

    /// <summary>A device the user removed is no longer searched.</summary>
    public void DeviceRemoved(string key)
    {
        if (Listening.TryGetValue(key, out List<RecordingCamera>? heard))
        {
            foreach (RecordingCamera c in heard.ToList())
            {
                SetListening(c, false);
            }
        }
        if (Enabled.ContainsKey(key))
        {
            SetEnabled(key, []);
        }
    }

    // ---- the index ----

    private static string StatusText(NvException e) => $"(status {(int)e.Status})";

    /// <summary>Asks for days of cameras to be searched (and refreshed with <paramref name="force"/>); returns a message when the core refused.</summary>
    public string? Want(IReadOnlyList<RecordingCamera> cameras, IReadOnlyList<long> days, bool force = false)
    {
        if (!Usable || cameras.Count == 0 || days.Count == 0)
        {
            return null;
        }
        foreach (RecordingCamera c in cameras)
        {
            foreach (long d in days)
            {
                FailedDays.GetValueOrDefault(c)?.Remove(d);
            }
        }
        try
        {
            _core.RecordingsWant(RecordingsRequest.Want(cameras, days, force));
            return null;
        }
        catch (NvException e)
        {
            return e.Status == NvStatus.Limit
                ? "Too many days are waiting to be searched; try again shortly"
                : $"Could not search {StatusText(e)}";
        }
    }

    /// <summary>Asks for months' calendars alone (a calendar paged to another month); returns a message when the core refused.</summary>
    public string? WantMonths(IReadOnlyList<RecordingCamera> cameras, IReadOnlyList<long> months)
    {
        if (!Usable || cameras.Count == 0 || months.Count == 0)
        {
            return null;
        }
        try
        {
            _core.RecordingsWant(RecordingsRequest.WantMonths(cameras, months));
            return null;
        }
        catch (NvException e)
        {
            return e.Status == NvStatus.Limit ? "Too many days are waiting to be searched; try again shortly" : null;
        }
    }

    /// <summary>Keeps today fresh for a window's cameras (null releases).</summary>
    public void Hold(ulong hold, IReadOnlyList<RecordingCamera>? cameras)
    {
        if (!Usable)
        {
            return;
        }
        try
        {
            _core.RecordingsHoldToday(hold, cameras is { Count: > 0 } ? RecordingsRequest.Hold(cameras) : null);
        }
        catch (NvException)
        {
        }
    }

    /// <summary>Whether some enabled device is searching, how many camera-days wait, and whether one waits for its device.</summary>
    public (bool Busy, int PendingDays, bool WaitingDevice) Searching
    {
        get
        {
            var states = IndexStates.Where(p => Enabled.ContainsKey(p.Key)).Select(p => p.Value).ToList();
            return (states.Any(s => s.State == "busy"), states.Sum(s => s.PendingDays),
                states.Any(s => s.State is "waiting_device" or "ended"));
        }
    }

    // ---- queries (memory only) ----

    private T? Query<T>(Func<string> call, Func<string, T?> parse) where T : class
    {
        if (!Usable)
        {
            return null;
        }
        try
        {
            return parse(call());
        }
        catch (NvException)
        {
            return null;
        }
    }

    /// <summary>Days with recordings.</summary>
    public CalendarAnswer? Calendar(long start, long end, IReadOnlyList<RecordingCamera> cameras) =>
        cameras.Count == 0 ? null : Query(() => _core.RecordingsCalendar(RecordingsRequest.Query(start, end, cameras)), CalendarAnswer.Parse);

    /// <summary>Recording spans.</summary>
    public SpansAnswer? Spans(long start, long end, IReadOnlyList<RecordingCamera> cameras) =>
        cameras.Count == 0 ? null : Query(() => _core.RecordingsSpans(RecordingsRequest.Query(start, end, cameras)), SpansAnswer.Parse);

    /// <summary>Events.</summary>
    public EventsAnswer? Events(long start, long end, IReadOnlyList<RecordingCamera> cameras) =>
        cameras.Count == 0 ? null : Query(() => _core.RecordingsEvents(RecordingsRequest.Query(start, end, cameras)), EventsAnswer.Parse);

    /// <summary>Incidents in a range, for cameras or a group.</summary>
    public IncidentsAnswer? Incidents(long start, long end, IReadOnlyList<RecordingCamera>? cameras, string? group) =>
        Query(() => _core.IncidentsGet(RecordingsRequest.Query(start, end, cameras, group)), IncidentsAnswer.Parse);

    /// <summary>One incident by key.</summary>
    public Incident? IncidentByKey(string key) => Query(() => _core.IncidentGet(key), IncidentAnswer.Parse)?.Incident;

    // ---- incidents ----

    /// <summary>Applies an edit; the incidents reload on the <c>incidents_changed</c> that follows. Returns the resulting keys, or null (with <see cref="Notice"/> set).</summary>
    public IReadOnlyList<string>? Edit(IncidentEdit edit)
    {
        if (_closed)
        {
            return null;
        }
        try
        {
            string answer = _core.IncidentEdit(edit.Json);
            Notice = null;
            return EditAnswer.Parse(answer)?.Keys ?? [];
        }
        catch (NvException e)
        {
            Notice = e.Status switch
            {
                NvStatus.InvalidArgument => "That edit cannot be made (an incident would be left without events or cameras)",
                NvStatus.UnknownId => "That incident changed; try again",
                NvStatus.Limit => "Too many edits",
                _ => $"The edit failed {StatusText(e)}",
            };
            return null;
        }
    }

    /// <summary>Locks or unlocks an incident; tells the user when its video is not being kept.</summary>
    public LockAnswer? Lock(string key, bool locked)
    {
        if (_closed)
        {
            return null;
        }
        try
        {
            LockAnswer? answer = LockAnswer.Parse(_core.IncidentLock(key, locked));
            Notice = locked ? answer?.Message : "Unlocked. Downloaded files stay where they are.";
            return answer;
        }
        catch (NvException e)
        {
            Notice = e.Status == NvStatus.Limit
                ? "Too many locked incidents (or one longer than 31 days)"
                : $"Could not change the lock {StatusText(e)}";
            return null;
        }
    }

    // ---- camera groups ----

    private void ReloadGroups()
    {
        if (_closed)
        {
            return;
        }
        try
        {
            Groups = IncidentGroups.Parse(_core.CameraGroupsGet()) ?? IncidentGroups.Empty;
        }
        catch (NvException)
        {
        }
    }

    /// <summary>Replaces the camera groups. Renaming or changing a group changes the keys of its incidents (edits of locked incidents follow).</summary>
    public bool PutGroups(IncidentGroups groups)
    {
        if (_closed)
        {
            return false;
        }
        try
        {
            _core.CameraGroupsPut(groups.PutJson);
            ReloadGroups();
            return true;
        }
        catch (NvException e)
        {
            Notice = e.Status == NvStatus.Limit ? "Too many groups or cameras (64 of each)" : $"Could not save the groups {StatusText(e)}";
            return false;
        }
    }

    /// <summary>A camera's name as the camera list shows it: the user's override, else the app's name for it, else "Channel N".</summary>
    public string CameraName(RecordingCamera c) =>
        Groups.CameraNames.FirstOrDefault(n => n.Camera == c)?.Name ?? CameraNamer?.Invoke(c) ?? $"Channel {c.Channel + 1}";

    // ---- thumbnails ----

    /// <summary>A view shows <paramref name="key"/>: asks the core for it unless it is known.</summary>
    public void RequestThumbnail(ThumbnailKey key)
    {
        if (!Available)
        {
            return;
        }
        if (!Thumbnails.NeedsRequest(key))
        {
            Thumbnails.Touch(key);
            return;
        }
        FetchThumbnail(key);
    }

    private void FetchThumbnail(ThumbnailKey key)
    {
        if (_closed)
        {
            return;
        }
        try
        {
            if (_core.ThumbnailGet(key.Json) is { } t)
            {
                Use(t, key);
            }
            else
            {
                Thumbnails.Set(key, new ThumbnailState.Pending());
            }
        }
        catch (NvException e)
        {
            Thumbnails.Set(key, new ThumbnailState.Failed($"status {(int)e.Status}"));
        }
    }

    // A cached thumbnail: the app's image as is, a keyframe decoded and shrunk by the decoder, then put back in its place.
    private void Use(RecordedThumbnail t, ThumbnailKey key)
    {
        if (t.Kind == NvThumbKind.Image)
        {
            Thumbnails.Set(key, new ThumbnailState.Image(t.Data));
            return;
        }
        if (ThumbnailDecoder is not { } decode)
        {
            Thumbnails.Set(key, new ThumbnailState.Failed("the picture could not be decoded"));
            return;
        }
        Thumbnails.Set(key, new ThumbnailState.Decoding());
        decode(t, small => Post(() =>
        {
            if (_closed)
            {
                return;
            }
            if (ReferenceEquals(small, ThumbnailDecodeQueue.Canceled))
            {
                Thumbnails.Set(key, new ThumbnailState.Dropped());
                return;
            }
            if (small is null)
            {
                Thumbnails.Set(key, new ThumbnailState.Failed("the picture could not be decoded"));
                return;
            }
            try
            {
                _core.ThumbnailPut(key.Json, small with { PtsUs = t.PtsUs });
                ThumbnailsPut++;
            }
            catch (NvException)
            {
            }
            Thumbnails.Set(key, new ThumbnailState.Image(small.Data));
            Changed();
        }));
    }

    /// <summary>The window moved on: drops the fetches still queued for it.</summary>
    public void CancelThumbnails()
    {
        if (_closed)
        {
            return;
        }
        ThumbnailDecoderCancel?.Invoke();
        try
        {
            _core.ThumbnailCancel(null);
        }
        catch (NvException)
        {
        }
        Thumbnails.ForgetPending();
    }

    // ---- downloads ----

    /// <summary>Queues an incident's download (every camera, main stream).</summary>
    public void Download(string incidentKey)
    {
        if (_closed)
        {
            return;
        }
        try
        {
            QueuedDownload? answer = QueuedDownload.Parse(_core.DownloadIncident(RecordingsRequest.Download(incidentKey)));
            Notice = answer?.Existing == true ? "Already downloaded or downloading" : null;
        }
        catch (NvException e)
        {
            Notice = e.Status switch
            {
                NvStatus.NotConfigured => "There is no downloads folder",
                NvStatus.Limit => "Too many downloads; forget finished ones first",
                NvStatus.UnknownId => "That incident changed; try again",
                _ => $"Could not download {StatusText(e)}",
            };
        }
    }

    /// <summary>Pauses or resumes a download.</summary>
    public void Pause(string id, bool paused)
    {
        if (_closed)
        {
            return;
        }
        try
        {
            _core.DownloadPause(id, null, paused);
        }
        catch (NvException e)
        {
            Notice = $"Could not {(paused ? "pause" : "resume")} {StatusText(e)}";
        }
    }

    /// <summary>Retries a download's failed cameras.</summary>
    public void Retry(string id)
    {
        if (_closed)
        {
            return;
        }
        try
        {
            _core.DownloadRetry(id, null);
        }
        catch (NvException e)
        {
            Notice = $"Could not retry {StatusText(e)}";
        }
    }

    /// <summary>Forgets a download (its files stay); a running one needs a pause first.</summary>
    public void Forget(string id)
    {
        if (_closed)
        {
            return;
        }
        try
        {
            _core.DownloadForget(id, true);
        }
        catch (NvException e)
        {
            Notice = e.Status == NvStatus.InvalidArgument ? "Pause the download first" : $"Could not forget {StatusText(e)}";
        }
    }

    /// <summary>Moves a download's folder to the Recycle Bin, once the core has stopped writing there (its <c>download_removed</c> event).</summary>
    public void Trash(string id)
    {
        if (_closed)
        {
            return;
        }
        try
        {
            if (TrashAnswer.Parse(_core.DownloadTrash(id)) is { } answer)
            {
                _board.TrashRequested(id, answer.Folder);
                PublishDownloads();
            }
        }
        catch (NvException e)
        {
            Notice = $"Could not move it to the Recycle Bin {StatusText(e)}";
        }
    }

    /// <summary>Shows a download's files (or its folder) in Explorer.</summary>
    public void Reveal(DownloadProgress d)
    {
        IReadOnlyList<string> files = d.Outputs();
        if (files.Count > 0)
        {
            Revealer(files);
        }
        else if (d.Folder is { } folder)
        {
            Revealer([folder]);
        }
    }

    private void PublishDownloads()
    {
        _publishScheduled = false;
        Downloads = _board.Downloads;
        DownloadsChanged?.Invoke();
    }

    /// <summary>A due progress redraw (the app calls this from a timer after <see cref="ProgressPending"/> turned true).</summary>
    public void FlushProgress()
    {
        if (_publishScheduled)
        {
            PublishDownloads();
        }
    }

    /// <summary>Whether progress alone changed since the last publish (the app redraws within a quarter second).</summary>
    public bool ProgressPending => _publishScheduled;

    // ---- playback routing ----

    /// <summary>Opens a playback for <paramref name="player"/> (its events go to it).</summary>
    public OpenedPlayback OpenPlayback(string json, RecordingsPlayer player)
    {
        if (!Available || _closed)
        {
            throw new NvException(NvStatus.Unsupported, "nv_playback_open");
        }
        OpenedPlayback opened = Playback.Open(json);
        _players[opened.Playback] = new WeakReference<RecordingsPlayer>(player);
        return opened;
    }

    /// <summary>Closes a playback and releases its tiles.</summary>
    public void ClosePlayback(ulong playback, IReadOnlyList<ulong> tiles)
    {
        _players.Remove(playback);
        try
        {
            Playback.Playback.PlaybackClose(playback);
        }
        catch (NvException e) when (e.Status is NvStatus.UnknownId or NvStatus.Closed)
        {
        }
        Playback.Released(tiles);
    }

    /// <summary>Releases the tiles of a playback the core closed.</summary>
    public void ForgetPlaybackTiles(IReadOnlyList<ulong> tiles) => Playback.Released(tiles);

    /// <summary>Takes a camera out of a playback.</summary>
    public void RemovePlaybackCamera(ulong playback, ulong tile)
    {
        try
        {
            Playback.Playback.PlaybackRemoveCamera(playback, tile);
        }
        catch (NvException e) when (e.Status is NvStatus.UnknownId or NvStatus.Closed)
        {
        }
        Playback.Released([tile]);
    }

    /// <summary>Adds a camera to a playback; its tile, or null (with <see cref="Notice"/> set).</summary>
    public ulong? AddPlaybackCamera(ulong playback, RecordingCamera camera)
    {
        try
        {
            return Playback.AddCamera(playback, new PlaybackCamera(camera).Json);
        }
        catch (NvException e)
        {
            Notice = e.Status == NvStatus.Limit ? "At most four cameras play at once" : $"Could not add the camera {StatusText(e)}";
            return null;
        }
    }

    /// <summary>A playback call that returns at once (play, pause, seek, rate).</summary>
    public void PlaybackCall(Action<IPlayback> body)
    {
        if (_closed)
        {
            return;
        }
        try
        {
            body(Playback.Playback);
        }
        catch (NvException e)
        {
            if (e.Status != NvStatus.UnknownId)
            {
                Notice = $"The player could not do that {StatusText(e)}";
            }
        }
    }

    /// <summary>The playhead and state of a playback, read from memory.</summary>
    public (long PositionMs, NvPlaybackState State)? PlaybackPosition(ulong playback)
    {
        if (_closed)
        {
            return null;
        }
        try
        {
            return Playback.Playback.PlaybackPosition(playback);
        }
        catch (NvException)
        {
            return null;
        }
    }

    /// <summary>A playback tile's sound through the core's tile audio path.</summary>
    public void SetPlaybackAudio(ulong tile, bool audible, float volume)
    {
        if (!_closed)
        {
            Playback.SetAudio(tile, audible, volume);
        }
    }

    /// <summary>Routes a playback event to its window's player.</summary>
    public void Apply(PlaybackEvent e)
    {
        ulong id = e switch
        {
            PlaybackEvent.State s => s.Playback,
            PlaybackEvent.Camera c => c.Playback,
            PlaybackEvent.Seek k => k.Playback,
            PlaybackEvent.Closed z => z.Playback,
            _ => 0,
        };
        if (!_players.TryGetValue(id, out WeakReference<RecordingsPlayer>? w) || !w.TryGetTarget(out RecordingsPlayer? player))
        {
            return;
        }
        if (e is PlaybackEvent.Closed)
        {
            _players.Remove(id);
        }
        player.Apply(e);
    }

    // ---- events ----

    /// <summary>Applies a recordings event.</summary>
    public void Apply(RecordingsEvent e)
    {
        if (_closed)
        {
            return;
        }
        switch (e)
        {
            case RecordingsEvent.Index i:
                IndexStates[i.Device] = (i.State, i.PendingDays);
                break;
            case RecordingsEvent.DayFailed f:
                if (f.Camera is { } camera)
                {
                    if (!FailedDays.TryGetValue(camera, out HashSet<long>? set))
                    {
                        FailedDays[camera] = set = [];
                    }
                    set.Add(DeviceTime.DayStart(f.Day));
                }
                Changed();
                break;
            case RecordingsEvent.Changed:
                IndexRevision++;
                Changed();
                break;
            case RecordingsEvent.IncidentsChanged:
                IncidentsRevision++;
                Changed();
                break;
            case RecordingsEvent.CameraGroupsChanged:
                ReloadGroups();
                Changed();
                break;
            case RecordingsEvent.Thumbnail t:
                ThumbnailEvent(t.Key, t.State, t.Reason);
                break;
            case RecordingsEvent.Download d:
                // State changes show at once; progress alone is published by FlushProgress within a quarter second.
                if (_board.Apply(d.Progress))
                {
                    PublishDownloads();
                }
                else
                {
                    _publishScheduled = true;
                }
                break;
            case RecordingsEvent.SoundWatch w:
                if (IsListening(w.Camera))
                {
                    ListenStates[w.Camera] = (w.State, w.Reason);
                    Changed();
                }
                break;
            case RecordingsEvent.SoundAnalyze a:
                ApplyAnalysis(a);
                break;
            case RecordingsEvent.DownloadRemoved r:
                if (_board.Removed(r.Id, r.Trash, r.Folder) is { } folder)
                {
                    Trasher(folder);
                }
                PublishDownloads();
                break;
        }
    }

    private void ThumbnailEvent(ThumbnailKey key, string state, string? reason)
    {
        // Only what a view asked for (another window's request counts too).
        if (Thumbnails[key] is not ThumbnailState.Pending)
        {
            return;
        }
        switch (state)
        {
            case "ready":
                FetchThumbnail(key);
                break;
            case "empty":
                Thumbnails.Set(key, new ThumbnailState.Empty());
                break;
            case "dropped":
                Thumbnails.Set(key, new ThumbnailState.Dropped());
                break;
            default:
                Thumbnails.Set(key, new ThumbnailState.Failed(reason));
                break;
        }
        Changed();
    }

    /// <summary>Routes one raw core event (JSON) to recordings or playback; false when it is neither.</summary>
    public bool Route(ReadOnlySpan<byte> json)
    {
        if (RecordingsEvent.Parse(json) is { } r)
        {
            Apply(r);
            return true;
        }
        if (PlaybackEvent.Parse(json) is { } p)
        {
            Apply(p);
            return true;
        }
        return false;
    }
}
