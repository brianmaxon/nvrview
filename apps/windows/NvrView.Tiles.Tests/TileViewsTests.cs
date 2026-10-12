// Tests of a window's picture settings per tile (NvrView.Tiles/TileViews.cs), tile identities and the filled crop's pan
// at 1x (Zoom.cs). The vectors are fixtures/synthetic/tile-views.json (copied beside the test binary), which
// the Mac app's NVRCoreTests (TileViewsTests.swift) reads too; the saved form is tested in NvrView.Shell.Tests.

using System.Text.Json;
using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class TileViewsTests
{
    public static IEnumerable<(string, Action)> All() =>
    [
        ("tile views: the limit matches the vectors", LimitMatches),
        ("tile views: fill pan vectors", PanVectors),
        ("tile views: effective fill vectors", FillVectors),
        ("tile views: the tiles changed longest ago go first", Bound),
        ("tile views: tile identity vectors", TileIdVectors),
        ("tile views: read back clamped and bounded", ReadBack),
    ];

    private static JsonDocument Vectors() =>
        JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "tile-views.json")));

    private static TileZoom Zoom(JsonElement e) => new(e[0].GetDouble(), e[1].GetDouble(), e[2].GetDouble());

    private static bool Close(double a, double b) => Math.Abs(a - b) < 1e-9;

    private static void LimitMatches()
    {
        using JsonDocument doc = Vectors();
        Check.Equal(doc.RootElement.GetProperty("limit").GetInt32(), TileViews<string>.Limit, "limit");
    }

    private static void PanVectors()
    {
        using JsonDocument doc = Vectors();
        JsonElement cases = doc.RootElement.GetProperty("pan");
        Check.True(cases.GetArrayLength() >= 8, "the vectors have their cases");
        foreach (JsonElement c in cases.EnumerateArray())
        {
            string name = c.GetProperty("name").GetString()!;
            JsonElement p = c.GetProperty("picture"), t = c.GetProperty("tile");
            ZoomBase b = ZoomBase.Of(p[0].GetDouble(), p[1].GetDouble(), t[0].GetDouble(), t[1].GetDouble(), c.GetProperty("fill").GetBoolean());
            TileZoom z = Zoom(c.GetProperty("zoom"));
            Check.Equal(c.GetProperty("canPan").GetBoolean(), z.CanPan(b), $"{name}: can pan");
            TileZoom output;
            if (c.TryGetProperty("zoomTo", out JsonElement to))
            {
                (double qx, double qy) = z.PicturePoint(0.5, 0.5, b);
                output = z.ZoomedTo(to.GetDouble(), qx, qy, 0.5, 0.5, b);
            }
            else
            {
                JsonElement d = c.GetProperty("drag");
                output = z.Panned(d[0].GetDouble(), d[1].GetDouble(), b);
            }
            TileZoom want = Zoom(c.GetProperty("result"));
            Check.True(Close(output.Scale, want.Scale) && Close(output.CenterX, want.CenterX) && Close(output.CenterY, want.CenterY),
                $"{name}: expected {want}, got {output}");
            JsonElement v = c.GetProperty("visible");
            UnitRect got = output.Visible(b);
            Check.True(Close(got.X, v[0].GetDouble()) && Close(got.Y, v[1].GetDouble()) && Close(got.Width, v[2].GetDouble()) && Close(got.Height, v[3].GetDouble()),
                $"{name}: visible {got}");
        }
    }

    private static void FillVectors()
    {
        using JsonDocument doc = Vectors();
        foreach (JsonElement c in doc.RootElement.GetProperty("fill").EnumerateArray())
        {
            string name = c.GetProperty("name").GetString()!;
            var views = new TileViews<string>();
            foreach (JsonElement op in c.GetProperty("ops").EnumerateArray())
            {
                switch (op[0].GetString())
                {
                    case "window":
                        views.SetWindowFill(op[1].GetBoolean());
                        break;
                    case "tile":
                        views.SetFill(op[1].GetString()!, op[2].GetBoolean());
                        break;
                    case "zoom":
                        views.SetZoom(op[1].GetString()!, new TileZoom(op[2].GetDouble(), op[3].GetDouble(), op[4].GetDouble()));
                        break;
                    default:
                        throw new CheckException($"{name}: unknown op {op}");
                }
            }
            foreach (JsonProperty f in c.GetProperty("fills").EnumerateObject())
            {
                Check.Equal(f.Value.GetBoolean(), views.Fills(f.Name), $"{name}: {f.Name} fills");
            }
            string choices = string.Join(", ", c.GetProperty("choices").EnumerateObject().Select(p => $"{p.Name}={p.Value.GetBoolean()}").Order(StringComparer.Ordinal));
            string gotChoices = string.Join(", ", views.Order.Where(k => views.FillChoice(k) is not null).Select(k => $"{k}={views.FillChoice(k)}").Order(StringComparer.Ordinal));
            Check.Equal(choices, gotChoices, $"{name}: choices");
            Check.Equal(c.GetProperty("mixed").GetBoolean(), views.Mixed, $"{name}: mixed");
            Check.Equal(c.GetProperty("count").GetInt32(), views.Count, $"{name}: count");
            if (c.TryGetProperty("zooms", out JsonElement zooms))
            {
                foreach (JsonProperty z in zooms.EnumerateObject())
                {
                    Check.Equal(Zoom(z.Value), views.Zoom(z.Name), $"{name}: {z.Name} zoom");
                }
            }
        }
    }

    private static void TileIdVectors()
    {
        using JsonDocument doc = Vectors();
        JsonElement cases = doc.RootElement.GetProperty("tileIDs");
        Check.True(cases.GetArrayLength() >= 5, "the vectors have their cases");
        foreach (JsonElement c in cases.EnumerateArray())
        {
            string name = c.GetProperty("name").GetString()!;
            List<string?> have = [.. c.GetProperty("have").EnumerateArray().Select(e => e.ValueKind == JsonValueKind.String ? e.GetString() : null)];
            string want = string.Join(", ", c.GetProperty("want").EnumerateArray().Select(e => e.GetString()));
            Check.Equal(want, string.Join(", ", TileIds.Assign(have)), name);
        }
    }

    private static void Bound()
    {
        var views = new TileViews<int>();
        int limit = TileViews<int>.Limit;
        for (int i = 0; i < limit; i++)
        {
            views.SetZoom(i, new TileZoom(2, 0.5, 0.5));
        }
        Check.Equal(limit, views.Count, "full");
        // Changing tile 0 again makes it the newest.
        views.SetFill(0, true);
        views.SetZoom(limit, new TileZoom(3, 0.5, 0.5));
        Check.Equal(limit, views.Count, "still full");
        Check.True(views.Zoom(0).Scale == 2 && views.Fills(0), "the camera changed again is kept");
        Check.Equal(TileZoom.None, views.Zoom(1), "the oldest is dropped");
        Check.Equal(3.0, views.Zoom(limit).Scale, "the newest is kept");
        Check.True(views.Order[0] == 2 && views.Order[^1] == limit, "order");
    }

    private static void ReadBack()
    {
        var saved = Enumerable.Range(0, TileViews<int>.Limit + 44).Select(i => (i, new TileView(new TileZoom(2, 0.5, 0.5)))).ToList();
        saved.Add((1000, new TileView(new TileZoom(20, -1, 7), Fill: true)));
        var views = new TileViews<int>(true, saved);
        Check.Equal(TileViews<int>.Limit, views.Count, "bounded");
        Check.Equal(45, views.Order[0], "the newest kept");
        Check.Equal(new TileZoom(8, 0, 1), views.Zoom(1000), "clamped");
        Check.True(views.FillChoice(1000) is null && views.Fills(1000), "a choice equal to the window's is not kept");
    }
}
