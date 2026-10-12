// Edit lists and the rest of sound (minor 8) in the fake recordings store.
// The same model as the macOS FakeCore's: EditCompute plans a follow cut or
// a grid over a finished download of the made-up store (one camera per
// incident here, so a follow cut is one segment and a grid one cell);
// SoundWatch keeps a list and sends sound_watch events; loudness lanes are
// null except for watched cameras (a steady -40 dBFS peak, -50 RMS);
// SoundLabelsPut keeps the labels as events with source local_audio on
// indexed days, replacing the same analyzer's labels in the request's range
// (as the core does); SoundAnalyzeDay queues a request that
// CompleteRecordingWork finishes with sound_analyze running and done (no
// seconds, no labels). GetTileLoudness is not a tile model's: the fake store
// knows no tiles, so every tile is unknown.

using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Interop;

namespace NvrView.Shell;

public sealed partial class FakeRecordings
{
    private readonly List<RecordingCamera> _watched = [];
    private readonly List<(RecordingCamera Camera, string Kind, long Start, long End, string Analyzer)> _labels = [];

    /// <summary>The cameras on the sound "always listen" list (tests).</summary>
    public IReadOnlyList<RecordingCamera> Watched
    {
        get
        {
            lock (_gate)
            {
                return [.. _watched];
            }
        }
    }
    private readonly List<(ulong Request, RecordingCamera Camera, long Day)> _analyses = [];
    private ulong _nextAnalysis = 1;

    private void RequireKnownCamera(RecordingCamera c, string function)
    {
        if (!Allowed(c))
        {
            throw Error(NvStatus.InvalidArgument, function);
        }
    }

    /// <inheritdoc/>
    public string EditCompute(string json)
    {
        const string F = "nv_edit_compute";
        JsonObject o = Object(json, F);
        string kind = o["kind"] is JsonValue k && k.TryGetValue(out string? ks) ? ks : throw Error(NvStatus.InvalidArgument, F);
        string id = o["download"] is JsonValue d && d.TryGetValue(out string? ds) ? ds : throw Error(NvStatus.InvalidArgument, F);
        switch (kind)
        {
            case "follow" or "grid":
                break;
            case "trim":
                throw Error(NvStatus.Unsupported, F);
            default:
                throw Error(NvStatus.InvalidArgument, F);
        }
        if (o.ContainsKey("in_ms") || o.ContainsKey("out_ms"))
        {
            throw Error(NvStatus.Unsupported, F);
        }
        lock (_gate)
        {
            Configured(F);
            if (_downloadsDir is null)
            {
                throw Error(NvStatus.NotConfigured, F);
            }
            FakeDownload download = _downloads.FirstOrDefault(x => x.Id == id) ?? throw Error(NvStatus.UnknownId, F);
            FakeEvent e = AllIncidents().FirstOrDefault(x => x.Key == download.Incident)
                ?? throw Error(NvStatus.InvalidArgument, F);
            if (download.State != "done" || !download.Cameras.Contains(e.Camera))
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            long start = (e.Start - 5) * 1000;
            long end = (e.End + 5) * 1000;
            var plan = new JsonObject
            {
                ["version"] = 1,
                ["kind"] = kind,
                ["download"] = id,
                ["start_ms"] = start,
                ["end_ms"] = end,
                ["suggested_file"] = $"{download.Folder}/{e.Start} {kind}.mp4",
                ["sources"] = new JsonArray(new JsonObject
                {
                    ["source"] = 0,
                    ["camera"] = CameraNode(e.Camera),
                    ["name"] = $"Camera {e.Camera.Channel + 1}",
                    ["file"] = $"{download.Folder}/Camera {e.Camera.Channel + 1}.mp4",
                    ["file_start_ms"] = start,
                    ["offset_ms"] = 0,
                    ["codec"] = "h264",
                    ["width"] = 1920,
                    ["height"] = 1080,
                    ["duration_ms"] = end - start,
                }),
            };
            if (kind == "follow")
            {
                plan["copy_possible"] = (o["cut"] as JsonValue)?.TryGetValue(out string? cut) != true || cut == "keyframe";
                plan["segments"] = new JsonArray(new JsonObject
                {
                    ["start_ms"] = e.Start * 1000,
                    ["end_ms"] = e.End * 1000,
                    ["source"] = 0,
                    ["source_in_ms"] = (e.Start * 1000) - start,
                });
                plan["labels"] = new JsonArray(new JsonObject { ["source"] = 0, ["name"] = $"Camera {e.Camera.Channel + 1}", ["time"] = true });
            }
            else
            {
                JsonObject? canvas = o["canvas"] as JsonObject;
                int width = canvas?["width"] is JsonValue w && w.TryGetValue(out int wv) ? wv : 1920;
                int height = canvas?["height"] is JsonValue h && h.TryGetValue(out int hv) ? hv : 1080;
                if (width is < 1 or > 16_384 || height is < 1 or > 16_384)
                {
                    throw Error(NvStatus.InvalidArgument, F);
                }
                plan["layout"] = "1up";
                plan["copy_possible"] = false;
                plan["canvas"] = new JsonObject { ["width"] = width, ["height"] = height };
                plan["cells"] = new JsonArray(new JsonObject
                {
                    ["rect"] = new JsonObject { ["x"] = 0, ["y"] = 0, ["width"] = width, ["height"] = height },
                    ["source"] = 0,
                    ["fit"] = o["fit"] is JsonValue f && f.TryGetValue(out string? fit) ? fit : "fit",
                    ["present"] = new JsonArray(new JsonArray(start, end)),
                });
            }
            return plan.ToJsonString();
        }
    }

    /// <inheritdoc/>
    public void SoundWatch(string camera, bool enabled)
    {
        const string F = "nv_sound_watch";
        RecordingCamera c = Camera(Object(camera, F), F);
        bool changed;
        lock (_gate)
        {
            Configured(F);
            if (enabled)
            {
                if (_watched.Contains(c))
                {
                    return;
                }
                if (_watched.Count >= 64)
                {
                    throw Error(NvStatus.Limit, F);
                }
                _watched.Add(c);
                changed = true;
            }
            else
            {
                changed = _watched.Remove(c);
            }
            if (changed)
            {
                Emit(new JsonObject
                {
                    ["type"] = "sound_watch",
                    ["camera"] = CameraNode(c),
                    ["state"] = enabled ? "watching" : "stopped",
                });
            }
        }
    }

    /// <inheritdoc/>
    public string RecordingsLoudness(string query)
    {
        const string F = "nv_recordings_loudness";
        JsonObject o = Object(query, F);
        if (o["start"] is not JsonValue s || !s.TryGetValue(out long start) ||
            o["end"] is not JsonValue e || !e.TryGetValue(out long end) || end < start || start is < 0 or > 253_402_300_799)
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        int bucket = o["bucket_s"] is JsonValue b && b.TryGetValue(out int bv) ? bv : 60;
        if (bucket is < 1 or > 3600)
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        long count = ((end - start) / bucket) + 1;
        if (count > 100_000)
        {
            throw Error(NvStatus.Limit, F);
        }
        lock (_gate)
        {
            Configured(F);
            List<RecordingCamera> cameras = o["cameras"] is JsonArray list ? [.. list.Select(n => Camera(n, F))] : [.. _watched];
            JsonNode?[] Fill(bool heard, int value) =>
                [.. Enumerable.Range(0, (int)count).Select(_ => heard ? (JsonNode?)JsonValue.Create(value) : null)];
            var lanes = new JsonArray([.. cameras.Select(c =>
            {
                bool heard = _watched.Contains(c) && Allowed(c);
                return (JsonNode)new JsonObject
                {
                    ["camera"] = CameraNode(c),
                    ["bucket_s"] = bucket,
                    ["start"] = start,
                    ["peak_db"] = new JsonArray(Fill(heard, -40)),
                    ["rms_db"] = new JsonArray(Fill(heard, -50)),
                };
            })]);
            return new JsonObject { ["revision"] = _revision, ["lanes"] = lanes }.ToJsonString();
        }
    }

    /// <inheritdoc/>
    public void SoundLabelsPut(string json)
    {
        const string F = "nv_sound_labels_put";
        JsonObject o = Object(json, F);
        RecordingCamera c = Camera(o["camera"], F);
        if (o["analyzer"] is not JsonValue a || !a.TryGetValue(out string? analyzer) || analyzer.Length == 0 || analyzer == "pattern-v1" ||
            o["start"] is not JsonValue s || !s.TryGetValue(out long start) ||
            o["end"] is not JsonValue e || !e.TryGetValue(out long end) || end < start ||
            o["labels"] is not JsonArray labels)
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        if (end - start > 31 * Day || labels.Count > 20_000)
        {
            throw Error(NvStatus.Limit, F);
        }
        var parsed = new List<(string Kind, long Start, long End)>();
        foreach (JsonNode? l in labels)
        {
            if (l is not JsonObject lo || lo["kind"] is not JsonValue kv || !kv.TryGetValue(out string? kind) || kind.Length == 0 ||
                !kind.All(ch => ch is (>= 'a' and <= 'z') or (>= '0' and <= '9') or '_') ||
                lo["start"] is not JsonValue ls || !ls.TryGetValue(out long labelStart) ||
                lo["end"] is not JsonValue le || !le.TryGetValue(out long labelEnd) || labelEnd < labelStart)
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            parsed.Add((kind, labelStart, labelEnd));
        }
        lock (_gate)
        {
            Configured(F);
            RequireKnownCamera(c, F);
            _labels.RemoveAll(l => l.Camera == c && l.Analyzer == analyzer && l.Start >= start && l.Start <= end);
            _labels.AddRange(parsed.Select(p => (c, p.Kind, p.Start, p.End, analyzer)));
            _revision++;
            _incidentsRevision++;
            Emit(new JsonObject { ["type"] = "recordings_changed", ["device"] = c.Device, ["revision"] = _revision, ["start"] = start, ["end"] = end });
            Emit(new JsonObject { ["type"] = "incidents_changed", ["revision"] = _incidentsRevision });
        }
    }

    /// <inheritdoc/>
    public string SoundAnalyzeDay(string json)
    {
        const string F = "nv_sound_analyze_day";
        JsonObject o = Object(json, F);
        RecordingCamera c = Camera(o["camera"], F);
        if (o["day"] is not JsonValue d || !d.TryGetValue(out long day) || day < 0)
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        ulong request;
        lock (_gate)
        {
            Configured(F);
            RequireKnownCamera(c, F);
            if (_analyses.Count >= 32)
            {
                throw Error(NvStatus.Limit, F);
            }
            request = _nextAnalysis++;
            _analyses.Add((request, c, day));
            Emit(AnalysisEvent(request, c, day, "queued"));
        }
        ScheduleWork();
        return new JsonObject { ["request"] = request, ["state"] = "queued" }.ToJsonString();
    }

    /// <inheritdoc/>
    public void SoundAnalyzeCancel(ulong request)
    {
        const string F = "nv_sound_analyze_cancel";
        lock (_gate)
        {
            Configured(F);
            int i = _analyses.FindIndex(a => a.Request == request);
            if (i < 0)
            {
                throw Error(NvStatus.UnknownId, F);
            }
            (_, RecordingCamera c, long day) = _analyses[i];
            _analyses.RemoveAt(i);
            Emit(AnalysisEvent(request, c, day, "canceled"));
        }
    }

    /// <inheritdoc/>
    public TileLoudness GetTileLoudness(ulong tile, int capacity) =>
        throw Error(NvStatus.UnknownId, "nv_tile_loudness");

    private static JsonObject AnalysisEvent(ulong request, RecordingCamera c, long day, string state) => new()
    {
        ["type"] = "sound_analyze",
        ["request"] = request,
        ["camera"] = CameraNode(c),
        ["day"] = day,
        ["state"] = state,
    };

    // Called with the gate held, from CompleteRecordingWork.
    private void CompleteSoundAnalyses()
    {
        foreach ((ulong request, RecordingCamera c, long day) in _analyses)
        {
            Emit(AnalysisEvent(request, c, day, "running"));
            JsonObject done = AnalysisEvent(request, c, day, "done");
            done["seconds"] = 0;
            done["labels"] = 0;
            Emit(done);
        }
        _analyses.Clear();
    }
}
