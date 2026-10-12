// Capture (minor 9) without a core: a model answering ICapture with the
// contract's JSON and events (docs/FFI.md "Capture"), for the capture UI
// (W5) before it meets the real core, and for tests. The same model as the
// macOS FakeCore's: nothing is written to disk (a recording is its events
// and its file name); the settings, the count limits (live, calls, two
// main-stream recorders per device), the disk guard at the start
// (FreeBytes), the encoder's calls (Open per encoded track and one frame
// each at the start, Close with flush at the end, units counted through
// RecordAudioEncoded), and the events (waiting_keyframe, recording, then
// finalizing and finished on a stop) are modeled. Events are queued by the
// calls and sent on CompleteCaptureWork() (or shortly after the call,
// unless RecordingWorkIsManual), so none arrives before the call that caused
// it returned. Not modeled: segments, gaps, caps, crash recovery, and ends
// other than a stop.

using System.Globalization;
using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>A capture model answering <see cref="ICapture"/>; events go to the sink as JSON.</summary>
/// <param name="sink">Receives every event.</param>
/// <param name="tileSource">A live tile's device, channel, and whether its camera sends audio; null for an unknown tile.</param>
public sealed class FakeCapture(Action<string> sink, Func<ulong, (ulong Device, int Channel, bool HasAudio)?> tileSource)
    : ICapture
{
    private readonly object _gate = new();
    private readonly Dictionary<ulong, Rec> _recordings = [];
    private readonly List<string> _queued = [];
    private Settings? _settings;
    private ICaptureEncoder? _encoder;
    private ulong _nextId = 2_000_000;

    /// <summary>Events go out only on <see cref="CompleteCaptureWork"/> (tests).</summary>
    public bool RecordingWorkIsManual { get; set; }

    /// <summary>The free space the disk guard sees (default: plenty).</summary>
    public ulong FreeBytes { get; set; } = ulong.MaxValue;

    private sealed record Settings(string LiveDir, int MaxLive, int MaxCalls, ulong MinFreeBytes, bool Main);

    private sealed class Rec
    {
        public required string Kind;
        public required ulong Device;
        public required int Channel;
        public required bool Main;
        public required string File;
        public required string Audio;
        public required string[] Tracks;
        public ICaptureEncoder? Encoder;
        public readonly Dictionary<uint, int> Encoded = [];
        public readonly long Started = Environment.TickCount64;
        public string State = "waiting_keyframe";
    }

    private static JsonObject Event(ulong id, Rec r, string? reason = null)
    {
        long ms = r.State == "waiting_keyframe" ? 0 : Math.Max(0, Environment.TickCount64 - r.Started);
        var o = new JsonObject
        {
            ["type"] = "recording", ["recording"] = id, ["kind"] = r.Kind, ["device"] = r.Device, ["channel"] = r.Channel,
            ["lens"] = 0, ["tier"] = r.Main ? "main" : "sub", ["state"] = r.State, ["file"] = r.File, ["segment"] = 1,
            ["bytes"] = ms * 100, ["duration_ms"] = ms, ["gaps"] = 0, ["audio"] = r.Audio,
            ["tracks"] = new JsonArray([.. r.Tracks.Select(t => (JsonNode)t)]), ["encode_dropped_ms"] = 0,
            ["dropped_frames"] = 0,
        };
        if (reason is not null)
        {
            o["reason"] = reason;
            o["fragmented"] = false;
        }
        return o;
    }

    /// <inheritdoc/>
    public void CaptureConfigure(string json)
    {
        JsonObject o;
        try
        {
            o = JsonNode.Parse(json) as JsonObject ?? throw new NvException(NvStatus.InvalidArgument, nameof(CaptureConfigure));
        }
        catch (JsonException)
        {
            throw new NvException(NvStatus.InvalidArgument, nameof(CaptureConfigure));
        }
        string dir = o["live_dir"]?.GetValueKind() == JsonValueKind.String ? o["live_dir"]!.GetValue<string>() : "";
        string quality = o["quality"]?.GetValue<string>() ?? "main";
        if (!Path.IsPathFullyQualified(dir) || quality is not ("main" or "sub"))
        {
            throw new NvException(NvStatus.InvalidArgument, nameof(CaptureConfigure));
        }
        int Count(string k, int d) => o[k] is { } v ? v.GetValue<int>() : d;
        lock (_gate)
        {
            _settings = new Settings(dir, Count("max_live", 4), Count("max_calls", 2),
                o["min_free_bytes"] is { } m ? m.GetValue<ulong>() : 2_000_000_000, quality == "main");
        }
    }

    /// <inheritdoc/>
    public void CaptureSetEncoder(ICaptureEncoder? encoder)
    {
        lock (_gate)
        {
            _encoder = encoder;
        }
    }

    /// <inheritdoc/>
    public NvStatus RecordAudioEncoded(ulong recording, uint track, ReadOnlySpan<byte> unit)
    {
        if (unit.IsEmpty || unit.Length > 8192)
        {
            return NvStatus.InvalidArgument;
        }
        lock (_gate)
        {
            if (!_recordings.TryGetValue(recording, out Rec? r) || !r.Encoded.ContainsKey(track))
            {
                return NvStatus.UnknownId;
            }
            r.Encoded[track]++;
            return NvStatus.Ok;
        }
    }

    /// <inheritdoc/>
    public RecordingStarted RecordStart(string request)
    {
        const string F = nameof(RecordStart);
        JsonObject o;
        try
        {
            o = JsonNode.Parse(request) as JsonObject ?? throw new NvException(NvStatus.InvalidArgument, F);
        }
        catch (JsonException)
        {
            throw new NvException(NvStatus.InvalidArgument, F);
        }
        string? kind = o["kind"]?.GetValue<string>();
        if (kind is not ("live" or "call") || o["tile"] is not { } tileNode)
        {
            throw new NvException(NvStatus.InvalidArgument, F);
        }
        ulong tile = tileNode.GetValue<ulong>();
        string? file = o["file"]?.GetValue<string>();
        if (kind == "call" && (file is null || !Path.IsPathFullyQualified(file) || !file.EndsWith(".mp4", StringComparison.Ordinal)))
        {
            throw new NvException(NvStatus.InvalidArgument, F);
        }
        if (tileSource(tile) is not { } source)
        {
            throw new NvException(NvStatus.UnknownId, F);
        }
        ulong id;
        Rec r;
        lock (_gate)
        {
            Settings s = _settings ?? throw new NvException(NvStatus.NotConfigured, F);
            r = new Rec
            {
                Kind = kind, Device = source.Device, Channel = source.Channel, Main = s.Main, File = "",
                Audio = source.HasAudio ? "passthrough" : "none", Tracks = ["video", "audio"],
            };
            if (kind == "live")
            {
                if (_recordings.Values.Count(x => x.Kind == "live") >= s.MaxLive)
                {
                    throw new NvException(NvStatus.Limit, F, "live");
                }
                if (o["quality"]?.GetValue<string>() is { } q)
                {
                    r.Main = q == "main";
                }
                int offset = o["utc_offset_s"]?.GetValue<int>() ?? 0;
                string when = DateTime.UtcNow.AddSeconds(offset).ToString("yyyy-MM-dd HH.mm.ss", CultureInfo.InvariantCulture);
                string name = o["name"]?.GetValue<string>() ?? $"Camera {source.Channel + 1}";
                r.File = Path.Combine(s.LiveDir, $"{when} {name}.mp4");
                if (!source.HasAudio)
                {
                    r.Tracks = ["video"];
                }
            }
            else
            {
                if (_recordings.Values.Count(x => x.Kind == "call") >= s.MaxCalls ||
                    _recordings.Values.Any(x => x.Kind == "call" && x.Device == source.Device && x.Channel == source.Channel))
                {
                    throw new NvException(NvStatus.Limit, F, "calls");
                }
                r.Encoder = _encoder ?? throw new NvException(NvStatus.NotConfigured, F);
                r.File = file!;
                r.Tracks = ["video", "Conversation", "Visitor", "You"];
                if (!source.HasAudio)
                {
                    r.Audio = "encoded";
                }
            }
            if (r.Main && _recordings.Values.Count(x => x.Device == source.Device && x.Main) >= 2)
            {
                throw new NvException(NvStatus.Limit, F, "main_streams");
            }
            if (FreeBytes < s.MinFreeBytes)
            {
                throw new NvException(NvStatus.Limit, F, "disk_low");
            }
            id = _nextId++;
            _recordings[id] = r;
            _queued.Add(Event(id, r).ToJsonString());
            r.State = "recording";
            _queued.Add(Event(id, r).ToJsonString());
        }
        // The encoder is called without the lock (it answers through
        // RecordAudioEncoded): the tracks open, and each gets a frame.
        if (r.Encoder is { } encoder)
        {
            uint[] tracks = r.Audio == "passthrough" ? [0, 2] : [0, 1, 2];
            float[] silence = new float[1024];
            foreach (uint t in tracks)
            {
                if (encoder.Open(id, t, 48_000, 1, 96_000) is null)
                {
                    continue;
                }
                lock (_gate)
                {
                    r.Encoded[t] = 0;
                }
                encoder.Encode(id, t, silence);
            }
        }
        ScheduleWork();
        return new RecordingStarted(id, r.File, "waiting_keyframe");
    }

    /// <inheritdoc/>
    public void RecordStop(ulong recording)
    {
        Rec? r;
        uint[] tracks;
        lock (_gate)
        {
            if (!_recordings.TryGetValue(recording, out r))
            {
                throw new NvException(NvStatus.UnknownId, nameof(RecordStop));
            }
            tracks = [.. r.Encoded.Keys.Order()];
        }
        foreach (uint t in tracks)
        {
            r.Encoder?.Close(recording, t, flush: true);
        }
        lock (_gate)
        {
            _recordings.Remove(recording);
            r.State = "finalizing";
            _queued.Add(Event(recording, r).ToJsonString());
            r.State = "finished";
            _queued.Add(Event(recording, r, "stopped").ToJsonString());
        }
        ScheduleWork();
    }

    /// <inheritdoc/>
    public string RecordStatus()
    {
        lock (_gate)
        {
            var list = new JsonArray([.. _recordings.OrderBy(p => p.Key).Select(p => (JsonNode)Event(p.Key, p.Value))]);
            return new JsonObject { ["recordings"] = list }.ToJsonString();
        }
    }

    /// <summary>The units the encoder answered for each opened track of a running recording (tests).</summary>
    public IReadOnlyDictionary<uint, int> EncodedUnits(ulong recording)
    {
        lock (_gate)
        {
            return _recordings.TryGetValue(recording, out Rec? r) ? new Dictionary<uint, int>(r.Encoded) : [];
        }
    }

    private void ScheduleWork()
    {
        if (!RecordingWorkIsManual)
        {
            _ = Task.Delay(10).ContinueWith(_ => CompleteCaptureWork(), TaskScheduler.Default);
        }
    }

    /// <summary>Sends the queued recording events.</summary>
    public void CompleteCaptureWork()
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
