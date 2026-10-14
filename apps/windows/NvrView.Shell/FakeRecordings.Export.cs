// Export (minor 10) in the fake recordings store. The same model as the
// macOS FakeCore's: DownloadRange queues a range download (an r- ID from the
// cameras, tiers, and range, so the same request is existing) that
// CompleteRecordingWork finishes like an incident's; its events carry
// incident null and range. EditCompute plans a trim over the download's
// made-up files (a keyframe every 2 s) and clips follow and grid plans to
// in_ms and out_ms; StillPlan plans from a position in a finished file (the
// fake store knows no playbacks, so a playback is unknown). dest must be an
// absolute directory, and without a data directory only the downloads
// directory is accepted. Camera names are Camera N, and Camera A, ... with
// share_safe; file names are cruder than the core's (no formatted time
// stamp).

using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Interop;

namespace NvrView.Shell;

public sealed partial class FakeRecordings
{
    /// <summary>The <c>dest</c> of a request: null, or an absolute directory.</summary>
    private static string? Destination(JsonNode? node, string function)
    {
        if (node is null)
        {
            return null;
        }
        if (node.GetValueKind() != JsonValueKind.String)
        {
            throw Error(NvStatus.InvalidArgument, function);
        }
        string s = node.GetValue<string>();
        bool absolute = s.StartsWith('/') || (s.Length > 2 && s[1] == ':' && (s[2] == '\\' || s[2] == '/'));
        if (!absolute || s.Length > 1024)
        {
            throw Error(NvStatus.InvalidArgument, function);
        }
        return s;
    }

    private static long? Millis(JsonNode? node) =>
        node is JsonValue v && v.TryGetValue(out long l) ? l : null;

    private static bool Flag(JsonObject o, string name) =>
        o[name] is JsonValue v && v.TryGetValue(out bool b) && b;

    /// <summary><c>Camera A</c>, <c>Camera B</c>, ... <c>Camera Z</c>, <c>Camera AA</c>.</summary>
    internal static string NeutralName(int n)
    {
        string s = "";
        int i = n;
        do
        {
            s = (char)('A' + (i % 26)) + s;
            i = (i / 26) - 1;
        }
        while (i >= 0);
        return "Camera " + s;
    }

    /// <summary>The made-up file of a finished download: first picture's time and end, in ms.</summary>
    private (long Start, long End)? FileSpan(FakeDownload d, RecordingCamera c)
    {
        if (!d.Cameras.Contains(c))
        {
            return null;
        }
        if (d.Range is { } r)
        {
            return (r[0] * 1000, r[1] * 1000);
        }
        FakeEvent? e = AllIncidents().FirstOrDefault(x => x.Key == d.Incident);
        return e is null || e.Camera != c ? null : ((e.Start - 5) * 1000, (e.End + 5) * 1000);
    }

    /// <summary>Applies the window, <c>dest</c>, and <c>share_safe</c> to a follow or grid plan.</summary>
    private static string Finish(JsonObject plan, long? inMs, long? outMs, string? dest, bool shareSafe)
    {
        if (Millis(plan["start_ms"]) is long start && Millis(plan["end_ms"]) is long end)
        {
            plan["start_ms"] = Math.Max(start, inMs ?? start);
            plan["end_ms"] = Math.Min(end, outMs ?? end);
        }
        if (plan["segments"] is JsonArray segments)
        {
            var clipped = new JsonArray();
            foreach (JsonNode? n in segments)
            {
                if (n is not JsonObject s || Millis(s["start_ms"]) is not long a || Millis(s["end_ms"]) is not long b)
                {
                    continue;
                }
                if (inMs is long lo && lo > a)
                {
                    s["source_in_ms"] = (Millis(s["source_in_ms"]) ?? 0) + (lo - a);
                    a = lo;
                }
                b = Math.Min(b, outMs ?? b);
                if (a < b)
                {
                    s["start_ms"] = a;
                    s["end_ms"] = b;
                    clipped.Add(s.DeepClone());
                }
            }
            plan["segments"] = clipped;
        }
        if (dest is not null && plan["suggested_file"] is JsonValue f && f.TryGetValue(out string? file))
        {
            plan["suggested_file"] = dest + "/" + file[(file.LastIndexOf('/') + 1)..];
        }
        if (shareSafe)
        {
            plan["share_safe"] = true;
            if (plan["sources"] is JsonArray sources)
            {
                for (int i = 0; i < sources.Count; i++)
                {
                    sources[i]!["name"] = NeutralName(i);
                }
            }
            if (plan["labels"] is JsonArray labels)
            {
                for (int i = 0; i < labels.Count; i++)
                {
                    labels[i]!["name"] = NeutralName(i);
                }
            }
        }
        return plan.ToJsonString();
    }

    private string TrimPlan(JsonObject o, string id, long? inMs, long? outMs, string? dest, bool shareSafe)
    {
        const string F = "nv_edit_compute";
        if (inMs is not long lo || outMs is not long hi)
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        List<RecordingCamera>? wanted = null;
        if (o["cameras"] is JsonArray list)
        {
            wanted = [.. list.Select(n => Camera(n, F)).Distinct()];
            if (wanted.Count == 0)
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
        }
        bool exact = o["cut"] is JsonValue cv && cv.TryGetValue(out string? cut) && cut == "exact";
        lock (_gate)
        {
            Configured(F);
            if (_downloadsDir is null)
            {
                throw Error(NvStatus.NotConfigured, F);
            }
            FakeDownload d = _downloads.FirstOrDefault(x => x.Id == id) ?? throw Error(NvStatus.UnknownId, F);
            if (d.State != "done")
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            var outputs = new JsonArray();
            long first = long.MaxValue;
            long last = long.MinValue;
            foreach (RecordingCamera c in wanted ?? d.Cameras)
            {
                if (FileSpan(d, c) is not { } f)
                {
                    continue;
                }
                long from = Math.Max(lo - f.Start, 0);
                long to = Math.Min(hi, f.End) - f.Start;
                if (from >= to)
                {
                    continue;
                }
                long begin = exact ? from : from / 2000 * 2000;
                string name = shareSafe ? NeutralName(outputs.Count) : $"Camera {c.Channel + 1}";
                outputs.Add((JsonNode)new JsonObject
                {
                    ["source"] = outputs.Count,
                    ["camera"] = CameraNode(c),
                    ["name"] = name,
                    ["file"] = $"{d.Folder}/Camera {c.Channel + 1}.mp4",
                    ["source_in_ms"] = begin,
                    ["source_out_ms"] = to,
                    ["copy_possible"] = !exact,
                    ["suggested_file"] = $"{dest ?? d.Folder}/{(f.Start + begin) / 1000} {name} trim.mp4",
                });
                first = Math.Min(first, f.Start + begin);
                last = Math.Max(last, f.Start + to);
            }
            if (outputs.Count == 0)
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            var plan = new JsonObject
            {
                ["version"] = 1,
                ["kind"] = "trim",
                ["download"] = id,
                ["start_ms"] = first,
                ["end_ms"] = last,
                ["outputs"] = outputs,
            };
            if (shareSafe)
            {
                plan["share_safe"] = true;
            }
            return plan.ToJsonString();
        }
    }

    /// <inheritdoc/>
    /// <summary>The tier of each camera in the last range download request (the Export tab's tests and self-test).</summary>
    public IReadOnlyList<string> LastRangeTiers { get; private set; } = [];

    public string DownloadRange(string request)
    {
        const string F = "nv_download_range";
        JsonObject o = Object(request, F);
        if (Millis(o["start"]) is not long start || Millis(o["end"]) is not long end || start < 0 || end <= start ||
            o["cameras"] is not JsonArray list || list.Count == 0)
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        if (end - start > 86_400 || list.Count > 16)
        {
            throw Error(NvStatus.Limit, F);
        }
        string? dest = Destination(o["dest"], F);
        bool shareSafe = Flag(o, "share_safe");
        var cameras = new List<RecordingCamera>();
        var tiers = new List<string>();
        foreach (JsonNode? n in list)
        {
            RecordingCamera c = Camera(n?["camera"], F);
            string tier = n?["tier"] is JsonValue tv && tv.TryGetValue(out string? t) ? t : "main";
            if (tier is not ("main" or "sub"))
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            if (!cameras.Contains(c))
            {
                cameras.Add(c);
                tiers.Add(tier);
            }
        }
        JsonObject answer;
        FakeDownload? queued = null;
        lock (_gate)
        {
            Configured(F);
            if (_downloadsDir is null)
            {
                throw Error(NvStatus.NotConfigured, F);
            }
            // No data directory in the fake: only the downloads directory.
            if (dest is not null && dest != _downloadsDir)
            {
                throw Error(NvStatus.NotConfigured, F);
            }
            if (cameras.Any(c => !Allowed(c)))
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            LastRangeTiers = tiers;
            ulong h = 0xcbf29ce484222325;
            string canonical = string.Join(',', cameras.Select((c, i) => $"{c.Device}/{c.Channel}/{c.Lens}/{tiers[i]}").Order(StringComparer.Ordinal));
            foreach (byte b in System.Text.Encoding.UTF8.GetBytes($"{start}-{end}-{canonical}"))
            {
                h = unchecked((h ^ b) * 0x100000001b3);
            }
            string id = $"r-{start}-{h:x}";
            if (_downloads.FirstOrDefault(d => d.Id == id) is { } existing)
            {
                answer = new JsonObject { ["download"] = id, ["folder"] = existing.Folder, ["existing"] = true };
            }
            else
            {
                if (_downloads.Count >= 1000)
                {
                    throw Error(NvStatus.Limit, F);
                }
                IEnumerable<string> names = cameras.Select((c, i) => shareSafe ? NeutralName(i) : $"Camera {c.Channel + 1}");
                string label = o["name"] is JsonValue nv && nv.TryGetValue(out string? text) && !string.IsNullOrEmpty(text) && !shareSafe ? text : "range";
                _nextDownload++;
                queued = new FakeDownload
                {
                    Id = id,
                    Incident = "",
                    Folder = $"{dest ?? _downloadsDir}/{start}-{end} {label} — {string.Join(", ", names)}",
                    Cameras = cameras,
                    Range = [start, end],
                };
                _downloads.Add(queued);
                answer = new JsonObject { ["download"] = id, ["folder"] = queued.Folder, ["existing"] = false };
            }
        }
        if (queued is not null)
        {
            Queued(queued);
        }
        return answer.ToJsonString();
    }

    /// <inheritdoc/>
    public string StillPlan(string request)
    {
        const string F = "nv_still_plan";
        JsonObject o = Object(request, F);
        string? dest = Destination(o["dest"], F);
        bool shareSafe = Flag(o, "share_safe");
        string format = o["format"] is JsonValue fv && fv.TryGetValue(out string? fs) ? fs : "png";
        if (format is not ("png" or "jpeg"))
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        string ext = format == "png" ? "png" : "jpg";
        bool playback = o.ContainsKey("playback");
        bool file = o.ContainsKey("download");
        if (playback == file)
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        if (playback)
        {
            if (Millis(o["playback"]) is null)
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            throw Error(NvStatus.UnknownId, F);
        }
        string id = o["download"] is JsonValue dv && dv.TryGetValue(out string? ds) ? ds : throw Error(NvStatus.InvalidArgument, F);
        long at = Millis(o["at_ms"]) ?? throw Error(NvStatus.InvalidArgument, F);
        RecordingCamera c = Camera(o["camera"], F);
        bool exact = o["exact"] is not JsonValue ev || !ev.TryGetValue(out bool eb) || eb;
        lock (_gate)
        {
            Configured(F);
            if (_downloadsDir is null)
            {
                throw Error(NvStatus.NotConfigured, F);
            }
            FakeDownload d = _downloads.FirstOrDefault(x => x.Id == id) ?? throw Error(NvStatus.UnknownId, F);
            if (d.State != "done" || FileSpan(d, c) is not { } f)
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            long pos = at - f.Start;
            if (pos < 0 || pos >= f.End - f.Start)
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            long sourceIn = exact ? pos : pos / 2000 * 2000;
            string name = shareSafe ? NeutralName(0) : $"Camera {c.Channel + 1}";
            var still = new JsonObject
            {
                ["camera"] = CameraNode(c),
                ["name"] = name,
                ["file"] = $"{d.Folder}/Camera {c.Channel + 1}.mp4",
                ["source_in_ms"] = sourceIn,
                ["position_ms"] = f.Start + sourceIn,
                ["suggested_file"] = $"{dest ?? d.Folder}/{f.Start + sourceIn} {name}.{ext}",
            };
            if (exact && sourceIn % 2000 != 0)
            {
                still["keyframe_in_ms"] = sourceIn / 2000 * 2000;
            }
            return new JsonObject { ["stills"] = new JsonArray(still) }.ToJsonString();
        }
    }
}
