// Sound in the Recordings window (docs/UX.md "Recordings", "Sound";
// FFI-RECORDINGS.md section 2a): the sound kinds and their names, the
// loudness lanes' answer and scale, and how many cameras one device may
// listen to. The same rules, with the same test vectors, as the macOS app's
// RecordingsSound.swift.

using System.Text.Json;

namespace NvrView.Shell;

/// <summary>The sound kinds of section 2a (the classifier's labels and the core's alarm detector map onto them; unknown kinds pass through).</summary>
public static class SoundKinds
{
    /// <summary>The documented kinds, in display order.</summary>
    public static IReadOnlyList<string> All { get; } =
        ["smoke_alarm", "co_alarm", "siren", "car_alarm", "glass_break", "dog_bark", "baby_cry", "shout", "knock", "doorbell"];

    /// <summary>The <c>source</c> of events from sound (the core's detector or the app's labels).</summary>
    public const string LocalAudio = "local_audio";

    /// <summary>Whether an event is from sound: its source says so, or it is one of the sound kinds.</summary>
    public static bool IsSound(string kind, string? source) => source == LocalAudio || All.Contains(kind);

    /// <summary>Whether an event is from sound.</summary>
    public static bool IsSound(RecordedEvent e) => IsSound(e.Kind, e.Source);

    /// <summary>"Smoke alarm", "Dog barking"; an unknown kind's own words.</summary>
    public static string Name(string kind) => kind switch
    {
        "smoke_alarm" => "Smoke alarm",
        "co_alarm" => "CO alarm",
        "siren" => "Siren",
        "car_alarm" => "Car alarm",
        "glass_break" => "Glass breaking",
        "dog_bark" => "Dog barking",
        "baby_cry" => "Baby crying",
        "shout" => "Shouting",
        "knock" => "Knocking",
        "doorbell" => "Doorbell",
        _ => CapitalizedFirst(kind.Replace('_', ' ')),
    };

    private static string CapitalizedFirst(string s) => s.Length == 0 ? s : char.ToUpperInvariant(s[0]) + s[1..];

    /// <summary>The distinct sounds of these events, by name in first-seen order.</summary>
    public static IReadOnlyList<string> Names(IEnumerable<RecordedEvent> events)
    {
        var names = new List<string>();
        foreach (RecordedEvent e in events.Where(IsSound))
        {
            string n = Name(e.Kind);
            if (!names.Contains(n))
            {
                names.Add(n);
            }
        }
        return names;
    }
}

/// <summary>One camera's loudness lane (<c>nv_recordings_loudness</c>): whole dBFS per bucket, null where no audio was analyzed (not silence).</summary>
public sealed record LoudnessLane(RecordingCamera Camera, long Bucket, long Start, IReadOnlyList<int?> Peak, IReadOnlyList<int?> Rms)
{
    /// <summary>Whether any bucket has audio.</summary>
    public bool Heard => Peak.Any(p => p is not null);
}

/// <summary><c>nv_recordings_loudness</c>.</summary>
public sealed record LoudnessAnswer(long Revision, IReadOnlyList<LoudnessLane> Lanes)
{
    /// <summary>Parses the answer; null when it does not parse.</summary>
    public static LoudnessAnswer? Parse(string json) => Answer.Read(json, root =>
    {
        var lanes = new List<LoudnessLane>();
        foreach (JsonElement l in Incident.Items(root, "lanes"))
        {
            if (l.TryGetProperty("camera", out JsonElement ce) && RecordingCamera.From(ce) is { } camera)
            {
                lanes.Add(new LoudnessLane(
                    camera, DownloadProgress.Long(l, "bucket_s") ?? 60, DownloadProgress.Long(l, "start") ?? 0,
                    Values(l, "peak_db"), Values(l, "rms_db")));
            }
        }
        return new LoudnessAnswer(DownloadProgress.Long(root, "revision") ?? 0, lanes);
    });

    private static List<int?> Values(JsonElement lane, string name) =>
        [.. Incident.Items(lane, name).Select(v => v.TryGetInt32(out int n) ? n : (int?)null)];
}

/// <summary>The loudness strips' arithmetic.</summary>
public static class LoudnessScale
{
    /// <summary>The quietest level drawn (dBFS); quieter is drawn as this.</summary>
    public const int FloorDb = -80;

    /// <summary>Bucket sizes the window asks for, in seconds.</summary>
    public static IReadOnlyList<long> Buckets { get; } = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600];

    /// <summary>The bucket for lanes <paramref name="width"/> wide showing <paramref name="visibleSeconds"/>: the smallest at least two points wide.</summary>
    public static long Bucket(double visibleSeconds, double width)
    {
        double perPoint = visibleSeconds / Math.Max(width, 1);
        foreach (long b in Buckets)
        {
            if (b >= perPoint * 2)
            {
                return b;
            }
        }
        return 3600;
    }

    /// <summary>0...1 for a level: 0 at <see cref="FloorDb"/> and below, 1 at 0 dBFS.</summary>
    public static double Level(int db) => Math.Clamp((double)(db - FloorDb) / -FloorDb, 0, 1);

    /// <summary>The camera whose loudness a lane shows: loudness belongs to a camera's first lens (a microphone is not tied to a lens).</summary>
    public static RecordingCamera Microphone(RecordingCamera c) => c with { Lens = 0 };

    /// <summary>The cameras to ask for, for these lanes (each microphone once, in order).</summary>
    public static IReadOnlyList<RecordingCamera> Microphones(IEnumerable<RecordingCamera> cameras) => [.. cameras.Select(Microphone).Distinct()];

    /// <summary><c>nv_recordings_loudness</c> for <paramref name="cameras"/> over [start, end].</summary>
    public static string Query(long start, long end, long bucket, IEnumerable<RecordingCamera> cameras) =>
        $"{{\"start\":{start},\"end\":{end},\"bucket_s\":{bucket},\"cameras\":[{string.Join(",", cameras.Select(c => c.Json))}]}}";
}

/// <summary>
/// Which cameras listen for sounds (<c>nv_sound_watch</c>): each keeps its sub-stream open with no tile, about
/// 110 kbit/s from the device, and counts toward the device's streams like a tile; a device has been seen to carry
/// six sub-streams on one session, and the Recordings player takes up to four, so at most four cameras of a device
/// listen.
/// </summary>
public static class SoundListening
{
    /// <summary>At most this many cameras of one device listen.</summary>
    public const int MaxPerDevice = 4;

    /// <summary>Whether <paramref name="camera"/> may be added to <paramref name="list"/> (true when it is in it).</summary>
    public static bool CanAdd(RecordingCamera camera, IEnumerable<RecordingCamera> list) =>
        list.Contains(camera) || list.Count(c => c.Device == camera.Device) < MaxPerDevice;

    /// <summary>What a camera's <c>sound_watch</c> state says to the user.</summary>
    public static string Describe(string state, string? reason) => (state, reason) switch
    {
        ("watching", _) => "Listening",
        ("stopped", _) => "Not listening",
        ("refused", "no_audio") => "No sound from this camera",
        ("refused", "device_closed") => "The device is not connected",
        ("refused", "sound_off") => "Sound analysis is off",
        ("refused", _) => "Could not listen",
        _ => state,
    };
}
