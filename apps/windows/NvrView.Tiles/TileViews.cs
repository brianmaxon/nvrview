// A window's picture settings per camera (docs/UX.md "Fill Tiles"): the
// window's Fill Tiles switch and, per camera, the zoom or crop position and a
// Fill choice of its own. The same rule and shared vectors
// (fixtures/synthetic/tile-views.json) as the macOS app's TileViews (NVRCore
// TileViews.swift).

namespace NvrView.Tiles;

/// <summary>How one camera's picture is shown in one window: its digital zoom (at 1x, where its filled crop sits) and its own Fill choice (null follows the window's).</summary>
public readonly record struct TileView(TileZoom Zoom, bool? Fill = null)
{
    public static readonly TileView Default = new(TileZoom.None);
}

/// <summary>
/// A window's picture settings per camera. Keyed by camera, so tiles of the same camera in one window share
/// them and they follow the camera through swaps and layout changes; another window has its own. Positions
/// are fractions of the picture, so the sub-stream and the main stream show the same part.
///
/// The rule for Fill: a tile fills if its camera has its own choice, else as the window says. A choice equal
/// to the window's is not kept, and setting the window's switch clears every camera's choice (all tiles
/// follow it). At most <see cref="Limit"/> cameras are kept; the one changed longest ago goes first. Pure.
/// </summary>
public sealed class TileViews<TKey> where TKey : notnull
{
    /// <summary>The most cameras one window keeps.</summary>
    public const int Limit = 256;

    private readonly Dictionary<TKey, TileView> _entries = [];
    private readonly List<TKey> _order = [];

    public TileViews(bool fill = false) => Fill = fill;

    /// <summary>Read back from saved entries (oldest first): the values clamped, entries with nothing to keep dropped, a camera listed twice taken from its later entry, and at most <see cref="Limit"/> kept (the newest).</summary>
    public TileViews(bool fill, IEnumerable<(TKey Key, TileView View)> saved)
    {
        Fill = fill;
        foreach ((TKey key, TileView view) in saved)
        {
            Put(key, new TileView(view.Zoom.Sanitize(), view.Fill == fill ? null : view.Fill));
        }
    }

    /// <summary>The window's Fill Tiles switch: what tiles without a choice of their own do.</summary>
    public bool Fill { get; private set; }

    /// <summary>The cameras kept, the one changed longest ago first.</summary>
    public IReadOnlyList<TKey> Order => _order;

    public int Count => _order.Count;

    /// <summary>The camera's zoom (1x and centered when nothing is kept).</summary>
    public TileZoom Zoom(TKey key) => _entries.TryGetValue(key, out TileView v) ? v.Zoom : TileZoom.None;

    /// <summary>Whether the camera's tiles fill: its own choice, else the window's.</summary>
    public bool Fills(TKey key) => FillChoice(key) ?? Fill;

    /// <summary>The camera's own Fill choice (null: it follows the window).</summary>
    public bool? FillChoice(TKey key) => _entries.TryGetValue(key, out TileView v) ? v.Fill : null;

    /// <summary>Some camera has a Fill choice of its own (the window's switch then shows mixed where the platform can).</summary>
    public bool Mixed => _entries.Values.Any(v => v.Fill is not null);

    public void SetZoom(TKey key, TileZoom zoom)
    {
        TileView v = _entries.TryGetValue(key, out TileView old) ? old : TileView.Default;
        Put(key, v with { Zoom = zoom });
    }

    /// <summary>The camera's tiles fill, or fit (a choice equal to the window's is not kept).</summary>
    public void SetFill(TKey key, bool fills)
    {
        TileView v = _entries.TryGetValue(key, out TileView old) ? old : TileView.Default;
        Put(key, v with { Fill = fills == Fill ? null : fills });
    }

    /// <summary>The window's switch: every tile follows it (the cameras' own choices are cleared; their positions stay).</summary>
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

    /// <summary>Forgets the camera (1x, centered, following the window).</summary>
    public void Remove(TKey key)
    {
        _entries.Remove(key);
        _order.Remove(key);
    }

    /// <summary>The kept cameras, the one changed longest ago first (what is saved).</summary>
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
