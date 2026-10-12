// The recordings screens' pure pieces (docs/UX.md "Recordings"): device-local
// days, the answers of the recordings queries as values, the kind filters (an
// "animal" filter that matches every animal kind), the requests and edits as
// JSON, and the downloads and thumbnails boards. The same rules, with the
// same test vectors, as the macOS app's RecordingsBrowse.swift. Nothing here
// calls the core or touches a window, so it is tested on Linux.

using System.Globalization;
using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>
/// Device-local seconds (docs/FFI-RECORDINGS.md, "Time"): seconds since
/// 1970-01-01 00:00:00 of the device's wall clock, no zone. Shown by
/// formatting them as if they were UTC.
/// </summary>
public static class DeviceTime
{
    /// <summary>Seconds in a day.</summary>
    public const long Day = 86_400;

    /// <summary>The first second of the day <paramref name="s"/> falls in.</summary>
    public static long DayStart(long s) => s - (((s % Day) + Day) % Day);

    /// <summary>Today on a device whose wall clock is <paramref name="utcOffset"/> seconds ahead of UTC.</summary>
    public static long Today(DateTimeOffset now, int utcOffset) => DayStart(Now(now, utcOffset));

    /// <summary>The device's "now" in device-local seconds.</summary>
    public static long Now(DateTimeOffset now, int utcOffset) => now.ToUnixTimeSeconds() + utcOffset;

    private static DateTime At(long s) => DateTime.UnixEpoch.AddSeconds(s);

    private static long Seconds(DateTime utc) => (long)(utc - DateTime.UnixEpoch).TotalSeconds;

    /// <summary>The first second of the month <paramref name="s"/> falls in, and of the next month.</summary>
    public static (long Start, long Next) Month(long s)
    {
        DateTime d = At(s);
        var start = new DateTime(d.Year, d.Month, 1, 0, 0, 0, DateTimeKind.Utc);
        return (Seconds(start), Seconds(start.AddMonths(1)));
    }

    /// <summary>The month before or after (<paramref name="by"/> months) the one <paramref name="s"/> falls in: its first day.</summary>
    public static long MonthAdding(long s, int by) => Seconds(At(Month(s).Start).AddMonths(by));

    /// <summary>
    /// The month's days as weeks of seven (null before the first and after
    /// the last day), weeks starting on <paramref name="firstWeekday"/>
    /// (1 Sunday ... 7 Saturday).
    /// </summary>
    public static List<long?[]> Weeks(long s, int firstWeekday = 1)
    {
        (long start, long next) = Month(s);
        int weekday = (int)At(start).DayOfWeek + 1;
        int lead = (((weekday - firstWeekday) % 7) + 7) % 7;
        var cells = new List<long?>();
        for (int i = 0; i < lead; i++)
        {
            cells.Add(null);
        }
        for (long d = start; d < next; d += Day)
        {
            cells.Add(d);
        }
        while (cells.Count % 7 != 0)
        {
            cells.Add(null);
        }
        var weeks = new List<long?[]>();
        for (int i = 0; i < cells.Count; i += 7)
        {
            weeks.Add([.. cells.GetRange(i, 7)]);
        }
        return weeks;
    }

    /// <summary>The day of the month (1 to 31).</summary>
    public static int DayOfMonth(long s) => At(s).Day;

    /// <summary>Formats device-local seconds with a .NET date format (<c>"HH:mm:ss"</c>, ...).</summary>
    public static string Format(long s, string pattern) => At(s).ToString(pattern, CultureInfo.InvariantCulture);

    /// <summary>"Sunday, October 11, 2026" style, in the user's language.</summary>
    public static string LongDay(long s) => At(s).ToString("D", CultureInfo.CurrentCulture);

    /// <summary>"October 2026".</summary>
    public static string MonthTitle(long s) => At(s).ToString("MMMM yyyy", CultureInfo.CurrentCulture);

    /// <summary>A time of day, "07:42:05" (24 hours, as devices burn it into the picture).</summary>
    public static string Time(long s) => Format(s, "HH:mm:ss");

    /// <summary>"07:42:05" to "07:42:40", joined by an en dash.</summary>
    public static string Span(long start, long end) => Time(start) + "\u2013" + Time(end);

    /// <summary>Division rounding toward minus infinity (times before 1970 stay whole seconds).</summary>
    public static long FloorDiv(long n, long d) => (n - (((n % d) + d) % d)) / d;
}

/// <summary>
/// The recordings screens' kind filters. Event kinds use the live detection
/// vocabulary (<see cref="IndicatorKind"/>), so the badges are the same; an
/// "animal" filter matches every animal kind a device reports, today's
/// <c>dog_cat</c> and any finer kind later (FFI-RECORDINGS.md, decision 10).
/// </summary>
public enum KindFilter
{
    People,
    Vehicle,
    Animal,
    Motion,
    Other,
}

/// <summary>Labels and matching for <see cref="KindFilter"/>.</summary>
public static class KindFilters
{
    /// <summary>Every filter, in display order.</summary>
    public static IReadOnlyList<KindFilter> All { get; } = Enum.GetValues<KindFilter>();

    /// <summary>The chip's label.</summary>
    public static string Label(this KindFilter f) => f switch
    {
        KindFilter.People => "People",
        KindFilter.Vehicle => "Vehicles",
        KindFilter.Animal => "Animals",
        KindFilter.Motion => "Motion",
        _ => "Other",
    };

    /// <summary>
    /// Animal kinds: the device's <c>dog_cat</c>, a plain <c>animal</c>, and the finer
    /// names a device or a local classifier may use (<c>dog</c>, <c>cat</c>, <c>bird</c>,
    /// <c>pet</c>, <c>wildlife</c>, anything ending in <c>_animal</c>). Sound kinds such as
    /// <c>dog_bark</c> are sounds, not animals seen.
    /// </summary>
    public static bool IsAnimal(string kind) =>
        kind is "dog_cat" or "animal" or "dog" or "cat" or "bird" or "pet" or "wildlife" ||
        kind.EndsWith("_animal", StringComparison.Ordinal);

    /// <summary>The badge kind of an event kind (animal kinds count as animals).</summary>
    public static IndicatorKind Indicator(string kind) =>
        IsAnimal(kind) ? IndicatorKind.Animal : IndicatorKinds.FromDetection(kind);

    /// <summary>The filter an event kind belongs to.</summary>
    public static KindFilter Of(string kind) => Indicator(kind) switch
    {
        IndicatorKind.Person => KindFilter.People,
        IndicatorKind.Vehicle => KindFilter.Vehicle,
        IndicatorKind.Animal => KindFilter.Animal,
        IndicatorKind.Motion => KindFilter.Motion,
        _ => KindFilter.Other,
    };

    /// <summary>Whether <paramref name="kind"/> belongs to <paramref name="filter"/>.</summary>
    public static bool Matches(this KindFilter filter, string kind) => Of(kind) == filter;

    /// <summary>Whether any of <paramref name="kinds"/> passes <paramref name="filters"/> (an empty set passes all).</summary>
    public static bool Passes(IEnumerable<string> kinds, ISet<KindFilter> filters) =>
        filters.Count == 0 || kinds.Any(k => filters.Contains(Of(k)));
}

// ---- Answers ----

/// <summary>An event of the index (device, local video, or local audio).</summary>
public sealed record RecordedEvent(RecordingCamera Camera, string Kind, string? Source, long Start, long End)
{
    internal static RecordedEvent? From(JsonElement e) =>
        e.ValueKind == JsonValueKind.Object && e.TryGetProperty("camera", out JsonElement c) &&
        RecordingCamera.From(c) is { } camera && DownloadProgress.Str(e, "kind") is { } kind &&
        DownloadProgress.Long(e, "start") is { } s && DownloadProgress.Long(e, "end") is { } en
            ? new RecordedEvent(camera, kind, DownloadProgress.Str(e, "source"), s, en)
            : null;
}

/// <summary>One camera of an incident.</summary>
public sealed record IncidentCamera(
    RecordingCamera Camera, string? Name, IReadOnlyList<(long Start, long End)> Intervals, bool Added)
{
    /// <summary>The range its cover thumbnail is asked for: its first interval, else the incident's range.</summary>
    public (long Start, long End) CoverRange(long incidentStart, long incidentEnd) =>
        Intervals.Count > 0 ? Intervals[0] : (incidentStart, incidentEnd);
}

/// <summary>
/// An incident (docs/FFI-RECORDINGS.md section 3). <see cref="Key"/> is
/// stable while it grows and opaque: stored and passed back, never parsed.
/// </summary>
public sealed record Incident(
    string Key,
    string Id,
    string? Name,
    string? Group,
    long Start,
    long End,
    long Margin,
    IReadOnlyList<string> Kinds,
    bool Growing,
    bool Edited,
    bool Locked,
    IReadOnlyList<IncidentCamera> Cameras,
    IReadOnlyList<RecordedEvent> Events,
    IReadOnlyList<string> Downloads)
{
    /// <summary>The name to show: the user's, else the time and the kinds.</summary>
    public string Title
    {
        get
        {
            if (!string.IsNullOrEmpty(Name))
            {
                return Name;
            }
            string[] unique = [.. Kinds.Select(k => KindFilters.Indicator(k).Label()).Distinct()];
            return DeviceTime.Time(Start) + " " + (unique.Length == 0 ? "Recording" : string.Join(", ", unique));
        }
    }

    /// <summary>Whether the incident involves <paramref name="camera"/>.</summary>
    public bool Involves(RecordingCamera camera) => Cameras.Any(c => c.Camera == camera);

    /// <summary>The cover thumbnail key of each camera (sub-stream).</summary>
    public IReadOnlyList<ThumbnailKey> CoverKeys => [.. Cameras.Select(c =>
    {
        (long s, long e) = c.CoverRange(Start, End);
        return new ThumbnailKey(c.Camera, "sub", s, e);
    })];

    internal static Incident? From(JsonElement e)
    {
        if (e.ValueKind != JsonValueKind.Object || DownloadProgress.Str(e, "key") is not { } key ||
            DownloadProgress.Long(e, "start") is not { } start || DownloadProgress.Long(e, "end") is not { } end)
        {
            return null;
        }
        var cameras = new List<IncidentCamera>();
        foreach (JsonElement c in Items(e, "cameras"))
        {
            if (!c.TryGetProperty("camera", out JsonElement ce) || RecordingCamera.From(ce) is not { } camera)
            {
                continue;
            }
            var intervals = new List<(long, long)>();
            foreach (JsonElement i in Items(c, "intervals"))
            {
                if (i.ValueKind == JsonValueKind.Array && i.GetArrayLength() == 2 &&
                    i[0].TryGetInt64(out long a) && i[1].TryGetInt64(out long b))
                {
                    intervals.Add((a, b));
                }
            }
            cameras.Add(new IncidentCamera(camera, DownloadProgress.Str(c, "name"), intervals, DownloadProgress.Bool(c, "added")));
        }
        return new Incident(
            key, DownloadProgress.Str(e, "id") ?? key, DownloadProgress.Str(e, "name"), DownloadProgress.Str(e, "group"),
            start, end, DownloadProgress.Long(e, "margin_s") ?? 0,
            [.. Items(e, "kinds").Where(k => k.ValueKind == JsonValueKind.String).Select(k => k.GetString()!)],
            DownloadProgress.Bool(e, "growing"), DownloadProgress.Bool(e, "edited"), DownloadProgress.Bool(e, "locked"),
            cameras, [.. Items(e, "events").Select(RecordedEvent.From).OfType<RecordedEvent>()],
            [.. Items(e, "downloads").Where(k => k.ValueKind == JsonValueKind.String).Select(k => k.GetString()!)]);
    }

    internal static IEnumerable<JsonElement> Items(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.Array ? v.EnumerateArray() : [];
}

/// <summary><c>nv_recordings_calendar</c>: the days each camera has recordings.</summary>
public sealed record CalendarAnswer(long Revision, IReadOnlyList<(RecordingCamera Camera, IReadOnlyList<long> Days)> Days)
{
    /// <summary>Every day any camera has.</summary>
    public IReadOnlySet<long> AllDays => Days.SelectMany(d => d.Days).ToHashSet();

    /// <summary>Parses the answer; null when it does not parse.</summary>
    public static CalendarAnswer? Parse(string json) => Answer.Read(json, root =>
    {
        var list = new List<(RecordingCamera, IReadOnlyList<long>)>();
        foreach (JsonElement c in Incident.Items(root, "days"))
        {
            if (c.TryGetProperty("camera", out JsonElement ce) && RecordingCamera.From(ce) is { } camera)
            {
                list.Add((camera, [.. Incident.Items(c, "days").Select(d => d.GetInt64())]));
            }
        }
        return new CalendarAnswer(DownloadProgress.Long(root, "revision") ?? 0, list);
    });
}

/// <summary>One recording file's range (a span of a timeline lane).</summary>
public sealed record RecordingSpan(long Start, long End, IReadOnlyList<string> Causes, string? Tier);

/// <summary><c>nv_recordings_spans</c>.</summary>
public sealed record SpansAnswer(long Revision, bool Truncated, IReadOnlyList<(RecordingCamera Camera, IReadOnlyList<RecordingSpan> Spans)> Spans)
{
    /// <summary>Parses the answer; null when it does not parse.</summary>
    public static SpansAnswer? Parse(string json) => Answer.Read(json, root =>
    {
        var list = new List<(RecordingCamera, IReadOnlyList<RecordingSpan>)>();
        foreach (JsonElement c in Incident.Items(root, "spans"))
        {
            if (c.TryGetProperty("camera", out JsonElement ce) && RecordingCamera.From(ce) is { } camera)
            {
                list.Add((camera, [.. Incident.Items(c, "spans").Select(s => new RecordingSpan(
                    DownloadProgress.Long(s, "start") ?? 0, DownloadProgress.Long(s, "end") ?? 0,
                    [.. Incident.Items(s, "causes").Select(x => x.GetString() ?? "")], DownloadProgress.Str(s, "tier")))]));
            }
        }
        return new SpansAnswer(DownloadProgress.Long(root, "revision") ?? 0, DownloadProgress.Bool(root, "truncated"), list);
    });
}

/// <summary><c>nv_recordings_events</c>.</summary>
public sealed record EventsAnswer(long Revision, bool Truncated, IReadOnlyList<RecordedEvent> Events)
{
    /// <summary>Parses the answer; null when it does not parse.</summary>
    public static EventsAnswer? Parse(string json) => Answer.Read(json, root => new EventsAnswer(
        DownloadProgress.Long(root, "revision") ?? 0, DownloadProgress.Bool(root, "truncated"),
        [.. Incident.Items(root, "events").Select(RecordedEvent.From).OfType<RecordedEvent>()]));
}

/// <summary><c>nv_incidents_get</c>.</summary>
public sealed record IncidentsAnswer(long Revision, bool Truncated, IReadOnlyList<Incident> Incidents)
{
    /// <summary>Parses the answer; null when it does not parse.</summary>
    public static IncidentsAnswer? Parse(string json) => Answer.Read(json, root => new IncidentsAnswer(
        DownloadProgress.Long(root, "revision") ?? 0, DownloadProgress.Bool(root, "truncated"),
        [.. Incident.Items(root, "incidents").Select(Incident.From).OfType<Incident>()]));
}

/// <summary><c>nv_incident_get</c>.</summary>
public sealed record IncidentAnswer(long Revision, Incident Incident)
{
    /// <summary>Parses the answer; null when it does not parse.</summary>
    public static IncidentAnswer? Parse(string json) => Answer.Read(json, root =>
        root.TryGetProperty("incident", out JsonElement i) && Incident.From(i) is { } incident
            ? new IncidentAnswer(DownloadProgress.Long(root, "revision") ?? 0, incident)
            : null);
}

/// <summary>The answer of <c>nv_incident_lock</c> (download members only when locking).</summary>
public sealed record LockAnswer(long Revision, IReadOnlyList<string> Keys, string? Download, string? Folder, string? DownloadStatus)
{
    /// <summary>What to tell the user after locking, if anything.</summary>
    public string? Message => DownloadStatus switch
    {
        "queued" => "Locked. Its video is downloading so it is kept.",
        "existing" => "Locked. Its video is already downloaded or downloading.",
        "when_settled" => "Locked. Its video downloads once the incident ends.",
        "not_configured" => "Locked, but its video is not being kept: there is no downloads folder.",
        "limit" => "Locked, but its video is not being kept: too many downloads. Forget finished ones.",
        _ => null,
    };

    /// <summary>Whether the user must be told the video is not being kept.</summary>
    public bool VideoNotKept => DownloadStatus is "not_configured" or "limit";

    /// <summary>Parses the answer; null when it does not parse.</summary>
    public static LockAnswer? Parse(string json) => Answer.Read(json, root => new LockAnswer(
        DownloadProgress.Long(root, "revision") ?? 0,
        [.. Incident.Items(root, "keys").Select(k => k.GetString() ?? "")],
        DownloadProgress.Str(root, "download"), DownloadProgress.Str(root, "folder"), DownloadProgress.Str(root, "download_status")));
}

/// <summary>The answer of <c>nv_download_incident</c>.</summary>
public sealed record QueuedDownload(string Download, string? Folder, bool Existing)
{
    /// <summary>Parses the answer; null when it does not parse.</summary>
    public static QueuedDownload? Parse(string json) => Answer.Read(json, root =>
        DownloadProgress.Str(root, "download") is { } d
            ? new QueuedDownload(d, DownloadProgress.Str(root, "folder"), DownloadProgress.Bool(root, "existing"))
            : null);
}

/// <summary>The answer of <c>nv_download_trash</c>.</summary>
public sealed record TrashAnswer(string Folder)
{
    /// <summary>Parses the answer; null when it does not parse.</summary>
    public static TrashAnswer? Parse(string json) => Answer.Read(json, root =>
        DownloadProgress.Str(root, "folder") is { } f ? new TrashAnswer(f) : null);
}

/// <summary>The answer of <c>nv_incident_edit</c>: the resulting incidents' keys.</summary>
public sealed record EditAnswer(long Revision, IReadOnlyList<string> Keys)
{
    /// <summary>Parses the answer; null when it does not parse.</summary>
    public static EditAnswer? Parse(string json) => Answer.Read(json, root => new EditAnswer(
        DownloadProgress.Long(root, "revision") ?? 0, [.. Incident.Items(root, "keys").Select(k => k.GetString() ?? "")]));
}

/// <summary><c>nv_recordings_status</c>: one device's lane.</summary>
public sealed record IndexDevice(string Device, string State, int PendingDays, long Revision);

/// <summary><c>nv_recordings_status</c>.</summary>
public sealed record IndexStatus(long Revision, IReadOnlyList<IndexDevice> Devices)
{
    /// <summary>Parses the answer; null when it does not parse.</summary>
    public static IndexStatus? Parse(string json) => Answer.Read(json, root => new IndexStatus(
        DownloadProgress.Long(root, "revision") ?? 0,
        [.. Incident.Items(root, "devices").Select(d => DownloadProgress.Str(d, "device") is { } id
            ? new IndexDevice(id, DownloadProgress.Str(d, "state") ?? "idle", (int)(DownloadProgress.Long(d, "pending_days") ?? 0),
                DownloadProgress.Long(d, "revision") ?? 0)
            : null).OfType<IndexDevice>()]));
}

/// <summary>
/// Camera groups for incidents (<c>nv_camera_groups_get</c>/<c>put</c>): cameras that
/// cover the same area, so their events join one incident.
/// </summary>
public sealed record IncidentGroups(IReadOnlyList<IncidentGroups.Group> Groups, IReadOnlyList<IncidentGroups.CameraName> CameraNames)
{
    /// <summary>A named set of cameras.</summary>
    public sealed record Group(string Name, IReadOnlyList<RecordingCamera> Cameras);

    /// <summary>A name override for one camera.</summary>
    public sealed record CameraName(RecordingCamera Camera, string Name);

    /// <summary>No groups and no names.</summary>
    public static IncidentGroups Empty { get; } = new([], []);

    /// <summary>Parses the answer; null when it does not parse.</summary>
    public static IncidentGroups? Parse(string json) => Answer.Read(json, root => new IncidentGroups(
        [.. Incident.Items(root, "groups").Select(g => DownloadProgress.Str(g, "name") is { } name
            ? new Group(name, [.. Incident.Items(g, "cameras").Select(RecordingCamera.From).OfType<RecordingCamera>()])
            : null).OfType<Group>()],
        [.. Incident.Items(root, "camera_names").Select(n =>
            n.TryGetProperty("camera", out JsonElement c) && RecordingCamera.From(c) is { } camera &&
            DownloadProgress.Str(n, "name") is { } name
                ? new CameraName(camera, name)
                : null).OfType<CameraName>()]));

    /// <summary>
    /// The JSON <c>nv_camera_groups_put</c> takes. Empty names and empty
    /// groups are left out; a camera stays in its first group only.
    /// </summary>
    public string PutJson
    {
        get
        {
            var seen = new HashSet<RecordingCamera>();
            var groups = new JsonArray();
            foreach (Group g in Groups)
            {
                string name = g.Name.Trim();
                RecordingCamera[] cams = [.. g.Cameras.Where(seen.Add)];
                if (name.Length > 0 && cams.Length > 0)
                {
                    groups.Add((JsonNode?)new JsonObject
                    {
                        ["name"] = name,
                        ["cameras"] = new JsonArray([.. cams.Select(c => (JsonNode?)JsonNode.Parse(c.Json))]),
                    });
                }
            }
            var names = new JsonArray([.. CameraNames.Select(n => (JsonNode?)new JsonObject
            {
                ["camera"] = JsonNode.Parse(n.Camera.Json),
                ["name"] = n.Name,
            })]);
            return new JsonObject { ["groups"] = groups, ["camera_names"] = names }.ToJsonString();
        }
    }
}

internal static class Answer
{
    /// <summary>Reads a JSON answer with <paramref name="read"/>; null when it does not parse.</summary>
    public static T? Read<T>(string json, Func<JsonElement, T?> read) where T : class
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json);
            return doc.RootElement.ValueKind == JsonValueKind.Object ? read(doc.RootElement) : null;
        }
        catch (Exception e) when (e is JsonException or InvalidOperationException or FormatException)
        {
            return null;
        }
    }
}

// ---- Requests ----

/// <summary>Builds the JSON of the recordings requests.</summary>
public static class RecordingsRequest
{
    private static string Cameras(IEnumerable<RecordingCamera> cameras) => "[" + string.Join(",", cameras.Select(c => c.Json)) + "]";

    // A JSON string with only the escapes JSON requires (quotes and backslashes stay readable).
    private static string Str(string s)
    {
        var sb = new System.Text.StringBuilder("\"");
        foreach (char c in s)
        {
            switch (c)
            {
                case '"':
                    sb.Append("\\\"");
                    break;
                case '\\':
                    sb.Append("\\\\");
                    break;
                case '\n':
                    sb.Append("\\n");
                    break;
                case '\r':
                    sb.Append("\\r");
                    break;
                case '\t':
                    sb.Append("\\t");
                    break;
                default:
                    if (c < 0x20)
                    {
                        sb.Append("\\u").Append(((int)c).ToString("x4", CultureInfo.InvariantCulture));
                    }
                    else
                    {
                        sb.Append(c);
                    }
                    break;
            }
        }
        return sb.Append('"').ToString();
    }

    private static string Longs(IEnumerable<long> values) => string.Join(",", values.Select(v => v.ToString(CultureInfo.InvariantCulture)));

    /// <summary><c>nv_recordings_enable</c>: these cameras of one device.</summary>
    public static string Enable(IEnumerable<RecordingCamera> cameras, int utcOffset) =>
        "{\"channels\":[" + string.Join(",", cameras.Select(c => $"{{\"channel\":{c.Channel},\"lens\":{c.Lens}}}")) +
        $"],\"utc_offset_s\":{utcOffset}}}";

    /// <summary><c>nv_recordings_want</c> for days.</summary>
    public static string Want(IEnumerable<RecordingCamera> cameras, IEnumerable<long> days, bool force = false) =>
        $"{{\"cameras\":{Cameras(cameras)},\"days\":[{Longs(days)}],\"force\":{(force ? "true" : "false")}}}";

    /// <summary><c>nv_recordings_want</c> with <c>months</c> alone: only those months' calendars (any second within each month).</summary>
    public static string WantMonths(IEnumerable<RecordingCamera> cameras, IEnumerable<long> months, bool force = false) =>
        $"{{\"cameras\":{Cameras(cameras)},\"months\":[{Longs(months)}],\"force\":{(force ? "true" : "false")}}}";

    /// <summary><c>nv_recordings_hold_today</c>.</summary>
    public static string Hold(IEnumerable<RecordingCamera> cameras) => $"{{\"cameras\":{Cameras(cameras)}}}";

    /// <summary>A query over [start, end]; <paramref name="cameras"/> null for all, <paramref name="group"/> for a camera group.</summary>
    public static string Query(long start, long end, IEnumerable<RecordingCamera>? cameras = null, string? group = null, int? limit = null)
    {
        var parts = new List<string> { $"\"start\":{start}", $"\"end\":{end}" };
        if (cameras is not null)
        {
            parts.Add($"\"cameras\":{Cameras(cameras)}");
        }
        if (group is not null)
        {
            parts.Add($"\"group\":{Str(group)}");
        }
        if (limit is { } l)
        {
            parts.Add($"\"limit\":{l}");
        }
        return "{" + string.Join(",", parts) + "}";
    }

    /// <summary><c>nv_download_incident</c> for the whole incident (every camera, main tier).</summary>
    public static string Download(string incidentKey) => $"{{\"incident\":{Str(incidentKey)}}}";

    internal static string Quote(string s) => Str(s);
}

/// <summary>An edit of incidents (<c>nv_incident_edit</c>).</summary>
public abstract record IncidentEdit
{
    /// <summary>Joins incidents.</summary>
    public sealed record Merge(IReadOnlyList<string> Keys) : IncidentEdit;

    /// <summary>Splits one at a second.</summary>
    public sealed record Split(string Key, long At) : IncidentEdit;

    /// <summary>Names an incident (null or blank clears the name).</summary>
    public sealed record Rename(string Key, string? Name) : IncidentEdit;

    /// <summary>Adds a camera.</summary>
    public sealed record AddCamera(string Key, RecordingCamera Camera) : IncidentEdit;

    /// <summary>Removes a camera.</summary>
    public sealed record RemoveCamera(string Key, RecordingCamera Camera) : IncidentEdit;

    /// <summary>Drops the edits of an incident.</summary>
    public sealed record Revert(string Key) : IncidentEdit;

    /// <summary>The JSON <c>nv_incident_edit</c> takes.</summary>
    public string Json
    {
        get
        {
            static string S(string s) => RecordingsRequest.Quote(s);
            return this switch
            {
                Merge m => $"{{\"op\":\"merge\",\"keys\":[{string.Join(",", m.Keys.Select(S))}]}}",
                Split s => $"{{\"op\":\"split\",\"key\":{S(s.Key)},\"at\":{s.At}}}",
                Rename r => $"{{\"op\":\"rename\",\"key\":{S(r.Key)},\"name\":{(string.IsNullOrWhiteSpace(r.Name) ? "null" : S(r.Name))}}}",
                AddCamera a => $"{{\"op\":\"add_camera\",\"key\":{S(a.Key)},\"camera\":{a.Camera.Json}}}",
                RemoveCamera r => $"{{\"op\":\"remove_camera\",\"key\":{S(r.Key)},\"camera\":{r.Camera.Json}}}",
                Revert r => $"{{\"op\":\"revert\",\"key\":{S(r.Key)}}}",
                _ => "{}",
            };
        }
    }
}

// ---- Downloads ----

/// <summary>
/// Every download the core reported, by ID, in the order they appeared.
/// Applies <c>download</c> and <c>download_removed</c> events; remembers
/// which downloads the user moved to the Recycle Bin, so the folder goes
/// there once the core says nothing writes into it.
/// </summary>
public sealed class DownloadBoard
{
    private readonly List<string> _order = [];
    private readonly Dictionary<string, DownloadProgress> _byId = [];
    private readonly Dictionary<string, string> _trashing = [];

    /// <summary>The downloads, in the order they appeared.</summary>
    public IReadOnlyList<DownloadProgress> Downloads => [.. _order.Select(id => _byId[id])];

    /// <summary>Downloads handed to the Recycle Bin (<c>nv_download_trash</c>), with their folders.</summary>
    public IReadOnlyDictionary<string, string> Trashing => _trashing;

    /// <summary>Replaces everything with <c>nv_downloads_get</c>'s list.</summary>
    public void Load(IReadOnlyList<DownloadProgress> list)
    {
        _order.Clear();
        _byId.Clear();
        foreach (DownloadProgress d in list)
        {
            if (_byId.TryAdd(d.Download, d))
            {
                _order.Add(d.Download);
            }
            else
            {
                _byId[d.Download] = d;
            }
        }
    }

    /// <summary>
    /// A <c>download</c> event: the newest state wins. Returns whether a state
    /// changed (the download's or a camera's, or a new download), as opposed
    /// to progress only.
    /// </summary>
    public bool Apply(DownloadProgress d)
    {
        if (!_byId.TryGetValue(d.Download, out DownloadProgress? old))
        {
            if (_trashing.ContainsKey(d.Download))
            {
                return false;
            }
            _order.Add(d.Download);
            _byId[d.Download] = d;
            return true;
        }
        _byId[d.Download] = d;
        return old.State != d.State || old.Paused != d.Paused ||
            !old.Cameras.Select(c => c.State).SequenceEqual(d.Cameras.Select(c => c.State)) ||
            !old.Cameras.Select(c => c.Paused).SequenceEqual(d.Cameras.Select(c => c.Paused));
    }

    /// <summary>The user moved a download to the Recycle Bin; <paramref name="folder"/> from the answer.</summary>
    public void TrashRequested(string id, string folder)
    {
        _trashing[id] = folder;
        Remove(id);
    }

    /// <summary>
    /// A <c>download_removed</c> event. Returns the folder to move to the
    /// Recycle Bin now, if this was a trash and nothing writes there any more.
    /// </summary>
    public string? Removed(string id, bool trash, string? folder)
    {
        Remove(id);
        if (!trash)
        {
            return null;
        }
        string? asked = _trashing.Remove(id, out string? f) ? f : null;
        return folder ?? asked;
    }

    private void Remove(string id)
    {
        _byId.Remove(id);
        _order.Remove(id);
    }
}

/// <summary>What the downloads panel shows for one download.</summary>
public static class DownloadText
{
    /// <summary>0 to 1: chunks done over chunks total.</summary>
    public static double Fraction(this DownloadProgress d)
    {
        if (d.State == "done")
        {
            return 1;
        }
        int done = d.Cameras.Sum(c => c.ChunksDone), total = d.Cameras.Sum(c => c.ChunksTotal);
        return total > 0 ? Math.Min(1, (double)done / total) : 0;
    }

    /// <summary>The folder's last path component.</summary>
    public static string FolderName(this DownloadProgress d) =>
        d.Folder is { Length: > 0 } f ? f.TrimEnd('/', '\\').Split('/', '\\')[^1] : d.Download;

    /// <summary>"Downloading", "Paused", "Failed: stalled", ...</summary>
    public static string StateText(this DownloadProgress d)
    {
        switch (d.State)
        {
            case "queued":
                return d.Cameras.Any(c => c.State == "waiting_device") ? "Waiting for the device" : "Queued";
            case "running":
                return "Downloading";
            case "paused":
                return "Paused";
            case "finalizing":
                return "Finishing";
            case "done":
                return "Done";
            case "failed":
                string[] reasons = [.. d.Cameras.Select(c => c.Reason).OfType<string>().Distinct().Order(StringComparer.Ordinal)];
                return "Failed" + (reasons.Length == 0 ? "" : ": " + string.Join(", ", reasons).Replace('_', ' '));
            default:
                return d.State;
        }
    }

    /// <summary>Whether some job may still write (forget needs a pause first).</summary>
    public static bool IsWorking(this DownloadProgress d) => d.State is "queued" or "running" or "finalizing" && !d.Paused;

    /// <summary>The finished files.</summary>
    public static IReadOnlyList<string> Outputs(this DownloadProgress d) => [.. d.Cameras.Select(c => c.Output).OfType<string>()];
}

// ---- Thumbnails ----

/// <summary>What the screens know about one cover thumbnail.</summary>
public abstract record ThumbnailState
{
    /// <summary>Asked; a <c>thumbnail</c> event follows.</summary>
    public sealed record Pending : ThumbnailState;

    /// <summary>The keyframe is being decoded into the small image.</summary>
    public sealed record Decoding : ThumbnailState;

    /// <summary>The app's small image (JPEG).</summary>
    public sealed record Image(byte[] Data) : ThumbnailState;

    /// <summary>No recording in the range.</summary>
    public sealed record Empty : ThumbnailState;

    /// <summary>The fetch failed, or the keyframe could not be decoded.</summary>
    public sealed record Failed(string? Reason) : ThumbnailState;

    /// <summary>Dropped from the device's queue; asked again when shown again.</summary>
    public sealed record Dropped : ThumbnailState;
}

/// <summary>
/// The thumbnails the screens hold, bounded: past the capacity, the entries
/// used least recently go (they are asked again, from the core's cache, if
/// shown again).
/// </summary>
public sealed class ThumbnailBoard(int capacity = 512)
{
    private readonly int _capacity = Math.Max(capacity, 1);
    private readonly Dictionary<ThumbnailKey, ThumbnailState> _states = [];
    private readonly Dictionary<ThumbnailKey, ulong> _used = [];
    private ulong _tick;

    /// <summary>How many entries are held.</summary>
    public int Count => _states.Count;

    /// <summary>The state of a key, or null when none.</summary>
    public ThumbnailState? this[ThumbnailKey key] => _states.TryGetValue(key, out ThumbnailState? s) ? s : null;

    /// <summary>Whether a view showing <paramref name="key"/> should ask the core for it now.</summary>
    public bool NeedsRequest(ThumbnailKey key) => this[key] is null or ThumbnailState.Dropped;

    /// <summary>Sets a key's state, evicting the least recently used entry that is not in flight past the capacity.</summary>
    public void Set(ThumbnailKey key, ThumbnailState state)
    {
        _tick++;
        _states[key] = state;
        _used[key] = _tick;
        if (_states.Count <= _capacity)
        {
            return;
        }
        ThumbnailKey? victim = _used
            .Where(p => _states[p.Key] is not (ThumbnailState.Pending or ThumbnailState.Decoding))
            .OrderBy(p => p.Value).Select(p => (ThumbnailKey?)p.Key).FirstOrDefault();
        if (victim is { } v)
        {
            _states.Remove(v);
            _used.Remove(v);
        }
    }

    /// <summary>Marks a key used (shown) without changing it.</summary>
    public void Touch(ThumbnailKey key)
    {
        if (_states.ContainsKey(key))
        {
            _used[key] = ++_tick;
        }
    }

    /// <summary>Every key waiting for the core (to forget them when the view moves on).</summary>
    public IReadOnlyList<ThumbnailKey> PendingKeys => [.. _states.Where(p => p.Value is ThumbnailState.Pending).Select(p => p.Key)];

    /// <summary>Forgets the pending keys.</summary>
    public void ForgetPending()
    {
        foreach (ThumbnailKey k in PendingKeys)
        {
            _states.Remove(k);
            _used.Remove(k);
        }
    }
}
