// Tests of the digital zoom's geometry (NvrView.Tiles/Zoom.cs). The vectors
// mirror the macOS app's ZoomTests (apps/macos/Tests/NVRCoreTests/
// ViewerInputTests.swift; parity: zoom-fills-tile).

using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class ZoomTests
{
    public static IEnumerable<(string, Action)> All() =>
    [
        ("zoom: the base is the picture as a multiple of the tile", Base),
        ("zoom: 1x looks as unzoomed", OneX),
        ("zoom: zooming grows the picture over the bars", GrowsOverTheBars),
        ("zoom: the bars go at the fill threshold", FillThreshold),
        ("zoom: a tall picture grows sideways", TallPicture),
        ("zoom: the scale is clamped 1 to 8", ScaleClamped),
        ("zoom: the point under the pointer stays", KeepsThePoint),
        ("zoom: near an edge it stays inside the picture", NearAnEdge),
        ("zoom: panning follows the drag and is clamped", Panning),
        ("zoom: fill zooms from the crop and pans to the picture's edges", Fill),
        ("zoom: fill at 1x crops the picture to the tile", FillOneX),
        ("zoom: the map sits in the picture's bottom left corner", MapFrame),
        ("zoom: the map marks the visible region", MapRegion),
    ];

    private static readonly ZoomBase WideInTall = ZoomBase.Of(1600, 900, 400, 400);
    private static readonly ZoomBase TallInWide = ZoomBase.Of(900, 1600, 800, 450);
    private static readonly ZoomBase Filled = ZoomBase.Of(1600, 900, 300, 300, fill: true);

    private static void Close(UnitRect expected, UnitRect actual, string what, double e = 1e-6) =>
        Check.True(
            Math.Abs(expected.X - actual.X) < e && Math.Abs(expected.Y - actual.Y) < e
            && Math.Abs(expected.Width - actual.Width) < e && Math.Abs(expected.Height - actual.Height) < e,
            $"{what}: expected {expected}, got {actual}");

    private static void Close((double X, double Y) expected, (double X, double Y) actual, string what, double e = 1e-9) =>
        Check.True(Math.Abs(expected.X - actual.X) < e && Math.Abs(expected.Y - actual.Y) < e, $"{what}: expected {expected}, got {actual}");

    private static void Base()
    {
        Check.Equal(new ZoomBase(1, 0.5625), WideInTall, "wide in tall");
        Check.Equal(new ZoomBase(0.31640625, 1), TallInWide, "tall in wide");
        Check.True(Math.Abs(Filled.Width - 16.0 / 9) < 1e-12 && Filled.Height == 1, "filled");
        Check.Equal(ZoomBase.Whole, ZoomBase.Of(0, 0, 10, 10), "no picture");
    }

    private static void OneX()
    {
        Check.True(!TileZoom.None.IsZoomed, "1x is not zoomed");
        Close(new(0, 0, 1, 1), TileZoom.None.Visible(ZoomBase.Whole), "whole");
        Close(new(0, 0.21875, 1, 0.5625), TileZoom.None.Destination(WideInTall), "letterboxed destination");
        Close(new(0, 0, 1, 1), TileZoom.None.Visible(WideInTall), "letterboxed source");
        Close(new(0, 0, 1, 1), TileZoom.None.Destination(Filled), "filled destination");
        Close(new(0.21875, 0, 0.5625, 1), TileZoom.None.Visible(Filled), "filled source");
    }

    private static void GrowsOverTheBars()
    {
        var z = new TileZoom(1.5, 0.5, 0.5);
        Close(new(0, 0.078125, 1, 0.84375), z.Destination(WideInTall), "1.5x destination");
        Close(new(1.0 / 6, 0, 2.0 / 3, 1), z.Visible(WideInTall), "1.5x source");
        var z2 = new TileZoom(2, 0.5, 0.5);
        Close(new(0, 0, 1, 1), z2.Destination(WideInTall), "2x destination");
        Close(new(0.25, 0.0625 / 1.125, 0.5, 1 / 1.125), z2.Visible(WideInTall), "2x source");
    }

    private static void FillThreshold()
    {
        Close(new(0, 0.0021875, 1, 0.995625), new TileZoom(1.77, 0.5, 0.5).Destination(WideInTall), "just below");
        var past = new TileZoom(1.78, 0.5, 0.5);
        Close(new(0, 0, 1, 1), past.Destination(WideInTall), "just past");
        Close(new(0.5 - 0.5 / 1.78, 0.5 - 0.5 / 1.00125, 1 / 1.78, 1 / 1.00125), past.Visible(WideInTall), "just past source");
    }

    private static void TallPicture()
    {
        var z = new TileZoom(2, 0.5, 0.5);
        Close(new(0.18359375, 0, 0.6328125, 1), z.Destination(TallInWide), "2x destination");
        Close(new(0, 0.25, 1, 0.5), z.Visible(TallInWide), "2x source");
        var z4 = new TileZoom(4, 0.5, 0.5);
        Close(new(0, 0, 1, 1), z4.Destination(TallInWide), "4x destination");
        Close(new(0.5 - 0.5 / 1.265625, 0.375, 1 / 1.265625, 0.25), z4.Visible(TallInWide), "4x source");
    }

    private static void ScaleClamped()
    {
        TileZoom top = TileZoom.None.ZoomedBy(100, 0.5, 0.5, WideInTall);
        Check.Equal(TileZoom.MaxScale, top.Scale, "max");
        Close(new(0.4375, 0.5 - 1.0 / 9, 0.125, 2.0 / 9), top.Visible(WideInTall), "max source");
        Check.Equal(TileZoom.None, TileZoom.None.ZoomedBy(0.1, 0.5, 0.5, WideInTall), "min");
        var z = new TileZoom(3, 0.9, 0.1);
        Check.Equal(TileZoom.None, z.ZoomedTo(1, 0.9, 0.1, 0.5, 0.5, WideInTall), "back to 1x forgets the center");
    }

    private static void KeepsThePoint()
    {
        (double ux, double uy) = (0.25, 0.8);
        var q = TileZoom.None.PicturePoint(ux, uy, ZoomBase.Whole);
        TileZoom z = TileZoom.None.ZoomedBy(2, ux, uy, ZoomBase.Whole);
        Check.Equal(2.0, z.Scale, "scale");
        Close(q, z.PicturePoint(ux, uy, ZoomBase.Whole), "the tile's shape");
        TileZoom z1 = TileZoom.None.ZoomedBy(2, 0.25, 0.4, WideInTall);
        Check.True(Math.Abs(z1.PicturePoint(0.25, 0.4, WideInTall).X - 0.25) < 1e-9, "across the filled axis");
        var q2 = z1.PicturePoint(0.6, 0.4, WideInTall);
        TileZoom z2 = z1.ZoomedBy(1.5, 0.6, 0.4, WideInTall);
        Check.True(Math.Abs(z2.Scale - 3) < 1e-12, "3x");
        Close(q2, z2.PicturePoint(0.6, 0.4, WideInTall), "letterboxed, once filled");
    }

    private static void NearAnEdge()
    {
        TileZoom z = TileZoom.None.ZoomedBy(2, 1, 1, ZoomBase.Whole);
        Close(new(0.5, 0.5, 0.5, 0.5), z.Visible(ZoomBase.Whole), "corner");
        TileZoom odd = new TileZoom(2, -3, 7).Clamped(ZoomBase.Whole);
        Close(new(0, 0.5, 0.5, 0.5), odd.Visible(ZoomBase.Whole), "far outside");
        TileZoom shortAxis = new TileZoom(1.5, 0.9, 0.9).Clamped(WideInTall);
        Close((1 - 1 / 3.0, 0.5), (shortAxis.CenterX, shortAxis.CenterY), "an axis not filled stays centered");
    }

    private static void Panning()
    {
        var z = new TileZoom(4, 0.5, 0.5);
        TileZoom p = z.Panned(0.5, 0, ZoomBase.Whole);
        Close((0.375, 0.5), (p.CenterX, p.CenterY), "half a tile right");
        TileZoom far = z.Panned(100, -100, WideInTall);
        Close(new(0, 1 - 1 / 2.25, 0.25, 1 / 2.25), far.Visible(WideInTall), "stops at the picture's edge");
        Close(new(0, 0, 1, 1), far.Destination(WideInTall), "still covering the tile");
        var shortAxis = new TileZoom(1.5, 0.5, 0.5);
        Close(shortAxis.Destination(WideInTall), shortAxis.Panned(0, 0.3, WideInTall).Destination(WideInTall), "an axis not filled does not move");
        Check.Equal(TileZoom.None, TileZoom.None.Panned(0.3, 0.3, ZoomBase.Whole), "nothing to pan at 1x");
    }

    private static void Fill()
    {
        TileZoom z = TileZoom.None.ZoomedBy(2, 0, 0, Filled);
        Close(new(0.21875, 0, 0.28125, 0.5), z.Visible(Filled), "2x at the corner");
        Close(new(0, 0, 1, 1), z.Destination(Filled), "the whole tile");
        Close(new(0, 0, 0.28125, 0.5), z.Panned(100, 0, Filled).Visible(Filled), "panned to the picture's left edge");
    }

    private static void FillOneX()
    {
        // A 16:9 picture filled into a square tile: all of the height, the middle 9/16 of the width.
        Close(new(0, 0, 1, 1), TileZoom.None.Destination(Filled), "covers the tile");
        Close(new(0.21875, 0, 0.5625, 1), TileZoom.None.Visible(Filled), "the middle of the picture");
        // Fitted, the same picture leaves bars.
        Close(new(0, 0.21875, 1, 0.5625), TileZoom.None.Destination(ZoomBase.Of(1600, 900, 300, 300)), "fitted has bars");
        // A tile of the picture's own shape is the same either way.
        Check.Equal(ZoomBase.Of(1600, 900, 1600, 900), ZoomBase.Of(1600, 900, 1600, 900, fill: true), "same shape");
    }

    private static void MapFrame()
    {
        // An 800x450 picture: a quarter of its width is 200, capped at 96; the height follows the shape.
        UnitRect f = ZoomMap.Frame(new(10, 20, 800, 450), 16.0 / 9);
        Close(new(18, 20 + 450 - 8 - 54, 96, 54), f, "wide picture", 1.01);
        // A short picture: the third of its height wins.
        UnitRect g = ZoomMap.Frame(new(0, 0, 400, 60), 16.0 / 9);
        Check.True(g.Height <= 21 && g.Width <= 38 && g.X == 8, $"a short picture caps the map by its height: {g}");
        // A tiny one: a quarter of the width.
        Check.True(ZoomMap.Frame(new(0, 0, 40, 40), 1).Width <= 11, "a tiny picture caps the map by its width");
        Check.Equal(new UnitRect(0, 0, 0, 0), ZoomMap.Frame(new(0, 0, 0, 10), 1), "no picture");
        Check.Equal(new UnitRect(0, 0, 0, 0), ZoomMap.Frame(new(0, 0, 100, 100), 0), "no shape");
    }

    private static void MapRegion()
    {
        Close(new(24, 13.5, 48, 27), ZoomMap.Region(new(0.25, 0.25, 0.5, 0.5), 96, 54), "half the picture");
        Close(new(0, 0, 2, 2), ZoomMap.Region(new(0, 0, 0.001, 0.001), 96, 54), "at least two units");
    }
}
