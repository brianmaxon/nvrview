// Alerts (docs/UX.md "Alerts"): some detections must not pass unseen. A pure
// model, tested without a window; never an "all clear".

using System.Globalization;

namespace NvrView.Tiles;

/// <summary>Which detections raise an alert: one list, so device alarms can be added later.</summary>
public static class AlertPolicy
{
    public static readonly IReadOnlySet<string> Kinds = new HashSet<string> { "smoke_alarm", "co_alarm" };
    public static readonly IReadOnlySet<string> Sources = new HashSet<string> { DetectionSource.LocalAudio };

    public static bool Raises(string kind, string source) => Kinds.Contains(kind) && Sources.Contains(source);
}

/// <summary>The red border's behavior: blinking about once a second, or steady without animations.</summary>
public static class AlertBorder
{
    public enum Mode
    {
        Blinking,
        Steady,
    }

    public const double BlinkPeriod = 1.0;

    public static Mode ModeFor(bool animationsEnabled) => animationsEnabled ? Mode.Blinking : Mode.Steady;

    /// <summary>Whether the border is drawn <paramref name="seconds"/> into the alert: on half the period when blinking.</summary>
    public static bool Visible(Mode mode, double seconds) =>
        mode == Mode.Steady || seconds % BlinkPeriod < BlinkPeriod / 2;
}

/// <summary>One raised alert for a camera (a device's channel: every lens).</summary>
public sealed class Alert
{
    public required string Device { get; init; }

    public required int Channel { get; init; }

    public required string Kind { get; set; }

    public required DateTime FirstAt { get; init; }

    public required DateTime LastAt { get; set; }

    /// <summary>Detection starts since it was raised.</summary>
    public int Count { get; set; }

    /// <summary>Whether the detection is still going.</summary>
    public bool Sounding { get; set; }

    /// <summary>"Smoke alarm (sound)".</summary>
    public string Reason => $"{DetectionTracker.Label(Kind)} (sound)";

    /// <summary>The header's lines: reason, first detection, count and last, state.</summary>
    public string[] Lines(DateTime now)
    {
        string times = Count == 1 ? "1 time" : $"{Count} times";
        return
        [
            Reason,
            $"First detected {Stamp(FirstAt, now)}",
            $"Detected {times}, last {Stamp(LastAt, now)}",
            Sounding ? "Sounding now" : "Not sounding now",
        ];
    }

    /// <summary>Local time, with the date when it is not today.</summary>
    public static string Stamp(DateTime at, DateTime now) =>
        at.Date == now.Date
            ? at.ToString("HH:mm:ss", CultureInfo.InvariantCulture)
            : at.ToString("yyyy-MM-dd HH:mm:ss", CultureInfo.InvariantCulture);
}

/// <summary>A line for the session's event log.</summary>
public readonly record struct AlertLogEntry(DateTime At, string Text);

/// <summary>
/// The session's alerts, by camera (device and channel). Detection starts
/// raise or count; stops only mark it quiet; only a dismissal clears it.
/// </summary>
public sealed class AlertCenter
{
    private readonly Dictionary<(string Device, int Channel), Alert> _alerts = [];
    private readonly List<AlertLogEntry> _log = [];

    /// <summary>Raised and dismissed alerts for the session, oldest first.</summary>
    public IReadOnlyList<AlertLogEntry> Log => _log;

    /// <summary>The alert for any lens of a channel's camera.</summary>
    public Alert? For(CameraRef camera) => _alerts.GetValueOrDefault((camera.Device, camera.Channel));

    public bool IsActive(CameraRef camera) => For(camera) is not null;

    public IReadOnlyList<Alert> All => [.. _alerts.Values.OrderBy(a => a.FirstAt)];

    /// <summary>
    /// Folds in a detection event. <paramref name="at"/> is when the sound was first heard (a
    /// start arrives seconds later). Returns whether anything changed.
    /// </summary>
    public bool Apply(DetectionEvent e, DateTime at)
    {
        if (e.Alarm || !AlertPolicy.Raises(e.Kind, e.Source))
        {
            return false;
        }
        var key = (e.Device, e.Channel);
        if (e.Active)
        {
            if (_alerts.TryGetValue(key, out Alert? a))
            {
                a.Count++;
                a.LastAt = at;
                a.Sounding = true;
                a.Kind = e.Kind;
                _log.Add(new AlertLogEntry(at, $"Alert again: {a.Reason}, channel {e.Channel + 1}, detection {a.Count}"));
            }
            else
            {
                a = new Alert { Device = e.Device, Channel = e.Channel, Kind = e.Kind, FirstAt = at, LastAt = at, Count = 1, Sounding = true };
                _alerts[key] = a;
                _log.Add(new AlertLogEntry(at, $"Alert raised: {a.Reason}, channel {e.Channel + 1}"));
            }
            return true;
        }
        if (_alerts.TryGetValue(key, out Alert? quiet) && quiet.Sounding)
        {
            quiet.Sounding = false;
            return true;
        }
        return false;
    }

    /// <summary>A lost session says nothing about whether it is still sounding.</summary>
    public void DeviceLost(string device)
    {
        foreach (Alert a in _alerts.Values.Where(a => a.Device == device && a.Sounding))
        {
            a.Sounding = false;
        }
    }

    /// <summary>The user dismissed the camera's alert: gone for every tile and window, count reset.</summary>
    public Alert? Dismiss(CameraRef camera, DateTime at)
    {
        if (!_alerts.Remove((camera.Device, camera.Channel), out Alert? a))
        {
            return null;
        }
        string times = a.Count == 1 ? "1 detection" : $"{a.Count} detections";
        _log.Add(new AlertLogEntry(at, $"Alert dismissed: {a.Reason}, channel {a.Channel + 1}, {times}"));
        return a;
    }
}
