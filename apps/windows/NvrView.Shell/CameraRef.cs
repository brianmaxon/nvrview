// A camera as layouts, groups, and leases name it (docs/FFI.md "Layouts"):
// the core's stable device key (never the per-run device ID), a channel, and
// the lens (0 for the first; 1 for the second lens of a multi-lens camera,
// which the device serves on the same channel).

using System.Globalization;

namespace NvrView.Shell;

/// <summary>A camera: device key, channel, lens.</summary>
public readonly record struct CameraRef(string Device, int Channel, int Lens = 0)
{
    /// <summary>The highest lens a layout may name.</summary>
    public const int MaxLens = 1;

    /// <summary>The same camera under another device key.</summary>
    public CameraRef Rekeyed(string device) => this with { Device = device };

    /// <summary>The first lens of the same channel.</summary>
    public CameraRef FirstLens => this with { Lens = 0 };

    public override string ToString() =>
        Lens == 0 ? $"{Device}/{Channel}" : $"{Device}/{Channel}/lens{Lens}";

    private const string DragPrefix = "nvrview-camera:";

    /// <summary>
    /// The text a camera is dragged as, "nvrview-camera:CHANNEL[.LENS]@DEVICE"
    /// (the same as the macOS app's).
    /// </summary>
    public string DragText =>
        string.Create(CultureInfo.InvariantCulture, $"{DragPrefix}{Channel}{(Lens == 0 ? "" : $".{Lens}")}@{Device}");

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
            && (!int.TryParse(numbers[1], NumberStyles.None, CultureInfo.InvariantCulture, out lens) || lens > MaxLens))
        {
            return null;
        }
        return new CameraRef(rest[(at + 1)..], channel, lens);
    }
}
