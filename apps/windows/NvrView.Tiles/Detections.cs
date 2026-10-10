// Which detections are active per camera, from the core's detection, motion,
// and alarm events (docs/FFI.md "Detections"). Pure, like the Mac app's
// DetectionTracker.

namespace NvrView.Tiles;

/// <summary>
/// A camera as the viewer shows it: a device (by the app's key), a channel,
/// and a lens (0, or 1 for the second lens of a multi-lens camera).
/// </summary>
public readonly record struct CameraRef(string Device, int Channel, int Lens = 0)
{
    public const int MaxLens = 1;

    /// <summary>The channel's first lens: the camera whose microphone it is.</summary>
    public CameraRef ChannelKey => this with { Lens = 0 };

    // Not the record's printout: it would print ChannelKey, a CameraRef, forever.
    public override string ToString() => Lens == 0 ? $"{Device}/{Channel}" : $"{Device}/{Channel}/lens{Lens}";
}

/// <summary>Where a detection came from (the events' <c>source</c>).</summary>
public static class DetectionSource
{
    public const string Device = "device";
    public const string LocalVideo = "local_video";
    public const string LocalAudio = "local_audio";

    /// <summary>Where it was detected, in words.</summary>
    public static string Label(string source) => source switch
    {
        Device => "camera AI",
        LocalVideo => "this PC",
        LocalAudio => "sound",
        _ => source,
    };
}

/// <summary>One detection event, already tied to a device key.</summary>
/// <param name="Device">The app's key for the device.</param>
/// <param name="Channel">The channel.</param>
/// <param name="Kind">The device's name for it (people, vehicle, dog_cat, face, package, motion, smoke_alarm, ...).</param>
/// <param name="Active">Start (true) or stop (false).</param>
/// <param name="Source">See <see cref="DetectionSource"/>.</param>
/// <param name="AtNanos">The core clock at arrival (0 when unknown).</param>
/// <param name="Lens">1 for the second lens of a multi-lens camera.</param>
/// <param name="Alarm">An <c>alarm</c> event (a device state that is not an object or motion, such as a doorbell press).</param>
public readonly record struct DetectionEvent(
    string Device, int Channel, string Kind, bool Active, string Source = DetectionSource.Device,
    long AtNanos = 0, int Lens = 0, bool Alarm = false)
{
    public const string MotionKind = "motion";

    /// <summary>The camera whose tiles show this detection.</summary>
    public CameraRef Camera => new(Device, Channel, Math.Clamp(Lens, 0, CameraRef.MaxLens));
}

/// <summary>Active detections per camera. Sound belongs to a channel's microphone, not a lens.</summary>
public sealed class DetectionTracker
{
    /// <summary>One active detection.</summary>
    public readonly record struct Active(string Kind, string Source, long SinceNanos);

    private readonly Dictionary<CameraRef, List<Active>> _active = [];
    // Kept per channel (keyed by its first lens), shown for every lens.
    private readonly Dictionary<CameraRef, List<Active>> _sound = [];

    internal IEnumerable<CameraRef> ActiveKeys => _active.Keys;

    internal IEnumerable<CameraRef> SoundKeys => _sound.Keys;

    /// <summary>Folds in one event for the camera it names.</summary>
    public void Apply(DetectionEvent e)
    {
        bool isSound = e.Source == DetectionSource.LocalAudio;
        CameraRef key = isSound ? e.Camera.ChannelKey : e.Camera;
        Dictionary<CameraRef, List<Active>> map = isSound ? _sound : _active;
        if (!map.TryGetValue(key, out List<Active>? list))
        {
            list = [];
        }
        list.RemoveAll(a => a.Kind == e.Kind && a.Source == e.Source);
        if (e.Active)
        {
            list.Add(new Active(e.Kind, e.Source, e.AtNanos));
        }
        list.Sort((a, b) => Order(a.Kind).CompareTo(Order(b.Kind)));
        if (list.Count == 0)
        {
            map.Remove(key);
        }
        else
        {
            map[key] = list;
        }
    }

    /// <summary>Forgets a device's detections (it disconnected: its stops may never come).</summary>
    public void Clear(string device)
    {
        foreach (CameraRef k in _active.Keys.Where(k => k.Device == device).ToArray())
        {
            _active.Remove(k);
        }
        foreach (CameraRef k in _sound.Keys.Where(k => k.Device == device).ToArray())
        {
            _sound.Remove(k);
        }
    }

    /// <summary>What is active for one camera (one lens): its own detections, then the channel's sound.</summary>
    public IReadOnlyList<Active> For(CameraRef camera)
    {
        var all = new List<Active>();
        if (_active.TryGetValue(camera, out List<Active>? own))
        {
            all.AddRange(own);
        }
        if (_sound.TryGetValue(camera.ChannelKey, out List<Active>? heard))
        {
            all.AddRange(heard);
        }
        all.Sort((a, b) => Order(a.Kind).CompareTo(Order(b.Kind)));
        return all;
    }

    /// <summary>Drops detections older than <paramref name="maxAgeNanos"/> (a lost stop must not leave a symbol up forever).</summary>
    public void Expire(long nowNanos, long maxAgeNanos)
    {
        foreach (Dictionary<CameraRef, List<Active>> map in new[] { _active, _sound })
        {
            foreach (CameraRef k in map.Keys.ToArray())
            {
                map[k].RemoveAll(a => a.SinceNanos != 0 && nowNanos - a.SinceNanos >= maxAgeNanos);
                if (map[k].Count == 0)
                {
                    map.Remove(k);
                }
            }
        }
    }

    /// <summary>Objects first, motion last.</summary>
    private static int Order(string kind) => kind switch
    {
        "people" => 0,
        "vehicle" => 1,
        "dog_cat" => 2,
        "face" => 3,
        "package" => 4,
        "smoke_alarm" or "co_alarm" => 6,
        DetectionEvent.MotionKind => 9,
        _ => 5,
    };

    /// <summary>The label a badge shows for a device's kind name.</summary>
    public static string Label(string kind) => kind switch
    {
        "people" or "person" => "Person",
        "vehicle" => "Vehicle",
        "dog_cat" or "animal" => "Animal",
        "face" => "Face",
        "package" => "Package",
        DetectionEvent.MotionKind => "Motion",
        "visitor" => "Visitor",
        "cry" => "Crying",
        "smoke_alarm" => "Smoke alarm",
        "co_alarm" => "CO alarm",
        _ => kind.Length == 0 ? "Detection" : char.ToUpperInvariant(kind[0]) + kind[1..].Replace('_', ' '),
    };

    /// <summary>The text of a sound badge: "Smoke alarm · sound".</summary>
    public static string SoundBadgeText(string kind) => $"{Label(kind)} · sound";
}
