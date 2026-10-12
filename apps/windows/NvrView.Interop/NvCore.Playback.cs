// Recorded playback (ABI 3 minor 6; docs/FFI.md "Recorded playback (minor
// 6)", docs/FFI-RECORDINGS.md section 6): IPlayback, the calls the playback
// views make, and NvCore's implementation over the C functions. A
// playback's tiles are tiles: their actions arrive at OnAction like a live
// tile's and are answered the same way; CloseTile refuses them. The events
// are parsed by NvrView.Shell (PlaybackEvent), which also has a fake.

namespace NvrView.Interop;

/// <summary>What <see cref="IPlayback.PlaybackOpen"/> made: the playback and one tile per camera, in request order.</summary>
/// <param name="Playback">The playback's ID.</param>
/// <param name="Tiles">Its tiles.</param>
public sealed record OpenedPlayback(ulong Playback, IReadOnlyList<ulong> Tiles);

/// <summary>
/// The playback calls (minor 6), as the core and the fake answer them. Not
/// from inside a callback, except <see cref="PlaybackPosition"/>. Errors
/// throw <see cref="NvException"/> with the C function's status.
/// </summary>
public interface IPlayback
{
    /// <summary><c>nv_playback_open</c>: opened paused at the request's position (JSON of section 6).</summary>
    OpenedPlayback PlaybackOpen(string json);

    /// <summary><c>nv_playback_add_camera</c>: joins at the current position; returns its tile.</summary>
    ulong PlaybackAddCamera(ulong playback, string json);

    /// <summary><c>nv_playback_remove_camera</c>: its tile closes (<c>tile_closed</c>).</summary>
    void PlaybackRemoveCamera(ulong playback, ulong tile);

    /// <summary><c>nv_playback_play</c>.</summary>
    void PlaybackPlay(ulong playback);

    /// <summary><c>nv_playback_pause</c>.</summary>
    void PlaybackPause(ulong playback);

    /// <summary><c>nv_playback_seek</c>: returns at once; <c>playback_seek</c> events follow.</summary>
    void PlaybackSeek(ulong playback, long positionMs, NvSeekMode mode);

    /// <summary><c>nv_playback_set_rate</c>: 0.25, 0.5, 1, 2, 4, 8, or 16.</summary>
    void PlaybackSetRate(ulong playback, double rate);

    /// <summary><c>nv_playback_set_offset</c>: this camera shows position + offsetMs.</summary>
    void PlaybackSetOffset(ulong playback, ulong tile, long offsetMs);

    /// <summary><c>nv_playback_position</c>: for the scrubber; any thread, also inside callbacks.</summary>
    (long PositionMs, NvPlaybackState State) PlaybackPosition(ulong playback);

    /// <summary><c>nv_playback_status</c>: the playback's details (JSON).</summary>
    string PlaybackStatus(ulong playback);

    /// <summary><c>nv_playback_close</c>: every tile closes, then <c>playback_closed</c>.</summary>
    void PlaybackClose(ulong playback);
}

public sealed unsafe partial class NvCore : IPlayback
{
    /// <summary>Cameras of one playback at most (4 tile entries always fit).</summary>
    public const int MaxPlaybackCameras = 4;

    /// <inheritdoc/>
    public OpenedPlayback PlaybackOpen(string json)
    {
        NotInCallback(nameof(NvNative.nv_playback_open));
        ulong playback = 0;
        ulong* tiles = stackalloc ulong[MaxPlaybackCameras];
        nuint count = 0;
        using (AcquireLease(out nint core))
        {
            Check(
                NvNative.nv_playback_open(core, json, &playback, tiles, MaxPlaybackCameras, &count),
                nameof(NvNative.nv_playback_open));
        }
        var list = new ulong[checked((int)count)];
        for (int i = 0; i < list.Length; i++)
        {
            list[i] = tiles[i];
        }
        return new OpenedPlayback(playback, list);
    }

    /// <inheritdoc/>
    public ulong PlaybackAddCamera(ulong playback, string json)
    {
        NotInCallback(nameof(NvNative.nv_playback_add_camera));
        ulong tile = 0;
        using (AcquireLease(out nint core))
        {
            Check(NvNative.nv_playback_add_camera(core, playback, json, &tile), nameof(NvNative.nv_playback_add_camera));
        }
        return tile;
    }

    /// <inheritdoc/>
    public void PlaybackRemoveCamera(ulong playback, ulong tile) =>
        Call(c => NvNative.nv_playback_remove_camera(c, playback, tile), nameof(NvNative.nv_playback_remove_camera));

    /// <inheritdoc/>
    public void PlaybackPlay(ulong playback) =>
        Call(c => NvNative.nv_playback_play(c, playback), nameof(NvNative.nv_playback_play));

    /// <inheritdoc/>
    public void PlaybackPause(ulong playback) =>
        Call(c => NvNative.nv_playback_pause(c, playback), nameof(NvNative.nv_playback_pause));

    /// <inheritdoc/>
    public void PlaybackSeek(ulong playback, long positionMs, NvSeekMode mode) =>
        Call(c => NvNative.nv_playback_seek(c, playback, positionMs, mode), nameof(NvNative.nv_playback_seek));

    /// <inheritdoc/>
    public void PlaybackSetRate(ulong playback, double rate) =>
        Call(c => NvNative.nv_playback_set_rate(c, playback, rate), nameof(NvNative.nv_playback_set_rate));

    /// <inheritdoc/>
    public void PlaybackSetOffset(ulong playback, ulong tile, long offsetMs) =>
        Call(c => NvNative.nv_playback_set_offset(c, playback, tile, offsetMs), nameof(NvNative.nv_playback_set_offset));

    /// <inheritdoc/>
    public (long PositionMs, NvPlaybackState State) PlaybackPosition(ulong playback)
    {
        // As TileAudioLevel: no lease, so a callback may call it.
        nint core = Volatile.Read(ref _dispatcher.Raw);
        if (core == 0)
        {
            throw new NvException(NvStatus.Closed, nameof(NvNative.nv_playback_position));
        }
        long position = 0;
        NvPlaybackState state = NvPlaybackState.Failed;
        Check(NvNative.nv_playback_position(core, playback, &position, &state), nameof(NvNative.nv_playback_position));
        return (position, state);
    }

    /// <inheritdoc/>
    public string PlaybackStatus(ulong playback) =>
        Answer((c, b, n, l) => NvNative.nv_playback_status(c, playback, b, n, l), nameof(NvNative.nv_playback_status));

    /// <inheritdoc/>
    public void PlaybackClose(ulong playback) =>
        Call(c => NvNative.nv_playback_close(c, playback), nameof(NvNative.nv_playback_close));
}
