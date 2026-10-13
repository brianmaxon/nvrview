// Smart alerts (minor 12) without a core: scripted verifications answering
// ISmartAlerts with the contract's JSON and events (docs/FFI.md "Smart
// alerts"), for the app's detector and views before they meet the real
// core, and for tests. The same model as the macOS FakeCore's: a trigger of
// a detector class (person, vehicle, animal) or from an external source asks
// for two pictures (verify_request events with a small made-up H.264
// keyframe); the answers decide it: a box of an allowed class at or above
// 0.5 in both pictures alerts (normal), fewer is quiet (not_confirmed, or
// nothing_seen for a sensor). ExpireRequests() stands for the answer
// timeout: a check with no answer is then could_not_look (quiet), or an
// unverified critical alert for a camera in AlwaysAlertCameras. Smoke and
// CO alert at once (critical); a mute suppresses its camera. Events are
// queued by the calls and sent on CompleteAlertWork() (or shortly after the
// call, unless AlertWorkIsManual). Not modeled: the engine's other rules,
// episodes and cooldowns, settling, outputs.

using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>A smart-alert model answering <see cref="ISmartAlerts"/>; events go to the sink as JSON.</summary>
/// <param name="sink">Receives every event.</param>
public sealed class FakeSmartAlerts(Action<string> sink) : ISmartAlerts
{
    /// <summary>The made-up keyframe handed out as every picture.</summary>
    public static readonly byte[] Keyframe =
        [0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1E, 0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80, 0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00];

    private sealed class Check
    {
        public (ulong Device, int Channel, int Lens)? Camera;
        public required string Class;
        public bool External;
        public readonly List<bool> Answers = [];
        public readonly HashSet<string> Outstanding = [];
    }

    private readonly object _gate = new();
    private readonly List<string> _queued = [];
    private readonly Dictionary<ulong, Check> _checks = [];
    private readonly Dictionary<string, ulong> _requests = [];
    private readonly List<JsonObject> _log = [];
    private readonly Dictionary<ulong, byte[]> _snapshots = [];
    private readonly Dictionary<ulong, JsonNode> _feedback = [];
    private readonly Dictionary<(ulong, int, int), DateTime> _mutes = [];
    private bool _configured, _enabled;
    private ulong _nextTrigger = 1, _nextRequest = 1, _nextAlert = 1;

    /// <summary>Events go out only on <see cref="CompleteAlertWork"/> (tests).</summary>
    public bool AlertWorkIsManual { get; set; }

    /// <summary>Cameras whose person checks are always-alert cases (they fail open with no answer).</summary>
    public HashSet<(ulong Device, int Channel, int Lens)> AlwaysAlertCameras { get; } = [];

    private static long NowMs => DateTimeOffset.UtcNow.ToUnixTimeMilliseconds();

    private static JsonObject CameraJson((ulong Device, int Channel, int Lens) c) =>
        new() { ["device"] = c.Device, ["channel"] = c.Channel, ["lens"] = c.Lens };

    private static (ulong, int, int)? ParseCamera(JsonNode? n) =>
        n is JsonObject o && o["device"]?.GetValue<ulong>() is { } d && o["channel"]?.GetValue<int>() is { } ch
            ? (d, ch, o["lens"]?.GetValue<int>() ?? 0)
            : null;

    private static JsonObject ParseObject(string json, string function)
    {
        try
        {
            return JsonNode.Parse(json) as JsonObject ?? throw new NvException(NvStatus.InvalidArgument, function);
        }
        catch (JsonException)
        {
            throw new NvException(NvStatus.InvalidArgument, function);
        }
    }

    /// <inheritdoc/>
    public void AlertsConfigure(string json)
    {
        JsonObject o = ParseObject(json, "nv_alerts_configure");
        bool enabled = true;
        if (o["enabled"] is { } e)
        {
            enabled = e.GetValueKind() switch
            {
                JsonValueKind.True => true,
                JsonValueKind.False => false,
                _ => throw new NvException(NvStatus.InvalidArgument, "nv_alerts_configure"),
            };
        }
        lock (_gate)
        {
            _configured = true;
            _enabled = enabled;
            if (!enabled)
            {
                _checks.Clear();
                _requests.Clear();
            }
            _queued.Add(new JsonObject
            {
                ["type"] = "alerts_configured", ["enabled"] = enabled, ["issues"] = new JsonArray(),
                ["outputs"] = new JsonArray(), ["inputs"] = new JsonArray(),
            }.ToJsonString());
        }
        Schedule();
    }

    /// <inheritdoc/>
    public ulong AlertsTriggerPut(string json)
    {
        const string F = "nv_alerts_trigger_put";
        JsonObject o = ParseObject(json, F);
        string source = o["source"]?.GetValue<string>() ?? throw new NvException(NvStatus.InvalidArgument, F);
        string kind = o["kind"]?.GetValue<string>() ?? throw new NvException(NvStatus.InvalidArgument, F);
        bool external = source == "external";
        var camera = ParseCamera(o["camera"]);
        if (external ? o["origin"] is null : camera is null)
        {
            throw new NvException(NvStatus.InvalidArgument, F);
        }
        string cls = kind.ToLowerInvariant() switch
        {
            "people" or "person" => "person",
            "vehicle" => "vehicle",
            "dog_cat" or "animal" => "animal",
            "smoke_alarm" => "smoke",
            "co_alarm" => "co",
            _ => external ? "sensor" : "other",
        };
        ulong id;
        lock (_gate)
        {
            if (!_configured || !_enabled)
            {
                throw new NvException(NvStatus.NotConfigured, F);
            }
            id = _nextTrigger++;
            if (camera is { } c && _mutes.TryGetValue(c, out DateTime until) && until > DateTime.UtcNow)
            {
                Decide(id, camera, cls, "suppress", reason: "muted");
            }
            else if (cls is "smoke" or "co")
            {
                Decide(id, camera, cls, "alert", severity: "critical");
            }
            else if (cls is "person" or "vehicle" or "animal" or "sensor")
            {
                _checks[id] = new Check { Camera = camera, Class = cls, External = external };
                Request(id);
                Request(id);
            }
            else
            {
                Decide(id, camera, cls, "quiet", reason: "unknown_kind");
            }
        }
        Schedule();
        return id;
    }

    private void Request(ulong check)
    {
        Check c = _checks[check];
        string key = $"v{_nextRequest++}";
        _requests[key] = check;
        c.Outstanding.Add(key);
        long pts = 1_000_000L * (long)_nextRequest;
        _queued.Add(new JsonObject
        {
            ["type"] = "verify_request", ["request"] = key, ["purpose"] = "check",
            ["camera"] = CameraJson(c.Camera ?? (0, 0, 0)), ["trigger_ids"] = new JsonArray(check), ["codec"] = "h264",
            ["width"] = 1280, ["height"] = 720, ["pts_us"] = pts, ["bytes"] = Keyframe.Length,
            ["frames"] = new JsonArray(new JsonObject { ["pts_us"] = pts, ["bytes"] = Keyframe.Length, ["keyframe"] = true }),
            ["from_cover"] = false, ["at_ms"] = NowMs, ["deadline_ms"] = NowMs + 5000,
            ["classes"] = c.External ? new JsonArray("person", "vehicle", "animal") : new JsonArray(c.Class),
            ["min_score"] = 0.5,
            ["crops"] = new JsonArray(new JsonObject { ["what"] = "frame", ["x"] = 0, ["y"] = 0, ["w"] = 1, ["h"] = 1 }),
        }.ToJsonString());
    }

    private void Decide(
        ulong id, (ulong, int, int)? camera, string cls, string decision, string? severity = null, string? reason = null,
        bool unverified = false)
    {
        var d = new JsonObject
        {
            ["trigger_id"] = id, ["class"] = cls, ["decision"] = decision, ["decided_ms"] = NowMs,
            ["explanation"] = decision == "alert" ? "Alerted (fake core)." : "Not alerted (fake core).",
            ["trace"] = new JsonObject { ["version"] = 1, ["trigger_id"] = id, ["steps"] = new JsonArray() },
        };
        if (camera is { } c)
        {
            d["camera"] = CameraJson(c);
        }
        string state = "logged";
        if (decision == "alert")
        {
            d["alert_id"] = _nextAlert++;
            d["rev"] = 1;
            d["severity"] = severity ?? "normal";
            d["late"] = false;
            d["unverified"] = unverified;
            state = "new";
        }
        else
        {
            d["reason"] = reason ?? "";
        }
        _log.Add(d);
        if (_log.Count > 1000)
        {
            _log.RemoveAt(0);
        }
        _queued.Add(new JsonObject
        {
            ["type"] = "alert", ["state"] = state, ["source"] = "device", ["decision"] = d.DeepClone(),
        }.ToJsonString());
    }

    private void Finish(ulong id, bool timedOut)
    {
        if (!_checks.Remove(id, out Check? c))
        {
            return;
        }
        foreach (string k in c.Outstanding)
        {
            _requests.Remove(k);
        }
        int hits = c.Answers.Count(a => a);
        if (hits >= 2)
        {
            Decide(id, c.Camera, c.External ? "person" : c.Class, "alert");
        }
        else if (c.Answers.Count == 0 && timedOut && c.Camera is { } cam && AlwaysAlertCameras.Contains(cam))
        {
            Decide(id, c.Camera, c.Class, "alert", severity: "critical", unverified: true);
        }
        else if (c.Answers.Count == 0)
        {
            Decide(id, c.Camera, c.Class, "quiet", reason: "could_not_look");
        }
        else
        {
            Decide(id, c.Camera, c.External ? "sensor" : c.Class, "quiet", reason: c.External ? "nothing_seen" : "not_confirmed");
        }
    }

    /// <inheritdoc/>
    public AlertFrames AlertFramesGet(string request)
    {
        lock (_gate)
        {
            if (!_requests.ContainsKey(request))
            {
                throw new NvException(NvStatus.UnknownId, "nv_alert_frames_get");
            }
        }
        return new AlertFrames(NvThumbKind.H264Keyframe, 1280, 720, 0, Keyframe);
    }

    /// <inheritdoc/>
    public void AlertDetectionsPut(string request, string json)
    {
        const string F = "nv_alert_detections_put";
        JsonObject o = ParseObject(json, F);
        if (o["detections"] is not JsonArray list)
        {
            throw new NvException(NvStatus.InvalidArgument, F);
        }
        lock (_gate)
        {
            if (!_requests.Remove(request, out ulong id) || !_checks.TryGetValue(id, out Check? c))
            {
                throw new NvException(NvStatus.UnknownId, F);
            }
            HashSet<string> allowed = c.External ? ["person", "vehicle", "animal"] : [c.Class];
            bool hit = list.OfType<JsonObject>().Any(d =>
            {
                string label = ((d["class"] ?? d["label"])?.GetValue<string>() ?? "").ToLowerInvariant() switch
                {
                    "car" or "truck" or "bus" or "motorcycle" => "vehicle",
                    "dog" or "cat" => "animal",
                    var l => l,
                };
                return allowed.Contains(label) && (d["score"]?.GetValue<double>() ?? 0) >= 0.5;
            });
            c.Answers.Add(hit);
            c.Outstanding.Remove(request);
            _queued.Add(new JsonObject { ["type"] = "verify_request_done", ["request"] = request, ["result"] = "answered" }.ToJsonString());
            if (c.Outstanding.Count == 0)
            {
                Finish(id, timedOut: false);
            }
        }
        Schedule();
    }

    /// <summary>The answer timeout: every unanswered request expires and its check is decided.</summary>
    public void ExpireRequests()
    {
        lock (_gate)
        {
            foreach (string key in _requests.Keys)
            {
                _queued.Add(new JsonObject { ["type"] = "verify_request_done", ["request"] = key, ["result"] = "timeout" }.ToJsonString());
            }
            _requests.Clear();
            foreach (ulong id in _checks.Keys.ToList())
            {
                Finish(id, timedOut: true);
            }
        }
        Schedule();
    }

    /// <inheritdoc/>
    public string AlertsSunTimes(string request) =>
        throw new NvException(NvStatus.Unsupported, "nv_alerts_sun_times");

    /// <inheritdoc/>
    public string AlertsGet(string? query)
    {
        JsonObject q = string.IsNullOrEmpty(query) ? [] : ParseObject(query, "nv_alerts_get");
        HashSet<string>? decisions = q["decision"] is JsonArray a ? [.. a.Select(x => x?.GetValue<string>() ?? "")] : null;
        int limit = Math.Min(q["limit"]?.GetValue<int>() ?? 100, 1000);
        lock (_gate)
        {
            var entries = new JsonArray();
            foreach (JsonObject d in Enumerable.Reverse(_log))
            {
                if (decisions is not null && !decisions.Contains(d["decision"]!.GetValue<string>()))
                {
                    continue;
                }
                if (entries.Count >= limit)
                {
                    break;
                }
                var e = new JsonObject { ["decision"] = d.DeepClone(), ["source"] = "device", ["deliveries"] = new JsonArray() };
                ulong? alert = d["alert_id"]?.GetValue<ulong>();
                e["snapshot"] = alert is { } x && _snapshots.ContainsKey(x);
                if (alert is { } y && _feedback.TryGetValue(y, out JsonNode? f))
                {
                    e["feedback"] = f.DeepClone();
                }
                entries.Add((JsonNode)e);
            }
            var muted = new JsonArray();
            foreach (var (cam, until) in _mutes)
            {
                if (until > DateTime.UtcNow)
                {
                    muted.Add((JsonNode)CameraJson(cam));
                }
            }
            return new JsonObject
            {
                ["entries"] = entries, ["more"] = false,
                ["status"] = new JsonObject
                {
                    ["configured"] = _configured, ["enabled"] = _enabled, ["open_checks"] = _checks.Count,
                    ["muted"] = muted, ["requests"] = _requests.Count,
                },
            }.ToJsonString();
        }
    }

    /// <inheritdoc/>
    public void AlertSnapshotPut(ulong alertId, ReadOnlySpan<byte> data)
    {
        if (data.IsEmpty)
        {
            throw new NvException(NvStatus.InvalidArgument, "nv_alert_snapshot_put");
        }
        if (data.Length > 1 << 20)
        {
            throw new NvException(NvStatus.Limit, "nv_alert_snapshot_put");
        }
        lock (_gate)
        {
            if (alertId == 0 || alertId >= _nextAlert)
            {
                throw new NvException(NvStatus.UnknownId, "nv_alert_snapshot_put");
            }
            _snapshots[alertId] = data.ToArray();
        }
    }

    /// <inheritdoc/>
    public byte[] AlertSnapshotGet(ulong alertId)
    {
        lock (_gate)
        {
            return _snapshots.TryGetValue(alertId, out byte[]? d)
                ? d
                : throw new NvException(NvStatus.UnknownId, "nv_alert_snapshot_get");
        }
    }

    /// <inheritdoc/>
    public void AlertFeedback(string json)
    {
        const string F = "nv_alert_feedback";
        JsonObject o = ParseObject(json, F);
        ulong id = o["alert_id"]?.GetValue<ulong>() ?? throw new NvException(NvStatus.InvalidArgument, F);
        string vote = o["vote"]?.GetValue<string>() ?? "";
        if (vote is not ("up" or "down"))
        {
            throw new NvException(NvStatus.InvalidArgument, F);
        }
        if (o["reason"] is { } r &&
            (vote != "down" || r.GetValue<string>() is not ("nothing_there" or "wrong_type" or "not_interesting" or "duplicate")))
        {
            throw new NvException(NvStatus.InvalidArgument, F);
        }
        lock (_gate)
        {
            if (id == 0 || id >= _nextAlert)
            {
                throw new NvException(NvStatus.UnknownId, F);
            }
            _feedback[id] = o.DeepClone();
        }
    }

    /// <inheritdoc/>
    public void AlertMute(string camera, uint seconds)
    {
        const string F = "nv_alert_mute";
        if (ParseCamera(ParseObject(camera, F)) is not { } cam)
        {
            throw new NvException(NvStatus.InvalidArgument, F);
        }
        lock (_gate)
        {
            if (!_configured)
            {
                throw new NvException(NvStatus.NotConfigured, F);
            }
            if (seconds == 0)
            {
                _mutes.Remove(cam);
            }
            else
            {
                _mutes[cam] = DateTime.UtcNow.AddSeconds(Math.Min(seconds, 86_400u));
            }
        }
    }

    private void Schedule()
    {
        if (!AlertWorkIsManual)
        {
            _ = Task.Delay(10).ContinueWith(_ => CompleteAlertWork(), TaskScheduler.Default);
        }
    }

    /// <summary>Sends the queued events.</summary>
    public void CompleteAlertWork()
    {
        string[] events;
        lock (_gate)
        {
            events = [.. _queued];
            _queued.Clear();
        }
        foreach (string e in events)
        {
            sink(e);
        }
    }
}
