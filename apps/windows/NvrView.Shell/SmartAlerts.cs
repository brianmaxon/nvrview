// Smart alerts (minor 12; docs/FFI.md "Smart alerts"): the events as the
// core writes them. The calls are ISmartAlerts in NvrView.Interop;
// FakeSmartAlerts answers them without a core.

using System.Text.Json;

namespace NvrView.Shell;

/// <summary>A <c>verify_request</c> event: one picture to check.</summary>
/// <param name="Request">The request's key (for AlertFramesGet and AlertDetectionsPut).</param>
/// <param name="Purpose"><c>check</c> or <c>recheck</c> ("still present").</param>
/// <param name="Device">The camera's device ID.</param>
/// <param name="Channel">The camera's channel.</param>
/// <param name="Lens">The camera's lens.</param>
/// <param name="TriggerIds">The checks the picture serves.</param>
/// <param name="Codec"><c>h264</c> or <c>h265</c>.</param>
/// <param name="Bytes">The frames' total size.</param>
/// <param name="FrameSizes">Each access unit's size, keyframe first.</param>
/// <param name="Classes">What to look for (<c>person</c>, <c>vehicle</c>, <c>animal</c>).</param>
/// <param name="MinScore">The score floor.</param>
/// <param name="DeadlineMs">When the core stops waiting for the answer (UTC ms).</param>
public sealed record VerifyRequest(
    string Request, string Purpose, ulong Device, int Channel, int Lens, IReadOnlyList<ulong> TriggerIds, string Codec,
    int Bytes, IReadOnlyList<int> FrameSizes, IReadOnlyList<string> Classes, double MinScore, long DeadlineMs)
{
    /// <summary>Parses a <c>verify_request</c>; null for another type or a malformed one.</summary>
    public static VerifyRequest? Parse(JsonElement e)
    {
        try
        {
            if (SmartAlertEvent.Str(e, "type") != "verify_request" || SmartAlertEvent.Str(e, "request") is not { } request ||
                !e.TryGetProperty("camera", out JsonElement cam) || !e.TryGetProperty("frames", out JsonElement frames) ||
                frames.ValueKind != JsonValueKind.Array)
            {
                return null;
            }
            return new VerifyRequest(
                request, SmartAlertEvent.Str(e, "purpose") ?? "check", cam.GetProperty("device").GetUInt64(),
                cam.GetProperty("channel").GetInt32(),
                cam.TryGetProperty("lens", out JsonElement l) ? l.GetInt32() : 0,
                [.. e.GetProperty("trigger_ids").EnumerateArray().Select(t => t.GetUInt64())],
                SmartAlertEvent.Str(e, "codec") ?? "h264", e.GetProperty("bytes").GetInt32(),
                [.. frames.EnumerateArray().Select(f => f.GetProperty("bytes").GetInt32())],
                [.. e.GetProperty("classes").EnumerateArray().Select(c => c.GetString() ?? "")],
                e.GetProperty("min_score").GetDouble(), e.GetProperty("deadline_ms").GetInt64());
        }
        catch (Exception x) when (x is KeyNotFoundException or InvalidOperationException or FormatException)
        {
            return null;
        }
    }
}

/// <summary>
/// One smart-alert event: its type, the fields the views switch on, and the
/// whole JSON (steps and decisions have many shapes; views decode what they
/// show).
/// </summary>
/// <param name="Type"><c>verify_request</c>, <c>verify_request_done</c>, <c>verify_step</c>, <c>alert</c>, <c>alerts_configured</c>, or <c>trigger_rejected</c>.</param>
/// <param name="State">For <c>alert</c>: <c>new</c>, <c>update</c>, <c>logged</c>, or <c>settled</c>; for <c>verify_request_done</c>: the result.</param>
/// <param name="TriggerId">For <c>verify_step</c>, and <c>alert</c> decisions.</param>
/// <param name="Request">For <c>verify_request</c> and <c>verify_request_done</c>.</param>
/// <param name="Json">The whole event.</param>
public sealed record SmartAlertEvent(string Type, string? State, ulong? TriggerId, string? Request, string Json)
{
    /// <summary>The event types this record parses.</summary>
    public static readonly IReadOnlySet<string> Types = new HashSet<string>
    {
        "verify_request", "verify_request_done", "verify_step", "alert", "alerts_configured", "trigger_rejected",
    };

    /// <summary>Parses one event; null for another type or malformed JSON.</summary>
    public static SmartAlertEvent? Parse(string json)
    {
        try
        {
            using JsonDocument d = JsonDocument.Parse(json);
            JsonElement e = d.RootElement;
            if (e.ValueKind != JsonValueKind.Object || Str(e, "type") is not { } type || !Types.Contains(type))
            {
                return null;
            }
            ulong? trigger = U64(e, "trigger_id");
            if (trigger is null && e.TryGetProperty("decision", out JsonElement dec) && dec.ValueKind == JsonValueKind.Object)
            {
                trigger = U64(dec, "trigger_id");
            }
            string? state = type == "verify_request_done" ? Str(e, "result") : Str(e, "state");
            return new SmartAlertEvent(type, state, trigger, Str(e, "request"), json);
        }
        catch (JsonException)
        {
            return null;
        }
    }

    internal static string? Str(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;

    internal static ulong? U64(JsonElement e, string name) =>
        e.TryGetProperty(name, out JsonElement v) && v.ValueKind == JsonValueKind.Number && v.TryGetUInt64(out ulong n)
            ? n : null;
}
