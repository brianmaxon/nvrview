// What a window's render thread draws while the Recordings player shows a
// follow cut or a grid instead of the device playback (docs/UX.md
// "Recordings", "Follow and Grid"): the edit's open files and the edit's clock.
// Once per frame the render thread reads the position, asks the files for the
// pictures of that time (decoded off the thread, keyframes only while
// scrubbing and from 2x, the newest request winning), and draws them with the
// same renderer as the live tiles.

using NvrView.Video;

namespace NvrView.App;

/// <summary>An edit the render thread draws (see the file comment).</summary>
/// <param name="Files">The open files.</param>
/// <param name="Position">The edit's position now (device-local milliseconds); safe from any thread.</param>
/// <param name="KeyframesOnly">Whether only keyframes are decoded now.</param>
internal sealed class EditStage(EditFiles files, Func<long> position, Func<bool> keyframesOnly)
{
    /// <summary>Space around the edit's area, in DIPs.</summary>
    public const double Inset = 4;

    private long _frames;

    public EditFiles Files { get; } = files;

    public long Position() => position();

    public bool KeyframesOnly() => keyframesOnly();

    /// <summary>Frames drawn of this edit.</summary>
    public long Frames
    {
        get => Interlocked.Read(ref _frames);
        set => Interlocked.Exchange(ref _frames, value);
    }
}
