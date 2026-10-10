// Where a split tree's tiles and dividers sit in a rectangle, how a picture
// fits a tile, and how the focused tile's rectangle animates. Pure, so the
// designer's hit testing and the renderer's rectangles are tested headless.
// Rectangles are in any unit (DIPs in the app), origin top left.

namespace NvrView.Shell;

/// <summary>A rectangle, origin top left.</summary>
public readonly record struct Rect(double X, double Y, double Width, double Height)
{
    public double Right => X + Width;

    public double Bottom => Y + Height;

    public double MidX => X + Width / 2;

    public double MidY => Y + Height / 2;

    public bool Contains(double px, double py) => px >= X && py >= Y && px < Right && py < Bottom;

    public bool Contains(Rect r) => r.X >= X - 1e-9 && r.Y >= Y - 1e-9 && r.Right <= Right + 1e-9 && r.Bottom <= Bottom + 1e-9;

    public Rect Inflate(double dx, double dy) => new(X - dx, Y - dy, Width + 2 * dx, Height + 2 * dy);

    /// <summary>Linear interpolation, <paramref name="t"/> clamped to 0..1.</summary>
    public static Rect Lerp(Rect a, Rect b, double t)
    {
        double k = Math.Clamp(t, 0, 1);
        return new(a.X + (b.X - a.X) * k, a.Y + (b.Y - a.Y) * k, a.Width + (b.Width - a.Width) * k, a.Height + (b.Height - a.Height) * k);
    }
}

/// <summary>One tile's rectangle.</summary>
public readonly record struct LeafFrame(NodePath Path, CameraRef? Camera, Rect Rect);

/// <summary>One divider: the gap between a split's two children, and the split's whole rectangle (what a drag is measured against).</summary>
public readonly record struct DividerFrame(NodePath Path, SplitAxis Axis, Rect Rect, Rect Region)
{
    /// <summary>Where the pointer can grab it: the gap widened to at least <paramref name="minimum"/>.</summary>
    public Rect HitRect(double minimum = 8) => Axis == SplitAxis.Row
        ? Rect.Inflate(Math.Max(minimum - Rect.Width, 0) / 2, 0)
        : Rect.Inflate(0, Math.Max(minimum - Rect.Height, 0) / 2);
}

public sealed class LayoutFrames(IReadOnlyList<LeafFrame> leaves, IReadOnlyList<DividerFrame> dividers)
{
    public static readonly LayoutFrames None = new([], []);

    public IReadOnlyList<LeafFrame> Leaves { get; } = leaves;

    public IReadOnlyList<DividerFrame> Dividers { get; } = dividers;

    public LeafFrame? LeafAt(double x, double y)
    {
        foreach (LeafFrame f in Leaves)
        {
            if (f.Rect.Contains(x, y))
            {
                return f;
            }
        }
        return null;
    }

    public LeafFrame? Leaf(NodePath path)
    {
        foreach (LeafFrame f in Leaves)
        {
            if (f.Path == path)
            {
                return f;
            }
        }
        return null;
    }

    /// <summary>The divider under the point, the innermost (smallest) first.</summary>
    public DividerFrame? DividerAt(double x, double y, double minimum = 8)
    {
        DividerFrame? best = null;
        double bestArea = double.MaxValue;
        foreach (DividerFrame d in Dividers)
        {
            if (d.HitRect(minimum).Contains(x, y) && d.Region.Width * d.Region.Height < bestArea)
            {
                best = d;
                bestArea = d.Region.Width * d.Region.Height;
            }
        }
        return best;
    }
}

public static class LayoutGeometry
{
    /// <summary>Tile and divider rectangles in <paramref name="bounds"/>, <paramref name="gap"/> units between tiles (sizes rounded to whole units).</summary>
    public static LayoutFrames Frames(this LayoutNode tree, Rect bounds, double gap = 0)
    {
        var leaves = new List<LeafFrame>();
        var dividers = new List<DividerFrame>();
        Walk(tree, NodePath.Root, bounds);
        return new LayoutFrames(leaves, dividers);

        void Walk(LayoutNode n, NodePath path, Rect r)
        {
            switch (n)
            {
                case LayoutLeaf l:
                    leaves.Add(new LeafFrame(path, l.Cam, r));
                    break;
                case LayoutSplit s when s.Axis == SplitAxis.Row:
                {
                    double g = Math.Min(gap, r.Width);
                    double aw = Math.Round((r.Width - g) * s.Ratio, MidpointRounding.AwayFromZero);
                    var ra = new Rect(r.X, r.Y, aw, r.Height);
                    var rb = new Rect(r.X + aw + g, r.Y, Math.Max(r.Width - aw - g, 0), r.Height);
                    dividers.Add(new DividerFrame(path, s.Axis, new Rect(ra.Right, r.Y, g, r.Height), r));
                    Walk(s.A, path.Append(Side.A), ra);
                    Walk(s.B, path.Append(Side.B), rb);
                    break;
                }
                case LayoutSplit s:
                {
                    double g = Math.Min(gap, r.Height);
                    double ah = Math.Round((r.Height - g) * s.Ratio, MidpointRounding.AwayFromZero);
                    var ra = new Rect(r.X, r.Y, r.Width, ah);
                    var rb = new Rect(r.X, r.Y + ah + g, r.Width, Math.Max(r.Height - ah - g, 0));
                    dividers.Add(new DividerFrame(path, s.Axis, new Rect(r.X, ra.Bottom, r.Width, g), r));
                    Walk(s.A, path.Append(Side.A), ra);
                    Walk(s.B, path.Append(Side.B), rb);
                    break;
                }
            }
        }
    }

    /// <summary>The ratio that puts the divider's middle at the point (clamped).</summary>
    public static double RatioDragging(DividerFrame divider, double x, double y, double gap = 0)
    {
        Rect r = divider.Region;
        if (divider.Axis == SplitAxis.Row)
        {
            double g = Math.Min(gap, r.Width);
            return r.Width - g > 0 ? LayoutNode.Clamp((x - r.X - g / 2) / (r.Width - g)) : 0.5;
        }
        double gh = Math.Min(gap, r.Height);
        return r.Height - gh > 0 ? LayoutNode.Clamp((y - r.Y - gh / 2) / (r.Height - gh)) : 0.5;
    }

    /// <summary>
    /// Fits a picture in a cell, letterboxed: the destination inside the cell
    /// with the picture's aspect ratio, centered, in whole units.
    /// </summary>
    public static Rect Fit(double pictureWidth, double pictureHeight, Rect cell)
    {
        if (pictureWidth <= 0 || pictureHeight <= 0 || cell.Width <= 0 || cell.Height <= 0)
        {
            return cell;
        }
        double pa = pictureWidth / pictureHeight, ca = cell.Width / cell.Height;
        if (pa > ca)
        {
            double h = Math.Round(cell.Width / pa);
            return new Rect(cell.X, Math.Floor(cell.Y + (cell.Height - h) / 2), cell.Width, h);
        }
        double w = Math.Round(cell.Height * pa);
        return new Rect(Math.Floor(cell.X + (cell.Width - w) / 2), cell.Y, w, cell.Height);
    }

    /// <summary>Smoothstep ease, <paramref name="t"/> clamped to 0..1.</summary>
    public static double Ease(double t)
    {
        double x = Math.Clamp(t, 0, 1);
        return x * x * (3 - 2 * x);
    }

    /// <summary>
    /// The tile at the point and direction's neighbor for the arrow keys
    /// (docs/UX.md "Keyboard"): only tiles whose center lies beyond the
    /// selected tile's edge in that direction; those touching that edge first
    /// (nearest edge, then the one across from the center), otherwise the
    /// nearest center, counting the offset across twice.
    /// </summary>
    public static int Neighbor(IReadOnlyList<Rect> tiles, int from, int dx, int dy)
    {
        Rect s = tiles[from];
        int best = -1;
        (int Touch, double A, double B) bestKey = (2, double.MaxValue, double.MaxValue);
        for (int i = 0; i < tiles.Count; i++)
        {
            Rect t = tiles[i];
            if (i == from || t.Width <= 0 || t.Height <= 0)
            {
                continue;
            }
            double cx = t.MidX, cy = t.MidY;
            bool beyond = dx > 0 ? cx > s.Right : dx < 0 ? cx < s.X : dy > 0 ? cy > s.Bottom : cy < s.Y;
            if (!beyond)
            {
                continue;
            }
            // Touching: shares the edge (within a gap) and overlaps across.
            const double slack = 12;
            bool touching = dx != 0
                ? (dx > 0 ? Math.Abs(t.X - s.Right) <= slack : Math.Abs(t.Right - s.X) <= slack) && t.Y < s.Bottom && t.Bottom > s.Y
                : (dy > 0 ? Math.Abs(t.Y - s.Bottom) <= slack : Math.Abs(t.Bottom - s.Y) <= slack) && t.X < s.Right && t.Right > s.X;
            (int, double, double) key;
            if (touching)
            {
                double edge = dx != 0 ? (dx > 0 ? t.X - s.Right : s.X - t.Right) : (dy > 0 ? t.Y - s.Bottom : s.Y - t.Bottom);
                bool across = dx != 0 ? t.Y <= s.MidY && t.Bottom > s.MidY : t.X <= s.MidX && t.Right > s.MidX;
                key = (0, Math.Abs(edge), across ? 0 : 1);
            }
            else
            {
                double along = dx != 0 ? Math.Abs(cx - s.MidX) : Math.Abs(cy - s.MidY);
                double off = dx != 0 ? Math.Abs(cy - s.MidY) : Math.Abs(cx - s.MidX);
                key = (1, along + 2 * off, 0);
            }
            if (key.CompareTo(bestKey) < 0)
            {
                bestKey = key;
                best = i;
            }
        }
        return best;
    }
}
