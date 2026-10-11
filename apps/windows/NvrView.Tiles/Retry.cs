// The Retry button of a tile that has shown no picture for a long time (docs/UX.md, "Tiles without
// a picture"): which messages a retry can help, and when the button appears. Pure rules on the
// viewer clock (seconds), so tests and the self-test step time by hand.

namespace NvrView.Tiles;

/// <summary>What a tile's status line says, as far as a retry is concerned.</summary>
public enum TileMessage
{
    /// <summary>No message: the tile plays, or shows a picture.</summary>
    None,
    Connecting,
    Reconnecting,
    /// <summary>The device is connected but the stream has not delivered a picture.</summary>
    Starting,
    /// <summary>The device answered the start with 400 and the core is waiting for its stream.</summary>
    WaitingForCamera,
    /// <summary>The device refused this stream.</summary>
    Refused,
    Recovering,
    /// <summary>The device refused the login: final, to protect the account.</summary>
    LoginFailed,
    NoPassword,
    InvalidAddress,
    Disconnected,
    DeviceRemoved,
    CameraOffline,
    /// <summary>The stream cannot be decoded here (or needs a codec pack).</summary>
    Undecodable,
}

public static class TileRetryRule
{
    /// <summary>How long a tile must have had a message a retry could help before the button shows.</summary>
    public const double OfferAfter = 30;

    /// <summary>
    /// Whether a retry can help with this message. Never for a refused login (final), a missing
    /// password or invalid address (the user must fix them), a closed or removed device, an offline
    /// camera, or a stream that cannot be decoded.
    /// </summary>
    public static bool Applies(TileMessage message) =>
        message is TileMessage.Connecting or TileMessage.Reconnecting or TileMessage.Starting
            or TileMessage.WaitingForCamera or TileMessage.Refused or TileMessage.Recovering;
}

/// <summary>How long each tile has had a message a retry could help.</summary>
public sealed class TileRetryWatch<TKey> where TKey : notnull
{
    private readonly Dictionary<TKey, double> _since = [];

    /// <summary>Notes the tile's message at <paramref name="now"/> and says whether the button shows.</summary>
    public bool Offered(TKey tile, TileMessage message, double now)
    {
        if (!TileRetryRule.Applies(message))
        {
            _since.Remove(tile);
            return false;
        }
        if (!_since.TryGetValue(tile, out double start))
        {
            start = now;
            _since[tile] = start;
        }
        return now - start >= TileRetryRule.OfferAfter;
    }

    /// <summary>The user retried: the delay starts again.</summary>
    public void Restart(TKey tile, double now)
    {
        if (_since.ContainsKey(tile))
        {
            _since[tile] = now;
        }
    }

    /// <summary>Forgets tiles that are no longer on screen.</summary>
    public void Keep(IEnumerable<TKey> tiles)
    {
        var keep = new HashSet<TKey>(tiles);
        foreach (TKey k in _since.Keys.Where(k => !keep.Contains(k)).ToList())
        {
            _since.Remove(k);
        }
    }
}
