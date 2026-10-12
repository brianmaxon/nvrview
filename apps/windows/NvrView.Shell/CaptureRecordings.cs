// The recordings' pure side in the app (docs/CAPTURE.md "Live recording", "Recording calls",
// "Indicators"): the settings as the core's configure JSON, the pills' text, the messages for a
// refused start and for a recording that ended by itself, and which cameras are being recorded,
// from the recording events. The app does the calls and draws. The same rules and test vectors as
// the macOS app's CaptureRecordings.swift.

using System.Globalization;
using System.Text.Json.Nodes;
using NvrView.Interop;
using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>"Record from": the stream a recording takes.</summary>
public enum RecordQuality
{
    /// <summary>The main stream (default).</summary>
    Main,

    /// <summary>The sub-stream.</summary>
    Sub,
}

/// <summary>Names of <see cref="RecordQuality"/>.</summary>
public static class RecordQualityNames
{
    /// <summary>The core's name: <c>main</c> or <c>sub</c>.</summary>
    public static string Name(this RecordQuality q) => q == RecordQuality.Main ? "main" : "sub";

    /// <summary>The setting's choice as the user reads it.</summary>
    public static string Title(this RecordQuality q) => q == RecordQuality.Main ? "Main stream" : "Sub-stream";

    /// <summary>The quality named <paramref name="name"/>; null for anything else.</summary>
    public static RecordQuality? Parse(string? name) => name switch
    {
        "main" => RecordQuality.Main,
        "sub" => RecordQuality.Sub,
        _ => null,
    };
}

/// <summary>The capture settings the core is configured with (<c>nv_capture_configure</c>).</summary>
public sealed record CaptureSettings(string LiveFolder)
{
    /// <summary>The shortest and longest "Stop a recording after", in minutes (docs/CAPTURE.md "Bounds").</summary>
    public const int MinMinutes = 1, MaxMinutes = 12 * 60;

    /// <summary>The smallest and largest "or at", in bytes (decimal).</summary>
    public const ulong MinBytes = 100_000_000, MaxBytes = 64_000_000_000;

    /// <summary>"Record from".</summary>
    public RecordQuality Quality { get; init; } = RecordQuality.Main;

    /// <summary>"Share-safe names" (shared with pictures).</summary>
    public bool ShareSafe { get; init; }

    /// <summary>"Stop a recording after" (minutes).</summary>
    public int Minutes { get; init; } = 60;

    /// <summary>"... or at" (bytes, decimal).</summary>
    public ulong Bytes { get; init; } = 4_000_000_000;

    /// <summary>A recording does not start with less free than this.</summary>
    public ulong MinFreeBytes { get; init; } = 2_000_000_000;

    /// <summary>A recording stops when free space falls under this plus its own size.</summary>
    public ulong StopFreeBytes { get; init; } = 1_000_000_000;

    /// <summary>The most live recordings at a time.</summary>
    public int MaxLive { get; init; } = 4;

    /// <summary>The most call recordings at a time.</summary>
    public int MaxCalls { get; init; } = 2;

    /// <summary>The settings within their bounds.</summary>
    public CaptureSettings Clamped => this with
    {
        Minutes = Math.Clamp(Minutes, MinMinutes, MaxMinutes),
        Bytes = Math.Clamp(Bytes, MinBytes, MaxBytes),
    };

    /// <summary>The <c>nv_capture_configure</c> JSON (docs/FFI.md "Capture").</summary>
    public string Json()
    {
        CaptureSettings c = Clamped;
        return new JsonObject
        {
            ["live_dir"] = c.LiveFolder,
            ["quality"] = c.Quality.Name(),
            ["share_safe"] = c.ShareSafe,
            ["max_duration_s"] = c.Minutes * 60,
            ["max_bytes"] = c.Bytes,
            ["min_free_bytes"] = c.MinFreeBytes,
            ["stop_free_bytes"] = c.StopFreeBytes,
            ["max_live"] = c.MaxLive,
            ["max_calls"] = c.MaxCalls,
        }.ToJsonString();
    }
}

/// <summary>The words the recordings show.</summary>
public static class RecordingText
{
    /// <summary>"0:42", "12:03", "1:02:03".</summary>
    public static string Elapsed(ulong ms)
    {
        ulong s = ms / 1000;
        ulong h = s / 3600, m = s / 60 % 60, sec = s % 60;
        CultureInfo inv = CultureInfo.InvariantCulture;
        return h > 0 ? string.Create(inv, $"{h}:{m:00}:{sec:00}") : string.Create(inv, $"{m}:{sec:00}");
    }

    /// <summary>The tile's pill: "● REC 1:23".</summary>
    public static string TilePill(ulong ms) => $"● REC {Elapsed(ms)}";

    /// <summary>The call bar's pill: "● Recording 0:42".</summary>
    public static string CallPill(ulong ms) => $"● Recording {Elapsed(ms)}";

    /// <summary>"4 GB", "500 MB" (decimal, as the settings).</summary>
    public static string Size(ulong bytes)
    {
        CultureInfo inv = CultureInfo.InvariantCulture;
        if (bytes >= 1_000_000_000)
        {
            double gb = bytes / 1e9;
            return gb == Math.Round(gb) ? string.Create(inv, $"{(long)gb} GB") : string.Create(inv, $"{gb:0.0} GB");
        }
        return string.Create(inv, $"{bytes / 1_000_000} MB");
    }

    /// <summary>"60 minutes", "1 minute", "2 hours", "90 minutes".</summary>
    public static string Minutes(int m)
    {
        if (m >= 60 && m % 60 == 0)
        {
            return m == 60 ? "60 minutes" : $"{m / 60} hours";
        }
        return m == 1 ? "1 minute" : $"{m} minutes";
    }

    /// <summary>
    /// What a refused start says, and whether to offer the sub-stream instead (a device already
    /// records two main streams).
    /// </summary>
    public static (string Text, bool OfferSubStream) Refused(NvStatus status, string? limit, CaptureSettings settings, bool call) =>
        (status, limit) switch
        {
            (NvStatus.Limit, "live") => ($"Already recording {settings.MaxLive} cameras, the most at a time. Stop one to record another.", false),
            (NvStatus.Limit, "calls") => ($"This call is already recorded, or {settings.MaxCalls} calls are, the most at a time.", false),
            (NvStatus.Limit, "main_streams") => ("This device already records two main streams, the most it allows.", true),
            (NvStatus.Limit, "disk_low") => ($"Not enough free disk space to record: at least {Size(settings.MinFreeBytes)} is needed.", false),
            (NvStatus.Limit, _) => ("Recording is at a limit and cannot start.", false),
            (NvStatus.NotConfigured, _) => (call ? "Call recording needs the audio encoder, which is not available." : "Recording is not set up.", false),
            (NvStatus.UnknownId, _) => ("This camera's stream is not open, so it cannot be recorded.", false),
            (NvStatus.Unsupported, _) => ("This version of the core cannot record.", false),
            _ => ($"Recording could not start ({status}).", false),
        };

    /// <summary>
    /// What an ended recording says: null for a quit (the app is going away). <c>finished</c> with
    /// <c>stopped</c> is "Recording saved".
    /// </summary>
    public static string? Ended(RecordingEvent e, CaptureSettings settings)
    {
        string what = e.Kind == "call" ? "Call recording" : "Recording";
        if (e.State == "recovered")
        {
            return "An unfinished recording was completed";
        }
        CaptureSettings clamped = settings.Clamped;
        return e.Reason switch
        {
            "stopped" => e.State == "failed" ? $"{what} failed" : $"{what} saved",
            "core_shutdown" => null,
            "max_duration" => $"{what} stopped at its {Minutes(clamped.Minutes)} limit",
            "max_size" => $"{what} stopped at its {Size(clamped.Bytes)} limit",
            "disk_low" => $"{what} stopped: the disk is almost full",
            "device_removed" => $"{what} stopped: the device was removed",
            "login_failed" => $"{what} stopped: the device refused the login",
            "write_failed" => $"{what} stopped: the file could not be written",
            "encoder_failed" => $"{what} stopped: its sound could not be encoded",
            "no_video" => $"{what} stopped: no picture arrived",
            "format_changed" => $"{what} stopped: the stream changed",
            "long_gap" => $"{what} stopped after a long outage",
            _ => e.State == "failed" ? $"{what} failed" : $"{what} saved",
        };
    }
}

/// <summary>One recording that has not ended.</summary>
public sealed record BoardEntry(
    ulong Id, string Kind, CameraRef? Camera, string File, string State, string Tier, ulong DurationMs, RecordingEvent? Event)
{
    /// <summary>A call recording.</summary>
    public bool IsCall => Kind == "call";
}

/// <summary>
/// The recordings that have not ended, by ID, with their camera: what the pills, the Record
/// buttons, the window header's menu and Stop All read. Fed the app's own starts
/// (<see cref="Begin"/>, so the camera is known even for a device that goes away) and the core's
/// recording events.
/// </summary>
public sealed class RecordingBoard
{
    /// <summary>The latest ended recordings kept.</summary>
    public const int EndedKept = 32;

    private readonly Dictionary<ulong, BoardEntry> _running = [];
    private readonly List<RecordingEvent> _ended = [];

    /// <summary>The running recordings, by ID.</summary>
    public IReadOnlyDictionary<ulong, BoardEntry> Running => _running;

    /// <summary>The latest ended recordings, newest last, at most <see cref="EndedKept"/>.</summary>
    public IReadOnlyList<RecordingEvent> Ended => _ended;

    /// <summary>The app started a recording (before any of its events is handled: they reach the UI thread after the start returned).</summary>
    public void Begin(ulong id, string kind, CameraRef camera, string file, string tier) =>
        _running[id] = new BoardEntry(id, kind, camera, file, "waiting_keyframe", tier, 0, null);

    /// <summary>
    /// A recording event. Events of a recording the app did not start are ignored (FFI.md: an ID
    /// never handed out), except <c>recovered</c>. Returns the event when the recording ended (and
    /// was known or recovered), null otherwise.
    /// </summary>
    public RecordingEvent? Apply(RecordingEvent e)
    {
        if (e.Ended)
        {
            bool known = _running.Remove(e.Recording);
            if (!known && e.State != "recovered")
            {
                return null;
            }
            _ended.Add(e);
            if (_ended.Count > EndedKept)
            {
                _ended.RemoveRange(0, _ended.Count - EndedKept);
            }
            return e;
        }
        if (!_running.TryGetValue(e.Recording, out BoardEntry? entry))
        {
            return null;
        }
        _running[e.Recording] = entry with { State = e.State, File = e.File, Tier = e.Tier, DurationMs = e.DurationMs, Event = e };
        return null;
    }

    /// <summary>The running recordings, oldest first.</summary>
    public IReadOnlyList<BoardEntry> List => _running.Count == 0 ? [] : [.. _running.Values.OrderBy(r => r.Id)];

    /// <summary>Recordings of <paramref name="camera"/> (its device, channel and lens).</summary>
    public IReadOnlyList<BoardEntry> RecordingsOf(CameraRef camera) => [.. List.Where(r => r.Camera == camera)];

    /// <summary>The live recording of a camera, if any.</summary>
    public BoardEntry? LiveOf(CameraRef camera) => RecordingsOf(camera).FirstOrDefault(r => !r.IsCall);

    /// <summary>The call recording of a camera, if any.</summary>
    public BoardEntry? CallOf(CameraRef camera) => RecordingsOf(camera).FirstOrDefault(r => r.IsCall);

    /// <summary>The tile's pill for <paramref name="camera"/>: the longest running recording of it; null when none runs.</summary>
    public string? Pill(CameraRef camera)
    {
        IReadOnlyList<BoardEntry> mine = RecordingsOf(camera);
        return mine.Count == 0 ? null : RecordingText.TilePill(mine.Max(r => r.DurationMs));
    }

    /// <summary>A camera's device key changed (the core's stable key arrived).</summary>
    public void Rekey(string oldKey, string newKey)
    {
        foreach ((ulong id, BoardEntry e) in _running.ToArray())
        {
            if (e.Camera is { } c && c.Device == oldKey)
            {
                _running[id] = e with { Camera = c with { Device = newKey } };
            }
        }
    }
}

/// <summary>The files a call recording shares a stem with, and how many are kept (docs/CAPTURE.md "Recording calls", "Files").</summary>
public static class CallRecordingFiles
{
    /// <summary>Call recordings kept beside the transcripts: the newest this many.</summary>
    public const int MaxRecordings = 100;

    /// <summary>... and at most this many bytes in all (10 GB).</summary>
    public const long MaxRecordingBytes = 10_000_000_000;

    /// <summary>
    /// The stem a call's recording and its transcript share (<c>stem.mp4</c> and <c>stem.txt</c>):
    /// the transcript's name without ".txt", numbered as <see cref="TranscriptFiles.UniqueName"/>
    /// numbers it until none of <c>stem.txt</c>, <c>stem.mp4</c> and <c>stem.partial.mp4</c> is in
    /// <paramref name="taken"/> (compared without regard to case).
    /// </summary>
    public static string SharedStem(string name, IEnumerable<string> taken)
    {
        var lower = new HashSet<string>(taken.Select(t => t.ToLowerInvariant()), StringComparer.Ordinal);
        string stem = name.EndsWith(".txt", StringComparison.Ordinal) ? name[..^4] : name;
        bool Free(string s) => new[] { "txt", "mp4", "partial.mp4" }.All(ext => !lower.Contains($"{s}.{ext}".ToLowerInvariant()));
        if (Free(stem))
        {
            return stem;
        }
        int n = 2;
        while (!Free($"{stem} {n}"))
        {
            n++;
        }
        return $"{stem} {n}";
    }

    /// <summary>The call recordings (<c>.mp4</c>, never <c>.partial.mp4</c>) to delete, oldest first.</summary>
    public static IReadOnlyList<string> ToDelete(IEnumerable<StoredTranscript> files) =>
        TranscriptFiles.ToDelete(
            files.Where(f => f.Name.EndsWith(".mp4", StringComparison.OrdinalIgnoreCase)
                && !f.Name.EndsWith(".partial.mp4", StringComparison.OrdinalIgnoreCase)),
            MaxRecordings, MaxRecordingBytes);
}

/// <summary>"Record calls": the consent note shown the first time it is turned on.</summary>
public static class RecordCallsText
{
    /// <summary>The consent note.</summary>
    public const string Consent =
        "Recording a conversation may need the other person's consent where you live. The visitor is not told that the call is recorded.";
}
