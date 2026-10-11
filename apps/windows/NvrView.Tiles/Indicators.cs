// Corner indicators (docs/UX.md "Corner indicators"): small symbols for what
// is being detected now, always visible. Pure pieces; the app draws them.

namespace NvrView.Tiles;

/// <summary>Which corner of a tile holds the indicators (a setting; top right by default).</summary>
public enum ChromeCorner
{
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

public static class ChromeCorners
{
    public const ChromeCorner Default = ChromeCorner.TopRight;

    public static string Title(this ChromeCorner c) => c switch
    {
        ChromeCorner.TopLeft => "Top left",
        ChromeCorner.TopRight => "Top right",
        ChromeCorner.BottomLeft => "Bottom left",
        _ => "Bottom right",
    };

    /// <summary>The stored name (camelCase); anything missing or unknown reads as the default.</summary>
    public static string Name(this ChromeCorner c) => c switch
    {
        ChromeCorner.TopLeft => "topLeft",
        ChromeCorner.TopRight => "topRight",
        ChromeCorner.BottomLeft => "bottomLeft",
        _ => "bottomRight",
    };

    public static ChromeCorner Parse(string? name) => name switch
    {
        "topLeft" => ChromeCorner.TopLeft,
        "topRight" => ChromeCorner.TopRight,
        "bottomLeft" => ChromeCorner.BottomLeft,
        "bottomRight" => ChromeCorner.BottomRight,
        _ => Default,
    };

    public static bool IsLeft(this ChromeCorner c) => c is ChromeCorner.TopLeft or ChromeCorner.BottomLeft;

    public static bool IsTop(this ChromeCorner c) => c is ChromeCorner.TopLeft or ChromeCorner.TopRight;

    /// <summary>The diagonally opposite corner (where call cards go by default).</summary>
    public static ChromeCorner Opposite(this ChromeCorner c) => c switch
    {
        ChromeCorner.TopLeft => ChromeCorner.BottomRight,
        ChromeCorner.TopRight => ChromeCorner.BottomLeft,
        ChromeCorner.BottomLeft => ChromeCorner.TopRight,
        _ => ChromeCorner.TopLeft,
    };

    /// <summary>
    /// Where a row of <paramref name="count"/> symbols of <paramref name="symbol"/> (with
    /// <paramref name="gap"/> between and <paramref name="inset"/> from the edges) starts in a
    /// top-left-origin tile; the row grows away from the corner's edge.
    /// </summary>
    public static (double X, double Y) RowOrigin(this ChromeCorner c, int count, double symbol, double gap, double inset, double width, double height)
    {
        double row = count * symbol + Math.Max(count - 1, 0) * gap;
        double x = c.IsLeft() ? inset : width - inset - row;
        double y = c.IsTop() ? inset : height - inset - symbol;
        return (x, y);
    }
}

/// <summary>What an indicator stands for.</summary>
public enum IndicatorKind
{
    Motion,
    Person,
    Vehicle,
    Animal,
    Face,
    Package,
    Visitor,
    SmokeAlarm,
    CoAlarm,
    Other,
}

public static class IndicatorKinds
{
    public static IndicatorKind FromDetection(string kind) => kind switch
    {
        DetectionEvent.MotionKind => IndicatorKind.Motion,
        "people" or "person" => IndicatorKind.Person,
        "vehicle" => IndicatorKind.Vehicle,
        "dog_cat" or "animal" => IndicatorKind.Animal,
        "face" => IndicatorKind.Face,
        "package" => IndicatorKind.Package,
        "visitor" => IndicatorKind.Visitor,
        "smoke_alarm" => IndicatorKind.SmokeAlarm,
        "co_alarm" => IndicatorKind.CoAlarm,
        _ => IndicatorKind.Other,
    };

    public static string Label(this IndicatorKind k) => k switch
    {
        IndicatorKind.Motion => "Motion",
        IndicatorKind.Person => "Person",
        IndicatorKind.Vehicle => "Vehicle",
        IndicatorKind.Animal => "Animal",
        IndicatorKind.Face => "Face",
        IndicatorKind.Package => "Package",
        IndicatorKind.Visitor => "Visitor",
        IndicatorKind.SmokeAlarm => "Smoke alarm",
        IndicatorKind.CoAlarm => "CO alarm",
        _ => "Detection",
    };

    /// <summary>Sound alarms get the restrained amber accent; everything else is monochrome.</summary>
    public static bool IsSound(this IndicatorKind k) => k is IndicatorKind.SmokeAlarm or IndicatorKind.CoAlarm;

    /// <summary>Every kind in display order.</summary>
    public static IReadOnlyList<IndicatorKind> All { get; } = Enum.GetValues<IndicatorKind>();
}

/// <summary>One symbol to show: a kind and where it was detected, in words.</summary>
/// <param name="Kind">What.</param>
/// <param name="Source">"camera AI", "this PC", "sound"; several joined with " and ".</param>
public readonly record struct IndicatorItem(IndicatorKind Kind, string Source)
{
    /// <summary>The tooltip and accessibility name, such as "Person (camera AI)".</summary>
    public string Description => $"{Kind.Label()} ({Source})";

    /// <summary>The kinds active for one camera, one item per kind in a fixed order. Empty never means "all clear".</summary>
    public static List<IndicatorItem> From(IEnumerable<DetectionTracker.Active> active)
    {
        var sources = new Dictionary<IndicatorKind, List<string>>();
        foreach (DetectionTracker.Active a in active)
        {
            IndicatorKind kind = IndicatorKinds.FromDetection(a.Kind);
            string s = DetectionSource.Label(a.Source);
            if (!sources.TryGetValue(kind, out List<string>? list))
            {
                sources[kind] = list = [];
            }
            if (!list.Contains(s))
            {
                list.Add(s);
            }
        }
        return Ordered(sources);
    }

    /// <summary>One item per kind across several lists (several lenses or cameras), sources joined.</summary>
    public static List<IndicatorItem> Union(IEnumerable<IReadOnlyList<IndicatorItem>> lists)
    {
        var sources = new Dictionary<IndicatorKind, List<string>>();
        foreach (IndicatorItem item in lists.SelectMany(l => l))
        {
            foreach (string s in item.Source.Split(" and "))
            {
                if (!sources.TryGetValue(item.Kind, out List<string>? list))
                {
                    sources[item.Kind] = list = [];
                }
                if (!list.Contains(s))
                {
                    list.Add(s);
                }
            }
        }
        return Ordered(sources);
    }

    private static List<IndicatorItem> Ordered(Dictionary<IndicatorKind, List<string>> sources) =>
        [.. IndicatorKinds.All.Where(sources.ContainsKey).Select(k => new IndicatorItem(k, string.Join(" and ", sources[k])))];

    public static bool Same(IReadOnlyList<IndicatorItem> a, IReadOnlyList<IndicatorItem> b) => a.SequenceEqual(b);
}

/// <summary>Keeps a kind showing for a short while after it stops, so brief gaps do not flicker.</summary>
public sealed class IndicatorHold
{
    public const double HoldSeconds = 2.0;

    private sealed class Entry(IndicatorItem item)
    {
        public IndicatorItem Item = item;
        public bool Active = true;
        public double StoppedAt;
    }

    private readonly Dictionary<IndicatorKind, Entry> _entries = [];

    public bool IsEmpty => _entries.Count == 0;

    /// <summary>Folds in what is active at <paramref name="now"/>.</summary>
    public void Update(IReadOnlyList<IndicatorItem> active, double now)
    {
        var live = new HashSet<IndicatorKind>();
        foreach (IndicatorItem item in active)
        {
            live.Add(item.Kind);
            _entries[item.Kind] = new Entry(item);
        }
        foreach ((IndicatorKind kind, Entry e) in _entries.ToArray())
        {
            if (live.Contains(kind))
            {
                continue;
            }
            if (e.Active)
            {
                e.Active = false;
                e.StoppedAt = now;
            }
            if (now - e.StoppedAt >= HoldSeconds)
            {
                _entries.Remove(kind);
            }
        }
    }

    /// <summary>What to show at <paramref name="now"/>: active kinds and those stopped under the hold.</summary>
    public List<IndicatorItem> Visible(double now) =>
        [.. IndicatorKinds.All.Where(k => _entries.TryGetValue(k, out Entry? e) && (e.Active || now - e.StoppedAt < HoldSeconds))
            .Select(k => _entries[k].Item)];

    /// <summary>When the next held kind runs out, so a timer can re-evaluate.</summary>
    public double? NextExpiry(double now) =>
        _entries.Values.Where(e => !e.Active).Select(e => e.StoppedAt + HoldSeconds).Where(t => t > now).Cast<double?>().Min();
}

/// <summary>
/// What every tile and sidebar row shows: per camera, the active kinds with the
/// hold. Updated on detection events and a slow tick, never per frame.
/// </summary>
public sealed class IndicatorBoard
{
    private readonly Dictionary<CameraRef, IndicatorHold> _holds = [];
    private Dictionary<CameraRef, List<IndicatorItem>> _shown = [];

    /// <summary>Re-reads the tracker at <paramref name="now"/>; returns whether anything shown changed.</summary>
    public bool Update(DetectionTracker tracker, double now)
    {
        var before = _shown;
        var live = new HashSet<CameraRef>(tracker.ActiveKeys);
        foreach (CameraRef c in tracker.SoundKeys)
        {
            for (int lens = 0; lens <= CameraRef.MaxLens; lens++)
            {
                live.Add(c with { Lens = lens });
            }
        }
        var next = new Dictionary<CameraRef, List<IndicatorItem>>();
        foreach (CameraRef camera in live.Union(_holds.Keys).ToArray())
        {
            if (!_holds.TryGetValue(camera, out IndicatorHold? hold))
            {
                _holds[camera] = hold = new IndicatorHold();
            }
            hold.Update(IndicatorItem.From(tracker.For(camera)), now);
            List<IndicatorItem> visible = hold.Visible(now);
            if (visible.Count == 0 && !live.Contains(camera))
            {
                _holds.Remove(camera);
            }
            if (visible.Count > 0)
            {
                next[camera] = visible;
            }
        }
        _shown = next;
        return !SameShown(before, next);
    }

    private static bool SameShown(Dictionary<CameraRef, List<IndicatorItem>> a, Dictionary<CameraRef, List<IndicatorItem>> b) =>
        a.Count == b.Count && a.All(kv => b.TryGetValue(kv.Key, out List<IndicatorItem>? o) && IndicatorItem.Same(kv.Value, o));

    /// <summary>One camera (one lens).</summary>
    public IReadOnlyList<IndicatorItem> Items(CameraRef camera) => _shown.TryGetValue(camera, out var l) ? l : [];

    /// <summary>A multi-lens channel's row summarizes all its lenses.</summary>
    public IReadOnlyList<IndicatorItem> Items(string device, int channel) =>
        IndicatorItem.Union(Enumerable.Range(0, CameraRef.MaxLens + 1).Select(lens => Items(new CameraRef(device, channel, lens))));

    /// <summary>A collapsed device row: the union of its cameras' kinds.</summary>
    public IReadOnlyList<IndicatorItem> Items(string device) =>
        IndicatorItem.Union(_shown.Where(kv => kv.Key.Device == device).Select(kv => (IReadOnlyList<IndicatorItem>)kv.Value));

    /// <summary>When the next held kind runs out (for a timer), or null.</summary>
    public double? NextExpiry(double now) => _holds.Values.Select(h => h.NextExpiry(now)).Min();
}
