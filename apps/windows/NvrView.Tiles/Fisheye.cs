// Fisheye dewarping (docs/RENDERING.md "Fisheye dewarping", docs/UX.md
// "Fisheye cameras"): the lens model, the views, the shader constants, and
// finding the image circle. Pure: the HLSL pixel shader (NvrView.Video)
// does only the per-pixel part of what is computed here, and the tests check
// this against known directions. The same math as the macOS app's
// NVRCore/Fisheye.swift, so a layout's views look the same in both.

using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text.Json.Nodes;

namespace NvrView.Tiles;

/// <summary>A direction or point in 3D, doubles.</summary>
public readonly record struct Vec3(double X, double Y, double Z)
{
    public static Vec3 operator +(Vec3 a, Vec3 b) => new(a.X + b.X, a.Y + b.Y, a.Z + b.Z);

    public static Vec3 operator -(Vec3 a, Vec3 b) => new(a.X - b.X, a.Y - b.Y, a.Z - b.Z);

    public static Vec3 operator *(double k, Vec3 a) => new(k * a.X, k * a.Y, k * a.Z);

    public static Vec3 operator /(Vec3 a, double k) => new(a.X / k, a.Y / k, a.Z / k);

    public double Dot(Vec3 b) => X * b.X + Y * b.Y + Z * b.Z;

    public double Length => Math.Sqrt(Dot(this));

    public Vec3 Normalized => this / Math.Max(Length, 1e-12);

    public Vec3 Cross(Vec3 b) => new(Y * b.Z - Z * b.Y, Z * b.X - X * b.Z, X * b.Y - Y * b.X);
}

/// <summary>A point in a picture or tile, as fractions (0..1).</summary>
public readonly record struct UnitPoint(double X, double Y);

/// <summary>A rectangle in a tile, as fractions (0..1).</summary>
public readonly record struct UnitRect(double X, double Y, double Width, double Height)
{
    public static UnitRect Whole => new(0, 0, 1, 1);

    public double Right => X + Width;

    public double Bottom => Y + Height;

    public bool Contains(UnitPoint p) => p.X >= X && p.X <= Right && p.Y >= Y && p.Y <= Bottom;
}

/// <summary>How a fisheye camera is mounted (a per-camera setting).</summary>
public enum FisheyeMount
{
    /// <summary>Looking down (the default): the horizon is the rim of the circle.</summary>
    Ceiling,

    /// <summary>Looking ahead: the horizon runs across the middle.</summary>
    Wall,

    /// <summary>Looking up.</summary>
    Desk,
}

/// <summary>What a tile shows of a fisheye camera.</summary>
public enum FisheyeViewMode
{
    /// <summary>The circle as the camera sends it (digital zoom applies).</summary>
    Original,

    /// <summary>One 360° strip (a wall mount: 180°).</summary>
    Panorama,

    /// <summary>Two 180° strips stacked.</summary>
    DoublePanorama,

    /// <summary>Four virtual 90° cameras.</summary>
    Quad,

    /// <summary>One virtual camera the user aims and zooms.</summary>
    Ptz,
}

public static class Fisheye
{
    public static double Radians(double degrees) => degrees * Math.PI / 180;

    public static double Degrees(double radians) => radians * 180 / Math.PI;

    public static string Title(this FisheyeMount m) => m switch
    {
        FisheyeMount.Wall => "Wall",
        FisheyeMount.Desk => "Desk",
        _ => "Ceiling",
    };

    /// <summary>The name in settings files ("ceiling", "wall", "desk").</summary>
    public static string Name(this FisheyeMount m) => m switch
    {
        FisheyeMount.Wall => "wall",
        FisheyeMount.Desk => "desk",
        _ => "ceiling",
    };

    public static FisheyeMount MountFromName(string? name) => name switch
    {
        "wall" => FisheyeMount.Wall,
        "desk" => FisheyeMount.Desk,
        _ => FisheyeMount.Ceiling,
    };

    public static string Title(this FisheyeViewMode m) => m switch
    {
        FisheyeViewMode.Panorama => "Panorama",
        FisheyeViewMode.DoublePanorama => "Double Panorama",
        FisheyeViewMode.Quad => "Quad",
        FisheyeViewMode.Ptz => "Virtual PTZ",
        _ => "Original",
    };

    /// <summary>The layout leaf's "view" value.</summary>
    public static string Name(this FisheyeViewMode m) => m switch
    {
        FisheyeViewMode.Panorama => "panorama",
        FisheyeViewMode.DoublePanorama => "double_panorama",
        FisheyeViewMode.Quad => "quad",
        FisheyeViewMode.Ptz => "ptz",
        _ => "original",
    };

    /// <summary>Reads a "view" value; unknown ones read as Original.</summary>
    public static FisheyeViewMode ModeFromName(string? name) => name switch
    {
        "panorama" => FisheyeViewMode.Panorama,
        "double_panorama" => FisheyeViewMode.DoublePanorama,
        "quad" => FisheyeViewMode.Quad,
        "ptz" => FisheyeViewMode.Ptz,
        _ => FisheyeViewMode.Original,
    };

    /// <summary>Drawn through the dewarp shader.</summary>
    public static bool Dewarps(this FisheyeViewMode m) => m != FisheyeViewMode.Original;

    /// <summary>A direction of the viewer frame (x right, y down, z forward) in the lens frame (x right and y down in the picture, z out of the lens).</summary>
    public static Vec3 ToLens(this FisheyeMount m, Vec3 v) => m switch
    {
        FisheyeMount.Ceiling => new(v.X, -v.Z, v.Y),
        FisheyeMount.Desk => new(v.X, v.Z, -v.Y),
        _ => v,
    };

    /// <summary>The inverse of <see cref="ToLens"/>.</summary>
    public static Vec3 FromLens(this FisheyeMount m, Vec3 l) => m switch
    {
        FisheyeMount.Ceiling => new(l.X, l.Z, -l.Y),
        FisheyeMount.Desk => new(l.X, -l.Z, l.Y),
        _ => l,
    };

    /// <summary>The direction (ψ pan, τ tilt; degrees) in the viewer frame.</summary>
    public static Vec3 ViewerDirection(double pan, double tilt)
    {
        double p = Radians(pan), t = Radians(tilt);
        return new(Math.Sin(p) * Math.Cos(t), Math.Sin(t), Math.Cos(p) * Math.Cos(t));
    }

    /// <summary>Pan (−180..180) and tilt (degrees) of a viewer-frame direction.</summary>
    public static (double Pan, double Tilt) PanTilt(Vec3 v)
    {
        Vec3 n = v.Normalized;
        return (Degrees(Math.Atan2(n.X, n.Z)), Degrees(Math.Asin(Math.Clamp(n.Y, -1, 1))));
    }
}

/// <summary>The lens's image circle: center as fractions of the width and height, radius as a fraction of the width.</summary>
public readonly record struct FisheyeCircle(double Cx, double Cy, double R)
{
    /// <summary>The largest circle centered in a picture of this size (the fallback).</summary>
    public static FisheyeCircle Inscribed(int width, int height)
    {
        double w = Math.Max(width, 1), h = Math.Max(height, 1);
        return new(0.5, 0.5, Math.Min(w, h) / 2 / w);
    }
}

/// <summary>The equidistant lens model, r = f·θ (docs/RENDERING.md).</summary>
public readonly record struct FisheyeLens(FisheyeCircle Circle, double Aspect = 1, double FieldOfView = FisheyeLens.DefaultFieldOfView)
{
    public const double DefaultFieldOfView = 180;

    /// <summary>θmax, radians.</summary>
    public double ThetaMax => Fisheye.Radians(FieldOfView / 2);

    /// <summary>Where a lens-frame direction lands in the picture (fractions), or null outside the field of view.</summary>
    public UnitPoint? Point(Vec3 direction)
    {
        double len = direction.Length;
        if (len <= 0)
        {
            return null;
        }
        Vec3 d = direction / len;
        double theta = Math.Acos(Math.Clamp(d.Z, -1, 1));
        if (theta > ThetaMax + 1e-9)
        {
            return null;
        }
        double rho = theta / ThetaMax;
        double s = Math.Sqrt(d.X * d.X + d.Y * d.Y);
        if (s <= 1e-12)
        {
            return new UnitPoint(Circle.Cx, Circle.Cy);
        }
        return new UnitPoint(Circle.Cx + rho * Circle.R * d.X / s, Circle.Cy + rho * Circle.R * Aspect * d.Y / s);
    }

    /// <summary>The lens-frame unit direction a picture point sees, or null outside the circle.</summary>
    public Vec3? Direction(UnitPoint p)
    {
        double x = (p.X - Circle.Cx) / Circle.R;
        double y = (p.Y - Circle.Cy) / Aspect / Circle.R;
        double rho = Math.Sqrt(x * x + y * y);
        if (rho > 1 + 1e-9)
        {
            return null;
        }
        if (rho <= 1e-12)
        {
            return new Vec3(0, 0, 1);
        }
        double theta = rho * ThetaMax;
        return new Vec3(Math.Sin(theta) * x / rho, Math.Sin(theta) * y / rho, Math.Cos(theta));
    }
}

/// <summary>Where a tile's virtual camera looks (degrees): pan ψ, tilt τ, horizontal field of view. Panorama and Quad use only the pan.</summary>
public readonly record struct FisheyeAim(double Pan, double Tilt, double Fov)
{
    public const double MinFov = 20;
    public const double MaxFov = 120;

    /// <summary>Where a virtual camera starts for a mount.</summary>
    public static FisheyeAim Default(FisheyeMount mount) => mount switch
    {
        FisheyeMount.Desk => new(0, -45, 90),
        FisheyeMount.Wall => new(0, 0, 90),
        _ => new(0, 45, 90),
    };

    /// <summary>Kept within the mount's range (pan wraps; a wall within ±90°), the field of view within 20..120°.</summary>
    public FisheyeAim Clamped(FisheyeMount mount)
    {
        double fov = Math.Clamp(double.IsFinite(Fov) ? Fov : 90, MinFov, MaxFov);
        double p = double.IsFinite(Pan) ? Pan : 0, t = double.IsFinite(Tilt) ? Tilt : 0;
        return mount switch
        {
            FisheyeMount.Ceiling => new(Wrap(p), Math.Clamp(t, 0, 90), fov),
            FisheyeMount.Desk => new(Wrap(p), Math.Clamp(t, -90, 0), fov),
            _ => new(Math.Clamp(p, -90, 90), Math.Clamp(t, -80, 80), fov),
        };
    }

    /// <summary>Wraps to −180..&lt;180.</summary>
    public static double Wrap(double degrees)
    {
        double x = (degrees + 180) % 360;
        return (x < 0 ? x + 360 : x) - 180;
    }

    /// <summary>
    /// After a drag of (dx, dy) pixels in a view (w x h): the picture follows the pointer
    /// (pan in every dewarped mode, tilt too in Virtual PTZ), by the angle moved across the view.
    /// </summary>
    public FisheyeAim Dragged(double dx, double dy, double w, double h, FisheyeViewMode mode, FisheyeMount mount)
    {
        if (w <= 0 || h <= 0)
        {
            return this;
        }
        double pan = Pan, tilt = Tilt;
        switch (mode)
        {
            case FisheyeViewMode.Original:
                return this;
            case FisheyeViewMode.Ptz:
                double vfov = Fisheye.Degrees(2 * Math.Atan(Math.Tan(Fisheye.Radians(Fov) / 2) * h / w));
                pan -= dx / w * Fov;
                tilt -= dy / h * vfov;
                break;
            case FisheyeViewMode.Panorama:
                pan -= dx / w * 360;
                break;
            case FisheyeViewMode.DoublePanorama:
                pan -= dx / w * 180;
                break;
            case FisheyeViewMode.Quad:
                pan -= dx / (w / 2) * 90;
                break;
        }
        return new FisheyeAim(pan, tilt, Fov).Clamped(mount);
    }

    /// <summary>Zoomed by <paramref name="factor"/> (more than 1 zooms in: a narrower field of view).</summary>
    public FisheyeAim Zoomed(double factor, FisheyeMount mount) =>
        !double.IsFinite(factor) || factor <= 0 ? this : (this with { Fov = Fov / factor }).Clamped(mount);
}

/// <summary>
/// How a pinch zooms a Virtual PTZ tile (docs/UX.md "Fisheye cameras"): the
/// field of view is divided by the pinch's scale, applied at once as the
/// macOS pinch is, and kept within 20..120 degrees by <see cref="FisheyeAim"/>.
/// A precision touchpad's pinch arrives as Ctrl+mouse wheel; a touch screen's
/// as a manipulation scale. Both reduce to a factor here.
/// </summary>
public static class FisheyePinch
{
    /// <summary>The most one input event may zoom (a stray large value must not jump the view).</summary>
    public const double MaxStep = 2;

    /// <summary>One mouse-wheel notch (120 units) zooms by this much, as on the macOS wheel.</summary>
    public const double NotchFactor = 1.25;

    /// <summary>A zoom factor (more than 1 zooms in) from a pinch scale step, kept finite and within 1/MaxStep..MaxStep; 1 when unusable.</summary>
    public static double Factor(double scale) =>
        double.IsFinite(scale) && scale > 0 ? Math.Clamp(scale, 1 / MaxStep, MaxStep) : 1;

    /// <summary>A zoom factor from wheel units (120 per notch; a positive delta zooms in), as a precision touchpad's pinch reports it.</summary>
    public static double WheelFactor(int wheelDelta) => Factor(Math.Pow(NotchFactor, wheelDelta / 120.0));

    /// <summary>The aim after a pinch step on a Virtual PTZ view (other views have no zoom).</summary>
    public static FisheyeAim Apply(FisheyeAim aim, double scale, FisheyeMount mount) => aim.Zoomed(Factor(scale), mount);
}

/// <summary>How directions map onto one view.</summary>
public abstract record FisheyeProjection
{
    private FisheyeProjection()
    {
    }

    /// <summary>A virtual camera (straight lines stay straight): (ψ, τ) of the view's center and its horizontal field of view, degrees.</summary>
    public sealed record Rectilinear(double Pan, double Tilt, double Fov) : FisheyeProjection;

    /// <summary>A panorama strip: pan from Pan0 over PanSpan, tilt from Tilt0 over TiltSpan (degrees; top left to bottom right).</summary>
    public sealed record Cylindrical(double Pan0, double PanSpan, double Tilt0, double TiltSpan) : FisheyeProjection;
}

/// <summary>One view of a dewarped tile: its rectangle in the tile and its projection.</summary>
public sealed record FisheyeView(UnitRect Rect, FisheyeProjection Projection)
{
    /// <summary>Rectilinear: tan of half the horizontal and vertical field of view for a view of w x h pixels.</summary>
    public static (double H, double V) Tangents(double fov, double w, double h)
    {
        double th = Math.Tan(Fisheye.Radians(fov) / 2);
        return (th, th * (w > 0 ? h / w : 1));
    }

    /// <summary>Forward, right and down of a virtual camera aimed at (ψ, τ).</summary>
    public static (Vec3 Forward, Vec3 Right, Vec3 Down) Basis(double pan, double tilt)
    {
        double p = Fisheye.Radians(pan);
        Vec3 f = Fisheye.ViewerDirection(pan, tilt);
        var r = new Vec3(Math.Cos(p), 0, -Math.Sin(p));
        return (f, r, f.Cross(r));
    }

    /// <summary>The viewer-frame direction the point (tx, ty) (0..1 within this view) looks along, for a view of w x h pixels. The reference for the shader.</summary>
    public Vec3 Direction(double tx, double ty, double w, double h)
    {
        switch (Projection)
        {
            case FisheyeProjection.Rectilinear r:
                (Vec3 f, Vec3 right, Vec3 down) = Basis(r.Pan, r.Tilt);
                (double th, double tv) = Tangents(r.Fov, w, h);
                return f + ((2 * tx - 1) * th) * right + ((2 * ty - 1) * tv) * down;
            case FisheyeProjection.Cylindrical c:
                return Fisheye.ViewerDirection(c.Pan0 + tx * c.PanSpan, c.Tilt0 + ty * c.TiltSpan);
            default:
                return new Vec3(0, 0, 1);
        }
    }

    /// <summary>The views of a tile w x h pixels big, in a mode, for a mount and aim. Original has none.</summary>
    public static FisheyeView[] Views(FisheyeViewMode mode, FisheyeMount mount, FisheyeAim aim, double width, double height)
    {
        double w = Math.Max(width, 1), h = Math.Max(height, 1);
        switch (mode)
        {
            case FisheyeViewMode.Original:
                return [];
            case FisheyeViewMode.Panorama or FisheyeViewMode.DoublePanorama when mount == FisheyeMount.Wall:
            {
                double span = Math.Clamp(180 * h / w, 30, 180);
                return [new(UnitRect.Whole, new FisheyeProjection.Cylindrical(-90, 180, -span / 2, span))];
            }
            case FisheyeViewMode.Panorama:
            {
                double span = Math.Clamp(360 * h / w, 30, 90);
                double tilt0 = mount == FisheyeMount.Ceiling ? 0 : -span;
                return [new(UnitRect.Whole, new FisheyeProjection.Cylindrical(aim.Pan - 180, 360, tilt0, span))];
            }
            case FisheyeViewMode.DoublePanorama:
            {
                double span = Math.Clamp(180 * (h / 2) / w, 30, 90);
                double tilt0 = mount == FisheyeMount.Ceiling ? 0 : -span;
                return
                [
                    new(new UnitRect(0, 0, 1, 0.5), new FisheyeProjection.Cylindrical(aim.Pan - 180, 180, tilt0, span)),
                    new(new UnitRect(0, 0.5, 1, 0.5), new FisheyeProjection.Cylindrical(aim.Pan, 180, tilt0, span)),
                ];
            }
            case FisheyeViewMode.Quad:
            {
                (double Pan, double Tilt, double Fov)[] aims = mount switch
                {
                    FisheyeMount.Ceiling => [(aim.Pan, 45, 90), (aim.Pan + 90, 45, 90), (aim.Pan + 180, 45, 90), (aim.Pan + 270, 45, 90)],
                    FisheyeMount.Desk => [(aim.Pan, -45, 90), (aim.Pan + 90, -45, 90), (aim.Pan + 180, -45, 90), (aim.Pan + 270, -45, 90)],
                    _ => [(-67.5, 0, 60), (-22.5, 0, 60), (22.5, 0, 60), (67.5, 0, 60)],
                };
                var views = new FisheyeView[4];
                for (int i = 0; i < 4; i++)
                {
                    double pan = mount == FisheyeMount.Wall ? aims[i].Pan : FisheyeAim.Wrap(aims[i].Pan);
                    views[i] = new(new UnitRect(i % 2 * 0.5, i / 2 * 0.5, 0.5, 0.5), new FisheyeProjection.Rectilinear(pan, aims[i].Tilt, aims[i].Fov));
                }
                return views;
            }
            default:
            {
                FisheyeAim a = aim.Clamped(mount);
                return [new(UnitRect.Whole, new FisheyeProjection.Rectilinear(a.Pan, a.Tilt, a.Fov))];
            }
        }
    }

    /// <summary>The view containing the tile point p and the point within it.</summary>
    public static (FisheyeView View, double Tx, double Ty)? Locate(UnitPoint p, IReadOnlyList<FisheyeView> views)
    {
        foreach (FisheyeView v in views)
        {
            if (v.Rect.Contains(p))
            {
                return (v, (p.X - v.Rect.X) / v.Rect.Width, (p.Y - v.Rect.Y) / v.Rect.Height);
            }
        }
        return null;
    }
}

/// <summary>One view's rows of the shader constants (5 x 16 bytes).</summary>
[StructLayout(LayoutKind.Sequential, Pack = 4)]
public struct DewarpViewRows
{
    /// <summary>Left, top, right, bottom in the tile (0..1).</summary>
    public float RectL, RectT, RectR, RectB;

    /// <summary>xyz an axis in the lens frame; w the projection (0 rectilinear, 1 cylindrical).</summary>
    public float XX, XY, XZ, XW;

    /// <summary>xyz an axis; w tan(h/2) (rectilinear) or ψ0 (cylindrical).</summary>
    public float YX, YY, YZ, YW;

    /// <summary>xyz an axis; w tan(v/2) (rectilinear) or Δψ (cylindrical).</summary>
    public float ZX, ZY, ZZ, ZW;

    /// <summary>Cylindrical: τ0, Δτ.</summary>
    public float E0, E1, E2, E3;
}

/// <summary>Four views of rows, inline.</summary>
[InlineArray(DewarpConstants.MaxViews)]
public struct DewarpViewArray
{
    private DewarpViewRows _first;
}

/// <summary>
/// The shader constants of one dewarped tile (docs/RENDERING.md, "Shader constants"): 22 rows
/// of four floats, 352 bytes, the same layout as the HLSL constant buffer and the macOS app's
/// Metal buffer.
/// </summary>
[StructLayout(LayoutKind.Sequential, Pack = 4)]
public struct DewarpConstants
{
    public const int MaxViews = 4;
    public const int Size = 352;

    /// <summary>cx, cy, r, r·W/H: the image circle in picture fractions.</summary>
    public float LensCx, LensCy, LensR, LensRy;

    /// <summary>θmax (radians), number of views, 0, 0.</summary>
    public float ThetaMax, ViewCount, Pad0, Pad1;

    public DewarpViewArray Views;
}

/// <summary>A dewarped tile's whole setup, and the reference mapping from a tile point to the picture point the shader samples.</summary>
public sealed record FisheyeDewarp(FisheyeLens Lens, FisheyeMount Mount, IReadOnlyList<FisheyeView> Views, double TileWidth, double TileHeight)
{
    public static FisheyeDewarp Of(FisheyeLens lens, FisheyeMount mount, FisheyeViewMode mode, FisheyeAim aim, double tileWidth, double tileHeight) =>
        new(lens, mount, FisheyeView.Views(mode, mount, aim, tileWidth, tileHeight), tileWidth, tileHeight);

    /// <summary>The viewer-frame direction the tile point shows, or null where no view is.</summary>
    public Vec3? Direction(UnitPoint p)
    {
        if (FisheyeView.Locate(p, Views) is not { } hit)
        {
            return null;
        }
        return hit.View.Direction(hit.Tx, hit.Ty, TileWidth * hit.View.Rect.Width, TileHeight * hit.View.Rect.Height);
    }

    /// <summary>The picture point (fractions) the tile point samples, or null when it draws black.</summary>
    public UnitPoint? PicturePoint(UnitPoint p) => Direction(p) is { } d ? Lens.Point(Mount.ToLens(d)) : null;

    /// <summary>The constants the shader reads.</summary>
    public DewarpConstants Constants()
    {
        var c = new DewarpConstants();
        FisheyeCircle r = Lens.Circle;
        c.LensCx = (float)r.Cx;
        c.LensCy = (float)r.Cy;
        c.LensR = (float)r.R;
        c.LensRy = (float)(r.R * Lens.Aspect);
        c.ThetaMax = (float)Lens.ThetaMax;
        int n = Math.Min(Views.Count, DewarpConstants.MaxViews);
        c.ViewCount = n;
        for (int i = 0; i < n; i++)
        {
            FisheyeView v = Views[i];
            ref DewarpViewRows row = ref c.Views[i];
            row.RectL = (float)v.Rect.X;
            row.RectT = (float)v.Rect.Y;
            row.RectR = (float)v.Rect.Right;
            row.RectB = (float)v.Rect.Bottom;
            switch (v.Projection)
            {
                case FisheyeProjection.Rectilinear p:
                    (Vec3 fw, Vec3 rt, Vec3 dn) = FisheyeView.Basis(p.Pan, p.Tilt);
                    (double th, double tv) = FisheyeView.Tangents(p.Fov, TileWidth * v.Rect.Width, TileHeight * v.Rect.Height);
                    SetAxes(ref row, Mount.ToLens(rt), Mount.ToLens(dn), Mount.ToLens(fw));
                    row.XW = 0;
                    row.YW = (float)th;
                    row.ZW = (float)tv;
                    break;
                case FisheyeProjection.Cylindrical p:
                    SetAxes(ref row, Mount.ToLens(new Vec3(1, 0, 0)), Mount.ToLens(new Vec3(0, 1, 0)), Mount.ToLens(new Vec3(0, 0, 1)));
                    row.XW = 1;
                    row.YW = (float)Fisheye.Radians(p.Pan0);
                    row.ZW = (float)Fisheye.Radians(p.PanSpan);
                    row.E0 = (float)Fisheye.Radians(p.Tilt0);
                    row.E1 = (float)Fisheye.Radians(p.TiltSpan);
                    break;
            }
        }
        return c;
    }

    private static void SetAxes(ref DewarpViewRows row, Vec3 x, Vec3 y, Vec3 z)
    {
        (row.XX, row.XY, row.XZ) = ((float)x.X, (float)x.Y, (float)x.Z);
        (row.YX, row.YY, row.YZ) = ((float)y.X, (float)y.Y, (float)y.Z);
        (row.ZX, row.ZY, row.ZZ) = ((float)z.X, (float)z.Y, (float)z.Z);
    }

    /// <summary>What the shader computes for a tile point, from the constants alone (the tests compare it with <see cref="PicturePoint"/>).</summary>
    public static UnitPoint? Shade(in DewarpConstants c, UnitPoint p)
    {
        int count = Math.Min((int)c.ViewCount, DewarpConstants.MaxViews);
        for (int i = 0; i < count; i++)
        {
            DewarpViewRows v = c.Views[i];
            if (p.X < v.RectL || p.X > v.RectR || p.Y < v.RectT || p.Y > v.RectB)
            {
                continue;
            }
            double tx = (p.X - v.RectL) / (v.RectR - v.RectL), ty = (p.Y - v.RectT) / (v.RectB - v.RectT);
            var ax = new Vec3(v.XX, v.XY, v.XZ);
            var ay = new Vec3(v.YX, v.YY, v.YZ);
            var az = new Vec3(v.ZX, v.ZY, v.ZZ);
            Vec3 dir;
            if (v.XW < 0.5)
            {
                dir = az + ((2 * tx - 1) * v.YW) * ax + ((2 * ty - 1) * v.ZW) * ay;
            }
            else
            {
                double psi = v.YW + tx * v.ZW, tau = v.E0 + ty * v.E1;
                dir = (Math.Cos(tau) * Math.Sin(psi)) * ax + Math.Sin(tau) * ay + (Math.Cos(tau) * Math.Cos(psi)) * az;
            }
            Vec3 d = dir.Normalized;
            double theta = Math.Acos(Math.Clamp(d.Z, -1, 1));
            if (theta > c.ThetaMax + 1e-6)
            {
                return null;
            }
            double s = Math.Sqrt(d.X * d.X + d.Y * d.Y);
            double rho = theta / c.ThetaMax;
            if (s <= 1e-9)
            {
                return new UnitPoint(c.LensCx, c.LensCy);
            }
            return new UnitPoint(c.LensCx + rho * c.LensR * d.X / s, c.LensCy + rho * c.LensRy * d.Y / s);
        }
        return null;
    }
}

/// <summary>
/// Eases each key's virtual-camera field of view toward a target in logarithmic steps on the
/// viewer's clock (time constant 0.06 s, like the macOS digital zoom): it finishes however few
/// refreshes there are and ends exactly on the target.
/// </summary>
public sealed class FisheyeZoomEasing<TKey>(double timeConstant = 0.06)
    where TKey : notnull
{
    private readonly Dictionary<TKey, double> _targets = [];
    private double? _last;

    public bool IsEasing => _targets.Count > 0;

    public double? Target(TKey key) => _targets.TryGetValue(key, out double t) ? t : null;

    public void Set(TKey key, double fov, double now)
    {
        if (_targets.Count == 0)
        {
            _last = now;
        }
        _targets[key] = Math.Clamp(fov, FisheyeAim.MinFov, FisheyeAim.MaxFov);
    }

    public void Cancel(TKey key) => _targets.Remove(key);

    /// <summary>One step at <paramref name="now"/>: the new field of view of every key being eased.</summary>
    public List<(TKey Key, double Fov)> Tick(double now, Func<TKey, double> current)
    {
        var output = new List<(TKey, double)>();
        if (_targets.Count == 0)
        {
            _last = null;
            return output;
        }
        double dt = Math.Max(now - (_last ?? now), 0);
        _last = now;
        foreach ((TKey key, double target) in _targets.ToArray())
        {
            double fov = Eased(current(key), target, dt, timeConstant);
            output.Add((key, fov));
            if (fov == target)
            {
                _targets.Remove(key);
            }
        }
        if (_targets.Count == 0)
        {
            _last = null;
        }
        return output;
    }

    /// <summary>One exponential step in log space; snaps to the target when within 0.2%.</summary>
    public static double Eased(double current, double target, double dt, double timeConstant)
    {
        if (current <= 0 || target <= 0)
        {
            return target;
        }
        double k = 1 - Math.Exp(-Math.Max(dt, 0) / timeConstant);
        double next = Math.Exp(Math.Log(current) + (Math.Log(target) - Math.Log(current)) * k);
        return Math.Abs(Math.Log(next / target)) < 0.002 ? target : next;
    }
}

/// <summary>
/// A tile's fisheye view as saved with the layout: the leaf's optional "fisheye" member,
/// {"view": "ptz", "pan": 12.5, "tilt": 45, "fov": 90} (no member: Original).
/// </summary>
public readonly record struct FisheyeTileSetting(FisheyeViewMode Mode, FisheyeAim? Aim = null)
{
    public const string Member = "fisheye";

    public static FisheyeTileSetting Original => new(FisheyeViewMode.Original);

    /// <summary>The aim in effect for a mount (the saved one, or the mount's default).</summary>
    public FisheyeAim AimFor(FisheyeMount mount) => (Aim ?? FisheyeAim.Default(mount)).Clamped(mount);

    /// <summary>The member's JSON, or null for no member (Original without a moved view).</summary>
    public string? ToJson()
    {
        if (Mode == FisheyeViewMode.Original && Aim is null)
        {
            return null;
        }
        var o = new JsonObject { ["view"] = Mode.Name() };
        if (Aim is { } a)
        {
            o["pan"] = a.Pan;
            o["tilt"] = a.Tilt;
            o["fov"] = a.Fov;
        }
        return o.ToJsonString();
    }

    /// <summary>Reads the member (JSON text); missing, malformed, or unknown views read as Original.</summary>
    public static FisheyeTileSetting FromJson(string? json)
    {
        if (json is null)
        {
            return Original;
        }
        try
        {
            if (JsonNode.Parse(json) is not JsonObject o)
            {
                return Original;
            }
            FisheyeViewMode mode = o["view"] is JsonValue v && v.TryGetValue(out string? s) ? Fisheye.ModeFromName(s) : FisheyeViewMode.Original;
            FisheyeAim? aim = Number(o, "pan") is { } p && Number(o, "tilt") is { } t && Number(o, "fov") is { } f ? new FisheyeAim(p, t, f) : null;
            return new FisheyeTileSetting(mode, aim);
        }
        catch (System.Text.Json.JsonException)
        {
            return Original;
        }
    }

    private static double? Number(JsonObject o, string key) =>
        o[key] is JsonValue v && v.TryGetValue(out double d) ? d : null;
}

/// <summary>Finds the image circle in a picture's luma plane (docs/RENDERING.md, "Finding the circle").</summary>
public static class FisheyeCircleDetector
{
    public const int Rays = 180;
    private const int Run = 4;
    private const int Margin = 20;

    /// <summary>The circle in a width x height luma plane read through luma(x, y) (8-bit), or null when no clear circle is there.</summary>
    public static FisheyeCircle? Detect(int width, int height, Func<int, int, byte> luma)
    {
        if (width < 16 || height < 16)
        {
            return null;
        }
        int shortSide = Math.Min(width, height);
        int m = Math.Max(4, shortSide * 4 / 100);
        var corner = new List<byte>(4 * m * m);
        foreach ((int x0, int y0) in new[] { (0, 0), (width - m, 0), (0, height - m), (width - m, height - m) })
        {
            for (int y = y0; y < y0 + m; y++)
            {
                for (int x = x0; x < x0 + m; x++)
                {
                    corner.Add(luma(x, y));
                }
            }
        }
        corner.Sort();
        int threshold = corner[corner.Count / 10] + Margin;
        double cx = (width - 1) / 2.0, cy = (height - 1) / 2.0;
        double stop = 0.1 * shortSide;
        var xs = new List<double>(Rays);
        var ys = new List<double>(Rays);
        for (int k = 0; k < Rays; k++)
        {
            double a = 2 * Math.PI * k / Rays;
            double dx = Math.Cos(a), dy = Math.Sin(a);
            double tx = dx > 1e-9 ? (width - 1 - cx) / dx : dx < -1e-9 ? -cx / dx : double.PositiveInfinity;
            double ty = dy > 1e-9 ? (height - 1 - cy) / dy : dy < -1e-9 ? -cy / dy : double.PositiveInfinity;
            double r = Math.Floor(Math.Min(tx, ty));
            int count = 0;
            while (r > stop)
            {
                int x = Math.Clamp((int)Math.Round(cx + r * dx, MidpointRounding.AwayFromZero), 0, width - 1);
                int y = Math.Clamp((int)Math.Round(cy + r * dy, MidpointRounding.AwayFromZero), 0, height - 1);
                if (luma(x, y) >= threshold)
                {
                    count++;
                    if (count >= Run)
                    {
                        double rim = r + (Run - 1);
                        xs.Add(cx + rim * dx);
                        ys.Add(cy + rim * dy);
                        break;
                    }
                }
                else
                {
                    count = 0;
                }
                r -= 1;
            }
        }
        if (Fit(xs, ys) is not { } fit)
        {
            return null;
        }
        List<double> keptX = xs, keptY = ys;
        for (int round = 0; round < 3; round++)
        {
            double[] res = Residuals(keptX, keptY, fit);
            double median = Median(res);
            double limit = Math.Max(3 * 1.4826 * median, 1);
            var nx = new List<double>();
            var ny = new List<double>();
            for (int i = 0; i < res.Length; i++)
            {
                if (res[i] <= limit)
                {
                    nx.Add(keptX[i]);
                    ny.Add(keptY[i]);
                }
            }
            keptX = nx;
            keptY = ny;
            if (keptX.Count < 3 || Fit(keptX, keptY) is not { } f)
            {
                return null;
            }
            fit = f;
        }
        double finalMedian = Median(Residuals(keptX, keptY, fit));
        double w = width, h = height;
        if (keptX.Count < Rays / 2 || finalMedian > 0.01 * w
            || fit.R < 0.25 * shortSide || fit.R > 0.75 * shortSide
            || fit.X < 0.25 * w || fit.X > 0.75 * w || fit.Y < 0.25 * h || fit.Y > 0.75 * h)
        {
            return null;
        }
        return new FisheyeCircle((fit.X + 0.5) / w, (fit.Y + 0.5) / h, fit.R / w);
    }

    private static double[] Residuals(List<double> xs, List<double> ys, (double X, double Y, double R) fit)
    {
        var res = new double[xs.Count];
        for (int i = 0; i < res.Length; i++)
        {
            res[i] = Math.Abs(Math.Sqrt((xs[i] - fit.X) * (xs[i] - fit.X) + (ys[i] - fit.Y) * (ys[i] - fit.Y)) - fit.R);
        }
        return res;
    }

    private static double Median(double[] values)
    {
        double[] sorted = (double[])values.Clone();
        Array.Sort(sorted);
        return sorted[sorted.Length / 2];
    }

    /// <summary>Algebraic least squares (x² + y² + Dx + Ey + F = 0), centered for conditioning.</summary>
    internal static (double X, double Y, double R)? Fit(List<double> xs, List<double> ys)
    {
        if (xs.Count < 3)
        {
            return null;
        }
        double n = xs.Count;
        double mx = xs.Sum() / n, my = ys.Sum() / n;
        double suu = 0, svv = 0, suv = 0, suuu = 0, svvv = 0, suvv = 0, svuu = 0;
        for (int i = 0; i < xs.Count; i++)
        {
            double u = xs[i] - mx, v = ys[i] - my;
            suu += u * u;
            svv += v * v;
            suv += u * v;
            suuu += u * u * u;
            svvv += v * v * v;
            suvv += u * v * v;
            svuu += v * u * u;
        }
        double det = suu * svv - suv * suv;
        if (Math.Abs(det) <= 1e-9)
        {
            return null;
        }
        double b1 = (suuu + suvv) / 2, b2 = (svvv + svuu) / 2;
        double uc = (b1 * svv - b2 * suv) / det;
        double vc = (b2 * suu - b1 * suv) / det;
        double r2 = uc * uc + vc * vc + (suu + svv) / n;
        return r2 > 0 ? (uc + mx, vc + my, Math.Sqrt(r2)) : null;
    }
}

/// <summary>
/// Finds a camera's circle from its first decoded pictures: one try per picture at most every
/// 0.5 s; after 8 failures the inscribed circle, and no more tries (docs/RENDERING.md).
/// </summary>
public sealed class FisheyeCircleTracker
{
    public const double Interval = 0.5;
    public const int MaxTries = 8;

    private double? _lastTry;

    public FisheyeCircle? Circle { get; private set; }

    public int Tries { get; private set; }

    /// <summary>The result is the fallback, not a measurement.</summary>
    public bool FellBack { get; private set; }

    /// <summary>Whether a picture arriving at <paramref name="now"/> should be measured.</summary>
    public bool WantsTry(double now) => Circle is null && (_lastTry is not { } last || now - last >= Interval);

    /// <summary>The outcome of a try at <paramref name="now"/> on a width x height picture.</summary>
    public void Record(FisheyeCircle? found, double now, int width, int height)
    {
        if (Circle is not null)
        {
            return;
        }
        _lastTry = now;
        Tries++;
        if (found is { } f)
        {
            Circle = f;
        }
        else if (Tries >= MaxTries)
        {
            Circle = FisheyeCircle.Inscribed(width, height);
            FellBack = true;
        }
    }
}
