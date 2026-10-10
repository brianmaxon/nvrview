// Per-tile stream quality and the tier each camera's tiles share (docs/UX.md
// "Stream quality per tile", "Full screen and focus"). Pure: the app asks
// which tier a camera should show and carries out the switch.

namespace NvrView.Tiles;

/// <summary>A device's stream tier.</summary>
public enum StreamTier
{
    Main,
    Sub,
    Mobile,
    Extern,
}

/// <summary>What a tile asks of its camera's stream.</summary>
public enum TileQuality
{
    /// <summary>The low stream in the grid, the main stream when focused or zoomed past 2x. Stored as no member at all.</summary>
    Auto,

    /// <summary>The main stream (a second lens's extern), also in the grid.</summary>
    Clear,

    /// <summary>The low stream, also when focused.</summary>
    Fluent,

    /// <summary>The middle stream (extern of a single-lens camera).</summary>
    Balanced,
}

public static class TileQualities
{
    public static string Title(this TileQuality q) => q switch
    {
        TileQuality.Auto => "Auto",
        TileQuality.Clear => "Clear",
        TileQuality.Fluent => "Fluent",
        _ => "Balanced",
    };

    /// <summary>The layout leaf's "quality" member: null (no member) for Auto.</summary>
    public static string? ToMember(this TileQuality q) => q switch
    {
        TileQuality.Clear => "clear",
        TileQuality.Fluent => "fluent",
        TileQuality.Balanced => "balanced",
        _ => null,
    };

    /// <summary>Reads a leaf's member; missing or a value a newer app wrote reads as Auto.</summary>
    public static TileQuality FromMember(string? member) => member switch
    {
        "clear" => TileQuality.Clear,
        "fluent" => TileQuality.Fluent,
        "balanced" => TileQuality.Balanced,
        _ => TileQuality.Auto,
    };

    /// <summary>The hover header's note for a tile not on Auto, or null.</summary>
    public static string? HeaderNote(this TileQuality q) => q == TileQuality.Auto ? null : q.Title();

    /// <summary>The menu's note on what the main stream costs.</summary>
    public const string CostNote = "The main stream on many tiles costs the device and this computer much more.";
}

/// <summary>How sharp a stream is, whatever its tier is called for a camera. Higher wins when tiles share a camera.</summary>
public enum StreamLevel
{
    /// <summary>sub (a second lens's mobile, or extern where it has no mobile).</summary>
    Low = 0,

    /// <summary>A single-lens camera's extern.</summary>
    Mid = 1,

    /// <summary>main (a second lens's extern).</summary>
    High = 2,
}

/// <summary>
/// Auto's switch to the high stream while a tile is zoomed in: up at 2x, back
/// down only below 1.6x, so a zoom hovering near the threshold does not flap.
/// </summary>
public static class ZoomHysteresis
{
    public const double Up = 2.0;
    public const double Down = 1.6;

    public static bool Next(bool high, double scale) => high ? scale >= Down : scale >= Up;
}

/// <summary>
/// The tier each camera runs at: the highest level any tile's demand or any
/// focus asks for, mapped to a tier per camera. A first lens uses the grid
/// tier / extern / the focus tier; a second lens mobile (or extern once mobile
/// is refused) / (none) / extern.
/// </summary>
public sealed class TierPlanner
{
    /// <summary>What one tile (one owner) asks of its camera.</summary>
    public readonly record struct Demand(TileQuality Quality, bool Focused, bool ZoomHigh);

    private readonly Dictionary<object, (CameraRef Camera, Demand Demand)> _demands = [];
    private readonly Dictionary<CameraRef, int> _focus = [];
    private readonly HashSet<CameraRef> _noMobile = [];

    public StreamTier GridTier { get; set; } = StreamTier.Sub;

    public StreamTier FocusTier { get; set; } = StreamTier.Main;

    /// <summary>Whether a camera serves a middle stream to offer as Balanced: a first lens whose channel's tiers list extern.</summary>
    public Func<CameraRef, bool> HasMiddleTier { get; set; } = _ => false;

    /// <summary>Second lenses whose mobile tier was refused (they stay on extern for the run).</summary>
    public bool NoMobile(CameraRef camera) => _noMobile.Contains(camera);

    /// <summary>A view focuses (or stops focusing) <paramref name="camera"/>.</summary>
    public void SetFocused(CameraRef camera, bool focused)
    {
        int n = _focus.GetValueOrDefault(camera);
        _focus[camera] = Math.Max(n + (focused ? 1 : -1), 0);
    }

    public bool IsFocused(CameraRef camera) => _focus.GetValueOrDefault(camera) > 0;

    /// <summary>The tiers a camera uses in a grid and when focused.</summary>
    public (StreamTier Grid, StreamTier Focus) Tiers(CameraRef camera) =>
        camera.Lens > 0 ? (_noMobile.Contains(camera) ? StreamTier.Extern : StreamTier.Mobile, StreamTier.Extern) : (GridTier, FocusTier);

    /// <summary>The tier <paramref name="camera"/> should show now: the highest level asked for.</summary>
    public StreamTier WantedTier(CameraRef camera) => TierFor(WantedLevel(camera), camera);

    public StreamLevel WantedLevel(CameraRef camera)
    {
        StreamLevel level = IsFocused(camera) ? StreamLevel.High : StreamLevel.Low;
        foreach ((CameraRef c, Demand d) in _demands.Values)
        {
            if (c == camera)
            {
                level = (StreamLevel)Math.Max((int)level, (int)LevelOf(d, camera));
            }
        }
        return level;
    }

    /// <summary>What one demand asks of <paramref name="camera"/>.</summary>
    public StreamLevel LevelOf(Demand demand, CameraRef camera) => EffectiveQuality(demand.Quality, camera) switch
    {
        TileQuality.Clear => StreamLevel.High,
        TileQuality.Fluent => StreamLevel.Low,
        TileQuality.Balanced => StreamLevel.Mid,
        _ => demand.Focused || demand.ZoomHigh ? StreamLevel.High : StreamLevel.Low,
    };

    /// <summary>The quality a tile can actually have: Balanced only where offered.</summary>
    public TileQuality EffectiveQuality(TileQuality quality, CameraRef camera) =>
        quality == TileQuality.Balanced && !OffersBalanced(camera) ? TileQuality.Auto : quality;

    /// <summary>Whether Balanced is offered for <paramref name="camera"/>: a first lens with extern.</summary>
    public bool OffersBalanced(CameraRef camera) => camera.Lens == 0 && HasMiddleTier(camera);

    /// <summary>The tier a level means for <paramref name="camera"/>.</summary>
    public StreamTier TierFor(StreamLevel level, CameraRef camera)
    {
        if (camera.Lens > 0)
        {
            return level == StreamLevel.Low && !_noMobile.Contains(camera) ? StreamTier.Mobile : StreamTier.Extern;
        }
        return level switch
        {
            StreamLevel.Low => GridTier,
            StreamLevel.Mid => StreamTier.Extern,
            _ => FocusTier,
        };
    }

    /// <summary>
    /// A tile states what it asks of its camera: its choice, whether it is focused, and its zoom
    /// scale (1 when not zoomed). Auto's zoom hysteresis lives here. Idempotent. Returns the
    /// cameras whose wanted tier may have changed (this one, and the one the owner left).
    /// </summary>
    public IReadOnlyList<CameraRef> SetDemand(object owner, CameraRef camera, TileQuality quality, bool focused, double zoomScale)
    {
        bool had = _demands.TryGetValue(owner, out var was);
        bool wasHigh = had && was.Camera == camera && was.Demand.ZoomHigh;
        _demands[owner] = (camera, new Demand(quality, focused, ZoomHysteresis.Next(wasHigh, zoomScale)));
        return had && was.Camera != camera ? [camera, was.Camera] : [camera];
    }

    /// <summary>Withdraws everything <paramref name="owner"/> asked for (its tile went away); the camera to re-plan, if any.</summary>
    public CameraRef? ClearDemands(object owner) =>
        _demands.Remove(owner, out var old) ? old.Camera : null;

    public Demand? DemandOf(object owner) => _demands.TryGetValue(owner, out var d) ? d.Demand : null;

    /// <summary>
    /// A tier switch failed (tile_tier failed). For a second lens whose mobile failed for any
    /// reason but being superseded, the grid uses extern from now on. Returns whether to re-plan.
    /// </summary>
    public bool TierFailed(CameraRef camera, StreamTier? tier, string? reason)
    {
        if (camera.Lens == 0 || tier != StreamTier.Mobile || reason == "superseded")
        {
            return false;
        }
        _noMobile.Add(camera);
        return true;
    }

    /// <summary>
    /// A tile could not start its stream (tile_error). A second lens whose mobile start was refused
    /// uses extern from now on; returns true when its tile must be reopened on extern. Other
    /// failures are left to the device's reconnect handling.
    /// </summary>
    public bool StartFailed(CameraRef camera, StreamTier openedTier, string? reason)
    {
        if (camera.Lens == 0 || reason != "start_rejected" || openedTier != StreamTier.Mobile)
        {
            return false;
        }
        _noMobile.Add(camera);
        return true;
    }
}
