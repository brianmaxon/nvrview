// Cameras (NvrView.Tiles.CameraRef: the core's stable device key, a channel,
// and the lens) as layouts and drags name them.

using System.Globalization;
using NvrView.Tiles;

namespace NvrView.Shell;

public static class CameraRefs
{
    private const string DragPrefix = "nvrview-camera:";

    /// <summary>The same camera under another device key.</summary>
    public static CameraRef Rekeyed(this CameraRef c, string device) => c with { Device = device };

    /// <summary>
    /// The text a camera is dragged as, "nvrview-camera:CHANNEL[.LENS]@DEVICE"
    /// (the same as the macOS app's).
    /// </summary>
    public static string DragText(this CameraRef c) =>
        string.Create(CultureInfo.InvariantCulture, $"{DragPrefix}{c.Channel}{(c.Lens == 0 ? "" : $".{c.Lens}")}@{c.Device}");

    /// <summary>The camera a dragged text names, or null.</summary>
    public static CameraRef? FromDragText(string? text)
    {
        if (text is null || !text.StartsWith(DragPrefix, StringComparison.Ordinal))
        {
            return null;
        }
        string rest = text[DragPrefix.Length..];
        int at = rest.IndexOf('@', StringComparison.Ordinal);
        if (at <= 0 || at == rest.Length - 1)
        {
            return null;
        }
        string[] numbers = rest[..at].Split('.');
        if (numbers.Length is < 1 or > 2
            || !int.TryParse(numbers[0], NumberStyles.None, CultureInfo.InvariantCulture, out int channel))
        {
            return null;
        }
        int lens = 0;
        if (numbers.Length == 2
            && (!int.TryParse(numbers[1], NumberStyles.None, CultureInfo.InvariantCulture, out lens) || lens > CameraRef.MaxLens))
        {
            return null;
        }
        return new CameraRef(rest[(at + 1)..], channel, lens);
    }
}
