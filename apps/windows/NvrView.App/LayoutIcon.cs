// A layout's shape as a small monochrome picture: each tile a rounded
// rectangle with a gap between tiles, from the layout's own tree. A PathIcon,
// so it takes the foreground of its row or menu item. The geometry is
// LayoutGeometryIcons.IconRects (tested in NvrView.Shell.Tests); this only
// draws it.

using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using NvrView.Shell;
using NvrView.Tiles;

namespace NvrView.App;

internal static class LayoutIcon
{
    public const double Width = 18;
    public const double Height = 12;

    public static PathIcon Create(LayoutNode tree)
    {
        double gap = tree.LeafCount > 16 ? 0.75 : 1.5;
        var path = new PathGeometry { FillRule = FillRule.Nonzero };
        foreach (TileRect r in tree.IconRects(Width, Height, gap))
        {
            path.Figures.Add(RoundedRect(r, Math.Min(1.5, Math.Min(r.Width, r.Height) / 2)));
        }
        return new PathIcon { Data = path, Width = Width, Height = Height };
    }

    private static PathFigure RoundedRect(TileRect r, double k)
    {
        Windows.Foundation.Point P(double x, double y) => new(x, y);
        ArcSegment Arc(double x, double y) => new() { Point = P(x, y), Size = new Windows.Foundation.Size(k, k), SweepDirection = SweepDirection.Clockwise };
        var figure = new PathFigure { StartPoint = P(r.X + k, r.Y), IsClosed = true, IsFilled = true };
        figure.Segments.Add(new LineSegment { Point = P(r.Right - k, r.Y) });
        figure.Segments.Add(Arc(r.Right, r.Y + k));
        figure.Segments.Add(new LineSegment { Point = P(r.Right, r.Bottom - k) });
        figure.Segments.Add(Arc(r.Right - k, r.Bottom));
        figure.Segments.Add(new LineSegment { Point = P(r.X + k, r.Bottom) });
        figure.Segments.Add(Arc(r.X, r.Bottom - k));
        figure.Segments.Add(new LineSegment { Point = P(r.X, r.Y + k) });
        figure.Segments.Add(Arc(r.X + k, r.Y));
        return figure;
    }

    /// <summary>The icon for <paramref name="count"/> cameras in an even grid (groups).</summary>
    public static PathIcon ForGrid(int count) => Create(LayoutNode.Balanced(Enumerable.Repeat<CameraRef?>(null, Math.Max(count, 1))));
}
