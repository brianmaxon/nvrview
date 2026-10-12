// Tests of the Save Picture rules (NvrView.Tiles/Pictures.cs). The naming vectors are
// fixtures/synthetic/capture-names.json (copied beside the test binary), generated from the Rust rules and read
// by the Mac app's tests too.

using System.Text.Json;
using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class PictureTests
{
    public static IEnumerable<(string, Action)> All() =>
    [
        ("picture: names match the shared vectors", Names),
        ("picture: sanitize matches the shared vectors", Sanitizing),
        ("picture: formats and extensions", Formats),
        ("picture: the region of a zoomed or filled tile", Regions),
    ];

    private static JsonDocument Vectors() =>
        JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "capture-names.json")));

    private static void Names()
    {
        using JsonDocument doc = Vectors();
        Check.Equal(PictureNaming.MaxNameChars, doc.RootElement.GetProperty("maxNameChars").GetInt32(), "max name chars");
        Check.Equal(PictureNaming.ShareSafeLabel, doc.RootElement.GetProperty("shareSafeLabel").GetString(), "share-safe label");
        JsonElement cases = doc.RootElement.GetProperty("picture");
        Check.True(cases.GetArrayLength() >= 10, "the vectors have their cases");
        foreach (JsonElement c in cases.EnumerateArray())
        {
            var taken = new HashSet<string>(c.GetProperty("taken").EnumerateArray().Select(e => e.GetString()!), StringComparer.Ordinal);
            PictureFormat format = PictureFormats.FromKey(c.GetProperty("format").GetString());
            string? got = PictureNaming.FileName(c.GetProperty("ms").GetInt64(), c.GetProperty("camera").GetString()!, c.GetProperty("shareSafe").GetBoolean(), format, taken.Contains);
            Check.Equal(c.GetProperty("expected").GetString(), got, "file name");
        }
    }

    private static void Sanitizing()
    {
        using JsonDocument doc = Vectors();
        foreach (JsonElement c in doc.RootElement.GetProperty("sanitize").EnumerateArray())
        {
            Check.Equal(c.GetProperty("expected").GetString(), PictureNaming.Sanitize(c.GetProperty("input").GetString()!), "sanitize");
        }
    }

    private static void Formats()
    {
        Check.Equal("png", PictureFormat.Png.Extension(), "png extension");
        Check.Equal("jpg", PictureFormat.Jpeg.Extension(), "jpeg extension");
        Check.Equal(PictureFormat.Jpeg, PictureFormats.FromExtension(".JPEG"), "JPEG by extension");
        Check.Equal(PictureFormat.Jpeg, PictureFormats.FromExtension("jpg"), "jpg by extension");
        Check.True(PictureFormats.FromExtension("gif") is null, "gif is not a picture format");
        Check.Equal(PictureFormat.Png, PictureFormats.FromKey(null), "default format");
        Check.Equal(PictureFormat.Jpeg, PictureFormats.FromKey(PictureFormat.Jpeg.Key()), "key round trip");
        // The stamp counts from the wall clock, with milliseconds.
        Check.Equal("1970-01-01 00.00.01.000", PictureNaming.Stamp(1000), "stamp");
        Check.Equal(1791738123250L + 3600_000, PictureNaming.LocalMillis(DateTimeOffset.FromUnixTimeMilliseconds(1791738123250).ToOffset(TimeSpan.FromHours(1))), "local millis");
    }

    private static void Regions()
    {
        // 1x: the whole picture.
        Check.Equal(new PixelRegion(0, 0, 1920, 1080), PictureRegion.Visible(1920, 1080, 400, 300, false, TileZoom.None)!.Value, "1x");
        Check.True(PictureRegion.Visible(0, 0, 400, 300, false, TileZoom.None) is null, "no picture");
        // 2x about the middle of a tile of the picture's shape: the middle quarter.
        Check.Equal(new PixelRegion(480, 270, 960, 540), PictureRegion.Visible(1920, 1080, 640, 360, false, new TileZoom(2, 0.5, 0.5))!.Value, "2x middle");
        // 2x with the visible region pushed to the top left corner.
        Check.Equal(new PixelRegion(0, 0, 960, 540), PictureRegion.Visible(1920, 1080, 640, 360, false, new TileZoom(2, 0.25, 0.25))!.Value, "2x corner");
        // Filled in a 4:3 tile at 1x: the middle 4:3 of a 16:9 picture.
        Check.Equal(new PixelRegion(240, 0, 1440, 1080), PictureRegion.Visible(1920, 1080, 400, 300, true, TileZoom.None)!.Value, "fill crop");
        // Never past the picture.
        PixelRegion r = PictureRegion.Visible(1920, 1080, 640, 360, false, new TileZoom(8, 0.99, 0.99))!.Value;
        Check.True(r.X >= 0 && r.Y >= 0 && r.X + r.Width <= 1920 && r.Y + r.Height <= 1080 && r.Width >= 1, "inside the picture");
    }
}
