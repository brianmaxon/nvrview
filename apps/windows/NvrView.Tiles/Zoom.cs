// A tile's digital zoom (docs/UX.md "Digital zoom"): where the zoomed picture
// is drawn in its tile and which part of the picture it shows. Pure geometry,
// the same rule and test vectors as the macOS app's TileZoom (NVRCore
// ViewerInput.swift).
//
// The rule, per axis, with b the picture's 1x size as a fraction of the tile
// (ZoomBase), s the scale and c the center (the picture point at the tile's
// center, a fraction of the picture): the picture is drawn L = s * b tiles
// long, starting at u0 (tile units, 0..1):
//
// - L <= 1 (still smaller than the tile): centered, u0 = (1 - L) / 2; the
//   rest of the axis is bars.
// - L > 1: u0 = 0.5 - c * L, clamped to 1 - L .. 0 so the picture always
//   covers the tile on that axis (panning stops at its edges).
//
// The destination is the picture's span u0 .. u0 + L intersected with the
// tile (0 .. 1), and the source the matching part of the picture, (d - u0) / L.
// So zooming in grows the picture over the bars until it fills each axis,
// then crops; at 1x the tile looks as it does unzoomed.

namespace NvrView.Tiles;

/// <summary>
/// How a picture sits in its tile at 1x: its size as a multiple of the tile's on each axis.
/// Fitted (letterboxed) one axis is 1 and the other at most 1; filled (cropped) one is 1 and the
/// other at least 1. <see cref="Whole"/> is a picture exactly the tile's shape.
/// </summary>
public readonly record struct ZoomBase(double Width, double Height)
{
    public static readonly ZoomBase Whole = new(1, 1);

    /// <summary>A picture (any unit; only its shape counts) fitted, or filled, in a tile.</summary>
    public static ZoomBase Of(double pictureWidth, double pictureHeight, double tileWidth, double tileHeight, bool fill = false)
    {
        if (pictureWidth <= 0 || pictureHeight <= 0 || tileWidth <= 0 || tileHeight <= 0)
        {
            return Whole;
        }
        double sx = tileWidth / pictureWidth, sy = tileHeight / pictureHeight;
        double k = fill ? Math.Max(sx, sy) : Math.Min(sx, sy);
        // The axis that touches the tile's edges is exactly 1.
        return new(sx == k ? 1 : pictureWidth * k / tileWidth, sy == k ? 1 : pictureHeight * k / tileHeight);
    }
}

/// <summary>
/// A tile's digital zoom: <see cref="Scale"/> times the picture's 1x size (1 shows the tile as it
/// is unzoomed) and the center, the picture point at the tile's center as a fraction of the full
/// picture. Independent of the picture's resolution, so the sub-stream and the main stream show
/// the same region. Unit points are positions in the tile (0..1, origin top left).
/// </summary>
public readonly record struct TileZoom(double Scale, double CenterX, double CenterY)
{
    public const double MaxScale = 8;
    public static readonly TileZoom None = new(1, 0.5, 0.5);

    public bool IsZoomed => Scale > 1.0001;

    // One axis: where the picture starts (tile units) and how long it is (tiles).
    private static (double Start, double Length) Span(double s, double b, double c)
    {
        double length = s * b;
        if (length <= 1)
        {
            return ((1 - length) / 2, length);
        }
        return (Math.Min(Math.Max(0.5 - c * length, 1 - length), 0), length);
    }

    private ((double Start, double Length) X, (double Start, double Length) Y) Spans(ZoomBase b)
    {
        double s = Math.Clamp(Scale, 1, MaxScale);
        return (Span(s, b.Width, CenterX), Span(s, b.Height, CenterY));
    }

    /// <summary>Where the picture is drawn, as a fraction of the tile: the zoomed picture intersected with the tile.</summary>
    public UnitRect Destination(ZoomBase b)
    {
        var (x, y) = Spans(b);
        double x0 = Math.Max(x.Start, 0), x1 = Math.Min(x.Start + x.Length, 1);
        double y0 = Math.Max(y.Start, 0), y1 = Math.Min(y.Start + y.Length, 1);
        return new(x0, y0, Math.Max(x1 - x0, 0), Math.Max(y1 - y0, 0));
    }

    /// <summary>The region of the picture shown (fraction of the full picture): the part under <see cref="Destination"/>.</summary>
    public UnitRect Visible(ZoomBase b)
    {
        var (x, y) = Spans(b);
        if (x.Length <= 0 || y.Length <= 0)
        {
            return new(0, 0, 1, 1);
        }
        UnitRect d = Destination(b);
        return new((d.X - x.Start) / x.Length, (d.Y - y.Start) / y.Length, d.Width / x.Length, d.Height / y.Length);
    }

    /// <summary>The scale clamped to 1..MaxScale and the center to where the picture covers the tile (centered on an axis it does not fill).</summary>
    public TileZoom Clamped(ZoomBase b)
    {
        double s = Math.Clamp(Scale, 1, MaxScale);
        if (s <= 1.0001 || b.Width <= 0 || b.Height <= 0)
        {
            return None;
        }
        var (x, y) = (this with { Scale = s }).Spans(b);
        return new(s, (0.5 - x.Start) / x.Length, (0.5 - y.Start) / y.Length);
    }

    /// <summary>The picture point (fraction of the full picture) under tile unit point (ux, uy); outside 0..1 over a bar.</summary>
    public (double X, double Y) PicturePoint(double ux, double uy, ZoomBase b)
    {
        var (x, y) = Spans(b);
        if (x.Length <= 0 || y.Length <= 0)
        {
            return (ux, uy);
        }
        return ((ux - x.Start) / x.Length, (uy - y.Start) / y.Length);
    }

    /// <summary>
    /// Zoomed to <paramref name="newScale"/>, keeping picture point (qx, qy) under tile unit point
    /// (ux, uy) (zooming around the pointer), then clamped: on an axis the picture does not fill yet
    /// it stays centered instead.
    /// </summary>
    public TileZoom ZoomedTo(double newScale, double qx, double qy, double ux, double uy, ZoomBase b)
    {
        double s = Math.Clamp(newScale, 1, MaxScale);
        if (s <= 1.0001 || b.Width <= 0 || b.Height <= 0)
        {
            return None;
        }
        double lx = s * b.Width, ly = s * b.Height;
        return new TileZoom(s, qx - (ux - 0.5) / lx, qy - (uy - 0.5) / ly).Clamped(b);
    }

    /// <summary>Zoomed by <paramref name="factor"/> around tile unit point (ux, uy).</summary>
    public TileZoom ZoomedBy(double factor, double ux, double uy, ZoomBase b)
    {
        (double qx, double qy) = PicturePoint(ux, uy, b);
        return ZoomedTo(Scale * factor, qx, qy, ux, uy, b);
    }

    /// <summary>Panned so the picture follows a drag of (dx, dy) tile units (a drag to the right moves the picture right), clamped.</summary>
    public TileZoom Panned(double dx, double dy, ZoomBase b)
    {
        if (!IsZoomed || b.Width <= 0 || b.Height <= 0)
        {
            return this;
        }
        TileZoom z = Clamped(b);
        return new TileZoom(z.Scale, z.CenterX - dx / (z.Scale * b.Width), z.CenterY - dy / (z.Scale * b.Height)).Clamped(b);
    }
}
