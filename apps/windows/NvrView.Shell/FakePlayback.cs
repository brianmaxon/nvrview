// Recorded playback (minor 6) without a core: a model answering IPlayback
// with the contract's JSON and events (docs/FFI-RECORDINGS.md, section 6),
// for the playback views (milestone P, W2) before they meet the real core,
// and for tests. The same model as the macOS FakeCore's: every camera
// enabled in the FakeRecordings store has recordings all day; the shared
// clock runs at the rate on a settable clock while playing and stops at the
// end (ended); seeks and rate changes start a new generation. Tile IDs are
// handed out but no pictures are made (the app's tile views show nothing
// for them). Events are queued by the calls and sent on
// CompletePlaybackWork() (or shortly after the call, unless
// RecordingWorkIsManual), so none arrives before the call that caused it
// returned; tile_closed and playback_closed are sent before
// PlaybackRemoveCamera and PlaybackClose return.

using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>A playback model answering <see cref="IPlayback"/>; events go to the sink as JSON.</summary>
/// <param name="sink">Receives every event.</param>
/// <param name="recordings">The store that says which devices are enabled, and the incidents.</param>
/// <param name="clockMs">The clock (milliseconds) positions run on; default the system's monotonic clock.</param>
public sealed class FakePlayback(Action<string> sink, FakeRecordings recordings, Func<long>? clockMs = null) : IPlayback
{
    private static readonly double[] Rates = [0.25, 0.5, 1, 2, 4, 8, 16];
    private const int MaxPlaybacks = 8;
    private const int MaxCameras = 4;
    private const long MaxRangeMs = 24L * 3600 * 1000;

    private readonly object _gate = new();
    private readonly Func<long> _clock = clockMs ?? (() => Environment.TickCount64);
    private readonly Dictionary<ulong, Pb> _playbacks = [];
    private readonly List<string> _queued = [];
    private ulong _nextId = 1_000_000;

    /// <summary>Events go out only on <see cref="CompletePlaybackWork"/> (tests).</summary>
    public bool RecordingWorkIsManual { get; set; }

    private sealed class Cam
    {
        public required ulong Tile;
        public required RecordingCamera Camera;
        public required string Tier;
        public long OffsetMs;
    }

    private sealed class Pb
    {
        public required long StartMs;
        public required long EndMs;
        public required long PositionMs; // at Anchor
        public required long Anchor;
        public required double Rate;
        public required string Sync;
        public NvPlaybackState State = NvPlaybackState.Paused;
        public uint Generation = 1;
        public readonly List<Cam> Cameras = [];
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

    private static long? Long(JsonNode? n) => n is JsonValue v && v.TryGetValue(out long x) ? x : null;

    private static Cam CameraOf(JsonNode? node, string defaultTier, string function)
    {
        if (node is not JsonObject o || o["camera"] is not JsonObject c || c["device"]?.GetValueKind() != JsonValueKind.String ||
            c["channel"] is not JsonValue ch || !ch.TryGetValue(out int channel) || channel is < 0 or > 255)
        {
            throw Error(NvStatus.InvalidArgument, function);
        }
        int lens = c["lens"] is JsonValue l && l.TryGetValue(out int n) ? n : 0;
        string tier = o["tier"] is JsonValue t && t.TryGetValue(out string? s) ? s : defaultTier;
        if (lens is < 0 or > 1 || tier is not ("sub" or "main"))
        {
            throw Error(NvStatus.InvalidArgument, function);
        }
        return new Cam
        {
            Tile = 0,
            Camera = new RecordingCamera(c["device"]!.GetValue<string>(), channel, lens),
            Tier = tier,
            OffsetMs = Long(o["offset_ms"]) ?? 0,
        };
    }

    private void CheckEnabled(IEnumerable<Cam> cameras, string function)
    {
        foreach (Cam c in cameras)
        {
            (bool configured, bool enabled) = recordings.PlaybackCheck(c.Camera.Device);
            if (!configured)
            {
                throw Error(NvStatus.NotConfigured, function);
            }
            if (!enabled)
            {
                throw Error(NvStatus.InvalidArgument, function);
            }
        }
    }

    private static string StateJson(ulong id, Pb p) => new JsonObject
    {
        ["type"] = "playback_state",
        ["playback"] = id,
        ["state"] = PlaybackEvent.Name(p.State),
        ["position_ms"] = p.PositionMs,
        ["rate"] = p.Rate,
    }.ToJsonString();

    private static IEnumerable<string> SeekJson(ulong id, Pb p) =>
        new[] { "starting", "active" }.Select(s => new JsonObject
        {
            ["type"] = "playback_seek",
            ["playback"] = id,
            ["position_ms"] = p.PositionMs,
            ["generation"] = p.Generation,
            ["state"] = s,
        }.ToJsonString());

    private static string CameraJson(ulong id, Cam c) => new JsonObject
    {
        ["type"] = "playback_camera",
        ["playback"] = id,
        ["tile"] = c.Tile,
        ["state"] = "playing",
    }.ToJsonString();

    // The position now; an end reached meanwhile is settled (ended, queued).
    private void Settle(ulong id, Pb p)
    {
        long now = _clock();
        if (p.State == NvPlaybackState.Playing)
        {
            p.PositionMs = Math.Min(p.EndMs, p.PositionMs + (long)((now - p.Anchor) * p.Rate));
            if (p.PositionMs >= p.EndMs)
            {
                p.State = NvPlaybackState.Ended;
                _queued.Add(StateJson(id, p));
            }
        }
        p.Anchor = now;
    }

    private T With<T>(ulong id, string function, Func<Pb, T> body)
    {
        T result;
        lock (_gate)
        {
            if (!_playbacks.TryGetValue(id, out Pb? p))
            {
                throw Error(NvStatus.UnknownId, function);
            }
            Settle(id, p);
            result = body(p);
        }
        ScheduleWork();
        return result;
    }

    private void With(ulong id, string function, Action<Pb> body) =>
        With(id, function, p =>
        {
            body(p);
            return 0;
        });

    /// <inheritdoc/>
    public OpenedPlayback PlaybackOpen(string json)
    {
        const string F = "nv_playback_open";
        JsonObject o = Object(json, F);
        string defaultTier = o["tier"] is JsonValue dt && dt.TryGetValue(out string? t) ? t : "sub";
        long startMs, endMs;
        var cams = new List<Cam>();
        if (o["incident"] is JsonValue iv && iv.TryGetValue(out string? key))
        {
            JsonObject inc = Object(recordings.IncidentGet(key), F)["incident"] as JsonObject ??
                throw Error(NvStatus.Internal, F);
            long margin = Long(inc["margin_s"]) ?? 0;
            startMs = ((Long(inc["start"]) ?? 0) - margin) * 1000;
            endMs = ((Long(inc["end"]) ?? 0) + margin + 1) * 1000;
            foreach (JsonNode? c in inc["cameras"] as JsonArray ?? [])
            {
                cams.Add(CameraOf(new JsonObject { ["camera"] = c?["camera"]?.DeepClone() }, defaultTier, F));
            }
        }
        else
        {
            startMs = Long(o["start_ms"]) ?? throw Error(NvStatus.InvalidArgument, F);
            endMs = Long(o["end_ms"]) ?? throw Error(NvStatus.InvalidArgument, F);
            foreach (JsonNode? c in o["cameras"] as JsonArray ?? throw Error(NvStatus.InvalidArgument, F))
            {
                cams.Add(CameraOf(c, defaultTier, F));
            }
        }
        if (startMs < 0 || endMs <= startMs || cams.Count == 0)
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        if (endMs - startMs > MaxRangeMs || cams.Count > MaxCameras)
        {
            throw Error(NvStatus.Limit, F);
        }
        if (cams.Select(c => c.Camera).Distinct().Count() != cams.Count)
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        double rate = o["rate"] is JsonValue rv && rv.TryGetValue(out double r) ? r : 1;
        string sync = o["sync"] is JsonValue sv && sv.TryGetValue(out string? s) ? s : "wait";
        if (!Rates.Contains(rate) || sync is not ("wait" or "free"))
        {
            throw Error(NvStatus.InvalidArgument, F);
        }
        long position = Math.Clamp(Long(o["position_ms"]) ?? startMs, startMs, endMs);
        CheckEnabled(cams, F);
        ulong id;
        lock (_gate)
        {
            if (_playbacks.Count >= MaxPlaybacks)
            {
                throw Error(NvStatus.Limit, F);
            }
            id = _nextId++;
            var p = new Pb { StartMs = startMs, EndMs = endMs, PositionMs = position, Anchor = _clock(), Rate = rate, Sync = sync };
            foreach (Cam c in cams)
            {
                c.Tile = _nextId++;
                p.Cameras.Add(c);
            }
            _playbacks[id] = p;
            _queued.AddRange(SeekJson(id, p));
            _queued.AddRange(cams.Select(c => CameraJson(id, c)));
            _queued.Add(StateJson(id, p));
        }
        ScheduleWork();
        return new OpenedPlayback(id, [.. cams.Select(c => c.Tile)]);
    }

    /// <inheritdoc/>
    public ulong PlaybackAddCamera(ulong playback, string json)
    {
        const string F = "nv_playback_add_camera";
        Cam cam = CameraOf(Object(json, F), "sub", F);
        CheckEnabled([cam], F);
        return With(playback, F, p =>
        {
            if (p.Cameras.Any(c => c.Camera == cam.Camera))
            {
                throw Error(NvStatus.InvalidArgument, F);
            }
            if (p.Cameras.Count >= MaxCameras)
            {
                throw Error(NvStatus.Limit, F);
            }
            cam.Tile = _nextId++;
            p.Cameras.Add(cam);
            _queued.Add(CameraJson(playback, cam));
            return cam.Tile;
        });
    }

    /// <inheritdoc/>
    public void PlaybackRemoveCamera(ulong playback, ulong tile)
    {
        With(playback, "nv_playback_remove_camera", p =>
        {
            if (p.Cameras.RemoveAll(c => c.Tile == tile) == 0)
            {
                throw Error(NvStatus.UnknownId, "nv_playback_remove_camera");
            }
        });
        sink(new JsonObject { ["type"] = "tile_closed", ["tile"] = tile, ["reason"] = "closed" }.ToJsonString());
    }

    /// <inheritdoc/>
    public void SetTier(ulong tile, NvStreamTier tier)
    {
        if (tier is not (NvStreamTier.Main or NvStreamTier.Sub))
        {
            throw Error(NvStatus.InvalidArgument, "nv_tile_set_tier");
        }
        ulong? found = null;
        lock (_gate)
        {
            foreach ((ulong id, Pb p) in _playbacks)
            {
                if (p.Cameras.FirstOrDefault(c => c.Tile == tile) is { } cam)
                {
                    cam.Tier = tier == NvStreamTier.Main ? "main" : "sub";
                    found = id;
                    break;
                }
            }
        }
        if (found is null)
        {
            throw Error(NvStatus.UnknownId, "nv_tile_set_tier");
        }
        ScheduleWork();
    }

    /// <inheritdoc/>
    public void PlaybackPlay(ulong playback) =>
        With(playback, "nv_playback_play", p =>
        {
            if (p.State == NvPlaybackState.Ended)
            {
                p.PositionMs = p.StartMs;
            }
            if (p.State != NvPlaybackState.Playing)
            {
                p.State = NvPlaybackState.Playing;
                _queued.Add(StateJson(playback, p));
            }
        });

    /// <inheritdoc/>
    public void PlaybackPause(ulong playback) =>
        With(playback, "nv_playback_pause", p =>
        {
            if (p.State is NvPlaybackState.Playing or NvPlaybackState.Buffering)
            {
                p.State = NvPlaybackState.Paused;
                _queued.Add(StateJson(playback, p));
            }
        });

    /// <inheritdoc/>
    public void PlaybackSeek(ulong playback, long positionMs, NvSeekMode mode)
    {
        if (mode is not (NvSeekMode.Keyframe or NvSeekMode.Exact))
        {
            throw Error(NvStatus.InvalidArgument, "nv_playback_seek");
        }
        With(playback, "nv_playback_seek", p =>
        {
            p.PositionMs = Math.Clamp(positionMs, p.StartMs, p.EndMs);
            if (p.State == NvPlaybackState.Ended)
            {
                p.State = NvPlaybackState.Paused;
            }
            p.Generation++;
            _queued.AddRange(SeekJson(playback, p));
            _queued.Add(StateJson(playback, p));
        });
    }

    /// <inheritdoc/>
    public void PlaybackSetRate(ulong playback, double rate)
    {
        if (!Rates.Contains(rate))
        {
            throw Error(NvStatus.InvalidArgument, "nv_playback_set_rate");
        }
        With(playback, "nv_playback_set_rate", p =>
        {
            p.Rate = rate;
            p.Generation++;
            _queued.Add(StateJson(playback, p));
        });
    }

    /// <inheritdoc/>
    public void PlaybackSetOffset(ulong playback, ulong tile, long offsetMs) =>
        With(playback, "nv_playback_set_offset", p =>
        {
            Cam c = p.Cameras.Find(c => c.Tile == tile) ?? throw Error(NvStatus.UnknownId, "nv_playback_set_offset");
            if (Math.Abs(offsetMs) > MaxRangeMs)
            {
                throw Error(NvStatus.InvalidArgument, "nv_playback_set_offset");
            }
            c.OffsetMs = offsetMs;
        });

    /// <inheritdoc/>
    public (long PositionMs, NvPlaybackState State) PlaybackPosition(ulong playback) =>
        With(playback, "nv_playback_position", p => (p.PositionMs, p.State));

    /// <inheritdoc/>
    public string PlaybackStatus(ulong playback) =>
        With(playback, "nv_playback_status", p =>
        {
            var cameras = new JsonArray();
            foreach (Cam c in p.Cameras)
            {
                cameras.Add((JsonNode)new JsonObject
                {
                    ["tile"] = c.Tile,
                    ["camera"] = JsonNode.Parse(c.Camera.Json),
                    ["tier"] = c.Tier,
                    ["offset_ms"] = c.OffsetMs,
                    ["state"] = "playing",
                    ["generation"] = p.Generation,
                });
            }
            return new JsonObject
            {
                ["playback"] = playback,
                ["state"] = PlaybackEvent.Name(p.State),
                ["position_ms"] = p.PositionMs,
                ["rate"] = p.Rate,
                ["sync"] = p.Sync,
                ["start_ms"] = p.StartMs,
                ["end_ms"] = p.EndMs,
                ["cameras"] = cameras,
            }.ToJsonString();
        });

    /// <inheritdoc/>
    public void PlaybackClose(ulong playback)
    {
        // What the playback already reported goes out first.
        CompletePlaybackWork();
        Pb? p;
        lock (_gate)
        {
            if (!_playbacks.Remove(playback, out p))
            {
                throw Error(NvStatus.UnknownId, "nv_playback_close");
            }
        }
        foreach (Cam c in p.Cameras)
        {
            sink(new JsonObject { ["type"] = "tile_closed", ["tile"] = c.Tile, ["reason"] = "closed" }.ToJsonString());
        }
        sink(new JsonObject { ["type"] = "playback_closed", ["playback"] = playback, ["reason"] = "closed" }.ToJsonString());
    }

    private void ScheduleWork()
    {
        if (!RecordingWorkIsManual)
        {
            _ = Task.Delay(10).ContinueWith(_ => CompletePlaybackWork(), TaskScheduler.Default);
        }
    }

    /// <summary>Sends the queued playback events.</summary>
    public void CompletePlaybackWork()
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
