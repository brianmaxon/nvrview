// Recordings, part 1 (minor 5) without a core: a made-up recording store
// with the contract's JSON (docs/FFI-RECORDINGS.md, sections 1 to 5 and 8),
// for the recordings views (milestone P, W1) before they meet the real core,
// and for tests. The same model as the macOS FakeCore's: every enabled
// camera recorded every day all day, with a person at 08:00 and a vehicle at
// 17:00 (device-local seconds). Asking for days indexes them at once.
// Thumbnail fetches and downloads are queued and finish on
// CompleteRecordingWork() (or shortly after the call, unless
// RecordingWorkIsManual): a fetch makes a stand-in keyframe, a download goes
// to done with an output path that is never written. Edits apply rename and
// revert; the other operations throw NV_ERR_UNSUPPORTED.

using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>A made-up recording store answering <see cref="IRecordings"/>; events go to the sink as JSON.</summary>
public sealed partial class FakeRecordings(Action<string> sink) : IRecordings, IEditSound
{
    private const long Day = 86_400;
    private readonly object _gate = new();
    private bool _configured;
    private string? _downloadsDir;
    // Allowed cameras per device key (null: every channel, first lens).
    private readonly Dictionary<string, List<RecordingCamera>?> _enabled = [];
    private readonly Dictionary<string, SortedSet<long>> _indexed = [];
    // Days that show in the calendar (a month asked for alone adds only these).
    private readonly Dictionary<string, SortedSet<long>> _calendar = [];
    private int _monthRequests;
    private long _revision;
    private long _incidentsRevision;
    private readonly Dictionary<string, string> _names = [];
    private readonly HashSet<string> _locks = [];
    private JsonNode _groups = new JsonObject();
    private long _groupsRevision;
    private readonly Dictionary<ulong, List<RecordingCamera>> _holds = [];
    private readonly Dictionary<ThumbnailKey, RecordedThumbnail> _thumbnails = [];
    private readonly List<ThumbnailKey> _pendingThumbnails = [];
    private readonly List<FakeDownload> _downloads = [];
    private int _nextDownload = 1;

    /// <summary>Thumbnails and downloads finish only on <see cref="CompleteRecordingWork"/> (tests).</summary>
    public bool RecordingWorkIsManual { get; set; }

    /// <summary>The bytes a fetch makes: a stand-in H.264 keyframe the platform decoder cannot read.</summary>
    public static readonly byte[] StandInKeyframe = [0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0, 0, 0, 1, 0x68, 0xce, 0, 0, 0, 1, 0x65, 0x88];

    private sealed class FakeDownload
    {
        public required string Id;
        public required string Incident;
        public required string Folder;
        public required List<RecordingCamera> Cameras;
        public string State = "queued";
        public bool Paused;
    }

    private sealed record FakeEvent(RecordingCamera Camera, string Kind, long Start, long End, string Source = "device")
    {
        public string Key => $"i-{Start}-{Camera.Device}-{Camera.Channel}-{Camera.Lens}";

        // The content hash's stand-in: the same in every run.
        public string Id
        {
            get
            {
                ulong h = 0xcbf29ce484222325;
                foreach (byte b in Encoding.UTF8.GetBytes(Key))
                {
                    h = unchecked((h ^ b) * 0x100000001b3);
                }
                return $"{Start}-{h:x}";
            }
        }
    }

    private static NvException Error(NvStatus status, string function) => new(status, function);

    private static JsonObject Object(string json, string function)
    {
        try
        {
            return JsonNode.Parse(json) as JsonObject ?? throw Error(NvStatus.InvalidArgument, function);
        }
        catch (JsonException)
        {
            throw Error(NvStatus.InvalidArgument, function);
        }
    }

    private static RecordingCamera Camera(JsonNode? node, string function)
    {
        if (node is not JsonObject o || o["device"]?.GetValueKind() != JsonValueKind.String ||
            o["channel"] is not JsonValue ch || !ch.TryGetValue(out int channel) || channel is < 0 or > 255)
        {
            throw Error(NvStatus.InvalidArgument, function);
        }
        int lens = o["lens"] is JsonValue l && l.TryGetValue(out int n) ? n : 0;
        if (lens is < 0 or > 1)
        {
            throw Error(NvStatus.InvalidArgument, function);
        }
        return new RecordingCamera(o["device"]!.GetValue<string>(), channel, lens);
    }

    private static JsonObject CameraNode(RecordingCamera c) =>
        new() { ["device"] = c.Device, ["channel"] = c.Channel, ["lens"] = c.Lens };

    private static FakeEvent[] EventsOf(RecordingCamera c, long day) =>
    [
        new(c, "people", day + 8 * 3600 + c.Channel, day + 8 * 3600 + 15),
        new(c, "vehicle", day + 17 * 3600 + c.Channel, day + 17 * 3600 + 20),
    ];

    /// <summary>For FakePlayback: whether the store is configured and the device enabled.</summary>
    internal (bool Configured, bool Enabled) PlaybackCheck(string device)
    {
        lock (_gate)
        {
            return (_configured, _enabled.ContainsKey(device));
        }
    }

    private void Configured(string function)
    {
        if (!_configured)
        {
            throw Error(NvStatus.NotConfigured, function);
        }
    }

    private bool Allowed(RecordingCamera c) =>
        _enabled.TryGetValue(c.Device, out List<RecordingCamera>? list) && (list?.Contains(c) ?? c.Lens == 0);

    private List<RecordingCamera> CamerasOf(string device) =>
        _enabled[device] ?? [.. Enumerable.Range(0, 4).Select(ch => new RecordingCamera(device, ch))];

    private List<RecordingCamera> RequestCameras(JsonObject o, string function)
    {
        if (o["cameras"] is not JsonArray list)
        {
            throw Error(NvStatus.InvalidArgument, function);
        }
        var cameras = new List<RecordingCamera>();
        foreach (JsonNode? n in list)
        {
            RecordingCamera c = Camera(n, function);
            if (!Allowed(c))
            {
                throw Error(NvStatus.InvalidArgument, function);
            }
            if (!cameras.Contains(c))
            {
                cameras.Add(c);
            }
        }
        return cameras;
    }

    private sealed record Query(long Start, long End, List<RecordingCamera>? Cameras, HashSet<string>? Kinds, int Limit);

    private static Query ParseQuery(string json, string function)
    {
        JsonObject o = Object(json, function);
        if (o["start"] is not JsonValue s || !s.TryGetValue(out long start) ||
            o["end"] is not JsonValue e || !e.TryGetValue(out long end) || end < start)
        {
            throw Error(NvStatus.InvalidArgument, function);
        }
        List<RecordingCamera>? cameras = o["cameras"] is JsonArray a ? [.. a.Select(n => Camera(n, function))] : null;
        HashSet<string>? kinds = o["kinds"] is JsonArray k ? [.. k.Select(n => n?.GetValue<string>() ?? "")] : null;
        int limit = o["limit"] is JsonValue l && l.TryGetValue(out int n) ? Math.Clamp(n, 0, 100_000) : 5000;
        return new Query(start, end, cameras, kinds, limit);
    }

    private List<FakeEvent> Matching(Query q)
    {
        var all = new List<FakeEvent>();
        foreach ((string device, SortedSet<long> days) in _indexed)
        {
            if (!_enabled.ContainsKey(device))
            {
                continue;
            }
            foreach (RecordingCamera c in CamerasOf(device).Where(c => q.Cameras?.Contains(c) ?? true))
            {
                foreach (long d in days)
                {
                    all.AddRange(EventsOf(c, d).Where(e =>
                        e.End >= q.Start && e.Start <= q.End && (q.Kinds?.Contains(e.Kind) ?? true)));
                }
                all.AddRange(_labels
                    .Where(l => l.Camera == c && days.Contains(DeviceTime.DayStart(l.Start)) && l.End >= q.Start && l.Start <= q.End &&
                        (q.Kinds?.Contains(l.Kind) ?? true))
                    .Select(l => new FakeEvent(c, l.Kind, l.Start, l.End, SoundKinds.LocalAudio)));
            }
        }
        return [.. all.OrderBy(e => e.Start).ThenBy(e => e.Key, StringComparer.Ordinal)];
    }

    private List<FakeEvent> AllIncidents() => Matching(new Query(0, long.MaxValue, null, null, int.MaxValue));

    private static JsonObject EventNode(FakeEvent e) => new()
    {
        ["camera"] = CameraNode(e.Camera),
        ["kind"] = e.Kind,
        ["source"] = e.Source,
        ["start"] = e.Start,
        ["end"] = e.End,
    };

    private JsonObject IncidentNode(FakeEvent e) => new()
    {
        ["key"] = e.Key,
        ["id"] = e.Id,
        ["name"] = _names.TryGetValue(e.Key, out string? name) ? name : null,
        ["group"] = null,
        ["start"] = e.Start,
        ["end"] = e.End,
        ["margin_s"] = 5,
        ["kinds"] = new JsonArray(e.Kind),
        ["growing"] = false,
        ["edited"] = _names.ContainsKey(e.Key),
        ["locked"] = _locks.Contains(e.Key),
        ["cameras"] = new JsonArray(new JsonObject
        {
            ["camera"] = CameraNode(e.Camera),
            ["name"] = $"Camera {e.Camera.Channel + 1}",
            ["intervals"] = new JsonArray(new JsonArray(e.Start, e.End)),
            ["download"] = new JsonArray(e.Start - 5, e.End + 5),
            ["added"] = false,
        }),
        ["events"] = new JsonArray(EventNode(e)),
        ["downloads"] = new JsonArray([.. _downloads.Where(d => d.Incident == e.Key).Select(d => (JsonNode)d.Id)]),
    };

    private void Emit(JsonObject o) => sink(o.ToJsonString());

    // ---- configuration and the index ----------------------------------

    /// <inheritdoc/>
    public void RecordingsConfigure(string json)
    {
        JsonObject o = Object(json, "nv_recordings_configure");
        if (o["downloads_dir"] is { } d && d.GetValueKind() != JsonValueKind.String)
        {
            throw Error(NvStatus.InvalidArgument, "nv_recordings_configure");
        }
        lock (_gate)
        {
            _configured = true;
            _downloadsDir = o["downloads_dir"]?.GetValue<string>();
        }
    }

    /// <inheritdoc/>
    public void RecordingsEnable(string deviceKey, string? json)
    {
        const string F = "nv_recordings_enable";
        if (string.IsNullOrEmpty(deviceKey) || Encoding.UTF8.GetByteCount(deviceKey) > 256)
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        List<RecordingCamera>? cameras = null;
        if (json is not null && Object(json, F)["channels"] is JsonArray list)
        {
            cameras = [];
            foreach (JsonNode? n in list)
            {
                if (n is not JsonObject c || c["channel"] is not JsonValue ch || !ch.TryGetValue(out int channel))
                {
                    throw Error(NvStatus.InvalidArgument, F);
                }
                cameras.Add(new RecordingCamera(deviceKey, channel, c["lens"] is JsonValue l && l.TryGetValue(out int lens) ? lens : 0));
            }
        }
        lock (_gate)
        {
            Configured(F);
            if (json is null)
            {
                _enabled.Remove(deviceKey);
                foreach (List<RecordingCamera> hold in _holds.Values)
                {
                    hold.RemoveAll(c => c.Device == deviceKey);
                }
            }
            else
            {
                _enabled[deviceKey] = cameras;
            }
        }
    }

    /// <summary>How many months were asked for alone (tests).</summary>
    public int MonthRequests
    {
        get
        {
            lock (_gate)
            {
                return _monthRequests;
            }
        }
    }

    private static long MonthStart(long s)
    {
        DateTime d = DateTimeOffset.FromUnixTimeSeconds(s).UtcDateTime;
        return new DateTimeOffset(d.Year, d.Month, 1, 0, 0, 0, TimeSpan.Zero).ToUnixTimeSeconds();
    }

    private static long NextMonth(long monthStart) =>
        new DateTimeOffset(DateTimeOffset.FromUnixTimeSeconds(monthStart).UtcDateTime.AddMonths(1), TimeSpan.Zero).ToUnixTimeSeconds();

    private static (long Start, long End) Span(SortedSet<long> days, SortedSet<long> months)
    {
        long start = long.MaxValue, end = 0;
        foreach (long d in days)
        {
            start = Math.Min(start, d);
            end = Math.Max(end, d + Day - 1);
        }
        foreach (long m in months)
        {
            start = Math.Min(start, m);
            end = Math.Max(end, NextMonth(m) - 1);
        }
        return (start, end);
    }

    /// <inheritdoc/>
    public void RecordingsWant(string json)
    {
        const string F = "nv_recordings_want";
        JsonObject o = Object(json, F);
        if (o["days"] is not JsonArray && o["months"] is not JsonArray)
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        var days = new SortedSet<long>();
        foreach (JsonNode? n in o["days"] as JsonArray ?? [])
        {
            if (n is not JsonValue v || !v.TryGetValue(out long s) || s < 0)
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            days.Add(s - s % Day);
        }
        var months = new SortedSet<long>();
        foreach (JsonNode? n in o["months"] as JsonArray ?? [])
        {
            if (n is not JsonValue v || !v.TryGetValue(out long s) || s < 0)
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            months.Add(MonthStart(s));
        }
        if (days.Count > 31 || months.Count > 12)
        {
            throw Error(NvStatus.Limit, F);
        }
        // A day's month calendar comes with it, as on the core; months alone bring only their calendars.
        long today = DateTimeOffset.UtcNow.ToUnixTimeSeconds() / Day * Day;
        var calendarDays = new SortedSet<long>();
        foreach (long m in days.Select(MonthStart).Concat(months).Distinct())
        {
            for (long d = m; d < NextMonth(m) && d <= today; d += Day)
            {
                calendarDays.Add(d);
            }
        }
        (long Start, long End) span = Span(days, months);
        var changed = new List<JsonObject[]>();
        long incidents;
        lock (_gate)
        {
            Configured(F);
            List<RecordingCamera> cameras = RequestCameras(o, F);
            foreach (string device in cameras.Select(c => c.Device).Distinct().Order(StringComparer.Ordinal))
            {
                if (!_indexed.TryGetValue(device, out SortedSet<long>? set))
                {
                    _indexed[device] = set = [];
                }
                set.UnionWith(days);
                if (!_calendar.TryGetValue(device, out SortedSet<long>? cal))
                {
                    _calendar[device] = cal = [];
                }
                cal.UnionWith(calendarDays);
                _monthRequests += months.Count;
                _revision++;
                changed.Add(
                [
                    new() { ["type"] = "recordings_index", ["device"] = device, ["state"] = "busy", ["pending_days"] = days.Count, ["revision"] = _revision - 1 },
                    new() { ["type"] = "recordings_changed", ["device"] = device, ["revision"] = _revision, ["start"] = span.Start, ["end"] = span.End },
                    new() { ["type"] = "recordings_index", ["device"] = device, ["state"] = "idle", ["pending_days"] = 0, ["revision"] = _revision },
                ]);
            }
            if (changed.Count > 0)
            {
                _incidentsRevision++;
            }
            incidents = _incidentsRevision;
        }
        foreach (JsonObject e in changed.SelectMany(x => x))
        {
            Emit(e);
        }
        if (changed.Count > 0)
        {
            Emit(new JsonObject { ["type"] = "incidents_changed", ["revision"] = incidents });
        }
    }

    /// <inheritdoc/>
    public void RecordingsHoldToday(ulong hold, string? json)
    {
        const string F = "nv_recordings_hold_today";
        if (hold == 0)
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        JsonObject? o = json is null ? null : Object(json, F);
        lock (_gate)
        {
            Configured(F);
            if (o is null)
            {
                _holds.Remove(hold);
            }
            else
            {
                _holds[hold] = RequestCameras(o, F);
            }
        }
    }

    /// <summary>The cameras each hold keeps fresh (for tests).</summary>
    public IReadOnlyDictionary<ulong, List<RecordingCamera>> Holds
    {
        get
        {
            lock (_gate)
            {
                return new Dictionary<ulong, List<RecordingCamera>>(_holds);
            }
        }
    }

    /// <inheritdoc/>
    public string RecordingsStatus()
    {
        lock (_gate)
        {
            Configured("nv_recordings_status");
            var devices = new JsonArray([.. _enabled.Keys.Order(StringComparer.Ordinal).Select(k => (JsonNode)new JsonObject
            {
                ["device"] = k,
                ["state"] = "idle",
                ["pending_days"] = 0,
                ["revision"] = _revision,
            })]);
            return new JsonObject { ["revision"] = _revision, ["devices"] = devices }.ToJsonString();
        }
    }

    /// <inheritdoc/>
    public string RecordingsCalendar(string query)
    {
        Query q = ParseQuery(query, "nv_recordings_calendar");
        lock (_gate)
        {
            Configured("nv_recordings_calendar");
            var lists = new List<JsonNode>();
            foreach ((string device, SortedSet<long> indexed) in _indexed.OrderBy(p => p.Key, StringComparer.Ordinal))
            {
                if (!_enabled.ContainsKey(device))
                {
                    continue;
                }
                SortedSet<long> days = _calendar.TryGetValue(device, out SortedSet<long>? cal) ? [.. indexed.Union(cal)] : indexed;
                foreach (RecordingCamera c in CamerasOf(device).Where(c => q.Cameras?.Contains(c) ?? true))
                {
                    long[] inRange = [.. days.Where(d => d + Day - 1 >= q.Start && d <= q.End)];
                    if (inRange.Length > 0)
                    {
                        lists.Add(new JsonObject { ["camera"] = CameraNode(c), ["days"] = new JsonArray([.. inRange.Select(d => (JsonNode)d)]) });
                    }
                }
            }
            return new JsonObject
            {
                ["revision"] = _revision,
                ["truncated"] = lists.Count > q.Limit,
                ["days"] = new JsonArray([.. lists.Take(q.Limit)]),
            }.ToJsonString();
        }
    }

    /// <inheritdoc/>
    public string RecordingsSpans(string query)
    {
        Query q = ParseQuery(query, "nv_recordings_spans");
        lock (_gate)
        {
            Configured("nv_recordings_spans");
            var lanes = new JsonArray();
            int count = 0;
            bool truncated = false;
            foreach ((string device, SortedSet<long> days) in _indexed.OrderBy(p => p.Key, StringComparer.Ordinal))
            {
                if (!_enabled.ContainsKey(device))
                {
                    continue;
                }
                foreach (RecordingCamera c in CamerasOf(device).Where(c => q.Cameras?.Contains(c) ?? true))
                {
                    var spans = new JsonArray();
                    foreach (long d in days)
                    {
                        for (long h = 0; h < 24; h++)
                        {
                            long s = d + h * 3600;
                            if (s + 3599 < q.Start || s > q.End)
                            {
                                continue;
                            }
                            if (count == q.Limit)
                            {
                                truncated = true;
                                continue;
                            }
                            count++;
                            spans.Add((JsonNode)new JsonObject
                            {
                                ["start"] = s,
                                ["end"] = s + 3599,
                                ["bytes"] = 1L << 28,
                                ["causes"] = new JsonArray("sched"),
                                ["tier"] = "main",
                            });
                        }
                    }
                    if (spans.Count > 0)
                    {
                        lanes.Add((JsonNode)new JsonObject { ["camera"] = CameraNode(c), ["spans"] = spans });
                    }
                }
            }
            return new JsonObject { ["revision"] = _revision, ["truncated"] = truncated, ["spans"] = lanes }.ToJsonString();
        }
    }

    /// <inheritdoc/>
    public string RecordingsEvents(string query)
    {
        Query q = ParseQuery(query, "nv_recordings_events");
        lock (_gate)
        {
            Configured("nv_recordings_events");
            List<FakeEvent> all = Matching(q);
            return new JsonObject
            {
                ["revision"] = _revision,
                ["truncated"] = all.Count > q.Limit,
                ["events"] = new JsonArray([.. all.Take(q.Limit).Select(e => (JsonNode)EventNode(e))]),
            }.ToJsonString();
        }
    }

    // ---- incidents, locks, groups -----------------------------------------

    /// <inheritdoc/>
    public string IncidentsGet(string query)
    {
        Query q = ParseQuery(query, "nv_incidents_get");
        lock (_gate)
        {
            Configured("nv_incidents_get");
            List<FakeEvent> all = Matching(q);
            all.Reverse();
            return new JsonObject
            {
                ["revision"] = _incidentsRevision,
                ["truncated"] = all.Count > q.Limit,
                ["incidents"] = new JsonArray([.. all.Take(q.Limit).Select(e => (JsonNode)IncidentNode(e))]),
            }.ToJsonString();
        }
    }

    /// <inheritdoc/>
    public string IncidentGet(string key)
    {
        lock (_gate)
        {
            Configured("nv_incident_get");
            FakeEvent e = AllIncidents().FirstOrDefault(e => e.Key == key) ?? throw Error(NvStatus.UnknownId, "nv_incident_get");
            return new JsonObject { ["revision"] = _incidentsRevision, ["incident"] = IncidentNode(e) }.ToJsonString();
        }
    }

    /// <inheritdoc/>
    public string IncidentEdit(string edit)
    {
        const string F = "nv_incident_edit";
        JsonObject o = Object(edit, F);
        string op = o["op"]?.GetValueKind() == JsonValueKind.String ? o["op"]!.GetValue<string>() : throw Error(NvStatus.InvalidArgument, F);
        long revision;
        string key;
        lock (_gate)
        {
            Configured(F);
            if (op is not ("rename" or "revert"))
            {
                throw Error(NvStatus.Unsupported, F);
            }
            key = o["key"]?.GetValueKind() == JsonValueKind.String ? o["key"]!.GetValue<string>() : throw Error(NvStatus.InvalidArgument, F);
            if (!AllIncidents().Any(e => e.Key == key))
            {
                throw Error(NvStatus.UnknownId, F);
            }
            if (op == "rename" && o["name"]?.GetValueKind() == JsonValueKind.String)
            {
                _names[key] = o["name"]!.GetValue<string>();
            }
            else
            {
                _names.Remove(key);
            }
            revision = ++_incidentsRevision;
        }
        Emit(new JsonObject { ["type"] = "incidents_changed", ["revision"] = revision });
        return new JsonObject { ["revision"] = revision, ["keys"] = new JsonArray(key) }.ToJsonString();
    }

    /// <inheritdoc/>
    public string IncidentLock(string key, bool locked)
    {
        const string F = "nv_incident_lock";
        JsonObject answer;
        FakeDownload? queued = null;
        bool changed;
        lock (_gate)
        {
            Configured(F);
            FakeEvent e = AllIncidents().FirstOrDefault(e => e.Key == key) ?? throw Error(NvStatus.UnknownId, F);
            changed = locked ? _locks.Add(key) : _locks.Remove(key);
            if (changed)
            {
                _incidentsRevision++;
            }
            answer = new JsonObject { ["revision"] = _incidentsRevision, ["keys"] = changed ? new JsonArray(key) : new JsonArray() };
            if (locked)
            {
                if (_downloadsDir is null)
                {
                    answer["download"] = null;
                    answer["folder"] = null;
                    answer["download_status"] = "not_configured";
                }
                else if (_downloads.FirstOrDefault(d => d.Incident == key) is { } existing)
                {
                    answer["download"] = existing.Id;
                    answer["folder"] = existing.Folder;
                    answer["download_status"] = "existing";
                }
                else
                {
                    queued = NewDownload(e, [e.Camera]);
                    answer["download"] = queued.Id;
                    answer["folder"] = queued.Folder;
                    answer["download_status"] = "queued";
                }
            }
        }
        if (changed)
        {
            Emit(new JsonObject { ["type"] = "incidents_changed", ["revision"] = answer["revision"]!.GetValue<long>() });
        }
        if (queued is not null)
        {
            Queued(queued);
        }
        return answer.ToJsonString();
    }

    /// <inheritdoc/>
    public string CameraGroupsGet()
    {
        lock (_gate)
        {
            Configured("nv_camera_groups_get");
            var o = (JsonObject)_groups.DeepClone();
            o["revision"] = _groupsRevision;
            o["groups"] ??= new JsonArray();
            o["camera_names"] ??= new JsonArray();
            return o.ToJsonString();
        }
    }

    /// <inheritdoc/>
    public void CameraGroupsPut(string json)
    {
        const string F = "nv_camera_groups_put";
        JsonObject o = Object(json, F);
        JsonArray groups = o["groups"] as JsonArray ?? [];
        JsonArray names = o["camera_names"] as JsonArray ?? [];
        if (groups.Count > 64 || names.Count > 4096)
        {
            throw Error(NvStatus.Limit, F);
        }
        long groupsRevision, incidents;
        lock (_gate)
        {
            Configured(F);
            _groups = new JsonObject { ["groups"] = groups.DeepClone(), ["camera_names"] = names.DeepClone() };
            groupsRevision = ++_groupsRevision;
            incidents = ++_incidentsRevision;
        }
        Emit(new JsonObject { ["type"] = "camera_groups_changed", ["revision"] = groupsRevision });
        Emit(new JsonObject { ["type"] = "incidents_changed", ["revision"] = incidents });
    }

    // ---- thumbnails ----------------------------------------------------------

    private static ThumbnailKey Key(string json, string function)
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json);
            return ThumbnailKey.From(doc.RootElement) is { } k && k.End >= k.Start && k.Tier is "main" or "sub"
                ? k
                : throw Error(NvStatus.InvalidArgument, function);
        }
        catch (JsonException)
        {
            throw Error(NvStatus.InvalidArgument, function);
        }
    }

    /// <inheritdoc/>
    public RecordedThumbnail? ThumbnailGet(string key)
    {
        ThumbnailKey k = Key(key, "nv_thumbnail_get");
        lock (_gate)
        {
            Configured("nv_thumbnail_get");
            if (_thumbnails.TryGetValue(k, out RecordedThumbnail? t))
            {
                return t;
            }
            if (!_pendingThumbnails.Contains(k))
            {
                _pendingThumbnails.Add(k);
            }
        }
        ScheduleWork();
        return null;
    }

    /// <inheritdoc/>
    public void ThumbnailPut(string key, RecordedThumbnail thumbnail)
    {
        ArgumentNullException.ThrowIfNull(thumbnail);
        ThumbnailKey k = Key(key, "nv_thumbnail_put");
        if (thumbnail.Data.Length > 64 << 20)
        {
            throw Error(NvStatus.Limit, "nv_thumbnail_put");
        }
        lock (_gate)
        {
            Configured("nv_thumbnail_put");
            _thumbnails[k] = thumbnail with { Data = [.. thumbnail.Data] };
        }
    }

    /// <inheritdoc/>
    public void ThumbnailCancel(string? key)
    {
        ThumbnailKey? k = key is null ? null : Key(key, "nv_thumbnail_cancel");
        lock (_gate)
        {
            Configured("nv_thumbnail_cancel");
            if (k is null)
            {
                _pendingThumbnails.Clear();
            }
            else
            {
                _pendingThumbnails.Remove(k);
            }
        }
    }

    // ---- downloads -------------------------------------------------------------

    private FakeDownload NewDownload(FakeEvent e, List<RecordingCamera> cameras)
    {
        var d = new FakeDownload
        {
            Id = $"{e.Id}-{_nextDownload++}",
            Incident = e.Key,
            Folder = $"{_downloadsDir}/{e.Start} {e.Kind} — Camera {e.Camera.Channel + 1}",
            Cameras = cameras,
        };
        _downloads.Add(d);
        return d;
    }

    private static JsonObject DownloadNode(FakeDownload d, string? state = null)
    {
        string s = state ?? d.State;
        bool done = s == "done";
        return new JsonObject
        {
            ["download"] = d.Id,
            ["incident"] = d.Incident,
            ["folder"] = d.Folder,
            ["state"] = d.Paused ? "paused" : s,
            ["paused"] = d.Paused,
            ["cameras"] = new JsonArray([.. d.Cameras.Select(c => (JsonNode)new JsonObject
            {
                ["camera"] = CameraNode(c),
                ["name"] = $"Camera {c.Channel + 1}",
                ["state"] = s,
                ["paused"] = d.Paused,
                ["chunks_done"] = done ? 1 : 0,
                ["chunks_total"] = 1,
                ["bytes"] = done ? 1 << 20 : 0,
                // As the core: null until the device gave an estimate (and after the last chunk).
                ["bytes_estimate"] = null,
                ["output"] = done ? $"{d.Folder}/Camera {c.Channel + 1}.mp4" : null,
            })]),
        };
    }

    private void EmitDownload(FakeDownload d, string? state = null)
    {
        JsonObject o = DownloadNode(d, state);
        var e = new JsonObject { ["type"] = "download" };
        foreach ((string k, JsonNode? v) in o)
        {
            e[k] = v?.DeepClone();
        }
        Emit(e);
    }

    private void Queued(FakeDownload d)
    {
        lock (_gate)
        {
            EmitDownload(d);
        }
        ScheduleWork();
    }

    /// <inheritdoc/>
    public string DownloadIncident(string request)
    {
        const string F = "nv_download_incident";
        JsonObject o = Object(request, F);
        string key = o["incident"]?.GetValueKind() == JsonValueKind.String ? o["incident"]!.GetValue<string>() : throw Error(NvStatus.InvalidArgument, F);
        JsonObject answer;
        FakeDownload? queued = null;
        lock (_gate)
        {
            Configured(F);
            if (_downloadsDir is null)
            {
                throw Error(NvStatus.NotConfigured, F);
            }
            FakeEvent e = AllIncidents().FirstOrDefault(e => e.Key == key) ?? throw Error(NvStatus.UnknownId, F);
            if (_downloads.FirstOrDefault(d => d.Incident == key) is { } existing)
            {
                answer = new JsonObject { ["download"] = existing.Id, ["folder"] = existing.Folder, ["existing"] = true };
            }
            else
            {
                if (_downloads.Count >= 1000)
                {
                    throw Error(NvStatus.Limit, F);
                }
                List<RecordingCamera> cameras = [e.Camera];
                if (o["cameras"] is JsonArray list)
                {
                    cameras = [];
                    foreach (JsonNode? n in list)
                    {
                        RecordingCamera c = Camera(n?["camera"], F);
                        if (c != e.Camera)
                        {
                            throw Error(NvStatus.InvalidArgument, F);
                        }
                        cameras.Add(c);
                    }
                }
                queued = NewDownload(e, cameras);
                answer = new JsonObject { ["download"] = queued.Id, ["folder"] = queued.Folder, ["existing"] = false };
            }
        }
        if (queued is not null)
        {
            Queued(queued);
        }
        return answer.ToJsonString();
    }

    private void Change(string download, string function, Action<FakeDownload> change)
    {
        lock (_gate)
        {
            Configured(function);
            if (_downloadsDir is null)
            {
                throw Error(NvStatus.NotConfigured, function);
            }
            FakeDownload d = _downloads.FirstOrDefault(d => d.Id == download) ?? throw Error(NvStatus.UnknownId, function);
            change(d);
            EmitDownload(d);
        }
        ScheduleWork();
    }

    private static RecordingCamera? CameraArg(string? camera, string function) =>
        camera is null ? null : Camera(Object(camera, function), function);

    /// <inheritdoc/>
    public void DownloadPause(string download, string? camera, bool paused)
    {
        RecordingCamera? c = CameraArg(camera, "nv_download_pause");
        Change(download, "nv_download_pause", d =>
        {
            if (c is not null && !d.Cameras.Contains(c))
            {
                throw Error(NvStatus.UnknownId, "nv_download_pause");
            }
            if (d.State != "done")
            {
                d.Paused = paused;
            }
        });
    }

    /// <inheritdoc/>
    public void DownloadRetry(string download, string? camera)
    {
        RecordingCamera? c = CameraArg(camera, "nv_download_retry");
        Change(download, "nv_download_retry", d =>
        {
            if (c is not null && !d.Cameras.Contains(c))
            {
                throw Error(NvStatus.UnknownId, "nv_download_retry");
            }
            if (d.State == "failed")
            {
                d.State = "queued";
            }
        });
    }

    /// <inheritdoc/>
    public void DownloadForget(string download, bool deletePartial)
    {
        const string F = "nv_download_forget";
        lock (_gate)
        {
            Configured(F);
            if (_downloadsDir is null)
            {
                throw Error(NvStatus.NotConfigured, F);
            }
            FakeDownload d = _downloads.FirstOrDefault(d => d.Id == download) ?? throw Error(NvStatus.UnknownId, F);
            if (d.State is "queued" or "running" && !d.Paused)
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            _downloads.Remove(d);
        }
        Emit(new JsonObject { ["type"] = "download_removed", ["download"] = download, ["trash"] = false });
    }

    /// <inheritdoc/>
    public string DownloadTrash(string download)
    {
        const string F = "nv_download_trash";
        string folder;
        lock (_gate)
        {
            Configured(F);
            if (_downloadsDir is null)
            {
                throw Error(NvStatus.NotConfigured, F);
            }
            FakeDownload d = _downloads.FirstOrDefault(d => d.Id == download) ?? throw Error(NvStatus.UnknownId, F);
            _downloads.Remove(d);
            folder = d.Folder;
        }
        Emit(new JsonObject { ["type"] = "download_removed", ["download"] = download, ["trash"] = true, ["folder"] = folder });
        return new JsonObject { ["folder"] = folder }.ToJsonString();
    }

    /// <inheritdoc/>
    public string DownloadsGet()
    {
        lock (_gate)
        {
            Configured("nv_downloads_get");
            if (_downloadsDir is null)
            {
                throw Error(NvStatus.NotConfigured, "nv_downloads_get");
            }
            return new JsonObject { ["downloads"] = new JsonArray([.. _downloads.Select(d => (JsonNode)DownloadNode(d))]) }.ToJsonString();
        }
    }

    // ---- sound settings ------------------------------------------------------

    /// <inheritdoc/>
    public void SoundConfigure(string json)
    {
        const string F = "nv_sound_configure";
        JsonObject o = Object(json, F);
        if (o["alarms"] is { } a && a.GetValueKind() is not (JsonValueKind.True or JsonValueKind.False))
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        if (o["loudness_history_s"] is { } h && (h is not JsonValue v || !v.TryGetValue(out int n) || n is < 0 or > 3600))
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
    }

    // ---- queued work -----------------------------------------------------------

    private void ScheduleWork()
    {
        if (!RecordingWorkIsManual)
        {
            _ = Task.Delay(20).ContinueWith(_ => CompleteRecordingWork(), TaskScheduler.Default);
        }
    }

    /// <summary>
    /// Finishes every queued thumbnail fetch (<c>thumbnail</c> <c>ready</c>
    /// with a stand-in keyframe) and download (<c>running</c>, then
    /// <c>done</c>), sending their events. Paused downloads wait.
    /// </summary>
    public void CompleteRecordingWork()
    {
        lock (_gate)
        {
            foreach (ThumbnailKey k in _pendingThumbnails)
            {
                _thumbnails[k] = new RecordedThumbnail(NvThumbKind.H264Keyframe, 640, 360, 0, StandInKeyframe);
                Emit(new JsonObject
                {
                    ["type"] = "thumbnail",
                    ["key"] = JsonNode.Parse(k.Json),
                    ["state"] = "ready",
                });
            }
            _pendingThumbnails.Clear();
            foreach (FakeDownload d in _downloads.Where(d => !d.Paused && d.State == "queued"))
            {
                EmitDownload(d, "running");
                d.State = "done";
                EmitDownload(d);
            }
            CompleteSoundAnalyses();
        }
    }
}
