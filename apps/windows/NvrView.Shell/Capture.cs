// Capture (minor 9; docs/FFI.md "Capture"): the recording event as the core
// writes it. The calls are ICapture in NvrView.Interop; FakeCapture answers
// them without a core.

using System.Text.Json;

namespace NvrView.Shell;

/// <summary>
/// A <c>recording</c> event: sent at once on a change of state and about once
/// a second while recording.
/// </summary>
/// <param name="Recording">The recording's ID.</param>
/// <param name="Kind"><c>live</c> or <c>call</c>.</param>
/// <param name="Device">The device's ID.</param>
/// <param name="Channel">The channel.</param>
/// <param name="Lens">The lens.</param>
/// <param name="Tier"><c>main</c> or <c>sub</c>.</param>
/// <param name="State"><c>waiting_keyframe</c>, <c>recording</c>, <c>segment</c>, <c>finalizing</c>, <c>finished</c>, <c>failed</c>, <c>recovered</c>.</param>
/// <param name="Reason">With <c>finished</c> or <c>failed</c>: <c>stopped</c>, <c>max_duration</c>, <c>max_size</c>, <c>disk_low</c>, <c>device_removed</c>, <c>login_failed</c>, <c>core_shutdown</c>, <c>write_failed</c>, <c>encoder_failed</c>, <c>no_video</c>, <c>format_changed</c>, <c>long_gap</c>.</param>
/// <param name="File">The file being written (absolute).</param>
/// <param name="Segment">The part (1 and up).</param>
/// <param name="Bytes">Bytes written over every part.</param>
/// <param name="DurationMs">Recorded duration.</param>
/// <param name="Gaps">Gaps counted.</param>
/// <param name="Audio"><c>passthrough</c>, <c>encoded</c>, <c>none</c>, <c>unsupported</c>.</param>
/// <param name="Tracks">The file's tracks (<c>video</c>, <c>audio</c>; a call's <c>Conversation</c>, <c>Visitor</c>, <c>You</c>), comma-joined.</param>
/// <param name="EncodeDroppedMs">Audio the encoder could not keep up with.</param>
/// <param name="DroppedFrames">Video frames lost on the way.</param>
/// <param name="Fragmented">On the last event: the fragmented file was published as the <c>.mp4</c>.</param>
public sealed record RecordingEvent(
    ulong Recording, string Kind, ulong Device, int Channel, int Lens, string Tier, string State, string? Reason,
    string File, int Segment, ulong Bytes, ulong DurationMs, int Gaps, string Audio, string Tracks,
    ulong EncodeDroppedMs, ulong? DroppedFrames, bool? Fragmented)
{
    /// <summary>Whether the recording has ended (<c>finished</c>, <c>failed</c>, <c>recovered</c>).</summary>
    public bool Ended => State is "finished" or "failed" or "recovered";

    /// <summary>The event type this record parses.</summary>
    public const string Type = "recording";

    /// <summary>Parses one event; null for another type or a malformed one.</summary>
    public static RecordingEvent? Parse(ReadOnlySpan<byte> json)
    {
        try
        {
            var reader = new Utf8JsonReader(json);
            using JsonDocument d = JsonDocument.ParseValue(ref reader);
            JsonElement e = d.RootElement;
            if (e.ValueKind != JsonValueKind.Object || Str(e, "type") != Type)
            {
                return null;
            }
            if (U64(e, "recording") is not { } id || Str(e, "kind") is not { } kind || U64(e, "device") is not { } device ||
                U64(e, "channel") is not { } channel || Str(e, "tier") is not { } tier ||
                Str(e, "state") is not { } state || Str(e, "file") is not { } file || Str(e, "audio") is not { } audio ||
                !e.TryGetProperty("tracks", out JsonElement tracks) || tracks.ValueKind != JsonValueKind.Array)
            {
                return null;
            }
            return new RecordingEvent(
                id, kind, device, (int)channel, (int)(U64(e, "lens") ?? 0), tier, state, Str(e, "reason"), file,
                (int)(U64(e, "segment") ?? 1), U64(e, "bytes") ?? 0, U64(e, "duration_ms") ?? 0, (int)(U64(e, "gaps") ?? 0),
                audio, string.Join(",", tracks.EnumerateArray().Select(t => t.GetString())),
                U64(e, "encode_dropped_ms") ?? 0, U64(e, "dropped_frames"),
                e.TryGetProperty("fragmented", out JsonElement f) && f.ValueKind is JsonValueKind.True or JsonValueKind.False
                    ? f.GetBoolean() : null);
        }
        catch (JsonException)
        {
            return null;
        }
    }

    private static string? Str(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    private static ulong? U64(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.Number && v.TryGetUInt64(out ulong n)
            ? n : null;
}
