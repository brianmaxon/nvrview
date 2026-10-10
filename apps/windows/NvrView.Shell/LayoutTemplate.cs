// Ready-made split trees ("New layout from template") and the geometry of
// layout icons. Pure: no drawing here. docs/UX.md "Layouts" describes the
// shapes; apps/macos/Sources/NVRCore/LayoutTemplate.swift mirrors them.

using NvrView.Tiles;

namespace NvrView.Shell;

/// <summary>A rectangle in layout space (the unit square) or icon space (points).</summary>
public readonly record struct TileRect(double X, double Y, double Width, double Height)
{
    public double Right => X + Width;

    public double Bottom => Y + Height;

    public bool Intersects(TileRect o) => X < o.Right && o.X < Right && Y < o.Bottom && o.Y < Bottom;
}

public static class LayoutGeometryIcons
{
    /// <summary>Every tile's rectangle in the unit square, in reading order. No rounding and no gaps.</summary>
    public static List<TileRect> UnitRects(this LayoutNode root)
    {
        var list = new List<TileRect>();
        Walk(root, new TileRect(0, 0, 1, 1), list);
        return list;

        static void Walk(LayoutNode n, TileRect r, List<TileRect> into)
        {
            if (n is not LayoutSplit s)
            {
                into.Add(r);
                return;
            }
            if (s.Axis == SplitAxis.Row)
            {
                double w = r.Width * s.Ratio;
                Walk(s.A, new TileRect(r.X, r.Y, w, r.Height), into);
                Walk(s.B, new TileRect(r.X + w, r.Y, r.Width - w, r.Height), into);
            }
            else
            {
                double h = r.Height * s.Ratio;
                Walk(s.A, new TileRect(r.X, r.Y, r.Width, h), into);
                Walk(s.B, new TileRect(r.X, r.Y + h, r.Width, r.Height - h), into);
            }
        }
    }

    /// <summary>
    /// The tiles of a layout icon <paramref name="width"/> x <paramref name="height"/> points: each tile's unit
    /// rectangle scaled and pulled in by half of <paramref name="gap"/> on every side, so neighbors are
    /// <paramref name="gap"/> apart. A tile never shrinks below one point.
    /// </summary>
    public static List<TileRect> IconRects(this LayoutNode root, double width, double height, double gap) =>
    [
        .. root.UnitRects().Select(u =>
        {
            var r = new TileRect(u.X * width, u.Y * height, u.Width * width, u.Height * height);
            double dx = Math.Min(gap / 2, Math.Max((r.Width - 1) / 2, 0));
            double dy = Math.Min(gap / 2, Math.Max((r.Height - 1) / 2, 0));
            return new TileRect(r.X + dx, r.Y + dy, r.Width - 2 * dx, r.Height - 2 * dy);
        }),
    ];
}

/// <summary>A ready-made arrangement; the first cameras of a list fill its tiles in reading order.</summary>
public enum LayoutTemplate
{
    Single,
    SideBySide,
    Stacked,
    Grid4,
    Grid9,
    Grid16,
    Grid25,
    Grid36,
    Big1Small5,
    Big1Small7,
    Big2Small8,
    Big1Small12,
    Big2Small12,
    Big1Small31,
}

public static class LayoutTemplates
{
    /// <summary>Smallest first (ties by name).</summary>
    public static IReadOnlyList<LayoutTemplate> All { get; } =
        [.. Enum.GetValues<LayoutTemplate>().OrderBy(t => t.TileCount()).ThenBy(t => t.ToString(), StringComparer.Ordinal)];

    public static int TileCount(this LayoutTemplate t) => t switch
    {
        LayoutTemplate.Single => 1,
        LayoutTemplate.SideBySide or LayoutTemplate.Stacked => 2,
        LayoutTemplate.Grid4 => 4,
        LayoutTemplate.Big1Small5 => 6,
        LayoutTemplate.Big1Small7 => 8,
        LayoutTemplate.Grid9 => 9,
        LayoutTemplate.Big2Small8 => 10,
        LayoutTemplate.Big1Small12 => 13,
        LayoutTemplate.Big2Small12 => 14,
        LayoutTemplate.Grid16 => 16,
        LayoutTemplate.Grid25 => 25,
        LayoutTemplate.Big1Small31 => 32,
        _ => 36,
    };

    /// <summary>The menu title: the tile count, then the shape.</summary>
    public static string Title(this LayoutTemplate t) => t switch
    {
        LayoutTemplate.Single => "1 · Single",
        LayoutTemplate.SideBySide => "2 · Side by side",
        LayoutTemplate.Stacked => "2 · Stacked",
        LayoutTemplate.Grid4 => "4 · Grid 2 × 2",
        LayoutTemplate.Grid9 => "9 · Grid 3 × 3",
        LayoutTemplate.Grid16 => "16 · Grid 4 × 4",
        LayoutTemplate.Grid25 => "25 · Grid 5 × 5",
        LayoutTemplate.Grid36 => "36 · Grid 6 × 6",
        LayoutTemplate.Big1Small5 => "6 · One large, 5 small",
        LayoutTemplate.Big1Small7 => "8 · One large, 7 small",
        LayoutTemplate.Big2Small8 => "10 · Two large, 8 small",
        LayoutTemplate.Big1Small12 => "13 · One large, 12 small",
        LayoutTemplate.Big2Small12 => "14 · Two large, 12 small",
        _ => "32 · One large, 31 small",
    };

    /// <summary>The shape with empty tiles.</summary>
    public static LayoutNode Shape(this LayoutTemplate t)
    {
        LayoutNode e = LayoutNode.Empty;
        LayoutNode Row(int n) => LayoutNode.Chain(SplitAxis.Row, [.. Enumerable.Repeat(e, n)]);
        LayoutNode Rows(int n, int columns) => LayoutNode.Chain(SplitAxis.Col, [.. Enumerable.Repeat(Row(columns), n)]);
        LayoutNode Col(int n) => LayoutNode.Chain(SplitAxis.Col, [.. Enumerable.Repeat(e, n)]);
        // Two parts along an axis, the first `first` of `total` units.
        LayoutNode Cut(SplitAxis axis, double first, double total, LayoutNode a, LayoutNode b) => LayoutNode.Split(axis, first / total, a, b);
        return t switch
        {
            LayoutTemplate.Single => e,
            LayoutTemplate.SideBySide => Row(2),
            LayoutTemplate.Stacked => Col(2),
            LayoutTemplate.Grid4 => Rows(2, 2),
            LayoutTemplate.Grid9 => Rows(3, 3),
            LayoutTemplate.Grid16 => Rows(4, 4),
            LayoutTemplate.Grid25 => Rows(5, 5),
            LayoutTemplate.Grid36 => Rows(6, 6),
            // 3 x 3 units: a 2 x 2 tile, a column of 2, a row of 3.
            LayoutTemplate.Big1Small5 => Cut(SplitAxis.Col, 2, 3, Cut(SplitAxis.Row, 2, 3, e, Col(2)), Row(3)),
            // 4 x 4 units: a 3 x 3 tile, a column of 3, a row of 4.
            LayoutTemplate.Big1Small7 => Cut(SplitAxis.Col, 3, 4, Cut(SplitAxis.Row, 3, 4, e, Col(3)), Row(4)),
            // Two large tiles across the top half, two rows of 4 below.
            LayoutTemplate.Big2Small8 => Cut(SplitAxis.Col, 1, 2, Row(2), Rows(2, 4)),
            // 4 x 4 units: a 2 x 2 tile, a 2 x 2 block of small tiles, two rows of 4.
            LayoutTemplate.Big1Small12 => Cut(SplitAxis.Col, 1, 2, Cut(SplitAxis.Row, 1, 2, e, Rows(2, 2)), Rows(2, 4)),
            // Two large tiles across the top (2 of 5 rows), three rows of 4 below.
            LayoutTemplate.Big2Small12 => Cut(SplitAxis.Col, 2, 5, Row(2), Rows(3, 4)),
            // 7 x 5 units: a 2 x 2 tile with a 5 x 2 block of small tiles beside it, three rows of 7 below.
            _ => Cut(SplitAxis.Col, 2, 5, Cut(SplitAxis.Row, 2, 7, e, Rows(2, 5)), Rows(3, 7)),
        };
    }

    /// <summary>The shape with <paramref name="cameras"/> in its tiles in reading order; the rest stay empty.</summary>
    public static LayoutNode Tree(this LayoutTemplate t, IReadOnlyList<CameraRef>? cameras = null)
    {
        cameras ??= [];
        int next = 0;
        return Fill(t.Shape());

        LayoutNode Fill(LayoutNode n)
        {
            if (n is LayoutSplit s)
            {
                LayoutNode a = Fill(s.A);
                return LayoutNode.Split(s.Axis, s.Ratio, a, Fill(s.B), s.Extras);
            }
            CameraRef? c = next < cameras.Count ? cameras[next] : null;
            next++;
            return LayoutNode.Leaf(c, ((LayoutLeaf)n).Extras);
        }
    }
}
