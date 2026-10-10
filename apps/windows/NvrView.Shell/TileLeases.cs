// One core tile per camera, shared by every window that shows it (docs/UX.md
// "Layouts": the same camera in several tiles and windows is streamed once).
//
// Windows lease the cameras their layouts show and give the leases back when
// they stop; the tile is opened with the first lease and closed with the
// last. Focus is counted the same way: a camera is focused while any window
// focuses it. Which stream tier a tile opens on and switches to is not
// decided here: the backend does it (Open gets whether the camera is
// focused; FocusChanged tells it when that changes), so the tier policy
// (grid and focus tiers, a second lens's own tiers, per-tile quality) can
// live in one place of the app.
//
// One thread (the UI thread in the app): opening and closing tiles must not
// happen inside a core callback.

using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>What the leases open, close, and tell about focus.</summary>
public interface ITileBackend<TTile> where TTile : class
{
    /// <summary>Opens a tile for the camera, on the tier it should show (<paramref name="focused"/>: some window focuses it); null while it cannot be opened (its device is not connected).</summary>
    TTile? Open(CameraRef camera, bool focused);

    /// <summary>Closes a tile the leases opened.</summary>
    void Close(CameraRef camera, TTile tile);

    /// <summary>The first window started focusing the camera, or the last stopped.</summary>
    void FocusChanged(CameraRef camera, TTile tile, bool focused);
}

/// <summary>Lease counting for shared tiles (see the file comment).</summary>
public sealed class TileLeases<TTile>(ITileBackend<TTile> backend) where TTile : class
{
    private sealed class Entry
    {
        public TTile? Tile;
        public int Leases;
        public int Focus;
    }

    private readonly Dictionary<CameraRef, Entry> _entries = [];

    /// <summary>Raised after a tile is opened or closed (the camera, and the tile or null).</summary>
    public event Action<CameraRef, TTile?>? TileChanged;

    /// <summary>Every camera with at least one lease.</summary>
    public IReadOnlyCollection<CameraRef> Cameras => _entries.Keys;

    /// <summary>The tile showing the camera, if it is open.</summary>
    public TTile? Tile(CameraRef camera) => _entries.GetValueOrDefault(camera)?.Tile;

    /// <summary>The camera a tile shows.</summary>
    public CameraRef? Camera(TTile tile)
    {
        foreach (KeyValuePair<CameraRef, Entry> kv in _entries)
        {
            if (ReferenceEquals(kv.Value.Tile, tile))
            {
                return kv.Key;
            }
        }
        return null;
    }

    public int LeaseCount(CameraRef camera) => _entries.GetValueOrDefault(camera)?.Leases ?? 0;

    public bool IsFocused(CameraRef camera) => (_entries.GetValueOrDefault(camera)?.Focus ?? 0) > 0;

    /// <summary>Takes a lease; opens the tile with the first. Returns the tile, null while it cannot be opened.</summary>
    public TTile? Acquire(CameraRef camera)
    {
        if (!_entries.TryGetValue(camera, out Entry? e))
        {
            e = new Entry();
            _entries[camera] = e;
        }
        e.Leases++;
        return Open(camera);
    }

    /// <summary>Gives a lease back; the last one closes the tile.</summary>
    public void Release(CameraRef camera)
    {
        if (!_entries.TryGetValue(camera, out Entry? e))
        {
            return;
        }
        e.Leases--;
        if (e.Leases > 0)
        {
            return;
        }
        _entries.Remove(camera);
        if (e.Tile is { } tile)
        {
            backend.Close(camera, tile);
            TileChanged?.Invoke(camera, null);
        }
    }

    /// <summary>Makes one owner's leases match <paramref name="wanted"/> where it held <paramref name="held"/>: acquires what is new first, then releases what went away (so a camera kept is never closed in between).</summary>
    public void Update(IReadOnlyDictionary<CameraRef, int> held, IReadOnlyDictionary<CameraRef, int> wanted)
    {
        foreach ((CameraRef camera, int n) in wanted)
        {
            for (int i = held.GetValueOrDefault(camera); i < n; i++)
            {
                Acquire(camera);
            }
        }
        foreach ((CameraRef camera, int n) in held)
        {
            for (int i = wanted.GetValueOrDefault(camera); i < n; i++)
            {
                Release(camera);
            }
        }
    }

    /// <summary>A window starts (or stops) focusing the camera.</summary>
    public void SetFocused(CameraRef camera, bool focused)
    {
        if (!_entries.TryGetValue(camera, out Entry? e))
        {
            return;
        }
        bool before = e.Focus > 0;
        e.Focus = Math.Max(e.Focus + (focused ? 1 : -1), 0);
        if (before != e.Focus > 0 && e.Tile is { } tile)
        {
            backend.FocusChanged(camera, tile, e.Focus > 0);
        }
    }

    /// <summary>Closes a device's tiles (it is being removed or reconnected); the leases stay, so <see cref="Reopen(string)"/> brings them back.</summary>
    public void CloseTiles(string deviceKey)
    {
        foreach ((CameraRef camera, Entry e) in _entries.ToArray())
        {
            if (camera.Device == deviceKey && e.Tile is { } tile)
            {
                e.Tile = null;
                backend.Close(camera, tile);
                TileChanged?.Invoke(camera, null);
            }
        }
    }

    /// <summary>Opens the leased tiles of a device that is (again) connected.</summary>
    public void Reopen(string deviceKey)
    {
        foreach (CameraRef camera in _entries.Keys.ToArray())
        {
            if (camera.Device == deviceKey)
            {
                Open(camera);
            }
        }
    }

    /// <summary>Closes and opens one camera's tile again (for a tier the device refused to start on).</summary>
    public TTile? Reopen(CameraRef camera)
    {
        if (!_entries.TryGetValue(camera, out Entry? e))
        {
            return null;
        }
        if (e.Tile is { } tile)
        {
            e.Tile = null;
            backend.Close(camera, tile);
            TileChanged?.Invoke(camera, null);
        }
        return Open(camera);
    }

    /// <summary>The core ended a tile on its own: forget it (the lease stays; <see cref="Reopen(string)"/> opens it again).</summary>
    public void TileEnded(TTile tile)
    {
        if (Camera(tile) is { } camera)
        {
            _entries[camera].Tile = null;
            backend.Close(camera, tile);
            TileChanged?.Invoke(camera, null);
        }
    }

    /// <summary>Closes every tile and forgets every lease.</summary>
    public void CloseAll()
    {
        foreach ((CameraRef camera, Entry e) in _entries.ToArray())
        {
            if (e.Tile is { } tile)
            {
                backend.Close(camera, tile);
                TileChanged?.Invoke(camera, null);
            }
        }
        _entries.Clear();
    }

    private TTile? Open(CameraRef camera)
    {
        Entry e = _entries[camera];
        if (e.Tile is { } open)
        {
            return open;
        }
        TTile? tile = backend.Open(camera, e.Focus > 0);
        if (tile is not null)
        {
            e.Tile = tile;
            TileChanged?.Invoke(camera, tile);
        }
        return tile;
    }
}
