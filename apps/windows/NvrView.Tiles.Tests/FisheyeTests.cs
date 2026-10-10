// Fisheye dewarping math (docs/RENDERING.md "Fisheye dewarping"): the same
// cases as the macOS app's FisheyeTests, on the same numbers.

using System.Runtime.CompilerServices;
using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class FisheyeTests
{
    public static IEnumerable<(string, Action)> All() =>
    [
        ("fisheye: picture and direction round trip", RoundTrip),
        ("fisheye: known directions", KnownDirections),
        ("fisheye: non-square pictures scale the vertical radius", NonSquare),
        ("fisheye: mounts rotate the viewer frame", Mounts),
        ("fisheye: pan and tilt round trip", PanTiltRoundTrip),
        ("fisheye: panorama columns are azimuths, rows tilts", Panorama),
        ("fisheye: double panorama stacks two halves", DoublePanorama),
        ("fisheye: quad looks four ways", Quad),
        ("fisheye: virtual PTZ is a rectilinear camera", Ptz),
        ("fisheye: aims are kept in range", AimRange),
        ("fisheye: dragging follows the pointer", Dragging),
        ("fisheye: zoom easing on the clock ends on the target", ZoomEasing),
        ("fisheye: constants describe the reference mapping", ConstantsMatch),
        ("fisheye: shader constants layout", ConstantsLayout),
        ("fisheye: tile setting member round trip", SettingMember),
        ("fisheye: aim defaults follow the mount", AimDefaults),
        ("fisheye: circles found within 1%", CircleFound),
        ("fisheye: full range without overlay", CircleFullRange),
        ("fisheye: no circle, no answer", NoCircle),
        ("fisheye: tracker tries on the clock, then falls back", Tracker),
    ];

    private static bool Close(double a, double b, double tol = 1e-6) => Math.Abs(a - b) <= tol;

    private static bool Close(Vec3 a, Vec3 b, double tol = 1e-6) => Close(a.X, b.X, tol) && Close(a.Y, b.Y, tol) && Close(a.Z, b.Z, tol);

    private static bool Close(UnitPoint? a, UnitPoint? b, double tol = 1e-6) =>
        a is null || b is null ? a is null && b is null : Close(a.Value.X, b.Value.X, tol) && Close(a.Value.Y, b.Value.Y, tol);

    private static readonly FisheyeLens Lens = new(new FisheyeCircle(0.49, 0.505, 0.46));
    private static readonly FisheyeLens Unit = new(new FisheyeCircle(0.5, 0.5, 0.5));

    private static void RoundTrip()
    {
        for (double u = 0.05; u <= 0.951; u += 0.05)
        {
            for (double v = 0.05; v <= 0.951; v += 0.05)
            {
                var p = new UnitPoint(u, v);
                if (Lens.Direction(p) is not { } d)
                {
                    Check.True(Math.Sqrt((u - 0.49) * (u - 0.49) + (v - 0.505) * (v - 0.505)) > 0.46 - 1e-9, "no direction only outside");
                    continue;
                }
                Check.True(Close(d.Dot(d), 1), "unit direction");
                Check.True(Close(Lens.Point(d), p, 1e-9), $"round trip at {u},{v}");
            }
        }
    }

    private static void KnownDirections()
    {
        Check.True(Close(Lens.Point(new Vec3(0, 0, 1)), new UnitPoint(0.49, 0.505)), "axis is the center");
        Check.True(Close(Lens.Point(new Vec3(1, 0, 0)), new UnitPoint(0.95, 0.505)), "90° right is the rim");
        Check.True(Close(Lens.Point(new Vec3(0, -1, 0)), new UnitPoint(0.49, 0.045)), "90° up is the rim");
        var d = new Vec3(Math.Sin(Math.PI / 4), 0, Math.Cos(Math.PI / 4));
        Check.True(Close(Lens.Point(d), new UnitPoint(0.49 + 0.23, 0.505)), "45° is half way");
        Check.True(Lens.Point(new Vec3(0, 0.1, -1)) is null, "behind a 180° lens");
        var wide = Lens with { FieldOfView = 200 };
        Check.True(wide.Point(new Vec3(1, 0, 0)) is { } w && Close(w.X, 0.49 + 0.46 * 90 / 100, 1e-9), "a wider lens");
    }

    private static void NonSquare()
    {
        var wide = new FisheyeLens(new FisheyeCircle(0.5, 0.5, 0.25), 16.0 / 9.0);
        Check.True(Close(wide.Point(new Vec3(0, 1, 0)), new UnitPoint(0.5, 0.5 + 0.25 * 16 / 9)), "vertical radius");
        var p = new UnitPoint(0.6, 0.4);
        Check.True(wide.Direction(p) is { } d && Close(wide.Point(d), p, 1e-9), "round trip");
    }

    private static UnitPoint? At(FisheyeMount m, double pan, double tilt) => Unit.Point(m.ToLens(Fisheye.ViewerDirection(pan, tilt)));

    private static void Mounts()
    {
        foreach (FisheyeMount m in Enum.GetValues<FisheyeMount>())
        {
            foreach (Vec3 v in new[] { new Vec3(1, 0, 0), new Vec3(0, 1, 0), new Vec3(0, 0, 1), new Vec3(0.3, -0.5, 0.8).Normalized })
            {
                Check.True(Close(m.FromLens(m.ToLens(v)), v), $"{m} inverse");
            }
        }
        Check.True(Close(At(FisheyeMount.Ceiling, 0, 90), new UnitPoint(0.5, 0.5)), "ceiling down");
        Check.True(Close(At(FisheyeMount.Ceiling, 0, 0), new UnitPoint(0.5, 0)), "ceiling pan 0 top");
        Check.True(Close(At(FisheyeMount.Ceiling, 90, 0), new UnitPoint(1, 0.5)), "ceiling pan 90 right");
        Check.True(Close(At(FisheyeMount.Ceiling, 180, 45), new UnitPoint(0.5, 0.75)), "ceiling pan 180");
        Check.True(Close(At(FisheyeMount.Desk, 0, -90), new UnitPoint(0.5, 0.5)), "desk up");
        Check.True(Close(At(FisheyeMount.Desk, 0, 0), new UnitPoint(0.5, 1)), "desk pan 0 bottom");
        Check.True(Close(At(FisheyeMount.Desk, 90, 0), new UnitPoint(1, 0.5)), "desk pan 90 right");
        Check.True(Close(At(FisheyeMount.Wall, 0, 0), new UnitPoint(0.5, 0.5)), "wall ahead");
        Check.True(Close(At(FisheyeMount.Wall, 0, 90), new UnitPoint(0.5, 1)), "wall down");
        Check.True(Close(At(FisheyeMount.Wall, -90, 0), new UnitPoint(0, 0.5)), "wall left");
    }

    private static void PanTiltRoundTrip()
    {
        for (double pan = -170; pan <= 170; pan += 34)
        {
            for (double tilt = -80; tilt <= 80; tilt += 20)
            {
                (double p, double t) = Fisheye.PanTilt(Fisheye.ViewerDirection(pan, tilt));
                Check.True(Close(p, pan, 1e-9) && Close(t, tilt, 1e-9), $"{pan},{tilt}");
            }
        }
    }

    private static void Panorama()
    {
        FisheyeDewarp d = FisheyeDewarp.Of(Unit, FisheyeMount.Ceiling, FisheyeViewMode.Panorama, FisheyeAim.Default(FisheyeMount.Ceiling), 1440, 240);
        Check.Equal(1, d.Views.Count, "one strip");
        Check.Equal<FisheyeProjection>(new FisheyeProjection.Cylindrical(-180, 360, 0, 60), d.Views[0].Projection, "strip");
        foreach ((double x, double pan) in new[] { (0.5, 0.0), (0.75, 90.0), (0.25, -90.0), (0.0, -180.0) })
        {
            foreach ((double y, double tilt) in new[] { (0.0, 0.0), (0.5, 30.0), (1.0, 60.0) })
            {
                Check.True(Close(d.Direction(new UnitPoint(x, y))!.Value.Normalized, Fisheye.ViewerDirection(pan, tilt)), $"at {x},{y}");
            }
        }
        Check.True(Close(d.PicturePoint(new UnitPoint(0.5, 0)), new UnitPoint(0.5, 0)), "top center is the rim's top");
        FisheyeAim def = FisheyeAim.Default(FisheyeMount.Ceiling);
        Check.Equal<FisheyeProjection>(new FisheyeProjection.Cylindrical(-180, 360, 0, 90), FisheyeView.Views(FisheyeViewMode.Panorama, FisheyeMount.Ceiling, def, 100, 100)[0].Projection, "tall");
        Check.Equal<FisheyeProjection>(new FisheyeProjection.Cylindrical(-180, 360, 0, 30), FisheyeView.Views(FisheyeViewMode.Panorama, FisheyeMount.Ceiling, def, 1000, 10)[0].Projection, "short");
        Check.Equal<FisheyeProjection>(new FisheyeProjection.Cylindrical(-150, 360, -60, 60),
            FisheyeView.Views(FisheyeViewMode.Panorama, FisheyeMount.Desk, new FisheyeAim(30, 0, 90), 1440, 240)[0].Projection, "desk");
        Check.Equal<FisheyeProjection>(new FisheyeProjection.Cylindrical(-90, 180, -45, 90),
            FisheyeView.Views(FisheyeViewMode.Panorama, FisheyeMount.Wall, FisheyeAim.Default(FisheyeMount.Wall), 360, 180)[0].Projection, "wall");
    }

    private static void DoublePanorama()
    {
        FisheyeDewarp d = FisheyeDewarp.Of(Unit, FisheyeMount.Ceiling, FisheyeViewMode.DoublePanorama, new FisheyeAim(10, 45, 90), 720, 480);
        Check.Equal(new UnitRect(0, 0, 1, 0.5), d.Views[0].Rect, "top");
        Check.Equal(new UnitRect(0, 0.5, 1, 0.5), d.Views[1].Rect, "bottom");
        Check.True(Close(d.Direction(new UnitPoint(0, 0))!.Value.Normalized, Fisheye.ViewerDirection(-170, 0)), "top left");
        Check.True(Close(d.Direction(new UnitPoint(1, 0.25))!.Value.Normalized, Fisheye.ViewerDirection(10, 30)), "top right");
        Check.True(Close(d.Direction(new UnitPoint(0.5, 0.75))!.Value.Normalized, Fisheye.ViewerDirection(100, 30)), "bottom middle");
        FisheyeView[] wall = FisheyeView.Views(FisheyeViewMode.DoublePanorama, FisheyeMount.Wall, FisheyeAim.Default(FisheyeMount.Wall), 720, 480);
        FisheyeView[] single = FisheyeView.Views(FisheyeViewMode.Panorama, FisheyeMount.Wall, FisheyeAim.Default(FisheyeMount.Wall), 720, 480);
        Check.True(wall.SequenceEqual(single), "a wall shows the single panorama");
    }

    private static void Quad()
    {
        FisheyeDewarp d = FisheyeDewarp.Of(Unit, FisheyeMount.Ceiling, FisheyeViewMode.Quad, FisheyeAim.Default(FisheyeMount.Ceiling), 800, 600);
        Check.Equal(4, d.Views.Count, "four views");
        (UnitPoint, double)[] centers = [(new(0.25, 0.25), 0), (new(0.75, 0.25), 90), (new(0.25, 0.75), -180), (new(0.75, 0.75), -90)];
        foreach ((UnitPoint c, double pan) in centers)
        {
            Check.True(Close(d.Direction(c)!.Value.Normalized, Fisheye.ViewerDirection(pan, 45)), $"view at pan {pan}");
        }
        Vec3 l = d.Direction(new UnitPoint(0, 0.25))!.Value.Normalized, r = d.Direction(new UnitPoint(0.5, 0.25))!.Value.Normalized;
        Check.True(Close(Math.Acos(l.Dot(r)), Math.PI / 2, 1e-9), "90° across");
    }

    private static void Ptz()
    {
        FisheyeDewarp d = FisheyeDewarp.Of(Unit, FisheyeMount.Ceiling, FisheyeViewMode.Ptz, new FisheyeAim(30, 40, 60), 640, 480);
        Check.True(Close(d.Direction(new UnitPoint(0.5, 0.5))!.Value.Normalized, Fisheye.ViewerDirection(30, 40)), "center");
        Vec3 l = d.Direction(new UnitPoint(0, 0.5))!.Value.Normalized, r = d.Direction(new UnitPoint(1, 0.5))!.Value.Normalized;
        Check.True(Close(Math.Acos(l.Dot(r)), Fisheye.Radians(60), 1e-9), "horizontal fov");
        Vec3 t = d.Direction(new UnitPoint(0.5, 0))!.Value.Normalized, b = d.Direction(new UnitPoint(0.5, 1))!.Value.Normalized;
        Check.True(Close(Math.Acos(t.Dot(b)), 2 * Math.Atan(Math.Tan(Fisheye.Radians(30)) * 0.75), 1e-9), "vertical fov");
        Vec3[] row = [.. new[] { 0.0, 0.3, 0.7, 1.0 }.Select(x => d.Direction(new UnitPoint(x, 0.2))!.Value)];
        Vec3 n = row[0].Cross(row[3]).Normalized;
        foreach (Vec3 p in row)
        {
            Check.True(Math.Abs(n.Dot(p.Normalized)) < 1e-9, "straight lines stay straight");
        }
        Check.True(Fisheye.PanTilt(t).Tilt < 40 && Fisheye.PanTilt(b).Tilt > 40, "top looks up");
    }

    private static void AimRange()
    {
        Check.Equal(new FisheyeAim(-170, 90, 120), new FisheyeAim(190, 120, 200).Clamped(FisheyeMount.Ceiling), "ceiling high");
        Check.Equal(new FisheyeAim(-180, 0, 20), new FisheyeAim(-540, -5, 5).Clamped(FisheyeMount.Ceiling), "ceiling low");
        Check.Equal(new FisheyeAim(10, 0, 90), new FisheyeAim(10, 10, 90).Clamped(FisheyeMount.Desk), "desk");
        Check.Equal(new FisheyeAim(90, -80, 90), new FisheyeAim(150, -85, 90).Clamped(FisheyeMount.Wall), "wall");
        Check.Equal(new FisheyeAim(0, 0, 90), new FisheyeAim(double.NaN, double.PositiveInfinity, double.NaN).Clamped(FisheyeMount.Wall), "not finite");
    }

    private static void Dragging()
    {
        FisheyeAim aim = FisheyeAim.Default(FisheyeMount.Ceiling);
        FisheyeAim right = aim.Dragged(450, 0, 900, 600, FisheyeViewMode.Ptz, FisheyeMount.Ceiling);
        Check.True(Close(right.Pan, -45) && right.Tilt == 45, "drag right turns left");
        FisheyeAim down = aim.Dragged(0, 300, 900, 600, FisheyeViewMode.Ptz, FisheyeMount.Ceiling);
        double vfov = Fisheye.Degrees(2 * Math.Atan(Math.Tan(Fisheye.Radians(45)) * 600 / 900));
        Check.True(Close(down.Tilt, 45 - vfov / 2), "drag down looks up");
        Check.True(Close(aim.Dragged(90, 50, 900, 600, FisheyeViewMode.Panorama, FisheyeMount.Ceiling).Pan, -36), "panorama");
        Check.True(Close(aim.Dragged(90, 0, 900, 600, FisheyeViewMode.DoublePanorama, FisheyeMount.Ceiling).Pan, -18), "double");
        Check.True(Close(aim.Dragged(45, 0, 900, 600, FisheyeViewMode.Quad, FisheyeMount.Ceiling).Pan, -9), "quad");
        Check.Equal(aim, aim.Dragged(45, 0, 900, 600, FisheyeViewMode.Original, FisheyeMount.Ceiling), "original");
        Check.Equal(45.0, aim.Zoomed(2, FisheyeMount.Ceiling).Fov, "zoom in");
        Check.Equal(FisheyeAim.MinFov, aim.Zoomed(100, FisheyeMount.Ceiling).Fov, "zoom min");
        Check.Equal(FisheyeAim.MaxFov, aim.Zoomed(0.1, FisheyeMount.Ceiling).Fov, "zoom max");
    }

    private static void ZoomEasing()
    {
        var clock = new ManualViewerClock(10);
        var easing = new FisheyeZoomEasing<int>();
        double fov = 90;
        easing.Set(1, 45, clock.Now);
        Check.True(easing.IsEasing, "easing");
        double last = fov;
        for (int i = 0; i < 3; i++)
        {
            clock.Advance(1.0 / 60);
            foreach ((int _, double f) in easing.Tick(clock.Now, _ => fov))
            {
                fov = f;
            }
            Check.True(fov < last && fov > 45, "part of the way");
            last = fov;
        }
        clock.Advance(2);
        foreach ((int _, double f) in easing.Tick(clock.Now, _ => fov))
        {
            fov = f;
        }
        Check.True(fov == 45 && !easing.IsEasing, "ends on the target");
        easing.Set(2, 500, clock.Now);
        Check.Equal<double?>(FisheyeAim.MaxFov, easing.Target(2), "target clamped");
    }

    private static void ConstantsMatch()
    {
        foreach (FisheyeMount mount in Enum.GetValues<FisheyeMount>())
        {
            foreach (FisheyeViewMode mode in Enum.GetValues<FisheyeViewMode>().Where(m => m.Dewarps()))
            {
                FisheyeDewarp d = FisheyeDewarp.Of(Lens, mount, mode, new FisheyeAim(20, mount == FisheyeMount.Desk ? -30 : 30, 70), 960, 540);
                DewarpConstants c = d.Constants();
                Check.Equal(d.Views.Count, (int)c.ViewCount, "view count");
                for (double x = 0.01; x <= 0.99; x += 0.07)
                {
                    for (double y = 0.01; y <= 0.99; y += 0.07)
                    {
                        var p = new UnitPoint(x, y);
                        Check.True(Close(FisheyeDewarp.Shade(c, p), d.PicturePoint(p), 1e-4), $"{mount} {mode} at {x:F2},{y:F2}");
                    }
                }
            }
        }
    }

    private static unsafe void ConstantsLayout()
    {
        // 22 rows of 16 bytes, as the HLSL cbuffer and the Metal buffer expect.
        Check.Equal(DewarpConstants.Size, sizeof(DewarpConstants), "size");
        Check.Equal(352, sizeof(DewarpConstants), "352 bytes");
        Check.Equal(80, sizeof(DewarpViewRows), "view rows");
        var c = new DewarpConstants();
        byte* b = (byte*)&c;
        Check.Equal(16L, (byte*)&c.ThetaMax - b, "params row");
        Check.Equal(32L, (byte*)Unsafe.AsPointer(ref c.Views[0]) - b, "view 0");
        Check.Equal(32L + 3 * 80, (byte*)Unsafe.AsPointer(ref c.Views[3]) - b, "view 3");
        var v = new DewarpViewRows();
        byte* vb = (byte*)&v;
        Check.Equal(32L, (byte*)&v.YX - vb, "axis Y row");
        Check.Equal(64L, (byte*)&v.E0 - vb, "extra row");
    }

    private static void SettingMember()
    {
        Check.Equal(FisheyeTileSetting.Original, FisheyeTileSetting.FromJson(null), "no member");
        var ptz = new FisheyeTileSetting(FisheyeViewMode.Ptz, new FisheyeAim(12.5, 45, 70));
        string json = ptz.ToJson()!;
        Check.True(json.Contains("\"view\":\"ptz\"", StringComparison.Ordinal), "view name");
        Check.Equal(ptz, FisheyeTileSetting.FromJson(json), "round trip");
        Check.Equal<string?>(null, FisheyeTileSetting.Original.ToJson(), "Original is no member");
        Check.Equal(FisheyeViewMode.DoublePanorama, FisheyeTileSetting.FromJson("{\"view\":\"double_panorama\"}").Mode, "double");
        Check.Equal(FisheyeTileSetting.Original, FisheyeTileSetting.FromJson("{\"view\":\"sphere\"}"), "unknown view");
        Check.Equal(FisheyeTileSetting.Original, FisheyeTileSetting.FromJson("not json"), "malformed");
        Check.Equal(new FisheyeTileSetting(FisheyeViewMode.Quad, new FisheyeAim(10, 45, 90)),
            FisheyeTileSetting.FromJson("{\"view\":\"quad\",\"pan\":10,\"tilt\":45,\"fov\":90}"), "integers");
    }

    private static void AimDefaults()
    {
        var s = new FisheyeTileSetting(FisheyeViewMode.Ptz);
        Check.Equal(new FisheyeAim(0, 45, 90), s.AimFor(FisheyeMount.Ceiling), "ceiling");
        Check.Equal(new FisheyeAim(0, -45, 90), s.AimFor(FisheyeMount.Desk), "desk");
        var moved = new FisheyeTileSetting(FisheyeViewMode.Ptz, new FisheyeAim(10, 60, 50));
        Check.Equal(new FisheyeAim(10, 0, 50), moved.AimFor(FisheyeMount.Desk), "kept in range");
    }

    /// <summary>A synthetic fisheye picture: a checkerboard circle on black, soft at the rim, with bright text-like bars in the bottom corners.</summary>
    private static Func<int, int, byte> Synthetic(int width, int height, FisheyeCircle c, byte black = 16, bool overlay = true) => (x, y) =>
    {
        double px = x + 0.5, py = y + 0.5;
        double dx = px - c.Cx * width, dy = py - c.Cy * height, r = c.R * width;
        if (dx * dx + dy * dy <= r * r)
        {
            bool cell = ((int)(px / 12) + (int)(py / 12)) % 2 == 0;
            double edge = Math.Min(1, (r - Math.Sqrt(dx * dx + dy * dy)) / (0.02 * r) + 0.35);
            return (byte)(black + (cell ? 170 : 60) * edge);
        }
        if (overlay && y > height - height / 20 && y < height - height / 60 && x / 3 % 2 == 0 && (x < width / 4 || x > width * 3 / 4))
        {
            return 235;
        }
        return black;
    };

    private static void CircleFound()
    {
        (int, int, FisheyeCircle)[] cases =
        [
            (384, 384, new(0.49, 0.505, 0.46)),
            (1024, 1024, new(0.501, 0.502, 0.471)),
            (640, 480, new(0.52, 0.48, 0.33)),
            (800, 800, new(0.5, 0.5, 0.40)),
        ];
        foreach ((int w, int h, FisheyeCircle c) in cases)
        {
            FisheyeCircle? found = FisheyeCircleDetector.Detect(w, h, Synthetic(w, h, c));
            Check.True(found is { } f && Math.Abs(f.Cx - c.Cx) < 0.01 && Math.Abs(f.Cy - c.Cy) < 0.01 && Math.Abs(f.R - c.R) < 0.01,
                $"{w}x{h}: {found} vs {c}");
        }
    }

    private static void CircleFullRange()
    {
        FisheyeCircle? found = FisheyeCircleDetector.Detect(512, 512, Synthetic(512, 512, new FisheyeCircle(0.5, 0.5, 0.45), 0, false));
        Check.True(found is { } f && Math.Abs(f.R - 0.45) < 0.01, $"full range: {found}");
    }

    private static void NoCircle()
    {
        Check.True(FisheyeCircleDetector.Detect(256, 256, (_, _) => 16) is null, "all black");
        Check.True(FisheyeCircleDetector.Detect(256, 256, (x, y) => (byte)(60 + (x ^ y) % 120)) is null, "edge to edge");
        Check.True(FisheyeCircleDetector.Detect(8, 8, (_, _) => 200) is null, "too small");
    }

    private static void Tracker()
    {
        var clock = new ManualViewerClock();
        var t = new FisheyeCircleTracker();
        Check.True(t.WantsTry(clock.Now), "first try");
        t.Record(null, clock.Now, 640, 480);
        clock.Advance(0.2);
        Check.True(!t.WantsTry(clock.Now), "not before half a second");
        clock.Advance(0.3);
        Check.True(t.WantsTry(clock.Now), "after half a second");
        for (int i = 1; i < FisheyeCircleTracker.MaxTries; i++)
        {
            Check.True(t.Circle is null, "no circle yet");
            clock.Advance(1);
            t.Record(null, clock.Now, 640, 480);
        }
        Check.True(t.FellBack && t.Circle == new FisheyeCircle(0.5, 0.5, 0.375), "inscribed fallback");
        Check.True(!t.WantsTry(clock.Now + 100), "no more tries");
        var found = new FisheyeCircleTracker();
        found.Record(new FisheyeCircle(0.5, 0.5, 0.4), 0, 384, 384);
        Check.True(found.Circle?.R == 0.4 && !found.FellBack && !found.WantsTry(10), "found");
    }
}
