// Recordings, part 1 (minor 5; docs/FFI.md "Recordings (minor 5)") as the
// app sees it: the events of docs/FFI-RECORDINGS.md section 8, parsed, and
// the request objects the calls take. Plain .NET, tested on Linux. The calls
// themselves are NvrView.Interop's IRecordings (the core) and FakeRecordings
// (a made-up store, for the views before they meet the core).

using System.Text.Json;
using System.Text.Json.Nodes;

namespace NvrView.Shell;

/// <summary>A camera in recordings JSON: <c>{"device": "&lt;device key&gt;", "channel": 3, "lens": 0}</c>.</summary>
public sealed record RecordingCamera(string Device, int Channel, int Lens = 0)
{
    /// <summary>The JSON object, for building requests.</summary>
    public string Json => new JsonObject
    {
        ["device"] = Device,
        ["channel"] = Channel,
        ["lens"] = Lens,
    }.ToJsonString();

    /// <summary>Reads a camera object (lens 0 when left out); null when malformed.</summary>
    public static RecordingCamera? From(JsonElement e) =>
        e.ValueKind == JsonValueKind.Object &&
        e.TryGetProperty("device", out JsonElement d) && d.ValueKind == JsonValueKind.String &&
        e.TryGetProperty("channel", out JsonElement c) && c.ValueKind == JsonValueKind.Number && c.TryGetInt32(out int channel)
            ? new RecordingCamera(d.GetString()!, channel,
                e.TryGetProperty("lens", out JsonElement l) && l.ValueKind == JsonValueKind.Number && l.TryGetInt32(out int lens) ? lens : 0)
            : null;
}

/// <summary>A thumbnail key (section 4): one camera, tier, and range.</summary>
public sealed record ThumbnailKey(RecordingCamera Camera, string Tier, long Start, long End)
{
    /// <summary>The JSON object the thumbnail functions take.</summary>
    public string Json =>
        $"{{\"camera\":{Camera.Json},\"tier\":{JsonValue.Create(Tier).ToJsonString()},\"start\":{Start},\"end\":{End}}}";

    /// <summary>Reads a key object (tier sub when left out); null when malformed.</summary>
    public static ThumbnailKey? From(JsonElement e)
    {
        if (e.ValueKind != JsonValueKind.Object || !e.TryGetProperty("camera", out JsonElement c) ||
            RecordingCamera.From(c) is not { } camera ||
            !e.TryGetProperty("start", out JsonElement s) || !s.TryGetInt64(out long start) ||
            !e.TryGetProperty("end", out JsonElement en) || !en.TryGetInt64(out long end))
        {
            return null;
        }
        string tier = e.TryGetProperty("tier", out JsonElement t) && t.ValueKind == JsonValueKind.String ? t.GetString()! : "sub";
        return new ThumbnailKey(camera, tier, start, end);
    }
}

/// <summary>One camera of a download.</summary>
public sealed record DownloadCamera(
    RecordingCamera Camera,
    string? Name,
    string State,
    bool Paused,
    int ChunksDone,
    int ChunksTotal,
    long Bytes,
    long? BytesEstimate,
    string? Reason,
    string? Detail,
    string? Output);

/// <summary>A download's progress (the <c>download</c> event, and each entry of <c>nv_downloads_get</c>).</summary>
public sealed record DownloadProgress(
    string Download,
    string? Incident,
    string? Folder,
    string State,
    bool Paused,
    IReadOnlyList<DownloadCamera> Cameras)
{
    /// <summary>Reads one download object; null when malformed.</summary>
    public static DownloadProgress? From(JsonElement e)
    {
        if (e.ValueKind != JsonValueKind.Object || Str(e, "download") is not { } id || Str(e, "state") is not { } state)
        {
            return null;
        }
        var cameras = new List<DownloadCamera>();
        if (e.TryGetProperty("cameras", out JsonElement list) && list.ValueKind == JsonValueKind.Array)
        {
            foreach (JsonElement c in list.EnumerateArray())
            {
                if (!c.TryGetProperty("camera", out JsonElement ce) || RecordingCamera.From(ce) is not { } camera)
                {
                    return null;
                }
                cameras.Add(new DownloadCamera(
                    camera, Str(c, "name"), Str(c, "state") ?? "queued", Bool(c, "paused"),
                    (int)(Long(c, "chunks_done") ?? 0), (int)(Long(c, "chunks_total") ?? 0), Long(c, "bytes") ?? 0,
                    Long(c, "bytes_estimate"), Str(c, "reason"), Str(c, "detail"), Str(c, "output")));
            }
        }
        return new DownloadProgress(id, Str(e, "incident"), Str(e, "folder"), state, Bool(e, "paused"), cameras);
    }

    /// <summary>The <c>downloads</c> list of <c>nv_downloads_get</c>; null when malformed.</summary>
    public static IReadOnlyList<DownloadProgress>? ParseList(string json)
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json);
            var list = new List<DownloadProgress>();
            foreach (JsonElement d in doc.RootElement.GetProperty("downloads").EnumerateArray())
            {
                if (From(d) is not { } p)
                {
                    return null;
                }
                list.Add(p);
            }
            return list;
        }
        catch (Exception e) when (e is JsonException or KeyNotFoundException or InvalidOperationException)
        {
            return null;
        }
    }

    internal static string? Str(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    // A number member, null when missing, null, or not a whole number. JsonElement.TryGetInt64 throws on anything
    // but a number, so a JSON null (bytes_estimate before the device's estimate, output before the file) is checked
    // first; it used to throw out of the event parser and drop the whole event.
    internal static long? Long(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.Number && v.TryGetInt64(out long n) ? n : null;

    internal static bool Bool(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.True;
}

/// <summary>What a device's backend can do with recordings (<c>device_info</c>'s <c>recordings</c>).</summary>
public sealed record RecordingSupport(bool Search, bool Playback, bool Download, bool Thumbnails)
{
    /// <summary>The flags of a <c>device_info</c> event (all false when it has none).</summary>
    public static RecordingSupport FromDeviceInfo(ReadOnlySpan<byte> json)
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json.ToArray());
            if (doc.RootElement.TryGetProperty("recordings", out JsonElement r) && r.ValueKind == JsonValueKind.Object)
            {
                return new RecordingSupport(
                    DownloadProgress.Bool(r, "search"), DownloadProgress.Bool(r, "playback"),
                    DownloadProgress.Bool(r, "download"), DownloadProgress.Bool(r, "thumbnails"));
            }
        }
        catch (JsonException)
        {
        }
        return new RecordingSupport(false, false, false, false);
    }
}

/// <summary>
/// One event of FFI-RECORDINGS.md section 8 (part 1). Unknown fields are
/// ignored; <see cref="Parse"/> returns null for other types and for events
/// missing what they cannot do without.
/// </summary>
public abstract record RecordingsEvent
{
    /// <summary>A device's search lane: <c>idle</c>, <c>busy</c>, <c>waiting_device</c>, <c>ended</c>.</summary>
    public sealed record Index(string Device, string State, int PendingDays, long Revision) : RecordingsEvent;

    /// <summary>One camera-day could not be searched; not retried automatically.</summary>
    public sealed record DayFailed(string Device, RecordingCamera? Camera, long Day, string? Reason) : RecordingsEvent;

    /// <summary>Days <c>Start</c> to <c>End</c> of a device changed; query again.</summary>
    public sealed record Changed(string Device, long Revision, long Start, long End) : RecordingsEvent;

    /// <summary>Incidents changed.</summary>
    public sealed record IncidentsChanged(long Revision) : RecordingsEvent;

    /// <summary>Camera groups changed.</summary>
    public sealed record CameraGroupsChanged(long Revision) : RecordingsEvent;

    /// <summary><c>ready</c> (call ThumbnailGet), <c>empty</c>, <c>failed</c> (Reason), <c>dropped</c>.</summary>
    public sealed record Thumbnail(ThumbnailKey Key, string State, string? Reason) : RecordingsEvent;

    /// <summary>A download progressed.</summary>
    public sealed record Download(DownloadProgress Progress) : RecordingsEvent;

    /// <summary>After forget or trash; with Trash, the folder the app moves to the Recycle Bin now.</summary>
    public sealed record DownloadRemoved(string Id, bool Trash, string? Folder) : RecordingsEvent;

    /// <summary>A camera on the "always listen" list: <c>watching</c>, <c>stopped</c>, or <c>refused</c> (Reason: <c>no_audio</c>, <c>device_closed</c>, <c>sound_off</c>).</summary>
    public sealed record SoundWatch(RecordingCamera Camera, string State, string? Reason) : RecordingsEvent;

    /// <summary>A recorded day's sound analysis: <c>queued</c>, <c>running</c>, <c>done</c> (Seconds, Labels), <c>failed</c> (Reason), or <c>canceled</c>.</summary>
    public sealed record SoundAnalyze(
        long Request, RecordingCamera Camera, long? Day, string State, string? Reason, long? Seconds, long? Labels)
        : RecordingsEvent;

    /// <summary>The event types this record parses.</summary>
    public static readonly IReadOnlySet<string> Types = new HashSet<string>
    {
        "recordings_index", "recordings_day_failed", "recordings_changed", "incidents_changed",
        "camera_groups_changed", "thumbnail", "download", "download_removed", "sound_watch", "sound_analyze",
    };

    /// <summary>Parses one event; null for another type or a malformed one.</summary>
    public static RecordingsEvent? Parse(ReadOnlySpan<byte> json)
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json.ToArray());
            JsonElement e = doc.RootElement;
            string? device = DownloadProgress.Str(e, "device");
            long? revision = DownloadProgress.Long(e, "revision");
            return DownloadProgress.Str(e, "type") switch
            {
                "recordings_index" when device is not null && DownloadProgress.Str(e, "state") is { } state =>
                    new Index(device, state, (int)(DownloadProgress.Long(e, "pending_days") ?? 0), revision ?? 0),
                "recordings_day_failed" when device is not null && DownloadProgress.Long(e, "day") is { } day =>
                    new DayFailed(device, e.TryGetProperty("camera", out JsonElement c) ? RecordingCamera.From(c) : null,
                        day, DownloadProgress.Str(e, "reason")),
                "recordings_changed" when device is not null && DownloadProgress.Long(e, "start") is { } start &&
                    DownloadProgress.Long(e, "end") is { } end =>
                    new Changed(device, revision ?? 0, start, end),
                "incidents_changed" when revision is { } r => new IncidentsChanged(r),
                "camera_groups_changed" when revision is { } r => new CameraGroupsChanged(r),
                "thumbnail" when e.TryGetProperty("key", out JsonElement k) && ThumbnailKey.From(k) is { } key &&
                    DownloadProgress.Str(e, "state") is { } state =>
                    new Thumbnail(key, state, DownloadProgress.Str(e, "reason")),
                "download" when DownloadProgress.From(e) is { } p => new Download(p),
                "download_removed" when DownloadProgress.Str(e, "download") is { } id =>
                    new DownloadRemoved(id, DownloadProgress.Bool(e, "trash"), DownloadProgress.Str(e, "folder")),
                "sound_watch" when e.TryGetProperty("camera", out JsonElement wc) && RecordingCamera.From(wc) is { } watched &&
                    DownloadProgress.Str(e, "state") is { } watchState =>
                    new SoundWatch(watched, watchState, DownloadProgress.Str(e, "reason")),
                "sound_analyze" when DownloadProgress.Long(e, "request") is { } request &&
                    e.TryGetProperty("camera", out JsonElement ac) && RecordingCamera.From(ac) is { } analyzed &&
                    DownloadProgress.Str(e, "state") is { } analyzeState =>
                    new SoundAnalyze(request, analyzed, DownloadProgress.Long(e, "day"), analyzeState,
                        DownloadProgress.Str(e, "reason"), DownloadProgress.Long(e, "seconds"),
                        DownloadProgress.Long(e, "labels")),
                _ => null,
            };
        }
        catch (Exception ex) when (ex is JsonException or InvalidOperationException or FormatException)
        {
            return null;
        }
    }
}
