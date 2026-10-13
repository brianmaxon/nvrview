// The recording player's stream choice (docs/UX.md "Recordings", "Quality"): each camera of a playback plays its main or
// its sub-stream, and the user can change it during playback. Pure, so the rules have unit tests; the player only
// applies the changes (a tile tier call, which restarts that camera's play at the current position). The same type as the
// macOS app's PlayerStreams (NVRCore), with the same tests.

using NvrView.Interop;
using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>What a player tile asks of its camera's stream.</summary>
public enum PlayerQuality
{
    /// <summary>The sub-stream in the grid and at 2x or faster; the main stream in a single large view at 1x or slower, and when zoomed past 2x.</summary>
    Auto,

    /// <summary>The main stream.</summary>
    High,

    /// <summary>The sub-stream.</summary>
    Low,
}

/// <summary>The words of <see cref="PlayerQuality"/>.</summary>
public static class PlayerQualities
{
    /// <summary>The words the menu and the header use.</summary>
    public static string Title(this PlayerQuality q) => q switch
    {
        PlayerQuality.High => "High",
        PlayerQuality.Low => "Low",
        _ => "Auto",
    };

    /// <summary>The menu's hint.</summary>
    public static string Detail(this PlayerQuality q) => q switch
    {
        PlayerQuality.High => "The main stream",
        PlayerQuality.Low => "The sub-stream",
        _ => "The sub-stream in the grid and at 2x or faster; the main stream alone at 1x or zoomed in",
    };
}

/// <summary>Which stream each camera of the player plays (see the file comment).</summary>
public sealed class PlayerStreams
{
    /// <summary>Auto's speed rule: from this speed up, the sub-stream (devices send keyframes only from here, so the main stream buys nothing).</summary>
    public const double SourceRate = 2.0;

    private readonly Dictionary<RecordingCamera, PlayerQuality> _choices = [];

    // Cameras Auto holds on the main stream because of their zoom (ZoomHysteresis: up at 2x, back down below 1.6x).
    private readonly HashSet<RecordingCamera> _zoomedHigh = [];
    private readonly Dictionary<RecordingCamera, NvStreamTier> _applied = [];

    /// <summary>The tier last asked of the core for a camera (what it plays), if any.</summary>
    public NvStreamTier? Applied(RecordingCamera camera) => _applied.TryGetValue(camera, out NvStreamTier t) ? t : null;

    /// <summary>The camera's choice (Auto when none).</summary>
    public PlayerQuality Quality(RecordingCamera camera) => _choices.GetValueOrDefault(camera, PlayerQuality.Auto);

    /// <summary>Sets the camera's choice.</summary>
    public void SetQuality(RecordingCamera camera, PlayerQuality quality)
    {
        if (quality == PlayerQuality.Auto)
        {
            _choices.Remove(camera);
        }
        else
        {
            _choices[camera] = quality;
        }
    }

    /// <summary>Whether the player shows <paramref name="camera"/> alone, or as its only camera: the "single large view" of Auto.</summary>
    public static bool IsLarge(RecordingCamera camera, IReadOnlyList<RecordingCamera> cameras, PlayerGridMode mode)
    {
        int i = cameras.ToList().IndexOf(camera);
        if (i < 0)
        {
            return false;
        }
        if (cameras.Count == 1)
        {
            return true;
        }
        return PlayerGrid.Normalized(mode, cameras.Count) is PlayerGridMode.Single s && s.Index == i;
    }

    /// <summary>Auto for one camera, given whether its zoom holds it high.</summary>
    public static NvStreamTier AutoTier(bool large, double rate, bool zoomHigh) =>
        rate >= SourceRate ? NvStreamTier.Sub : large || zoomHigh ? NvStreamTier.Main : NvStreamTier.Sub;

    /// <summary>The stream <paramref name="camera"/> should play now. Updates the zoom memory.</summary>
    public NvStreamTier Wanted(RecordingCamera camera, IReadOnlyList<RecordingCamera> cameras, PlayerGridMode mode, double rate, double zoomScale)
    {
        switch (Quality(camera))
        {
            case PlayerQuality.High:
                return NvStreamTier.Main;
            case PlayerQuality.Low:
                return NvStreamTier.Sub;
            default:
                bool high = ZoomHysteresis.Next(_zoomedHigh.Contains(camera), zoomScale);
                if (high)
                {
                    _zoomedHigh.Add(camera);
                }
                else
                {
                    _zoomedHigh.Remove(camera);
                }
                return AutoTier(IsLarge(camera, cameras, mode), rate, high);
        }
    }

    /// <summary>The tiers the playback opens with (nothing applied yet); they count as applied.</summary>
    public List<NvStreamTier> Opening(IReadOnlyList<RecordingCamera> cameras, PlayerGridMode mode, double rate, Func<RecordingCamera, double> zoom)
    {
        var tiers = new List<NvStreamTier>();
        foreach (RecordingCamera c in cameras)
        {
            NvStreamTier tier = Wanted(c, cameras, mode, rate, zoom(c));
            _applied[c] = tier;
            tiers.Add(tier);
        }
        return tiers;
    }

    /// <summary>A camera that joined with this tier.</summary>
    public void Joined(RecordingCamera camera, NvStreamTier tier) => _applied[camera] = tier;

    /// <summary>Cameras whose stream differs from what plays, with the stream they need; they count as applied (the caller sends them).</summary>
    public List<(RecordingCamera Camera, NvStreamTier Tier)> Changes(IReadOnlyList<RecordingCamera> cameras, PlayerGridMode mode, double rate, Func<RecordingCamera, double> zoom)
    {
        var changes = new List<(RecordingCamera, NvStreamTier)>();
        foreach (RecordingCamera c in cameras)
        {
            NvStreamTier tier = Wanted(c, cameras, mode, rate, zoom(c));
            if (Applied(c) == tier)
            {
                continue;
            }
            _applied[c] = tier;
            changes.Add((c, tier));
        }
        return changes;
    }

    /// <summary>Keeps the cameras that are still in the player.</summary>
    public void Retain(IReadOnlyList<RecordingCamera> cameras)
    {
        var keep = cameras.ToHashSet();
        foreach (RecordingCamera c in _choices.Keys.Where(c => !keep.Contains(c)).ToList())
        {
            _choices.Remove(c);
        }
        _zoomedHigh.RemoveWhere(c => !keep.Contains(c));
        foreach (RecordingCamera c in _applied.Keys.Where(c => !keep.Contains(c)).ToList())
        {
            _applied.Remove(c);
        }
    }

    /// <summary>What a tile's header says about its stream: "main stream", "sub stream", with the choice in front when it is not Auto ("High · main stream").</summary>
    public string Label(RecordingCamera camera)
    {
        string stream = (Applied(camera) ?? NvStreamTier.Sub) == NvStreamTier.Main ? "main stream" : "sub stream";
        PlayerQuality q = Quality(camera);
        return q == PlayerQuality.Auto ? stream : $"{q.Title()} · {stream}";
    }
}
