// Edit lists and the rest of sound (minor 8) in the fake recordings store.
// The same model as the macOS FakeCore's: EditCompute plans a follow cut or
// a grid over a finished download of the made-up store (one camera per
// incident here, so a follow cut is one segment and a grid one cell);
// SoundWatch keeps a list and sends sound_watch events; loudness lanes are
// null except for watched cameras (a steady -40 dBFS peak, -50 RMS);
// SoundLabelsPut keeps the labels as events with source local_audio on
// indexed days, replacing the same analyzer's labels in the request's range
// (as the core does); SoundAnalyzeDay queues a request that
// CompleteRecordingWork finishes from the synthetic downloads (no_source, no_audio, or
// running and done with heard seconds and a label). GetTileLoudness is not a tile model's: the fake store
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

    // Seconds of audio analyzed from downloads: first-lens camera, span.
    private readonly List<(RecordingCamera Camera, long Start, long End)> _analyzed = [];

    private void RequireKnownCamera(RecordingCamera c, string function)
    {
        if (!Allowed(c))
        {
            throw Error(NvStatus.InvalidArgument, function);
        }
    }

    // The event of camera c that happens along with the incident's event e (the same kind, overlapping it): e itself for its
    // own camera. The made-up store has one event per incident, so an incident with several cameras (an edit would make
    // one) is the events of the same kind that overlap.
    private FakeEvent? ConcurrentEvent(FakeEvent e, RecordingCamera c)
    {
        if (c == e.Camera)
        {
            return e;
        }
        return Allowed(c)
            ? EventsOf(c, DeviceTime.DayStart(e.Start)).FirstOrDefault(x => x.Kind == e.Kind && x.Start <= e.End && e.Start <= x.End)
            : null;
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
            // Every camera of the download is a source of 640 by 360 h264 at 10 frames a second from 5 s before its
            // event to 5 s after (the synthetic files the self-test writes are such files); the fake writes none itself.
            var members = new List<(RecordingCamera Camera, FakeEvent Event)>();
            foreach (RecordingCamera c in download.Cameras)
            {
                if (ConcurrentEvent(e, c) is { } concurrent)
                {
                    members.Add((c, concurrent));
                }
            }
            if (members.Count == 0)
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            (long Start, long End) Range(int i) => ((members[i].Event.Start - 5) * 1000, (members[i].Event.End + 5) * 1000);
            var plan = new JsonObject
            {
                ["version"] = 1,
                ["kind"] = kind,
                ["download"] = id,
                ["suggested_file"] = $"{download.Folder}/{e.Start} {kind}.mp4",
                ["sources"] = new JsonArray([.. members.Select((m, i) => (JsonNode)new JsonObject
                {
                    ["source"] = i,
                    ["camera"] = CameraNode(m.Camera),
                    ["name"] = $"Camera {m.Camera.Channel + 1}",
                    ["file"] = $"{download.Folder}/Camera {m.Camera.Channel + 1}.mp4",
                    ["file_start_ms"] = Range(i).Start,
                    ["offset_ms"] = 0,
                    ["codec"] = "h264",
                    ["width"] = 640,
                    ["height"] = 360,
                    ["duration_ms"] = Range(i).End - Range(i).Start,
                })]),
            };
            if (kind == "follow")
            {
                // At each moment the camera whose event started last (section 7's overlap rule, without priorities, holds, or
                // pre-roll); where no event runs, the previous shot continues while its file has video.
                long[] bounds = [.. members.SelectMany(m => new[] { m.Event.Start * 1000, m.Event.End * 1000 }).Distinct().Order()];
                var segments = new List<(long Start, long End, int Source)>();
                for (int bi = 0; bi + 1 < bounds.Length; bi++)
                {
                    long a0 = bounds[bi], b0 = bounds[bi + 1];
                    int pick = -1;
                    long newest = long.MinValue;
                    for (int i = 0; i < members.Count; i++)
                    {
                        FakeEvent ev = members[i].Event;
                        if (ev.Start * 1000 <= a0 && a0 < ev.End * 1000 && ev.Start >= newest)
                        {
                            newest = ev.Start;
                            pick = i;
                        }
                    }
                    if (pick < 0 && segments.Count > 0 && segments[^1].End == a0 && Range(segments[^1].Source).End >= b0)
                    {
                        pick = segments[^1].Source;
                    }
                    if (pick < 0)
                    {
                        continue;
                    }
                    if (segments.Count > 0 && segments[^1].Source == pick && segments[^1].End == a0)
                    {
                        segments[^1] = (segments[^1].Start, b0, pick);
                    }
                    else
                    {
                        segments.Add((a0, b0, pick));
                    }
                }
                if (segments.Count == 0)
                {
                    throw Error(NvStatus.InvalidArgument, F);
                }
                plan["start_ms"] = segments[0].Start;
                plan["end_ms"] = segments[^1].End;
                plan["copy_possible"] = (o["cut"] as JsonValue)?.TryGetValue(out string? cut) != true || cut == "keyframe";
                plan["segments"] = new JsonArray([.. segments.Select(g => (JsonNode)new JsonObject
                {
                    ["start_ms"] = g.Start,
                    ["end_ms"] = g.End,
                    ["source"] = g.Source,
                    ["source_in_ms"] = g.Start - Range(g.Source).Start,
                })]);
                plan["labels"] = new JsonArray([.. members.Select((m, i) => (JsonNode)new JsonObject
                {
                    ["source"] = i,
                    ["name"] = $"Camera {m.Camera.Channel + 1}",
                    ["time"] = true,
                })]);
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
                string layout = o["layout"] is JsonValue l && l.TryGetValue(out string? ls) && ls is "2up" or "4up"
                    ? ls! : members.Count == 1 ? "1up" : members.Count == 2 ? "2up" : "4up";
                (int cols, int rows) = layout == "4up" ? (2, 2) : layout == "1up" ? (1, 1) : width >= height ? (2, 1) : (1, 2);
                plan["layout"] = layout;
                plan["start_ms"] = Enumerable.Range(0, members.Count).Min(i => Range(i).Start);
                plan["end_ms"] = Enumerable.Range(0, members.Count).Max(i => Range(i).End);
                plan["copy_possible"] = false;
                plan["canvas"] = new JsonObject { ["width"] = width, ["height"] = height };
                string fit = o["fit"] is JsonValue f && f.TryGetValue(out string? fs) ? fs : "fit";
                plan["cells"] = new JsonArray([.. members.Take(cols * rows).Select((m, i) =>
                {
                    int x0 = i % cols * width / cols, x1 = (i % cols + 1) * width / cols;
                    int y0 = i / cols * height / rows, y1 = (i / cols + 1) * height / rows;
                    return (JsonNode)new JsonObject
                    {
                        ["rect"] = new JsonObject { ["x"] = x0, ["y"] = y0, ["width"] = x1 - x0, ["height"] = y1 - y0 },
                        ["source"] = i,
                        ["fit"] = fit,
                        ["present"] = new JsonArray(new JsonArray(Range(i).Start, Range(i).End)),
                    };
                })]);
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
            var lanes = new JsonArray([.. cameras.Select(c =>
            {
                bool live = _watched.Contains(c) && Allowed(c);
                // A bucket is heard when the camera listens or an analyzed download reaches into it.
                bool[] heard = [.. Enumerable.Range(0, (int)count).Select(i =>
                {
                    long from = start + (i * (long)bucket), to = from + bucket - 1;
                    return live || _analyzed.Any(a => a.Camera == c && a.Start <= to && a.End >= from);
                })];
                JsonNode?[] Fill(int value) => [.. heard.Select(h => h ? (JsonNode?)JsonValue.Create(value) : null)];
                return (JsonNode)new JsonObject
                {
                    ["camera"] = CameraNode(c),
                    ["bucket_s"] = bucket,
                    ["start"] = start,
                    ["peak_db"] = new JsonArray(Fill(-40)),
                    ["rms_db"] = new JsonArray(Fill(-50)),
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
    // As the core does from finished downloads: with no downloads folder a request fails no_source, with no finished
    // download of that camera reaching into the day no_audio; otherwise running, then done, with every second of
    // the downloads inside the day heard (loudness) and one smoke_alarm label per download (the pattern detector's,
    // replacing its earlier ones over that audio).
    private void CompleteSoundAnalyses()
    {
        bool changed = false;
        foreach ((ulong request, RecordingCamera c, long day) in _analyses)
        {
            JsonObject last;
            if (_downloadsDir is null)
            {
                last = AnalysisEvent(request, c, day, "failed");
                last["reason"] = "no_source";
            }
            else
            {
                long dayStart = DeviceTime.DayStart(day);
                List<(long Start, long End)> spans = [];
                foreach (FakeDownload d in _downloads.Where(d => d.State == "done" && d.Cameras.Contains(c)))
                {
                    long s = Math.Max(d.Start, dayStart), e = Math.Min(d.End, dayStart + DeviceTime.Day - 1);
                    if (s <= e)
                    {
                        spans.Add((s, e));
                    }
                }
                if (spans.Count == 0)
                {
                    last = AnalysisEvent(request, c, day, "failed");
                    last["reason"] = "no_audio";
                }
                else
                {
                    RecordingCamera mic = c with { Lens = 0 };
                    long seconds = 0;
                    foreach ((long s, long e) in spans)
                    {
                        seconds += e - s + 1;
                        _analyzed.RemoveAll(a => a.Camera == mic && a.Start == s);
                        _analyzed.Add((mic, s, e));
                        _labels.RemoveAll(l => l.Camera == c && l.Analyzer == "pattern-v1" && l.Start >= s && l.Start <= e);
                        _labels.Add((c, "smoke_alarm", s, s + Math.Min(4, e - s), "pattern-v1"));
                    }
                    _revision++;
                    _incidentsRevision++;
                    changed = true;
                    Emit(AnalysisEvent(request, c, day, "running"));
                    last = AnalysisEvent(request, c, day, "done");
                    last["seconds"] = seconds;
                    last["labels"] = spans.Count;
                }
            }
            Emit(last);
        }
        _analyses.Clear();
        if (changed)
        {
            Emit(new JsonObject { ["type"] = "incidents_changed", ["revision"] = _incidentsRevision });
        }
    }
}
