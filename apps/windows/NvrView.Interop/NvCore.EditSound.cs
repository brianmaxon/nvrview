// Edit lists and the rest of sound (ABI 3 minor 8; docs/FFI.md "Edit lists
// and sound (minor 8)", docs/FFI-RECORDINGS.md sections 7 and 2a):
// IEditSound, the calls the timeline and export views make, and NvCore's
// implementation over the C functions. Answers stay JSON text in the
// contract's shapes; the sound_watch and sound_analyze events are parsed by
// NvrView.Shell (RecordingsEvent), which also has a fake.

namespace NvrView.Interop;

/// <summary>
/// A live tile's recent loudness: one byte per second and value, oldest
/// first, from <see cref="StartSeconds"/> (a Unix second). 0 means no audio
/// was analyzed; 1 to 128 is -127 to 0 dBFS (<c>dBFS = byte - 128</c>).
/// </summary>
/// <param name="StartSeconds">The Unix second of the first value.</param>
/// <param name="Peak">Peak per second.</param>
/// <param name="Rms">RMS per second.</param>
public sealed record TileLoudness(long StartSeconds, byte[] Peak, byte[] Rms)
{
    /// <summary>Whole dBFS of a stored byte, or null when no audio was analyzed.</summary>
    public static int? Decibels(byte value) => value == 0 ? null : Math.Min(value, (byte)128) - 128;
}

/// <summary>
/// The edit-list and sound calls (minor 8), as the core and the fake answer
/// them. Errors throw <see cref="NvException"/> with the C function's
/// status. Not from inside a callback.
/// </summary>
public interface IEditSound
{
    /// <summary>
    /// <c>nv_edit_compute</c>: the edit list (follow cut or grid) of a
    /// finished download. <c>in_ms</c>, <c>out_ms</c>, and the trim kind
    /// throw <see cref="NvStatus.Unsupported"/> until export.
    /// </summary>
    string EditCompute(string json);

    /// <summary><c>nv_sound_watch</c>: keeps a camera's sub-stream open for sound only.</summary>
    void SoundWatch(string camera, bool enabled);

    /// <summary><c>nv_recordings_loudness</c>: loudness lanes for the timeline.</summary>
    string RecordingsLoudness(string query);

    /// <summary><c>nv_sound_labels_put</c>: labels the app's classifier produced.</summary>
    void SoundLabelsPut(string json);

    /// <summary><c>nv_sound_analyze_day</c>: asks for a recorded day's sound to be analyzed (<c>{"request":N,"state":"queued"}</c>; <c>sound_analyze</c> events follow).</summary>
    string SoundAnalyzeDay(string json);

    /// <summary><c>nv_sound_analyze_cancel</c>.</summary>
    void SoundAnalyzeCancel(ulong request);

    /// <summary><c>nv_tile_loudness</c>: a live tile's newest <paramref name="capacity"/> seconds of loudness.</summary>
    TileLoudness GetTileLoudness(ulong tile, int capacity);
}

public sealed unsafe partial class NvCore : IEditSound
{
    /// <summary>Most seconds one <see cref="TileLoudness"/> call asks for (the core keeps at most 3600).</summary>
    public const int MaxTileLoudness = 3600;

    /// <inheritdoc/>
    public string EditCompute(string json) =>
        Answer((c, b, n, l) => NvNative.nv_edit_compute(c, json, b, n, l), nameof(NvNative.nv_edit_compute));

    /// <inheritdoc/>
    public void SoundWatch(string camera, bool enabled) =>
        Call(c => NvNative.nv_sound_watch(c, camera, enabled), nameof(NvNative.nv_sound_watch));

    /// <inheritdoc/>
    public string RecordingsLoudness(string query) =>
        Answer((c, b, n, l) => NvNative.nv_recordings_loudness(c, query, b, n, l), nameof(NvNative.nv_recordings_loudness));

    /// <inheritdoc/>
    public void SoundLabelsPut(string json) =>
        Call(c => NvNative.nv_sound_labels_put(c, json), nameof(NvNative.nv_sound_labels_put));

    /// <inheritdoc/>
    public string SoundAnalyzeDay(string json) =>
        Once((c, b, n, l) => NvNative.nv_sound_analyze_day(c, json, b, n, l), nameof(NvNative.nv_sound_analyze_day));

    /// <inheritdoc/>
    public void SoundAnalyzeCancel(ulong request) =>
        Call(c => NvNative.nv_sound_analyze_cancel(c, request), nameof(NvNative.nv_sound_analyze_cancel));

    /// <inheritdoc/>
    public TileLoudness GetTileLoudness(ulong tile, int capacity)
    {
        NotInCallback(nameof(NvNative.nv_tile_loudness));
        int cap = Math.Clamp(capacity, 0, MaxTileLoudness);
        byte[] peak = new byte[cap];
        byte[] rms = new byte[cap];
        long start = 0;
        nuint count = 0;
        using (AcquireLease(out nint core))
        {
            fixed (byte* p = peak)
            fixed (byte* r = rms)
            {
                Check(
                    NvNative.nv_tile_loudness(core, tile, p, r, (nuint)cap, &start, &count),
                    nameof(NvNative.nv_tile_loudness));
            }
        }
        int n = checked((int)count);
        return new TileLoudness(start, peak[..n], rms[..n]);
    }
}
