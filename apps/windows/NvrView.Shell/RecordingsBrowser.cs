// What one Recordings window shows (docs/UX.md "Recordings"): a day of the
// enabled cameras, its incidents with the filters, the timeline lanes, the
// calendar's month, and the player. Asks the core for the day it shows
// (nv_recordings_want) and for a month the calendar pages to (months, minor
// 7), and holds today while today is shown, under its own hold number;
// releases the hold when it closes or moves to another day. Every query
// answers from the core's memory; device work arrives as events, after which
// the service calls Reload. Selecting one incident plays it (RecordingsPlayer).
// The same model as the macOS app's RecordingsBrowser.

namespace NvrView.Shell;

/// <summary>What the middle of the window shows.</summary>
public enum RecordingsCenter
{
    Player,
    Downloads,
}

/// <summary>The scrubber's span: the selected incident (with its margins) or the whole day.</summary>
public enum ScrubScope
{
    Incident,
    Day,
}

/// <summary>Which incidents the list shows: all, a camera group's, or a camera's.</summary>
public abstract record IncidentScope
{
    /// <summary>Every camera.</summary>
    public sealed record All : IncidentScope;

    /// <summary>A camera group's cameras.</summary>
    public sealed record Group(string Name) : IncidentScope;

    /// <summary>One camera.</summary>
    public sealed record Camera(RecordingCamera Value) : IncidentScope;

    /// <summary>All cameras.</summary>
    public static IncidentScope Everything { get; } = new All();
}

/// <summary>One timeline lane: a camera's recording spans and events for the day.</summary>
public sealed record TimelineLane(
    RecordingCamera Camera, string Name, IReadOnlyList<RecordingSpan> Spans, IReadOnlyList<RecordedEvent> Events, bool Failed);

/// <summary>One recordings window's state (see the file comment).</summary>
public sealed class RecordingsBrowser
{
    private readonly Guid _token;
    private (long Day, HashSet<RecordingCamera> Cameras)? _asked;
    private readonly Dictionary<long, HashSet<RecordingCamera>> _askedMonths = [];
    private List<RecordingCamera> _heldCameras = [];
    private HashSet<KindFilter> _kindFilters = [];
    private IncidentScope _scope = IncidentScope.Everything;
    private bool _lockedOnly;
    private HashSet<string> _selection = [];

    /// <param name="model">The shared recordings model.</param>
    /// <param name="files">Opens the files of a follow cut or grid (none: they cannot be opened).</param>
    /// <param name="audio">The edit's sound (none: silent).</param>
    public RecordingsBrowser(RecordingsService model, IEditFilesFactory? files = null, IEditAudio? audio = null)
    {
        Model = model;
        HoldId = model.NewHold();
        Player = new RecordingsPlayer(model);
        EditPlayer = new EditPlayer(model, Player, files ?? new NoEditFiles(), audio ?? new NoEditAudio());
        long today = model.Today;
        Day = today;
        Month = DeviceTime.Month(today).Start;
        _token = model.Subscribe(Reload);
        Show(today);
    }

    /// <summary>The shared recordings model.</summary>
    public RecordingsService Model { get; }

    /// <summary>This window's <c>nv_recordings_hold_today</c> number.</summary>
    public ulong HoldId { get; }

    /// <summary>The player of this window.</summary>
    public RecordingsPlayer Player { get; }

    /// <summary>The follow cut and grids of this window.</summary>
    public EditPlayer EditPlayer { get; }

    /// <summary>What the player shows now.</summary>
    public PlayerView PlayerView { get; private set; } = PlayerView.Cameras;

    /// <summary>Raised on the UI thread when anything the window shows changed.</summary>
    public event Action? Changed;

    /// <summary>The shown day (device-local, its first second).</summary>
    public long Day { get; private set; }

    /// <summary>The calendar's month (its first second).</summary>
    public long Month { get; private set; }

    /// <summary>What the middle of the window shows.</summary>
    public RecordingsCenter Center { get; set; } = RecordingsCenter.Player;

    /// <summary>The scrubber's span.</summary>
    public ScrubScope ScrubScope { get; set; } = ScrubScope.Incident;

    /// <summary>The side panes the user wants (a narrow window may still hide them; RecordingsPanes.Shown).</summary>
    public bool ShowSidebar { get; set; } = true;

    /// <summary>The inspector the user wants.</summary>
    public bool ShowInspector { get; set; } = true;

    /// <summary>The part of the day the timeline lanes show (zoom 1: the whole day in the lanes' width).</summary>
    public TimelineViewport Timeline
    {
        get => _timeline;
        set
        {
            bool zoomed = value.Zoom != _timeline.Zoom;
            _timeline = value;
            if (zoomed)
            {
                ReloadLoudness();
            }
        }
    }

    private TimelineViewport _timeline = new();
    private Dictionary<RecordingCamera, LoudnessLane> _loudness = [];
    private (long Day, long Bucket, List<RecordingCamera> Cameras, int Revision)? _loudnessAsked;
    private double _lanesWidth = 600;

    /// <summary>The lanes' width (the window reports it), for the loudness bucket.</summary>
    public double LanesWidth
    {
        get => _lanesWidth;
        set
        {
            if (Math.Abs(value - _lanesWidth) >= 1)
            {
                _lanesWidth = value;
                ReloadLoudness();
            }
        }
    }

    /// <summary>About one loudness value per two points at the zoom shown.</summary>
    public long LoudnessBucket => LoudnessScale.Bucket(DeviceTime.Day * _timeline.Visible, _lanesWidth);

    /// <summary>How often the loudness lanes were read (self-test).</summary>
    public int LoudnessReads { get; private set; }

    /// <summary>The loudness a lane shows (its camera's microphone), if any.</summary>
    public LoudnessLane? LoudnessOf(RecordingCamera camera) => _loudness.GetValueOrDefault(LoudnessScale.Microphone(camera));

    /// <summary>Analyzes the shown day's sound from the downloaded video, for one lane's camera or for all the searched cameras (the lanes).</summary>
    public void AnalyzeSound(RecordingCamera? camera = null) =>
        Model.AnalyzeSound(camera is { } c ? [c] : [.. Lanes.Select(l => l.Camera)], Day);

    /// <summary>Cancels a lane's analysis.</summary>
    public void CancelAnalysis(RecordingCamera camera) => Model.CancelAnalysis(camera, Day);

    /// <summary>A lane's sound analysis while it waits or runs.</summary>
    public AnalysisEntry? AnalysisOf(RecordingCamera camera) => Model.Analysis(camera, Day) is { Active: true } e ? e : null;

    /// <summary>Whether any lane can still be asked to analyze.</summary>
    public bool CanAnalyze => Lanes.Any(l => !Model.IsAnalyzing(l.Camera, Day));

    /// <summary>
    /// Reads the shown day's loudness lanes when the day, the cameras, the bucket, or the index changed
    /// (<paramref name="force"/>: also when nothing did; the window does so now and then for today, since live
    /// seconds arrive with no event).
    /// </summary>
    public void ReloadLoudness(bool force = false)
    {
        long bucket = LoudnessBucket;
        List<RecordingCamera> cams = Cameras;
        int revision = Model.IndexRevision;
        if (!force && _loudnessAsked is { } a && a.Day == Day && a.Bucket == bucket && a.Cameras.SequenceEqual(cams) && a.Revision == revision)
        {
            return;
        }
        _loudnessAsked = (Day, bucket, cams, revision);
        LoudnessReads++;
        LoudnessAnswer? answer = Model.Loudness(Day, Day + DeviceTime.Day - 1, bucket, LoudnessScale.Microphones(cams));
        _loudness = (answer?.Lanes ?? []).GroupBy(l => l.Camera).ToDictionary(g => g.Key, g => g.First());
    }

    /// <summary>Days of the calendar's month that have recordings on an enabled camera.</summary>
    public IReadOnlySet<long> RecordedDays { get; private set; } = new HashSet<long>();

    /// <summary>The day's timeline lanes.</summary>
    public IReadOnlyList<TimelineLane> Lanes { get; private set; } = [];

    /// <summary>The day's incidents in scope, newest first, before the kind and lock filters.</summary>
    public IReadOnlyList<Incident> DayIncidents { get; private set; } = [];

    /// <summary>What the list shows.</summary>
    public IReadOnlyList<Incident> Incidents { get; private set; } = [];

    /// <summary>Whether this window holds today.</summary>
    public bool Holding { get; private set; }

    /// <summary>A message from the last request, if it was refused.</summary>
    public string? Message { get; set; }

    /// <summary>How often the window re-read the index (self-test).</summary>
    public int Reloads { get; private set; }

    /// <summary>Months asked for alone, in order (self-test).</summary>
    public List<long> MonthRequests { get; } = [];

    /// <summary>The kinds the list is limited to (none: all).</summary>
    public IReadOnlySet<KindFilter> ActiveKinds => _kindFilters;

    /// <summary>Turns a kind filter on or off.</summary>
    public void SetKindFilter(KindFilter filter, bool on)
    {
        if (on ? _kindFilters.Add(filter) : _kindFilters.Remove(filter))
        {
            ApplyFilters();
            Changed?.Invoke();
        }
    }

    /// <summary>Which incidents the list shows.</summary>
    public IncidentScope Scope
    {
        get => _scope;
        set
        {
            if (!Equals(_scope, value))
            {
                _scope = value;
                ReloadIncidents();
                Changed?.Invoke();
            }
        }
    }

    /// <summary>Only locked incidents.</summary>
    public bool LockedOnly
    {
        get => _lockedOnly;
        set
        {
            if (_lockedOnly != value)
            {
                _lockedOnly = value;
                ApplyFilters();
                Changed?.Invoke();
            }
        }
    }

    /// <summary>Selected incidents, by key. One selected incident plays.</summary>
    public IReadOnlySet<string> Selection
    {
        get => _selection;
        set
        {
            if (_selection.SetEquals(value))
            {
                return;
            }
            _selection = [.. value];
            Player.FailedFor = null;
            SyncPlayer();
            Changed?.Invoke();
        }
    }

    /// <summary>Whether the shown day is today.</summary>
    public bool IsToday => Day == Model.Today;

    /// <summary>The cameras this window shows (every enabled camera).</summary>
    public List<RecordingCamera> Cameras => Model.EnabledCameras;

    // ---- navigation ----

    /// <summary>Shows a day: asks the core for it, holds today only while it is today.</summary>
    public void Show(long newDay)
    {
        long d = DeviceTime.DayStart(newDay);
        if (d > Model.Today)
        {
            return;
        }
        if (d != Day)
        {
            _selection = [];
            Model.CancelThumbnails();
            CloseEdit();
            Player.Close();
        }
        Day = d;
        Month = DeviceTime.Month(d).Start;
        _asked = null;
        Reload();
    }

    /// <summary>Pages the calendar; a month other than the shown day's is asked for alone (its calendar only), once per set of cameras.</summary>
    public void ShowMonth(int adding)
    {
        long next = DeviceTime.Month(DeviceTime.MonthAdding(Month, adding)).Start;
        if (next > Model.Today)
        {
            return;
        }
        Month = next;
        AskMonth();
        ReloadCalendar();
        Changed?.Invoke();
    }

    private void AskMonth()
    {
        if (Month == DeviceTime.Month(Day).Start)
        {
            return;
        }
        List<RecordingCamera> cams = Cameras;
        HashSet<RecordingCamera> known = _askedMonths.GetValueOrDefault(Month) ?? [];
        RecordingCamera[] fresh = [.. cams.Where(c => !known.Contains(c))];
        if (fresh.Length == 0)
        {
            return;
        }
        if (Model.WantMonths(fresh, [Month]) is { } refused)
        {
            Message = refused;
            return;
        }
        if (!_askedMonths.TryGetValue(Month, out HashSet<RecordingCamera>? set))
        {
            _askedMonths[Month] = set = [];
        }
        set.UnionWith(cams);
        MonthRequests.Add(Month);
    }

    /// <summary>The previous day.</summary>
    public void PreviousDay() => Show(Day - DeviceTime.Day);

    /// <summary>The next day (not past today).</summary>
    public void NextDay() => Show(Day + DeviceTime.Day);

    /// <summary>Searches the shown day again on the device.</summary>
    public void Refresh()
    {
        Message = Model.Want(Cameras, [Day], force: true);
        _asked = (Day, [.. Cameras]);
        Changed?.Invoke();
    }

    /// <summary>The window closes: its playback closes, its hold ends, its thumbnail requests are dropped.</summary>
    public void Close()
    {
        CloseEdit();
        Player.Close();
        if (Holding)
        {
            Model.Hold(HoldId, null);
        }
        Holding = false;
        _heldCameras = [];
        Model.Unsubscribe(_token);
        Model.CancelThumbnails();
    }

    // ---- reading the index ----

    /// <summary>Re-reads everything the window shows (after an event, a navigation, or an enablement change). Asks for cameras newly enabled.</summary>
    public void Reload()
    {
        Reloads++;
        List<RecordingCamera> cameras = Cameras;
        HashSet<RecordingCamera> cams = [.. cameras];
        if (_asked is not { } asked || asked.Day != Day || !cams.IsSubsetOf(asked.Cameras))
        {
            RecordingCamera[] fresh = _asked is { } a && a.Day == Day ? [.. cameras.Where(c => !a.Cameras.Contains(c))] : [.. cameras];
            // Remembered before the call, so an event it makes (delivered at once) does not ask again.
            _asked = (Day, cams);
            if (fresh.Length > 0)
            {
                Message = Model.Want(fresh, [Day]);
            }
        }
        AskMonth();
        UpdateHold();
        ReloadCalendar();
        ReloadLanes();
        ReloadLoudness();
        ReloadIncidents();
        SyncPlayer();
        Changed?.Invoke();
    }

    // Holds today (for the enabled cameras) while today is shown; the core is told only when that changes.
    private void UpdateHold()
    {
        List<RecordingCamera> cams = IsToday ? Cameras : [];
        if (cams.Count > 0)
        {
            if (_heldCameras.SequenceEqual(cams))
            {
                return;
            }
            Model.Hold(HoldId, cams);
            _heldCameras = cams;
            Holding = true;
        }
        else if (Holding)
        {
            Model.Hold(HoldId, null);
            _heldCameras = [];
            Holding = false;
        }
    }

    private void ReloadCalendar()
    {
        (long start, long next) = DeviceTime.Month(Month);
        RecordedDays = Model.Calendar(start, next - 1, Cameras)?.AllDays ?? new HashSet<long>();
    }

    private void ReloadLanes()
    {
        long end = Day + DeviceTime.Day - 1;
        List<RecordingCamera> cameras = Cameras;
        var spans = Model.Spans(Day, end, cameras)?.Spans ?? [];
        var events = Model.Events(Day, end, cameras)?.Events ?? [];
        Lanes = [.. cameras.Select(c => new TimelineLane(
            c, CameraName(c), spans.FirstOrDefault(s => s.Camera == c).Spans ?? [], [.. events.Where(e => e.Camera == c)],
            Model.FailedDays.GetValueOrDefault(c)?.Contains(Day) == true))];
    }

    private void ReloadIncidents()
    {
        long end = Day + DeviceTime.Day - 1;
        List<RecordingCamera>? cams = null;
        string? group = null;
        switch (_scope)
        {
            case IncidentScope.Group g:
                group = g.Name;
                break;
            case IncidentScope.Camera c:
                cams = [c.Value];
                break;
            default:
                cams = Cameras is { Count: > 0 } all ? all : null;
                break;
        }
        DayIncidents = Model.Incidents(Day, end, cams, group)?.Incidents ?? [];
        ApplyFilters();
    }

    private void ApplyFilters() =>
        Incidents = [.. DayIncidents.Where(i => NvrView.Shell.KindFilters.Passes(i, _kindFilters) && (!_lockedOnly || i.Locked))];

    /// <summary>The selected keys that name an incident of the day.</summary>
    public List<string> SelectedKeys => [.. DayIncidents.Select(i => i.Key).Where(_selection.Contains)];

    // ---- names ----

    /// <summary>A camera's name as the camera list shows it.</summary>
    public string CameraName(RecordingCamera c) => Model.CameraName(c);

    /// <summary>"Today, Sunday, October 11, 2026" style.</summary>
    public string DayTitle => (IsToday ? "Today, " : "") + DeviceTime.LongDay(Day);

    // ---- selection and the player ----

    /// <summary>The one selected incident, if exactly one is selected.</summary>
    public Incident? Selected
    {
        get
        {
            List<string> keys = SelectedKeys;
            return keys.Count == 1 ? DayIncidents.FirstOrDefault(i => i.Key == keys[0]) : null;
        }
    }

    // Plays the selected incident from its start when it is not the one the
    // player plays; otherwise only follows its cameras (an edit added or
    // removed one) without moving the playhead.
    private void SyncPlayer()
    {
        if (Selected is not { } incident || incident.Key == Player.FailedFor)
        {
            return;
        }
        bool switched = Player.IncidentKey != incident.Key || Player.Playback is null;
        if (switched)
        {
            Player.Show(incident, Day);
        }
        else
        {
            Player.Follow(incident, Day);
        }
        SyncEdit(switched);
    }

    // ---- follow cut and grids ----

    /// <summary>The transport the controls drive now.</summary>
    public IPlayerTransport Driver => PlayerView == PlayerView.Cameras ? Player : EditPlayer;

    /// <summary>The incident a follow cut or grid would be made of, and its finished download.</summary>
    public (Incident Incident, DownloadProgress Download)? Editable =>
        (PlayingIncident ?? Selected) is { } incident && Model.FinishedDownload(incident.Key) is { } d ? (incident, d) : null;

    /// <summary>
    /// Shows the device playback, or a follow cut or grid of the incident's downloaded files from the playhead
    /// (the device playback pauses meanwhile and continues from the edit's position afterward).
    /// </summary>
    public void ShowView(PlayerView view)
    {
        if (view == PlayerView.Cameras)
        {
            if (PlayerView == PlayerView.Cameras)
            {
                return;
            }
            long at = EditPlayer.PositionMs;
            bool wasPlaying = EditPlayer.Transport.IsPlaying;
            CloseEdit();
            if (Player.Playback is not null && at > 0)
            {
                Player.Seek(at);
                if (wasPlaying)
                {
                    Player.Play();
                }
            }
            Changed?.Invoke();
            return;
        }
        if (view == PlayerView)
        {
            return;
        }
        if (Editable is not { } e)
        {
            Message = "Download the incident to see a follow cut or a grid";
            Changed?.Invoke();
            return;
        }
        long from = PlayerView == PlayerView.Cameras ? Player.PositionMs : EditPlayer.PositionMs;
        Player.Pause();
        PlayerView = view;
        EditPlayer.Open(view, e.Download.Download, from);
        if (EditPlayer.Message is { } refused)
        {
            Message = refused;
        }
        Changed?.Invoke();
    }

    // Another incident selected while a follow cut or grid shows: its own edit when it is downloaded, else back to the cameras.
    private void SyncEdit(bool started)
    {
        if (PlayerView == PlayerView.Cameras)
        {
            return;
        }
        if (Editable is not { } e)
        {
            CloseEdit();
            return;
        }
        // The device playback just started for the new incident waits.
        if (started || Player.Transport.IsPlaying)
        {
            Player.Pause();
        }
        if (EditPlayer.Download != e.Download.Download || EditPlayer.View != PlayerView)
        {
            EditPlayer.Open(PlayerView, e.Download.Download, null);
        }
    }

    private void CloseEdit()
    {
        EditPlayer.Close();
        PlayerView = PlayerView.Cameras;
    }

    /// <summary>A click on an incident row: selects it alone, or with Ctrl adds or removes it (several selected incidents can be merged).</summary>
    public void Click(string key, bool extend)
    {
        if (extend)
        {
            HashSet<string> next = [.. _selection];
            if (!next.Remove(key))
            {
                next.Add(key);
            }
            Selection = next;
        }
        else
        {
            Selection = new HashSet<string> { key };
        }
    }

    /// <summary>Selects the incident that holds an event.</summary>
    public void Select(RecordedEvent e)
    {
        Incident? hit = DayIncidents.FirstOrDefault(i =>
            i.Involves(e.Camera) && i.Events.Any(x => x.Camera == e.Camera && x.Start <= e.End && e.Start <= x.End));
        if (hit is not null)
        {
            Selection = new HashSet<string> { hit.Key };
        }
    }

    /// <summary>
    /// A click on a lane at <paramref name="seconds"/>: the player seeks there when it plays that camera;
    /// a click on an event of another camera selects (and so plays) its incident; anywhere else that camera plays alone from there.
    /// </summary>
    public void LaneClick(RecordingCamera camera, long seconds, RecordedEvent? near = null)
    {
        long ms = seconds * 1000;
        if (PlayerView != PlayerView.Cameras)
        {
            // Inside the follow cut's or grid's range, for one of its cameras: it seeks there. Anywhere else the cameras come back.
            if (EditPlayer.Plan is { } plan && plan.Involves(camera) && plan.StartMs <= ms && ms <= plan.EndMs)
            {
                EditPlayer.Seek(ms);
                EditPlayer.Play();
                return;
            }
            ShowView(PlayerView.Cameras);
        }
        if (Player.Cameras.Contains(camera) && Player.Day == Day && Player.Playback is not null)
        {
            Player.Seek(ms);
            Player.Play();
        }
        else if (near is not null && DayIncidents.Any(i => i.Involves(near.Camera)))
        {
            Select(near);
        }
        else
        {
            Selection = new HashSet<string>();
            Player.Show([camera], Day, ms);
        }
    }

    /// <summary>The incident the player is positioned for, if it is still of the day.</summary>
    public Incident? PlayingIncident =>
        Player.IncidentKey is { } key && Player.Playback is not null ? DayIncidents.FirstOrDefault(i => i.Key == key) : null;

    /// <summary>The second "Split Here" would split an incident at: the playhead's, when the player plays that incident and the playhead is inside it.</summary>
    public long? SplitSecond(Incident incident) =>
        Player.IncidentKey == incident.Key && Player.Playback is not null
            ? IncidentSplit.Second(incident.Start, incident.End, Player.PositionMs)
            : null;

    /// <summary>Splits an incident at the playhead.</summary>
    public void SplitHere(Incident incident)
    {
        if (SplitSecond(incident) is { } at)
        {
            Split(incident.Key, at);
        }
    }

    /// <summary>Joins the selected incidents.</summary>
    public void Merge()
    {
        List<string> keys = SelectedKeys;
        if (keys.Count >= 2 && Model.Edit(new IncidentEdit.Merge(keys)) is { } merged)
        {
            Selection = new HashSet<string>(merged.Take(1));
        }
    }

    /// <summary>Splits an incident at a second.</summary>
    public void Split(string key, long at)
    {
        if (Model.Edit(new IncidentEdit.Split(key, at)) is not { } keys)
        {
            return;
        }
        // The part that holds the playhead stays selected (and keeps playing).
        List<Incident> parts = [.. keys.Select(Model.IncidentByKey).OfType<Incident>()];
        Incident? keep = parts.FirstOrDefault(p => p.Start <= at && at <= p.End && Player.PositionMs >= p.Start * 1000);
        if (keep is not null)
        {
            Player.Renamed(keep.Key);
            Selection = new HashSet<string> { keep.Key };
        }
        else
        {
            Selection = new HashSet<string>(keys.Take(1));
        }
    }

    /// <summary>Names an incident (empty clears the name).</summary>
    public void Rename(string key, string name) => Model.Edit(new IncidentEdit.Rename(key, name));

    /// <summary>Drops an incident's edits.</summary>
    public void Revert(string key) => Model.Edit(new IncidentEdit.Revert(key));

    /// <summary>Adds a camera to an incident.</summary>
    public void Add(RecordingCamera camera, string key) => Model.Edit(new IncidentEdit.AddCamera(key, camera));

    /// <summary>Remove from Incident: the camera leaves the incident (and the player).</summary>
    public void Remove(RecordingCamera camera, string key)
    {
        if (Model.Edit(new IncidentEdit.RemoveCamera(key, camera)) is null)
        {
            return;
        }
        if (Player.IncidentKey == key)
        {
            Player.Remove(camera);
        }
    }

    /// <summary>Locks or unlocks an incident.</summary>
    public void SetLocked(string key, bool locked) => Model.Lock(key, locked);

    /// <summary>Queues an incident's download.</summary>
    public void Download(string key) => Model.Download(key);

    /// <summary>Cameras that could be added to an incident.</summary>
    public List<RecordingCamera> Addable(Incident i) => [.. Cameras.Where(c => !i.Involves(c))];

    /// <summary>The scope choices: all, each group, each camera.</summary>
    public List<(IncidentScope Scope, string Title)> Scopes =>
    [
        (IncidentScope.Everything, "All Cameras"),
        .. Model.Groups.Groups.Select(g => ((IncidentScope)new IncidentScope.Group(g.Name), g.Name)),
        .. Cameras.Select(c => ((IncidentScope)new IncidentScope.Camera(c), CameraName(c))),
    ];

    /// <summary>The scrubber's range in milliseconds: the incident with its margins, or the player's day.</summary>
    public (long Start, long End)? ScrubRange
    {
        get
        {
            if (PlayerView != PlayerView.Cameras)
            {
                return EditPlayer.Plan is { } plan ? (plan.StartMs, plan.EndMs) : null;
            }
            if (Player.Playback is null)
            {
                return null;
            }
            if (ScrubScope == ScrubScope.Incident && PlayingIncident is { } i)
            {
                return ((i.Start - i.Margin) * 1000, (i.End + i.Margin + 1) * 1000);
            }
            return (Player.StartMs, Player.EndMs);
        }
    }
}
