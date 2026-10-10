using NvrView.Tiles;

namespace NvrView.Shell.Tests;

/// <summary>Layout templates and icon geometry. Pure; no drawing.</summary>
internal static class LayoutTemplateTests
{
    private static CameraRef Cam(int i) => new("dev-a", i);

    private static bool Near(double a, double b) => Math.Abs(a - b) < 1e-9;

    private static IEnumerable<double> Ratios(LayoutNode n) =>
        n is LayoutSplit s ? Ratios(s.A).Concat(Ratios(s.B)).Prepend(s.Ratio) : [];

    public static void Run()
    {
        T.Run("templates: each has its tile count and a storable shape", () =>
        {
            T.Equal(Enum.GetValues<LayoutTemplate>().Length, LayoutTemplates.All.Count);
            foreach (LayoutTemplate t in LayoutTemplates.All)
            {
                LayoutNode shape = t.Shape();
                T.Equal(t.TileCount(), shape.LeafCount, $"{t}");
                T.Check(t.Title().StartsWith($"{t.TileCount()} ", StringComparison.Ordinal), $"{t} title");
                T.Check(shape.Depth <= LayoutNode.MaxDepth && shape.LeafCount <= LayoutNode.MaxLeaves, $"{t} limits");
                T.Check(Ratios(shape).All(r => r is >= LayoutNode.MinRatio and <= LayoutNode.MaxRatio), $"{t} ratios");
                LayoutNode filled = t.Tree([.. Enumerable.Range(0, 5).Select(Cam)]);
                var layout = new SavedLayout("l1", t.Title(), filled);
                T.Check(SavedLayout.Parse(layout.ToJson()).Root == filled, $"{t} round trip");
            }
        });

        T.Run("templates: the tiles cover the unit square without overlap", () =>
        {
            foreach (LayoutTemplate t in LayoutTemplates.All)
            {
                List<TileRect> rects = t.Shape().UnitRects();
                T.Equal(t.TileCount(), rects.Count, $"{t}");
                T.Check(Near(rects.Sum(r => r.Width * r.Height), 1), $"{t} area");
                T.Check(rects.All(r => r.X >= -1e-9 && r.Y >= -1e-9 && r.Right <= 1 + 1e-9 && r.Bottom <= 1 + 1e-9), $"{t} inside");
                for (int i = 0; i < rects.Count; i++)
                {
                    for (int j = i + 1; j < rects.Count; j++)
                    {
                        TileRect a = rects[i], b = rects[j];
                        double w = Math.Min(a.Right, b.Right) - Math.Max(a.X, b.X);
                        double h = Math.Min(a.Bottom, b.Bottom) - Math.Max(a.Y, b.Y);
                        T.Check(w <= 1e-9 || h <= 1e-9 || w * h < 1e-9, $"{t} tiles {i} and {j} overlap");
                    }
                }
            }
        });

        T.Run("templates: the large tiles are the size they are named for", () =>
        {
            TileRect Big(LayoutTemplate t) => t.Shape().UnitRects()[0];
            T.Check(Near(Big(LayoutTemplate.Big1Small5).Width, 2.0 / 3) && Near(Big(LayoutTemplate.Big1Small5).Height, 2.0 / 3));
            T.Check(Near(Big(LayoutTemplate.Big1Small7).Width, 0.75) && Near(Big(LayoutTemplate.Big1Small7).Height, 0.75));
            T.Check(Near(Big(LayoutTemplate.Big1Small12).Width, 0.5) && Near(Big(LayoutTemplate.Big1Small12).Height, 0.5));
            T.Equal(new TileRect(0.5, 0, 0.5, 0.5), LayoutTemplate.Big2Small8.Shape().UnitRects()[1]);
            T.Check(Near(Big(LayoutTemplate.Big1Small31).Width, 2.0 / 7) && Near(Big(LayoutTemplate.Big1Small31).Height, 2.0 / 5));
            foreach (LayoutTemplate t in new[] { LayoutTemplate.Grid4, LayoutTemplate.Grid9, LayoutTemplate.Grid16, LayoutTemplate.Grid25, LayoutTemplate.Grid36 })
            {
                double n = Math.Sqrt(t.TileCount());
                T.Check(t.Shape().UnitRects().All(r => Near(r.Width, 1 / n) && Near(r.Height, 1 / n)), $"{t} is even");
            }
        });

        T.Run("templates: cameras fill in reading order, the rest stay empty", () =>
        {
            T.Equal("dev-a/0,dev-a/1,,", string.Join(",", LayoutTemplate.Grid4.Tree([Cam(0), Cam(1)]).CameraList.Select(c => c is { } x ? $"{x.Device}/{x.Channel}" : "")));
            T.Equal(2, LayoutTemplate.SideBySide.Tree([.. Enumerable.Range(0, 5).Select(Cam)]).CameraList.Count);
            T.Equal(1, LayoutTemplate.Single.Tree().LeafCount);
            List<int> counts = [.. LayoutTemplates.All.Select(t => t.TileCount())];
            T.Check(counts.SequenceEqual(counts.Order()), "ordered by size");
        });

        T.Run("icons: tiles stay inside with a gap between them", () =>
        {
            foreach (LayoutTemplate t in LayoutTemplates.All)
            {
                List<TileRect> rects = t.Shape().IconRects(18, 12, 1);
                T.Equal(t.TileCount(), rects.Count, $"{t}");
                T.Check(rects.All(r => r.X >= 0 && r.Y >= 0 && r.Right <= 18 + 1e-9 && r.Bottom <= 12 + 1e-9 && r.Width >= 1 - 1e-9 && r.Height >= 1 - 1e-9), $"{t} inside");
                for (int i = 0; i < rects.Count; i++)
                {
                    for (int j = i + 1; j < rects.Count; j++)
                    {
                        T.Check(!rects[i].Intersects(rects[j]), $"{t} tiles {i} and {j}");
                    }
                }
            }
            List<TileRect> two = LayoutTemplate.SideBySide.Shape().IconRects(18, 12, 2);
            T.Check(Near(two[1].X - two[0].Right, 2));
        });

        T.Run("icons: a saved layout draws from its own tree; a group is a balanced grid", () =>
        {
            LayoutNode tree = LayoutNode.Split(SplitAxis.Row, 0.25, LayoutNode.Leaf(Cam(0)), LayoutNode.Split(SplitAxis.Col, 0.5, LayoutNode.Empty, LayoutNode.Empty));
            T.Equal(
                string.Join("|", new[] { new TileRect(0, 0, 0.25, 1), new TileRect(0.25, 0, 0.75, 0.5), new TileRect(0.25, 0.5, 0.75, 0.5) }),
                string.Join("|", tree.UnitRects()));
            T.Equal(4, LayoutNode.Balanced(Enumerable.Range(0, 4).Select(Cam)).UnitRects().Count);
            T.Equal(new TileRect(0, 0, 1, 1), LayoutNode.Balanced(Array.Empty<CameraRef>()).UnitRects()[0]);
        });
    }
}
