// Recorded playback (minor 6; docs/FFI.md "Recorded playback (minor 6)") as
// the app sees it: the events of docs/FFI-RECORDINGS.md section 6, parsed,
// and builders of the requests the calls take. Plain .NET, tested on Linux.
// The calls themselves are NvrView.Interop's IPlayback (the core) and
// FakePlayback (a model, for the views before they meet the core).

using System.Globalization;
using System.Text.Json;
using System.Text.Json.Nodes;
using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>One camera of a playback request: <c>{"camera":{...},"tier":"sub","offset_ms":0}</c>.</summary>
public sealed record PlaybackCamera(RecordingCamera Camera, string Tier = "sub", long OffsetMs = 0)
{
    /// <summary>The JSON object <c>nv_playback_open</c> (in <c>cameras</c>) and <c>nv_playback_add_camera</c> take.</summary>
    public string Json =>
        $"{{\"camera\":{Camera.Json},\"tier\":{JsonValue.Create(Tier).ToJsonString()},\"offset_ms\":{OffsetMs}}}";
}

/// <summary>Builders of <c>nv_playback_open</c>'s JSON.</summary>
public static class PlaybackRequest
{
    /// <summary>
    /// A range of these cameras (device-local milliseconds), opened paused at
    /// positionMs (the start when null). Rate 0.25, 0.5, 1, 2, 4, 8, or 16;
    /// sync <c>wait</c> or <c>free</c>.
    /// </summary>
    public static string Range(long startMs, long endMs, IEnumerable<PlaybackCamera> cameras, long? positionMs = null,
        double rate = 1, string sync = "wait")
    {
        string position = positionMs is { } p ? $",\"position_ms\":{p}" : "";
        string list = string.Join(",", cameras.Select(c => c.Json));
        return $"{{\"start_ms\":{startMs},\"end_ms\":{endMs}{position},\"rate\":{rate.ToString("R", CultureInfo.InvariantCulture)}," +
            $"\"sync\":{JsonValue.Create(sync).ToJsonString()},\"cameras\":[{list}]}}";
    }

    /// <summary>An incident's cameras and range with its margins.</summary>
    public static string Incident(string key, string tier = "sub") =>
        new JsonObject { ["incident"] = key, ["tier"] = tier }.ToJsonString();
}

/// <summary>
/// One playback event (FFI-RECORDINGS.md sections 6 and 8). Unknown fields
/// are ignored; <see cref="Parse"/> returns null for other types and for
/// events missing what they cannot do without.
/// </summary>
public abstract record PlaybackEvent
{
    /// <summary>On every state change and after every seek; not periodic.</summary>
    public sealed record State(ulong Playback, NvPlaybackState Value, long PositionMs, double Rate) : PlaybackEvent;

    /// <summary>
    /// One camera: <c>buffering</c>, <c>playing</c>, <c>late</c>, <c>no_recording</c>, <c>ended</c>,
    /// <c>failed</c> (Reason: <c>start_rejected</c>, <c>stream_ended</c>, <c>undecodable</c>,
    /// <c>session_closed</c>, <c>limit</c>, <c>device_in_use</c>).
    /// </summary>
    public sealed record Camera(ulong Playback, ulong Tile, string Value, string? Reason) : PlaybackEvent;

    /// <summary>A seek's progress: <c>starting</c>, <c>active</c>, <c>failed</c>, with its generation.</summary>
    public sealed record Seek(ulong Playback, long PositionMs, uint Generation, string Value) : PlaybackEvent;

    /// <summary><c>closed</c>, <c>device_removed</c>, <c>login_failed</c>, <c>core_shutdown</c>.</summary>
    public sealed record Closed(ulong Playback, string Reason) : PlaybackEvent;

    /// <summary>The event types this record parses.</summary>
    public static readonly IReadOnlySet<string> Types = new HashSet<string>
    {
        "playback_state", "playback_camera", "playback_seek", "playback_closed",
    };

    /// <summary>A state's name in the events.</summary>
    public static string Name(NvPlaybackState s) => s switch
    {
        NvPlaybackState.Buffering => "buffering",
        NvPlaybackState.Playing => "playing",
        NvPlaybackState.Paused => "paused",
        NvPlaybackState.Ended => "ended",
        _ => "failed",
    };

    private static NvPlaybackState? StateOf(string? name) => name switch
    {
        "buffering" => NvPlaybackState.Buffering,
        "playing" => NvPlaybackState.Playing,
        "paused" => NvPlaybackState.Paused,
        "ended" => NvPlaybackState.Ended,
        "failed" => NvPlaybackState.Failed,
        _ => null,
    };

    private static ulong? U64(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.TryGetUInt64(out ulong n) ? n : null;

    /// <summary>Parses one event; null for another type or a malformed one.</summary>
    public static PlaybackEvent? Parse(ReadOnlySpan<byte> json)
    {
        try
        {
            using JsonDocument doc = JsonDocument.Parse(json.ToArray());
            JsonElement e = doc.RootElement;
            if (e.ValueKind != JsonValueKind.Object || U64(e, "playback") is not { } pb)
            {
                return null;
            }
            string? state = DownloadProgress.Str(e, "state");
            long? position = DownloadProgress.Long(e, "position_ms");
            return DownloadProgress.Str(e, "type") switch
            {
                "playback_state" when StateOf(state) is { } s && position is { } p =>
                    new State(pb, s, p, e.TryGetProperty("rate", out JsonElement r) && r.TryGetDouble(out double rate) ? rate : 1),
                "playback_camera" when U64(e, "tile") is { } tile && state is not null =>
                    new Camera(pb, tile, state, DownloadProgress.Str(e, "reason")),
                "playback_seek" when position is { } p && state is not null &&
                    e.TryGetProperty("generation", out JsonElement g) && g.TryGetUInt32(out uint generation) =>
                    new Seek(pb, p, generation, state),
                "playback_closed" => new Closed(pb, DownloadProgress.Str(e, "reason") ?? "closed"),
                _ => null,
            };
        }
        catch (JsonException)
        {
            return null;
        }
    }
}
