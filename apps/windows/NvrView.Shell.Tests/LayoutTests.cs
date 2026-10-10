// The split tree's operations and its JSON, including layouts the macOS app
// wrote (the strings below are that app's encoder output for the same trees).

using System.Text.Json.Nodes;
using NvrView.Tiles;

namespace NvrView.Shell.Tests;

internal static class Fixtures
{
    public static CameraRef Cam(int channel, string device = "dev-a", int lens = 0) => new(device, channel, lens);

    public static NodePath P(string steps) => new(steps);

    /// <summary>The layout docs/FFI.md shows.</summary>
    public static readonly LayoutNode Contract = LayoutNode.Split(SplitAxis.Row, 0.62, LayoutNode.Leaf(Cam(3)),
        LayoutNode.Split(SplitAxis.Col, 0.34, LayoutNode.Leaf(null), LayoutNode.Leaf(Cam(9))));
}

internal static class LayoutTreeTests
{
    private static CameraRef Cam(int c, string d = "dev-a") => Fixtures.Cam(c, d);

    private static NodePath P(string s) => Fixtures.P(s);

    public static void Run()
    {
        LayoutNode contract = Fixtures.Contract;

        T.Run("tree: reading leaves and cameras", () =>
        {
            IReadOnlyList<LeafInfo> leaves = contract.Leaves;
            T.Equal("a,ba,bb", string.Join(",", leaves.Select(l => l.Path.Steps)));
            T.Check(contract.CameraList.SequenceEqual([Cam(3), null, Cam(9)]));
            T.Equal(3, contract.LeafCount);
            T.Equal(3, contract.Depth);
            T.Check(contract.Cameras.SetEquals([Cam(3), Cam(9)]));
            T.Check(contract[P("bb")] == LayoutNode.Leaf(Cam(9)));
            T.Check(contract[P("aa")] is null);
        });

        T.Run("tree: split adds one empty tile next to the old", () =>
        {
            (LayoutNode tree, NodePath added) = T.NotNull(contract.Splitting(P("a"), SplitAxis.Col));
            T.Equal(P("ab"), added);
            T.Equal(4, tree.LeafCount);
            T.Check(tree[P("a")] == LayoutNode.Split(SplitAxis.Col, 0.5, LayoutNode.Leaf(Cam(3)), LayoutNode.Leaf(null)));
            T.Check(tree[P("b")] == contract[P("b")]);
            T.Check(contract.Splitting(P("b"), SplitAxis.Row) is null, "only leaves split");
            T.Check(contract.Splitting(P("aa"), SplitAxis.Row) is null);
        });

        T.Run("tree: remove lets the neighbor take the space", () =>
        {
            (LayoutNode tree, NodePath survivor) = T.NotNull(contract.Removing(P("ba")));
            T.Equal(P("b"), survivor);
            T.Check(tree == LayoutNode.Split(SplitAxis.Row, 0.62, LayoutNode.Leaf(Cam(3)), LayoutNode.Leaf(Cam(9))));
            (LayoutNode single, _) = T.NotNull(tree.Removing(P("a")));
            T.Check(single == LayoutNode.Leaf(Cam(9)));
            (LayoutNode empty, NodePath root) = T.NotNull(single.Removing(NodePath.Root));
            T.Check(empty == LayoutNode.Empty, "removing the last tile leaves one empty tile");
            T.Check(root.IsRoot);
            T.Check(contract.Removing(P("b")) is null);
        });

        T.Run("tree: swap exchanges cameras only", () =>
        {
            LayoutNode swapped = T.NotNull(contract.Swapping(P("a"), P("bb")));
            T.Check(swapped.CameraList.SequenceEqual([Cam(9), null, Cam(3)]));
            T.Equal(contract.Shape(), swapped.Shape());
            T.Check(contract.Swapping(P("a"), P("a")) == contract);
            T.Check(contract.Swapping(P("a"), P("b")) is null);
            LayoutNode moved = T.NotNull(contract.Swapping(P("a"), P("ba")));
            T.Check(moved.CameraList.SequenceEqual([null, Cam(3), Cam(9)]));
        });

        T.Run("tree: ratios are clamped", () =>
        {
            var tight = (LayoutSplit)T.NotNull(contract.SettingRatio(0.01, NodePath.Root));
            T.Equal(LayoutNode.MinRatio, tight.Ratio);
            var wide = (LayoutSplit)T.NotNull(T.NotNull(contract.SettingRatio(5, P("b")))[P("b")]);
            T.Equal(LayoutNode.MaxRatio, wide.Ratio);
            T.Check(contract.SettingRatio(0.5, P("a")) is null);
        });

        T.Run("tree: assign, clear a device, rekey", () =>
        {
            LayoutNode assigned = T.NotNull(contract.Assigning(Cam(1, "dev-b"), P("ba")));
            T.Check(assigned.CameraList.SequenceEqual([Cam(3), Cam(1, "dev-b"), Cam(9)]));
            T.Check(assigned.Clearing("dev-a").CameraList.SequenceEqual([null, Cam(1, "dev-b"), null]));
            T.Check(contract.Assigning(Cam(1), P("b")) is null);
            T.Check(assigned.Rekeying("dev-a", "d-0000000000000001").CameraList
                .SequenceEqual([Cam(3, "d-0000000000000001"), Cam(1, "dev-b"), Cam(9, "d-0000000000000001")]));
            Dictionary<CameraRef, int> counts = LayoutNode.Balanced([Cam(1), Cam(1), Cam(2)]).CameraCounts();
            T.Equal(2, counts[Cam(1)]);
            T.Equal(1, counts[Cam(2)]);
        });

        T.Run("tree: balanced grids have equal tiles", () =>
        {
            foreach (int count in new[] { 0, 1, 2, 3, 4, 5, 7, 9, 10, 16 })
            {
                List<CameraRef> cameras = [.. Enumerable.Range(0, count).Select(i => Cam(i))];
                LayoutNode tree = LayoutNode.Balanced(cameras);
                T.Equal(Math.Max(count, 1), tree.LeafCount, $"count {count}");
                if (count == 0)
                {
                    T.Check(tree == LayoutNode.Empty);
                    continue;
                }
                T.Check(tree.CameraList.SequenceEqual(cameras.Select(c => (CameraRef?)c)));
                int columns = (int)Math.Ceiling(Math.Sqrt(count));
                int rows = (count + columns - 1) / columns;
                foreach (LeafFrame f in tree.Frames(new Rect(0, 0, 1200, 1200)).Leaves)
                {
                    T.Check(Math.Abs(f.Rect.Height - 1200.0 / rows) <= 1, $"count {count}");
                }
            }
        });

        T.Run("tree: presets", () =>
        {
            var expected = new Dictionary<LayoutPreset, int>
            {
                [LayoutPreset.Single] = 1, [LayoutPreset.TwoByTwo] = 4, [LayoutPreset.OnePlusThree] = 4, [LayoutPreset.ThreeByThree] = 9,
            };
            foreach (LayoutPreset p in LayoutPresets.All)
            {
                LayoutNode tree = p.Tree([Cam(0), Cam(1)]);
                T.Equal(expected[p], tree.LeafCount, p.Title());
                T.Equal<CameraRef?>(Cam(0), tree.CameraList[0]);
            }
        });

        T.Run("tree: members kept for the app (a tile's quality)", () =>
        {
            LayoutNode q = T.NotNull(contract.SettingMember(P("a"), "quality", "\"clear\""));
            T.Equal("\"clear\"", q.Member(P("a"), "quality"));
            T.Check(q.SettingMember(P("a"), "camera", "null") is null, "known members cannot be set this way");
            LayoutNode back = T.NotNull(q.SettingMember(P("a"), "quality", null));
            T.Check(back == contract);
            // The tile keeps it through a swap; a split moves it into a.
            T.Equal("\"clear\"", T.NotNull(q.Swapping(P("a"), P("bb"))).Member(P("a"), "quality"));
            T.Equal("\"clear\"", T.NotNull(q.Splitting(P("a"), SplitAxis.Row)).Tree.Member(P("aa"), "quality"));
        });

        T.Run("tree: a tile's fisheye view is a member beside its quality", () =>
        {
            var ptz = new FisheyeTileSetting(FisheyeViewMode.Ptz, new FisheyeAim(12.5, 45, 70));
            LayoutNode q = T.NotNull(contract.SettingMember(P("a"), "quality", "\"clear\""));
            LayoutNode f = T.NotNull(q.SettingMember(P("a"), FisheyeTileSetting.Member, ptz.ToJson()));
            T.Equal(ptz, FisheyeTileSetting.FromJson(f.Member(P("a"), FisheyeTileSetting.Member)));
            T.Equal("\"clear\"", f.Member(P("a"), "quality"));
            // Through the layout JSON, as the core stores it (and as the macOS app writes it).
            var saved = new SavedLayout("l1", "Layout 1", f, 1);
            LayoutNode read = SavedLayout.Parse(saved.ToJson()).Root;
            T.Equal(ptz, FisheyeTileSetting.FromJson(read.Member(P("a"), FisheyeTileSetting.Member)));
            const string mac = """{"id":"m","name":"Mac","root":{"camera":{"channel":5,"device":"d-0123456789abcdef"},"fisheye":{"view":"double_panorama"}},"version":1}""";
            T.Equal(FisheyeViewMode.DoublePanorama, FisheyeTileSetting.FromJson(SavedLayout.Parse(mac).Root.Member(NodePath.Root, FisheyeTileSetting.Member)).Mode);
            // Original removes the member.
            LayoutNode back = T.NotNull(f.SettingMember(P("a"), FisheyeTileSetting.Member, FisheyeTileSetting.Original.ToJson()));
            T.Check(back == q);
        });

        T.Run("tree: random edits keep every invariant", () =>
        {
            var rng = new SplitMix(0x5eed);
            LayoutNode tree = LayoutNode.Empty;
            var bounds = new Rect(0, 0, 1600, 900);
            for (int step = 0; step < 2000; step++)
            {
                IReadOnlyList<LeafInfo> leaves = tree.Leaves;
                NodePath pick = leaves[(int)(rng.Next() % (ulong)leaves.Count)].Path;
                switch (rng.Next() % 5)
                {
                    case 0:
                        int before = tree.LeafCount;
                        if (tree.Splitting(pick, rng.Next() % 2 == 0 ? SplitAxis.Row : SplitAxis.Col) is { } s)
                        {
                            T.Equal(before + 1, s.Tree.LeafCount);
                            T.Check(s.Tree[s.NewLeaf] == LayoutNode.Leaf(null));
                            tree = s.Tree;
                        }
                        break;
                    case 1:
                        int had = tree.LeafCount;
                        tree = T.NotNull(tree.Removing(pick)).Tree;
                        T.Equal(Math.Max(had - 1, 1), tree.LeafCount);
                        break;
                    case 2:
                        NodePath other = leaves[(int)(rng.Next() % (ulong)leaves.Count)].Path;
                        LayoutNode t = T.NotNull(tree.Swapping(pick, other));
                        T.Check(Sorted(t).SequenceEqual(Sorted(tree)));
                        T.Equal(tree.Shape(), t.Shape());
                        tree = t;
                        break;
                    case 3:
                        tree = T.NotNull(tree.Assigning(rng.Next() % 4 == 0 ? null : Cam((int)(rng.Next() % 16)), pick));
                        break;
                    default:
                        if (tree.Frames(bounds).Dividers is { Count: > 0 } d)
                        {
                            tree = T.NotNull(tree.SettingRatio((rng.Next() % 1000) / 1000.0, d[0].Path));
                        }
                        break;
                }
                T.Check(tree.LeafCount <= LayoutNode.MaxLeaves);
                CheckRatios(tree);
                CheckFrames(tree, bounds);
                var layout = new SavedLayout("l", "Random", tree, 1);
                List<SavedLayout> back = SavedLayout.List($"{{\"layouts\":[{layout.ToJson()}]}}");
                T.Check(back.Count == 1 && back[0] == layout, $"round trip at step {step}");
            }

            static IEnumerable<int> Sorted(LayoutNode n) => n.CameraList.OfType<CameraRef>().Select(c => c.Channel).Order();
        });
    }

    private static void CheckRatios(LayoutNode n)
    {
        if (n is LayoutSplit s)
        {
            T.Check(s.Ratio >= LayoutNode.MinRatio && s.Ratio <= LayoutNode.MaxRatio);
            CheckRatios(s.A);
            CheckRatios(s.B);
        }
    }

    private static void CheckFrames(LayoutNode tree, Rect bounds)
    {
        LayoutFrames frames = tree.Frames(bounds);
        T.Equal(tree.LeafCount, frames.Leaves.Count);
        T.Equal(tree.LeafCount - 1, frames.Dividers.Count);
        double area = frames.Leaves.Sum(f => f.Rect.Width * f.Rect.Height);
        T.Check(Math.Abs(area - bounds.Width * bounds.Height) < 0.5);
        foreach (LeafFrame f in frames.Leaves)
        {
            T.Check(bounds.Contains(f.Rect));
        }
    }
}

/// <summary>A small deterministic generator (SplitMix64) for the random edit test.</summary>
internal sealed class SplitMix(ulong seed)
{
    private ulong _state = seed;

    public ulong Next()
    {
        _state += 0x9E3779B97F4A7C15;
        ulong z = _state;
        z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9;
        z = (z ^ (z >> 27)) * 0x94D049BB133111EB;
        return z ^ (z >> 31);
    }
}

internal static class LayoutJsonTests
{
    private static CameraRef Cam(int c, string d = "dev-a", int lens = 0) => Fixtures.Cam(c, d, lens);

    private static NodePath P(string s) => Fixtures.P(s);

    /// <summary>A layout as a newer app might store it: unknown members on the layout, splits, leaves, and cameras.</summary>
    private const string WithUnknownMembers = """
        {"id":"l9","name":"Future","version":3,"theme":{"accent":"blue","n":[1,2.5,null,true]},
         "root":{"split":"row","ratio":0.5,"snap":true,
           "a":{"camera":{"device":"dev-a","channel":1,"hint":"x"},"label":"Tile A","weight":2},
           "b":{"camera":null,"pinned":false}}}
        """;

    // Written by the macOS app's encoder (SavedLayout.json) for the trees the
    // tests below build: a 2/3 split with a tile quality and a second lens; the
    // layout above after swapping its two tiles; the 3 x 3 preset.
    private const string MacSample = """{"id":"layout-0a1b2c3d","name":"Mac Sample","root":{"a":{"camera":{"channel":3,"device":"d-0123456789abcdef"},"quality":"clear"},"b":{"a":{"camera":null},"b":{"camera":{"channel":4,"device":"d-0123456789abcdef","lens":1}},"ratio":0.34,"split":"col"},"ratio":0.6666666666666666,"split":"row"},"version":2}""";
    private const string MacSwapped = """{"id":"l9","name":"Future","root":{"a":{"camera":null,"label":"Tile A","weight":2},"b":{"camera":{"channel":1,"device":"dev-a","hint":"x"},"pinned":false},"ratio":0.5,"snap":true,"split":"row"},"theme":{"accent":"blue","n":[1,2.5,null,true]},"version":3}""";
    private const string MacPreset = """{"id":"p","name":"Presets","root":{"a":{"a":{"camera":{"channel":3,"device":"d-0123456789abcdef"}},"b":{"a":{"camera":{"channel":4,"device":"d-0123456789abcdef","lens":1}},"b":{"camera":null},"ratio":0.5,"split":"row"},"ratio":0.3333333333333333,"split":"row"},"b":{"a":{"a":{"camera":null},"b":{"a":{"camera":null},"b":{"camera":null},"ratio":0.5,"split":"row"},"ratio":0.3333333333333333,"split":"row"},"b":{"a":{"camera":null},"b":{"a":{"camera":null},"b":{"camera":null},"ratio":0.5,"split":"row"},"ratio":0.3333333333333333,"split":"row"},"ratio":0.5,"split":"col"},"ratio":0.3333333333333333,"split":"col"},"version":0}""";

    private static bool SameJson(string x, string y) => JsonNode.DeepEquals(JsonNode.Parse(x), JsonNode.Parse(y));

    public static void Run()
    {
        T.Run("json: encodes the contract shape", () =>
        {
            var layout = new SavedLayout("l1", "Layout 1", Fixtures.Contract, 1);
            JsonObject o = T.NotNull(JsonNode.Parse(layout.ToJson()) as JsonObject);
            T.Equal("l1", (string?)o["id"]);
            T.Equal("Layout 1", (string?)o["name"]);
            T.Equal(1, (int?)o["version"]);
            T.Equal("row", (string?)o["root"]!["split"]);
            T.Equal(0.62, (double?)o["root"]!["ratio"]);
            T.Equal("dev-a", (string?)o["root"]!["a"]!["camera"]!["device"]);
            T.Equal(3, (int?)o["root"]!["a"]!["camera"]!["channel"]);
            JsonObject empty = (JsonObject)o["root"]!["b"]!["a"]!;
            T.Check(empty.ContainsKey("camera") && empty["camera"] is null, "an empty tile is \"camera\": null");
        });

        T.Run("json: reads the contract example", () =>
        {
            const string json = """
                {"revision":7,"layouts":[{"id":"entrances","name":"Entrances","version":1,
                 "root":{"split":"row","ratio":0.62,
                   "a":{"camera":{"device":"dev-a","channel":3}},
                   "b":{"split":"col","ratio":0.34,"a":{"camera":null},"b":{"camera":{"device":"dev-a","channel":9}}}}}]}
                """;
            List<SavedLayout> list = SavedLayout.List(json, out long revision);
            T.Equal(7L, revision);
            T.Check(list.Count == 1 && list[0] == new SavedLayout("entrances", "Entrances", Fixtures.Contract, 1));
        });

        T.Run("json: skips what it cannot read", () =>
        {
            const string json = """
                [{"id":"ok","name":"OK","root":{"camera":null}},
                 {"id":"bad","name":"Bad","root":{"split":"row","ratio":2,"a":{"camera":null},"b":{"camera":null}}},
                 {"id":"odd","name":"Odd","root":{"hexagon":true}},
                 {"name":"no id","root":{"camera":null}}]
                """;
            T.Equal("ok", string.Join(",", SavedLayout.List(json).Select(l => l.Id)));
            T.Equal(0, SavedLayout.List("nonsense").Count);
            // Too deep: 18 levels.
            LayoutNode deep = LayoutNode.Leaf(null);
            for (int i = 0; i < 17; i++)
            {
                deep = LayoutNode.Split(SplitAxis.Row, 0.5, deep, LayoutNode.Leaf(null));
            }
            T.Equal(0, SavedLayout.List($"[{new SavedLayout("d", "Deep", deep).ToJson()}]").Count);
            // Ratios read are clamped into the draggable range.
            var read = (LayoutSplit)SavedLayout.List("""[{"id":"n","name":"N","root":{"split":"col","ratio":0.02,"a":{"camera":null},"b":{"camera":null}}}]""")[0].Root;
            T.Equal(LayoutNode.MinRatio, read.Ratio);
        });

        T.Run("json: keeps members it does not know, written back exactly", () =>
        {
            SavedLayout layout = SavedLayout.List($"[{WithUnknownMembers}]")[0];
            T.Check(layout.Extras.ContainsKey("theme"));
            T.Equal("true", layout.Root.Extras.Node["snap"]);
            NodeExtras a = T.NotNull(layout.Root[P("a")]).Extras;
            T.Equal("\"Tile A\"", a.Node["label"]);
            T.Equal("2", a.Node["weight"]);
            T.Equal("\"x\"", a.Camera["hint"]);
            T.Check(SameJson(WithUnknownMembers, layout.ToJson()));
        });

        T.Run("json: edits keep unknown members where they belong", () =>
        {
            SavedLayout layout = SavedLayout.List($"[{WithUnknownMembers}]")[0];
            SavedLayout dragged = layout with { Root = T.NotNull(layout.Root.SettingRatio(0.7, NodePath.Root)), Name = "Renamed" };
            JsonNode o = JsonNode.Parse(dragged.ToJson())!;
            T.Equal(true, (bool?)o["root"]!["snap"]);
            T.Equal("blue", (string?)o["theme"]!["accent"]);

            LayoutNode swapped = T.NotNull(layout.Root.Swapping(P("a"), P("b")));
            T.Equal("\"Tile A\"", T.NotNull(swapped[P("a")]).Extras.Node["label"]);
            T.Check(T.NotNull(swapped[P("a")]).Extras.Camera.IsEmpty);
            T.Equal("false", T.NotNull(swapped[P("b")]).Extras.Node["pinned"]);
            T.Equal("\"x\"", T.NotNull(swapped[P("b")]).Extras.Camera["hint"]);

            LayoutNode assigned = T.NotNull(layout.Root.Assigning(Cam(0, "dev-b"), P("a")));
            T.Equal("\"Tile A\"", T.NotNull(assigned[P("a")]).Extras.Node["label"]);
            T.Check(T.NotNull(assigned[P("a")]).Extras.Camera.IsEmpty, "the old camera's members go");
            T.Check(layout.Root.Assigning(Cam(1), P("a")) == layout.Root, "the same camera changes nothing");

            LayoutNode split = T.NotNull(layout.Root.Splitting(P("a"), SplitAxis.Col)).Tree;
            T.Equal("\"Tile A\"", T.NotNull(split[P("aa")]).Extras.Node["label"]);
            T.Check(T.NotNull(split[P("a")]).Extras.IsEmpty);

            T.Equal("\"x\"", T.NotNull(layout.Root.Rekeying("dev-a", "dev-z")[P("a")]).Extras.Camera["hint"]);
            T.Check(T.NotNull(layout.Root.Clearing("dev-a")[P("a")]).Extras.Camera.IsEmpty);
        });

        T.Run("json: the lens is written only for the second lens", () =>
        {
            CameraRef lens0 = Cam(3), lens1 = Cam(3, lens: 1);
            LayoutNode tree = LayoutNode.Split(SplitAxis.Row, 0.5, LayoutNode.Leaf(lens0), LayoutNode.Leaf(lens1));
            string json = new SavedLayout("l1", "Lenses", tree).ToJson();
            T.Check(json.Contains("\"b\":{\"camera\":{\"channel\":3,\"device\":\"dev-a\",\"lens\":1}}", StringComparison.Ordinal), json);
            T.Check(json.Contains("\"a\":{\"camera\":{\"channel\":3,\"device\":\"dev-a\"}}", StringComparison.Ordinal), json);
            T.Check(SavedLayout.List($"[{json}]")[0].Root == tree);
            T.Check(SavedLayout.List("""[{"id":"o","name":"Old","root":{"camera":{"device":"dev-a","channel":3}}}]""")[0].Root == LayoutNode.Leaf(lens0));
            foreach (string bad in new[] { "2", "-1", "1.5", "true", "\"1\"" })
            {
                T.Equal(0, SavedLayout.List("""[{"id":"b","name":"Bad","root":{"camera":{"device":"dev-a","channel":3,"lens":""" + bad + "}}}]").Count, $"lens {bad}");
            }
        });

        T.Run("json: byte for byte what the macOS app writes", () =>
        {
            const string key = "d-0123456789abcdef";
            LayoutNode tree = LayoutNode.Split(SplitAxis.Row, 2.0 / 3, LayoutNode.Leaf(Cam(3, key)),
                LayoutNode.Split(SplitAxis.Col, 0.34, LayoutNode.Leaf(null), LayoutNode.Leaf(Cam(4, key, 1))));
            tree = T.NotNull(tree.SettingMember(P("a"), "quality", "\"clear\""));
            T.Equal(MacSample, new SavedLayout("layout-0a1b2c3d", "Mac Sample", tree, 2).ToJson());

            SavedLayout future = SavedLayout.List($"[{WithUnknownMembers}]")[0];
            T.Equal(MacSwapped, (future with { Root = T.NotNull(future.Root.Swapping(P("a"), P("b"))) }).ToJson());

            T.Equal(MacPreset, new SavedLayout("p", "Presets", LayoutPreset.ThreeByThree.Tree([Cam(3, key), Cam(4, key, 1)])).ToJson());

            // And each reads back to the same layout and the same text.
            foreach (string mac in new[] { MacSample, MacSwapped, MacPreset })
            {
                SavedLayout read = SavedLayout.Parse(mac);
                T.Equal(mac, read.ToJson());
            }
            T.Equal(Cam(4, key, 1), T.NotNull(SavedLayout.Parse(MacSample).Root[P("bb")]).Camera);
        });
    }
}
