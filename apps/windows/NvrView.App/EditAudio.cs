// The follow cut's and grids' sound (docs/UX.md "Recordings", "Follow and
// Grid"): one Windows MediaPlayer per source file, the audible one playing at
// the shown position and kept within a quarter second of the picture's clock,
// the others paused. A file without sound plays nothing. The player's own
// mute and volume are applied by the caller (a muted edit has no audible
// source).

using NvrView.Shell;
using Windows.Media.Core;
using Windows.Media.Playback;

namespace NvrView.App;

/// <summary>The edit's sound through Windows MediaPlayer (see the file comment).</summary>
internal sealed class EditAudio : IEditAudio
{
    /// <summary>How far the sound may drift from the picture before it is moved.</summary>
    public const long MaxDriftMs = 250;

    private readonly Dictionary<int, MediaPlayer> _players = [];

    /// <inheritdoc/>
    public int? Active { get; private set; }

    /// <inheritdoc/>
    public void Update(int? source, string? file, long? filePositionMs, bool playing, float volume)
    {
        if (Active != source && Active is { } old && _players.TryGetValue(old, out MediaPlayer? previous))
        {
            previous.Pause();
        }
        Active = source;
        if (source is not { } s || file is null || filePositionMs is not { } position || !playing)
        {
            if (Active is { } a && _players.TryGetValue(a, out MediaPlayer? current))
            {
                current.Pause();
            }
            return;
        }
        try
        {
            if (!_players.TryGetValue(s, out MediaPlayer? player))
            {
                player = new MediaPlayer { AutoPlay = false, Source = MediaSource.CreateFromUri(new Uri(file)) };
                _players[s] = player;
            }
            player.Volume = Math.Clamp(volume, 0, 1);
            MediaPlaybackSession session = player.PlaybackSession;
            double at = session.Position.TotalMilliseconds;
            if (session.PlaybackState != MediaPlaybackState.Playing || Math.Abs(at - position) > MaxDriftMs)
            {
                session.Position = TimeSpan.FromMilliseconds(position);
                player.Play();
            }
        }
        catch (Exception e) when (e is System.Runtime.InteropServices.COMException or UriFormatException)
        {
            // A file the system cannot play stays silent; the picture does not depend on it.
            AppLog.Limited("edit-audio", 3, $"the edit's sound could not play {System.IO.Path.GetFileName(file)}: {e.Message}");
        }
    }

    /// <inheritdoc/>
    public void Close()
    {
        foreach (MediaPlayer p in _players.Values)
        {
            p.Pause();
            p.Dispose();
        }
        _players.Clear();
        Active = null;
    }

    /// <inheritdoc/>
    public void Dispose() => Close();
}
