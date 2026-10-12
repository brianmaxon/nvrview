// Tests of the tile header model (NvrView.Tiles/TileHeader.cs). The vectors are
// fixtures/synthetic/tile-header.json (copied beside the test binary), which the Mac app's
// NVRCoreTests (TileHeaderTests.swift) reads too.

using System.Text.Json;
using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class TileHeaderTests
{
    public static IEnumerable<(string, Action)> All() =>
    [
        ("header: metrics match the vectors", Metrics),
        ("header: shared vectors", SharedVectors),
        ("header: invariants over widths and capabilities", Invariants),
    ];

    private static JsonDocument Vectors() =>
        JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "tile-header.json")));

    private static readonly string[] TileKeys =
    [
        "audio", "talk", "doorbell", "panTilt", "presets", "lens", "fisheye", "picture", "recording", "zoomed",
        "alert", "listening", "conversation", "call",
    ];

    private static TileHeaderTile Tile(JsonElement json)
    {
        foreach (JsonProperty p in json.EnumerateObject())
        {
            Check.True(TileKeys.Contains(p.Name), $"unknown tile key {p.Name}");
        }
        bool Flag(string key, bool fallback = false) => json.TryGetProperty(key, out JsonElement v) ? v.GetBoolean() : fallback;
        string call = json.TryGetProperty("call", out JsonElement c) ? c.GetString()! : "none";
        return new TileHeaderTile
        {
            Audio = Flag("audio"),
            Talk = Flag("talk"),
            Doorbell = Flag("doorbell"),
            PanTilt = Flag("panTilt"),
            Presets = Flag("presets"),
            Lens = Flag("lens"),
            Fisheye = Flag("fisheye"),
            Picture = Flag("picture", true),
            Recording = Flag("recording"),
            Zoomed = Flag("zoomed"),
            Alert = Flag("alert"),
            Listening = Flag("listening"),
            Conversation = Flag("conversation"),
            Call = call switch
            {
                "none" => TileHeaderCall.None,
                "inset" => TileHeaderCall.Inset,
                "window" => TileHeaderCall.Window,
                _ => throw new CheckException($"unknown call {call}"),
            },
        };
    }

    private static string Ids(JsonElement array)
    {
        foreach (JsonElement e in array.EnumerateArray())
        {
            Check.True(TileHeaderItems.FromId(e.GetString()!) is not null, $"unknown item {e.GetString()}");
        }
        return string.Join(", ", array.EnumerateArray().Select(e => e.GetString()));
    }

    private static string Ids(IEnumerable<TileHeaderItem> items) => string.Join(", ", items.Select(i => i.Id()));

    private static void Metrics()
    {
        using JsonDocument doc = Vectors();
        JsonElement m = doc.RootElement.GetProperty("metrics");
        TileHeaderMetrics s = TileHeaderMetrics.Standard;
        Check.Equal(m.GetProperty("icon").GetDouble(), s.Icon, "icon");
        Check.Equal(m.GetProperty("titleMin").GetDouble(), s.TitleMin, "title min");
        Check.Equal(m.GetProperty("pill").GetDouble(), s.Pill, "pill");
        Check.Equal(m.GetProperty("padding").GetDouble(), s.Padding, "padding");
        Check.Equal(m.GetProperty("hysteresis").GetDouble(), s.Hysteresis, "hysteresis");
        Check.Equal(m.GetProperty("narrow").GetDouble(), s.Narrow, "narrow");
    }

    private static void SharedVectors()
    {
        using JsonDocument doc = Vectors();
        JsonElement cases = doc.RootElement.GetProperty("cases");
        Check.True(cases.GetArrayLength() >= 10, "the vectors have their cases");
        foreach (JsonElement c in cases.EnumerateArray())
        {
            string name = c.GetProperty("name").GetString()!;
            TileHeaderTile tile = Tile(c.GetProperty("tile"));
            double titleWidth = c.GetProperty("titleWidth").GetDouble();
            TileHeaderLayout? previous = null;
            int i = 0;
            foreach (JsonElement step in c.GetProperty("steps").EnumerateArray())
            {
                string at = $"{name}, step {i++}";
                if (step.TryGetProperty("tile", out JsonElement t))
                {
                    tile = Tile(t);
                }
                double width = step.GetProperty("width").GetDouble();
                TileHeaderLayout r = TileHeaderLayout.Compute(width, titleWidth, tile, previous: previous);
                Check.Equal(Ids(step.GetProperty("visible")), Ids(r.Visible), $"{at}: visible");
                Check.Equal(Ids(step.GetProperty("overflow")), Ids(r.Overflow), $"{at}: overflow");
                Check.Equal(step.GetProperty("titleTruncated").GetBoolean(), r.TitleTruncated, $"{at}: title truncated");
                Check.Equal(step.GetProperty("anchor").GetDouble(), r.Anchor, $"{at}: anchor");
                if (step.TryGetProperty("menu", out JsonElement menu))
                {
                    string expected = string.Join("; ", menu.EnumerateArray().Select(e => string.Join("|", e.EnumerateArray().Select(s => s.GetString()))));
                    string got = string.Join("; ", r.ContextMenu.Select(e => $"{e.Item.Id()}|{e.Label}|{e.MacShortcut}|{e.WindowsShortcut}"));
                    Check.Equal(expected, got, $"{at}: menu");
                }
                if (step.TryGetProperty("moreMenu", out JsonElement more))
                {
                    Check.Equal(Ids(more), Ids(r.MoreMenu.Select(e => e.Item)), $"{at}: more menu");
                }
                previous = r;
            }
        }
    }

    /// <summary>Rules every layout keeps, over a sweep of widths and capabilities.</summary>
    private static void Invariants()
    {
        for (int mask = 0; mask < 1 << 11; mask++)
        {
            bool Bit(int b) => (mask & (1 << b)) != 0;
            var tile = new TileHeaderTile
            {
                Audio = Bit(0), Talk = Bit(1), Doorbell = Bit(2), PanTilt = Bit(3), Presets = Bit(4), Lens = Bit(5),
                Fisheye = Bit(6), Picture = Bit(7), Recording = Bit(8), Zoomed = Bit(9), Alert = Bit(10),
            };
            List<TileHeaderItem> menu = [.. TileHeaderLayout.Menu(tile).Select(e => e.Item)];
            for (double width = 40; width <= 640; width += 30)
            {
                TileHeaderLayout r = TileHeaderLayout.Compute(width, 80, tile);
                string at = $"mask {mask}, width {width}";
                Check.True(r.Visible.Contains(TileHeaderItem.Title), $"{at}: the title shows");
                Check.Equal(r.Overflow.Count > 0, r.ShowsMore, $"{at}: more when anything is hidden");
                // Nothing is only in the header: every visible action is in the menu, in the same order.
                List<TileHeaderItem> actions = [.. r.Visible.Where(v => v is not (TileHeaderItem.Title or TileHeaderItem.RecordingPill or TileHeaderItem.More))];
                Check.Equal(Ids(actions), Ids(menu.Where(actions.Contains)), $"{at}: header order is menu order");
                Check.True(!r.Overflow.Contains(TileHeaderItem.RecordingPill) && !r.Overflow.Contains(TileHeaderItem.DismissAlert), $"{at}: indicators never overflow");
                if (!tile.Picture || width < 120)
                {
                    Check.Equal(0, actions.Count, $"{at}: no buttons under a veil or below 120");
                }
                if (!tile.Picture)
                {
                    Check.True(!r.Visible.Contains(TileHeaderItem.RecordingPill), $"{at}: no pill under a veil");
                }
                if (tile.Picture && width >= 120)
                {
                    double used = r.Visible.Sum(v => v switch
                    {
                        TileHeaderItem.Title => 48.0,
                        TileHeaderItem.RecordingPill => 64.0,
                        _ => 26.0,
                    });
                    double least = 48 + (tile.Recording ? 64 : 0) + (tile.Alert ? 26 : 0) + 26;
                    Check.True(used <= Math.Max(width - 16, least), $"{at}: the header fits");
                }
            }
        }
    }
}
