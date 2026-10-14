// Analyzing a recorded day's sound (nv_sound_analyze_day; docs/UX.md
// "Recordings", "Sound"; FFI-RECORDINGS.md "Sound from downloaded days"): the
// request, the state a camera-day is in as sound_analyze events arrive, and
// the words for each outcome. The same rules, with the same test vectors, as
// the macOS app's SoundAnalysis.swift.

using NvrView.Interop;

namespace NvrView.Shell;

/// <summary>Where a camera-day's analysis is.</summary>
public enum AnalysisState
{
    /// <summary>Waiting for its turn.</summary>
    Queued,

    /// <summary>Reading the downloaded video's audio.</summary>
    Running,

    /// <summary>Finished.</summary>
    Done,

    /// <summary>Ended with a reason.</summary>
    Failed,

    /// <summary>Canceled by the user.</summary>
    Canceled,
}

/// <summary>One camera-day's latest <c>sound_analyze</c> state.</summary>
public sealed record AnalysisEntry(ulong Request, AnalysisState State, string? Reason = null, long? Seconds = null, long? Labels = null)
{
    /// <summary>Waiting or running: the lane shows progress and a cancel.</summary>
    public bool Active => State is AnalysisState.Queued or AnalysisState.Running;
}

/// <summary>The sound analysis rules.</summary>
public static class SoundAnalysis
{
    /// <summary><c>nv_sound_analyze_day</c>'s input for a camera and a day (any second of it).</summary>
    public static string Query(RecordingCamera camera, long day) => $"{{\"camera\":{camera.Json},\"day\":{day}}}";

    /// <summary><c>nv_sound_analyze_day</c>'s input for one span of a day (a loudness gap), inclusive device-local seconds inside the day.</summary>
    public static string Query(RecordingCamera camera, long day, long start, long end) =>
        $"{{\"camera\":{camera.Json},\"day\":{day},\"start\":{start},\"end\":{end}}}";

    /// <summary>The state an event names, or null for one this app does not know.</summary>
    public static AnalysisState? ParseState(string name) => name switch
    {
        "queued" => AnalysisState.Queued,
        "running" => AnalysisState.Running,
        "done" => AnalysisState.Done,
        "failed" => AnalysisState.Failed,
        "canceled" => AnalysisState.Canceled,
        _ => null,
    };

    /// <summary>Folds an event into a camera-day's entry. A late queued or running never takes back a finished state of the same request.</summary>
    public static AnalysisEntry Fold(AnalysisEntry? old, ulong request, AnalysisState state, string? reason, long? seconds, long? labels) =>
        old is { Active: false } && old.Request == request && state is AnalysisState.Queued or AnalysisState.Running
            ? old
            : new AnalysisEntry(request, state, reason, seconds, labels);

    /// <summary>The words for a failed analysis.</summary>
    public static string Failure(string? reason) => reason switch
    {
        "no_audio" => "Nothing downloaded for this day — download the day or an incident first",
        "no_source" => "Set a downloads folder in Settings",
        "source_unavailable" => "The downloaded video could not be read",
        "not_enabled" => "Choose this camera in \"Cameras to search\" first",
        null => "Could not analyze the sound",
        _ => $"Could not analyze the sound ({reason.Replace('_', ' ')})",
    };

    /// <summary>The words for a call the core refused.</summary>
    public static string Refusal(NvStatus status) => status switch
    {
        NvStatus.NotConfigured => Failure("no_source"),
        NvStatus.InvalidArgument => Failure("not_enabled"),
        NvStatus.Limit => "Too many sound analyses are waiting; try again in a moment",
        _ => $"Could not analyze the sound (status {(int)status})",
    };

    /// <summary>What a lane's progress says (null once the analysis is over).</summary>
    public static string? Progress(AnalysisEntry e) => e.State switch
    {
        AnalysisState.Queued => "Waiting to analyze sound",
        AnalysisState.Running => "Analyzing sound",
        _ => null,
    };

    /// <summary>The message a finished analysis leaves (null while it is active).</summary>
    public static string? Outcome(AnalysisEntry e)
    {
        switch (e.State)
        {
            case AnalysisState.Failed:
                return Failure(e.Reason);
            case AnalysisState.Canceled:
                return "Sound analysis canceled";
            case AnalysisState.Done:
                long seconds = e.Seconds ?? 0, labels = e.Labels ?? 0;
                if (seconds == 0)
                {
                    return "No sound found in the downloaded video";
                }
                string found = labels == 0 ? "" : $"; found {labels} {(labels == 1 ? "sound" : "sounds")}";
                return $"Analyzed {Duration(seconds)} of sound{found}";
            default:
                return null;
        }
    }

    /// <summary>"45 s", "12 min", "3 h 5 min".</summary>
    public static string Duration(long seconds)
    {
        if (seconds < 60)
        {
            return $"{seconds} s";
        }
        if (seconds < 3600)
        {
            return $"{seconds / 60} min";
        }
        long m = seconds % 3600 / 60;
        return m == 0 ? $"{seconds / 3600} h" : $"{seconds / 3600} h {m} min";
    }
}
