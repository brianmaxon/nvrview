// A window's picture settings per tile (docs/UX.md "Digital zoom", "Fill Tiles"): the window's Fill Tiles
// switch and, per tile, the zoom or crop position and a Fill choice of its own. The same rule and shared
// vectors (fixtures/synthetic/tile-views.json) as the macOS app's TileViews (NVRCore TileViews.swift).

namespace NvrView.Tiles;

/// <summary>How one tile's picture is shown: its digital zoom (at 1x, where its filled crop sits) and its own Fill choice (null follows the window's).</summary>
public readonly record struct TileView(TileZoom Zoom, bool? Fill = null)
{
    public static readonly TileView Default = new(TileZoom.None);
}

/// <summary>
/// A window's picture settings per key. The app keys it by tile (<see cref="TileViewKey"/>: the layout, the tile,
/// and the camera the tile shows), so two tiles of one camera are independent views; another window has its
/// own. Positions are fractions of the picture, so the sub-stream and the main stream show the same part.
///
/// The rule for Fill: a tile fills if it has its own choice, else as the window says. A choice equal to the
/// window's is not kept, and setting the window's switch clears every tile's choice (all tiles follow it). At
/// most <see cref="Limit"/> keys are kept; the one changed longest ago goes first. Pure.
/// </summary>
public sealed class TileViews<TKey> where TKey : notnull
{
    /// <summary>The most tiles one window keeps.</summary>
    public const int Limit = 256;

    private readonly Dictionary<TKey, TileView> _entries = [];
    private readonly List<TKey> _order = [];

    public TileViews(bool fill = false) => Fill = fill;

    /// <summary>Read back from saved entries (oldest first): the values clamped, entries with nothing to keep dropped, a key listed twice taken from its later entry, and at most <see cref="Limit"/> kept (the newest).</summary>
    public TileViews(bool fill, IEnumerable<(TKey Key, TileView View)> saved)
    {
        Fill = fill;
        foreach ((TKey key, TileView view) in saved)
        {
            Restore(key, view);
        }
    }

    /// <summary>The window's Fill Tiles switch: what tiles without a choice of their own do.</summary>
    public bool Fill { get; private set; }

    /// <summary>The keys kept, the one changed longest ago first.</summary>
    public IReadOnlyList<TKey> Order => _order;

    public int Count => _order.Count;

    /// <summary>The key's zoom (1x and centered when nothing is kept).</summary>
    public TileZoom Zoom(TKey key) => _entries.TryGetValue(key, out TileView v) ? v.Zoom : TileZoom.None;

    /// <summary>Whether the key's tile fills: its own choice, else the window's.</summary>
    public bool Fills(TKey key) => FillChoice(key) ?? Fill;

    /// <summary>The key's own Fill choice (null: it follows the window).</summary>
    public bool? FillChoice(TKey key) => _entries.TryGetValue(key, out TileView v) ? v.Fill : null;

    /// <summary>Some key has a Fill choice of its own (the window's switch then shows mixed where the platform can).</summary>
    public bool Mixed => _entries.Values.Any(v => v.Fill is not null);

    /// <summary>Some key that <paramref name="which"/> picks has a Fill choice of its own.</summary>
    public bool MixedWhere(Func<TKey, bool> which) => _entries.Any(kv => kv.Value.Fill is not null && which(kv.Key));

    public void SetZoom(TKey key, TileZoom zoom)
    {
        TileView v = _entries.TryGetValue(key, out TileView old) ? old : TileView.Default;
        Put(key, v with { Zoom = zoom });
    }

    /// <summary>The key's tile fills, or fits (a choice equal to the window's is not kept).</summary>
    public void SetFill(TKey key, bool fills)
    {
        TileView v = _entries.TryGetValue(key, out TileView old) ? old : TileView.Default;
        Put(key, v with { Fill = fills == Fill ? null : fills });
    }

    /// <summary>The window's switch: every tile follows it (the tiles' own choices are cleared; their positions stay).</summary>
    public void SetWindowFill(bool fills)
    {
        Fill = fills;
        foreach (TKey key in _order.ToList())
        {
            TileView v = _entries[key];
            if (v.Fill is null)
            {
                continue;
            }
            if (v.Zoom.IsDefault)
            {
                _entries.Remove(key);
                _order.Remove(key);
            }
            else
            {
                _entries[key] = v with { Fill = null };
            }
        }
    }

    /// <summary>Forgets the key (1x, centered, following the window).</summary>
    public void Remove(TKey key)
    {
        _entries.Remove(key);
        _order.Remove(key);
    }

    /// <summary>Forgets every key <paramref name="drop"/> picks.</summary>
    public void RemoveAll(Func<TKey, bool> drop)
    {
        foreach (TKey key in _order.Where(drop).ToList())
        {
            Remove(key);
        }
    }

    /// <summary>Moves views to new keys at once (a key moved away is left empty unless another moves onto it). The moved views become the newest.</summary>
    public void Move(IEnumerable<(TKey From, TKey To)> moves)
    {
        var taken = moves.Select(m => (View: _entries.TryGetValue(m.From, out TileView v) ? v : (TileView?)null, m.From, m.To)).ToList();
        foreach (var m in taken)
        {
            Remove(m.From);
        }
        foreach (var m in taken)
        {
            Remove(m.To);
            if (m.View is { } view)
            {
                Put(m.To, view);
            }
        }
    }

    /// <summary>Sets a key's whole view as read or migrated: clamped, and the choice kept only where it differs from the window's.</summary>
    public void Restore(TKey key, TileView view) =>
        Put(key, new TileView(view.Zoom.Sanitize(), view.Fill == Fill ? null : view.Fill));

    /// <summary>The kept keys, the one changed longest ago first (what is saved).</summary>
    public IEnumerable<(TKey Key, TileView View)> Saved => _order.Select(k => (k, _entries[k]));

    private void Put(TKey key, TileView v)
    {
        _order.Remove(key);
        if (v.Zoom.IsDefault && v.Fill is null)
        {
            _entries.Remove(key);
            return;
        }
        _entries[key] = v;
        _order.Add(key);
        while (_order.Count > Limit)
        {
            _entries.Remove(_order[0]);
            _order.RemoveAt(0);
        }
    }
}

/// <summary>
/// Which tile a view belongs to: the window's content (<see cref="Layout"/>, named as <see cref="LayoutKey(string)"/>
/// and the others do), the tile's identity in that layout (the leaf's "tile" member, <see cref="TileIds"/>), and the
/// camera the tile shows. A tile given another camera has a new key, so its view starts at the default; a swap moves
/// each view with its camera. The macOS app's TileKey (named apart from the keyboard's TileKey here).
/// </summary>
public readonly record struct TileViewKey(string Layout, string Tile, CameraRef Camera)
{
    /// <summary>The views key of a stored layout.</summary>
    public static string LayoutKey(string id) => "layout:" + id;

    /// <summary>The views key of a group's grid.</summary>
    public static string GroupKey(string id) => "group:" + id;

    /// <summary>The views key of the window's unsaved arrangement.</summary>
    public const string ScratchKey = "scratch";

    public override string ToString() => $"{Layout}#{Tile}@{Camera}";
}

/// <summary>Tile identities (the layout leaf's "tile" member), as the macOS app gives them.</summary>
public static class TileIds
{
    /// <summary>The leaf member holding the tile's identity.</summary>
    public const string Member = "tile";

    /// <summary>Whether a stored identity can be used.</summary>
    public static bool Valid(string? id) => !string.IsNullOrEmpty(id) && id.Length <= 64;

    /// <summary>
    /// Identities for tiles in reading order, given the ones they have (null: none, or one that cannot be used):
    /// each valid identity is kept the first time it appears; every other tile gets <c>t&lt;n&gt;</c> for its place
    /// n in reading order, or else the first free <c>t&lt;n&gt;.&lt;k&gt;</c> (k = 1, 2, ...). The same input gives
    /// the same identities, so a layout stored before tiles had identities reads the same way on every launch.
    /// Shared vectors: "tileIDs" in fixtures/synthetic/tile-views.json.
    /// </summary>
    public static List<string> Assign(IReadOnlyList<string?> have)
    {
        var used = new HashSet<string>(StringComparer.Ordinal);
        var kept = new List<string?>(have.Count);
        foreach (string? id in have)
        {
            if (Valid(id) && used.Add(id!))
            {
                kept.Add(id);
            }
            else
            {
                kept.Add(null);
            }
        }
        var result = new List<string>(have.Count);
        for (int i = 0; i < kept.Count; i++)
        {
            if (kept[i] is { } id)
            {
                result.Add(id);
                continue;
            }
            string candidate = $"t{i}";
            for (int k = 1; used.Contains(candidate); k++)
            {
                candidate = $"t{i}.{k}";
            }
            used.Add(candidate);
            result.Add(candidate);
        }
        return result;
    }
}

/// <summary>What the app does with a window's views keyed by tile.</summary>
public static class TileKeyViews
{
    /// <summary>The window's content took another name (an unsaved arrangement saved as a layout, a group edited into the window's own arrangement): its views go along, replacing any the new name had.</summary>
    public static void Rename(this TileViews<TileViewKey> views, string from, string to)
    {
        if (from == to)
        {
            return;
        }
        views.RemoveAll(k => k.Layout == to);
        views.Move(views.Order.Where(k => k.Layout == from).Select(k => (k, k with { Layout = to })).ToList());
    }

    /// <summary>Whether a tile of <paramref name="layout"/> has a Fill choice of its own.</summary>
    public static bool MixedIn(this TileViews<TileViewKey> views, string layout) => views.MixedWhere(k => k.Layout == layout);
}
