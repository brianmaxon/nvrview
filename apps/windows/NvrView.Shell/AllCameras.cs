// The "All Cameras" arrangement (docs/UX.md "Layouts"): every camera of every
// online channel of every connected device, in sidebar order, capped at the
// layout limit; and the rule that keeps an unedited All Cameras window in step
// with the devices. Pure logic, no clocks.

using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>One channel as All Cameras needs it.</summary>
public readonly record struct AllCamerasChannel(int Channel, bool Connected, int Lenses = 1);

/// <summary>
/// One device as All Cameras needs it. Channels of a device that is not
/// connected are not known to be online, so they are left out until it connects.
/// </summary>
public sealed record AllCamerasDevice(string Key, bool Connected, IReadOnlyList<AllCamerasChannel> Channels);

public static class AllCameras
{
    public const string Name = "All Cameras";

    /// <summary>The cameras of one channel: one per lens.</summary>
    public static IEnumerable<CameraRef> CamerasOf(string deviceKey, AllCamerasChannel c) =>
        Enumerable.Range(0, Math.Clamp(c.Lenses, 1, CameraRef.MaxLens + 1)).Select(lens => new CameraRef(deviceKey, c.Channel, lens));

    /// <summary>The cameras of one device's online channels (none when it is not connected).</summary>
    public static IEnumerable<CameraRef> CamerasOf(AllCamerasDevice d) =>
        d.Connected ? d.Channels.Where(c => c.Connected).SelectMany(c => CamerasOf(d.Key, c)) : [];

    /// <summary>The cameras of every device, in order, up to <paramref name="limit"/> (the layout limit by default).</summary>
    public static List<CameraRef> Select(IEnumerable<AllCamerasDevice> devices, int limit = LayoutNode.MaxLeaves) =>
        [.. devices.SelectMany(CamerasOf).Take(limit)];
}

/// <summary>
/// Keeps an unedited All Cameras window in step with the devices: it rebuilds
/// only when the camera list changes, and never after the user has edited it.
/// </summary>
public sealed class AllCamerasWatch(IReadOnlyList<CameraRef> shown)
{
    public IReadOnlyList<CameraRef> Shown { get; private set; } = shown;

    public bool Edited { get; private set; }

    /// <summary>The user split, swapped, resized, or otherwise edited the window.</summary>
    public void MarkEdited() => Edited = true;

    /// <summary>Offers the current list; true when the window should be rebuilt from it.</summary>
    public bool Refresh(IReadOnlyList<CameraRef> cameras)
    {
        if (Edited || Shown.SequenceEqual(cameras))
        {
            return false;
        }
        Shown = cameras;
        return true;
    }
}
