// Smart alerts (ABI 3 minor 12; docs/FFI.md "Smart alerts",
// docs/NOTIFICATIONS.md): ISmartAlerts, the calls the app's detector and
// alert views make, and NvCore's implementation over the C functions. The
// core wakes cameras and holds pictures; the app fetches each picture
// (AlertFramesGet), decodes it, runs its detector, and answers with boxes
// (AlertDetectionsPut). Requests and answers stay JSON text in the
// contract's shapes; NvrView.Shell parses the events (SmartAlertEvent) and
// has a fake.

using System.Text.Json.Nodes;

namespace NvrView.Interop;

/// <summary>A verification request's frames: Annex-B access units from a keyframe (parameter sets included) to the picture to check.</summary>
/// <param name="Kind"><see cref="NvThumbKind.H264Keyframe"/> or <see cref="NvThumbKind.H265Keyframe"/> (the codec).</param>
/// <param name="Width">Of the picture (0 when unknown).</param>
/// <param name="Height">Of the picture (0 when unknown).</param>
/// <param name="PtsUs">The picture's stream timestamp.</param>
/// <param name="Data">The access units, back to back (their sizes are in the request event).</param>
public sealed record AlertFrames(NvThumbKind Kind, uint Width, uint Height, long PtsUs, byte[] Data);

/// <summary>One detector box for <see cref="ISmartAlerts.AlertDetectionsPut"/>, normalized to the whole picture.</summary>
/// <param name="Label">An alert class (<c>person</c>, <c>vehicle</c>, <c>animal</c>) or a detector label (<c>car</c>, <c>dog</c>); others are ignored by the core.</param>
/// <param name="Score">The detector's score.</param>
/// <param name="X">Left.</param>
/// <param name="Y">Top.</param>
/// <param name="W">Width.</param>
/// <param name="H">Height.</param>
public sealed record AlertDetection(string Label, double Score, double X, double Y, double W, double H)
{
    /// <summary><c>{"detections":[...]}</c>.</summary>
    public static string Json(IEnumerable<AlertDetection> detections)
    {
        var list = new JsonArray();
        foreach (AlertDetection d in detections)
        {
            list.Add((JsonNode)new JsonObject
            {
                ["label"] = d.Label,
                ["score"] = d.Score,
                ["box"] = new JsonObject { ["x"] = d.X, ["y"] = d.Y, ["w"] = d.W, ["h"] = d.H },
            });
        }
        return new JsonObject { ["detections"] = list }.ToJsonString();
    }
}

/// <summary>
/// The smart-alert calls (minor 12), as the core and the fake answer them.
/// None waits on a device. Not from inside a callback. Errors throw
/// <see cref="NvException"/> with the C function's status.
/// </summary>
public interface ISmartAlerts
{
    /// <summary><c>nv_alerts_configure</c>: the settings document; the first call starts the service (an <c>alerts_configured</c> event reports what was clamped).</summary>
    void AlertsConfigure(string json);

    /// <summary><c>nv_alerts_get</c>: the event log, newest first, filtered by <paramref name="query"/> (JSON, or null).</summary>
    string AlertsGet(string? query);

    /// <summary><c>nv_alerts_sun_times</c> (minor 13): today's sunrise, sunset, and night window at a location (JSON in, JSON out). No screen uses it on Windows yet.</summary>
    string AlertsSunTimes(string request);

    /// <summary><c>nv_alerts_trigger_put</c>: a trigger from the app; its ID.</summary>
    ulong AlertsTriggerPut(string json);

    /// <summary><c>nv_alert_frames_get</c>: a verification request's access units.</summary>
    AlertFrames AlertFramesGet(string request);

    /// <summary><c>nv_alert_detections_put</c>: the detector's boxes (<see cref="AlertDetection.Json"/>). A picture that could not be checked is not answered.</summary>
    void AlertDetectionsPut(string request, string json);

    /// <summary><c>nv_alert_snapshot_put</c>: the app's picture of an alert (JPEG).</summary>
    void AlertSnapshotPut(ulong alertId, ReadOnlySpan<byte> data);

    /// <summary><c>nv_alert_snapshot_get</c>.</summary>
    byte[] AlertSnapshotGet(ulong alertId);

    /// <summary><c>nv_alert_feedback</c>: <c>{"alert_id","vote","reason"?}</c>.</summary>
    void AlertFeedback(string json);

    /// <summary><c>nv_alert_mute</c>: mutes a camera (JSON) for <paramref name="seconds"/>; 0 unmutes.</summary>
    void AlertMute(string camera, uint seconds);
}

public sealed unsafe partial class NvCore : ISmartAlerts
{
    /// <inheritdoc/>
    public void AlertsConfigure(string json) =>
        Call(c => NvNative.nv_alerts_configure(c, json), nameof(NvNative.nv_alerts_configure));

    /// <inheritdoc/>
    public string AlertsSunTimes(string request) =>
        Answer((c, b, n, l) => NvNative.nv_alerts_sun_times(c, request, b, n, l), nameof(NvNative.nv_alerts_sun_times));

    /// <inheritdoc/>
    public string AlertsGet(string? query) =>
        Answer((c, b, n, l) => NvNative.nv_alerts_get(c, query, b, n, l), nameof(NvNative.nv_alerts_get));

    /// <inheritdoc/>
    public ulong AlertsTriggerPut(string json)
    {
        NotInCallback();
        ulong id = 0;
        NvStatus s;
        using (AcquireLease(out nint core))
        {
            s = NvNative.nv_alerts_trigger_put(core, json, &id);
        }
        Check(s, nameof(NvNative.nv_alerts_trigger_put));
        return id;
    }

    /// <inheritdoc/>
    public AlertFrames AlertFramesGet(string request)
    {
        NotInCallback();
        int cap = 256 * 1024;
        using (AcquireLease(out nint core))
        {
            for (int attempt = 0; attempt < 4; attempt++)
            {
                byte[] buffer = new byte[cap];
                NvThumbnail info = default;
                nuint len = 0;
                NvStatus s;
                fixed (byte* p = buffer)
                {
                    s = NvNative.nv_alert_frames_get(core, request, &info, p, (nuint)cap, &len);
                }
                switch (s)
                {
                    case NvStatus.BufferTooSmall:
                        cap = Math.Max(checked((int)len), 1);
                        continue;
                    case NvStatus.Ok:
                        return new AlertFrames(info.Kind, info.Width, info.Height, info.PtsUs, buffer[..checked((int)len)]);
                    default:
                        throw new NvException(s, nameof(NvNative.nv_alert_frames_get));
                }
            }
        }
        throw new NvException(NvStatus.BufferTooSmall, nameof(NvNative.nv_alert_frames_get));
    }

    /// <inheritdoc/>
    public void AlertDetectionsPut(string request, string json) =>
        Call(c => NvNative.nv_alert_detections_put(c, request, json), nameof(NvNative.nv_alert_detections_put));

    /// <inheritdoc/>
    public void AlertSnapshotPut(ulong alertId, ReadOnlySpan<byte> data)
    {
        NotInCallback();
        NvStatus s;
        using (AcquireLease(out nint core))
        {
            fixed (byte* p = data)
            {
                s = NvNative.nv_alert_snapshot_put(core, alertId, p, (nuint)data.Length);
            }
        }
        Check(s, nameof(NvNative.nv_alert_snapshot_put));
    }

    /// <inheritdoc/>
    public byte[] AlertSnapshotGet(ulong alertId)
    {
        NotInCallback();
        int cap = 64 * 1024;
        using (AcquireLease(out nint core))
        {
            for (int attempt = 0; attempt < 4; attempt++)
            {
                byte[] buffer = new byte[cap];
                nuint len = 0;
                NvStatus s;
                fixed (byte* p = buffer)
                {
                    s = NvNative.nv_alert_snapshot_get(core, alertId, p, (nuint)cap, &len);
                }
                switch (s)
                {
                    case NvStatus.BufferTooSmall:
                        cap = Math.Max(checked((int)len), 1);
                        continue;
                    case NvStatus.Ok:
                        return buffer[..checked((int)len)];
                    default:
                        throw new NvException(s, nameof(NvNative.nv_alert_snapshot_get));
                }
            }
        }
        throw new NvException(NvStatus.BufferTooSmall, nameof(NvNative.nv_alert_snapshot_get));
    }

    /// <inheritdoc/>
    public void AlertFeedback(string json) =>
        Call(c => NvNative.nv_alert_feedback(c, json), nameof(NvNative.nv_alert_feedback));

    /// <inheritdoc/>
    public void AlertMute(string camera, uint seconds) =>
        Call(c => NvNative.nv_alert_mute(c, camera, seconds), nameof(NvNative.nv_alert_mute));
}
