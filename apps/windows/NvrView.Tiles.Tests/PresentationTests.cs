// Tests of calls in their own window and movable insets (NvrView.Tiles/Insets.cs, CallWindow.cs,
// Calls.cs). The vectors are the macOS CallPresentationTests' (apps/macos NVRCoreTests), the same
// numbers in the same order; keep the two copies in step. Not ported: the three InsetBoard
// detached-item scenarios (the Windows app has no InsetBoard; its call cards are driven by Call
// and CallInsets).

using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class PresentationTests
{
    private static readonly CameraRef Door = new("d-1", 3);
    private static readonly DateTime T0 = new(2023, 11, 14, 22, 13, 20, DateTimeKind.Utc);

    public static IEnumerable<(string, Action)> All() =>
    [
        ("presentation: each axis snaps to its edge within 24 points", AxisSnaps),
        ("presentation: a dragged column becomes a fraction of the free space", DraggedColumn),
        ("presentation: anchors are clamped and saved clamped", AnchorsClamp),
        ("presentation: the column sits at its anchor and grows away from the nearer edge", ColumnAtAnchor),
        ("presentation: the corner rule is the anchor rule at the corners", CornerIsAnchor),
        ("presentation: the doorbell's choice wins over the global default", ResolveVectors),
        ("presentation: a call keeps its presentation, latch and draft", CallKeepsState),
        ("presentation: call windows open where left or top right of the active screen", PlacementVectors),
        ("presentation: rectangles flip between top-left and bottom-left origins", RectFlip),
        ("presentation: call windows ask for the main stream when wider than the sub-stream", QualityVectors),
    ];

    private static bool Near(double a, double b) => Math.Abs(a - b) < 1e-9;

    private static void AxisSnaps()
    {
        // (position, size, stage, fraction). Margin 12, snap 24.
        (double Position, double Size, double Stage, double Fraction)[] vectors =
        [
            // Free space 100 (424 - 2 * 12 - 300).
            (12, 300, 424, 0), // at the start margin
            (36, 300, 424, 0), // 24 from the start: snaps
            (37, 300, 424, 0.25), // 25 from the start: free
            (62, 300, 424, 0.5),
            (87, 300, 424, 0.75), // 25 from the end: free
            (88, 300, 424, 1), // 24 from the end: snaps
            (112, 300, 424, 1), // at the end margin
            (-50, 300, 424, 0), // dragged past the start
            (500, 300, 424, 1), // dragged past the end
            // Free space 40: both ends within reach; the nearer wins, the start on a tie.
            (32, 300, 364, 0),
            (33, 300, 364, 1),
            // No free space.
            (12, 300, 300, 0),
            (100, 300, 300, 0),
        ];
        foreach (var v in vectors)
        {
            double f = InsetPlacement.Fraction(v.Position, v.Size, v.Stage);
            Check.True(Near(f, v.Fraction), $"position {v.Position} size {v.Size} stage {v.Stage}: {f}");
        }
    }

    private static void DraggedColumn()
    {
        // (origin, column, stage) -> anchor. Free space 676 x 376.
        (double Ox, double Oy, double Cw, double Ch, double Sw, double Sh, double Ax, double Ay)[] vectors =
        [
            (181, 106, 300, 200, 1000, 600, 0.25, 0.25),
            (350, 200, 300, 200, 1000, 600, 0.5, 0.5),
            (688, 30, 300, 200, 1000, 600, 1, 0),
            (350, 400, 300, 200, 1000, 600, 0.5, 1),
            (0, 0, 300, 200, 1000, 600, 0, 0),
        ];
        foreach (var v in vectors)
        {
            InsetAnchor a = InsetPlacement.Anchor(v.Ox, v.Oy, v.Cw, v.Ch, v.Sw, v.Sh);
            Check.True(Near(a.X, v.Ax) && Near(a.Y, v.Ay), $"origin ({v.Ox}, {v.Oy}): {a}");
        }
        // Away from the edges the origin comes back.
        (double x, double y) = InsetPlacement.Origin(new InsetAnchor(0.25, 0.25), 300, 200, 1000, 600);
        Check.True(x == 181 && y == 106, "the origin comes back");
        // A resize keeps the fraction: the same anchor on a larger stage.
        (double wx, double wy) = InsetPlacement.Origin(new InsetAnchor(1, 0.5), 300, 200, 2000, 1000);
        Check.True(wx == 2000 - 12 - 300 && wy == 12 + 0.5 * (1000 - 24 - 200), "a resize keeps the fraction");
    }

    private static void AnchorsClamp()
    {
        Check.Equal(new InsetAnchor(0, 1), new InsetAnchor(-1, 2), "clamped to 0..1");
        Check.Equal(new InsetAnchor(0, 0), new InsetAnchor(double.NaN, double.PositiveInfinity), "not finite counts as 0");
        Check.Equal(new InsetAnchor(0, 0), InsetAnchor.FromCorner(ChromeCorner.TopLeft), "top left");
        Check.Equal(new InsetAnchor(1, 0), InsetAnchor.FromCorner(ChromeCorner.TopRight), "top right");
        Check.Equal(new InsetAnchor(0, 1), InsetAnchor.FromCorner(ChromeCorner.BottomLeft), "bottom left");
        Check.Equal(new InsetAnchor(1, 1), InsetAnchor.FromCorner(ChromeCorner.BottomRight), "bottom right");
        Check.Equal(new InsetAnchor(1, 0.25), InsetAnchor.FromJson("""{"x":1.5,"y":0.25}""")!.Value, "a saved value is clamped");
        Check.Equal(new InsetAnchor(0.3, 0.7), InsetAnchor.FromJson(new InsetAnchor(0.3, 0.7).ToJson())!.Value, "round trip");
        Check.True(InsetAnchor.FromJson("""{"x":"a"}""") is null && InsetAnchor.FromJson("not json") is null && InsetAnchor.FromJson(null) is null, "bad saved values are ignored");
        Check.True(!new InsetAnchor(0, 0.5).GrowsUpward && new InsetAnchor(0, 0.51).GrowsUpward, "grows upward below the middle only");
    }

    private static void ColumnAtAnchor()
    {
        // Stage 1000 x 600, inset width 300: (anchor, heights) -> (x, y) per drawn inset, newest first.
        (double Ax, double Ay, double[] Heights, (double X, double Y)[] Frames)[] vectors =
        [
            // Corners: the same as the corner rule (two of three fit).
            (0, 0, [200, 200, 200], [(12, 12), (12, 220)]),
            (1, 0, [200, 200, 200], [(688, 12), (688, 220)]),
            (0, 1, [200, 200, 200], [(12, 388), (12, 180)]),
            (1, 1, [200, 200, 200], [(688, 388), (688, 180)]),
            // The middle grows downward.
            (0.5, 0.5, [100, 100], [(350, 196), (350, 304)]),
            // Below the middle grows upward: the newest at the column's bottom.
            (0.25, 0.75, [100, 150], [(181, 408.5), (181, 250.5)]),
            // Above the middle grows downward.
            (0.25, 0.25, [100, 150], [(181, 91.5), (181, 199.5)]),
            // One inset taller than the stage is still drawn.
            (0, 1, [700], [(12, -112)]),
            (0, 0, [700, 100], [(12, 12)]),
            (0, 0, [], []),
        ];
        foreach (var v in vectors)
        {
            IReadOnlyList<InsetFrame> frames = InsetGeometry.Frames(v.Heights.Length, 300, v.Heights, new InsetAnchor(v.Ax, v.Ay), 1000, 600);
            Check.Equal(v.Frames.Length, frames.Count, $"anchor ({v.Ax}, {v.Ay}) heights {string.Join(",", v.Heights)}");
            for (int i = 0; i < frames.Count; i++)
            {
                Check.True(Near(frames[i].X, v.Frames[i].X) && Near(frames[i].Y, v.Frames[i].Y), $"anchor ({v.Ax}, {v.Ay}): {frames[i]} want {v.Frames[i]}");
                Check.Equal(300.0, frames[i].Width, "width");
            }
        }
    }

    private static void CornerIsAnchor()
    {
        double[] heights = [120, 180, 90, 300];
        foreach (ChromeCorner corner in new[] { ChromeCorner.TopLeft, ChromeCorner.TopRight, ChromeCorner.BottomLeft, ChromeCorner.BottomRight })
        {
            foreach ((double w, double h) in new[] { (1000.0, 600.0), (640.0, 360.0), (300.0, 200.0) })
            {
                IReadOnlyList<InsetFrame> a = InsetGeometry.Frames(4, 220, heights, corner, w, h);
                IReadOnlyList<InsetFrame> b = InsetGeometry.Frames(4, 220, heights, InsetAnchor.FromCorner(corner), w, h);
                Check.True(a.SequenceEqual(b), $"{corner} on {w}x{h}");
            }
        }
    }

    private static void ResolveVectors()
    {
        // (global "Open calls in their own window", per-doorbell override) -> presentation.
        (bool Global, CallPresentation? Override, CallPresentation Result)[] vectors =
        [
            (false, null, CallPresentation.Inset),
            (true, null, CallPresentation.Window),
            (false, CallPresentation.Window, CallPresentation.Window),
            (true, CallPresentation.Inset, CallPresentation.Inset),
            (false, CallPresentation.Inset, CallPresentation.Inset),
            (true, CallPresentation.Window, CallPresentation.Window),
        ];
        foreach (var v in vectors)
        {
            Check.Equal(v.Result, CallPresentations.Resolve(v.Global, v.Override), $"global {v.Global} override {v.Override}");
        }
    }

    private static void CallKeepsState()
    {
        var call = new Call(Door, T0, 10, CallAudio.Press(Door, []));
        Check.True(call.Presentation == CallPresentation.Inset && !call.Latched && call.Draft.Length == 0, "defaults");
        call = new Call(Door, T0, 10, new CallAudioDecision(false, true, []), CallOrigin.Press, CallPresentation.Window);
        Check.Equal(CallPresentation.Window, call.Presentation, "opened in a window");
        // A press joining the call keeps its presentation.
        call.Pressed(12);
        Check.True(call.Presentation == CallPresentation.Window && call.Presses == 2, "a press keeps the presentation");
        // Keep talking and typing are activity.
        call.SetLatched(true, 20);
        Check.True(call.Latched && call.LastActivity == 20, "latched");
        call.MicrophoneStopped();
        Check.True(!call.Latched, "the microphone stopped");
        call.Edited("One moment", 30);
        Check.True(call.Draft == "One moment" && call.LastActivity == 30, "edited");
        // Sent: the field empties, unless the user typed on meanwhile.
        call.Edited("One moment please", 31);
        call.Sent("One moment");
        Check.Equal("One moment please", call.Draft, "typed on meanwhile");
        call.Sent("One moment please");
        Check.Equal("", call.Draft, "sent");
        // In its own window the call never ends by itself.
        Check.True(!call.Expired(31 + Call.IdleTimeout), "a call in its own window does not expire");
        Check.True(!call.Expired(31 + (100 * Call.IdleTimeout)), "not even much later");
        // Pop In (with the activity it counts as) restores the minute, starting fresh.
        double popIn = 31 + (10 * Call.IdleTimeout);
        call.Presentation = CallPresentation.Inset;
        call.Activity(popIn);
        Check.True(!call.Expired(popIn + Call.IdleTimeout - 1), "not yet expired after Pop In");
        Check.True(call.Expired(popIn + Call.IdleTimeout), "expired a minute after Pop In");
    }

    // Visible frames, top-left origin: a screen with a 25 point menu bar, and a second screen to its right.
    private static readonly WindowRect ScreenA = new(0, 25, 1440, 875);
    private static readonly WindowRect ScreenB = new(1440, 0, 1920, 1080);
    private static readonly WindowRect Small = new(0, 0, 400, 300);

    private static void PlacementVectors()
    {
        // (mode, remembered, screens, active, index) -> frame.
        (CallWindowPlacement.Mode Mode, WindowRect? Remembered, WindowRect[] Screens, WindowRect? Active, int Index, WindowRect Frame)[] vectors =
        [
            // Where I left it.
            (CallWindowPlacement.Mode.Remembered, new WindowRect(100, 100, 500, 420), [ScreenA, ScreenB], ScreenB, 0,
             new WindowRect(100, 100, 500, 420)),
            // Further windows cascade 24 right and down.
            (CallWindowPlacement.Mode.Remembered, new WindowRect(100, 100, 500, 420), [ScreenA, ScreenB], ScreenA, 2,
             new WindowRect(148, 148, 500, 420)),
            // ... kept inside the remembered frame's screen.
            (CallWindowPlacement.Mode.Remembered, new WindowRect(900, 450, 500, 420), [ScreenA], ScreenA, 2,
             new WindowRect(940, 480, 500, 420)),
            // The middle of the top edge on the second screen: still there.
            (CallWindowPlacement.Mode.Remembered, new WindowRect(1300, 20, 480, 410), [ScreenA, ScreenB], ScreenA, 0,
             new WindowRect(1300, 20, 480, 410)),
            // ... and with that screen gone: the top right of the active screen, the size kept.
            (CallWindowPlacement.Mode.Remembered, new WindowRect(1300, 20, 480, 410), [ScreenA], null, 0,
             new WindowRect(948, 37, 480, 410)),
            (CallWindowPlacement.Mode.Remembered, new WindowRect(2000, 100, 500, 420), [ScreenA], ScreenA, 0,
             new WindowRect(928, 37, 500, 420)),
            // The title bar under the menu bar: not reachable, so not used.
            (CallWindowPlacement.Mode.Remembered, new WindowRect(100, 10, 500, 420), [ScreenA], ScreenA, 0,
             new WindowRect(928, 37, 500, 420)),
            // Top right of the active screen, whatever was remembered.
            (CallWindowPlacement.Mode.ActiveScreen, new WindowRect(100, 100, 500, 420), [ScreenA, ScreenB], ScreenB, 0,
             new WindowRect(2848, 12, 500, 420)),
            // Nothing remembered: 480 wide; further windows 24 left and down.
            (CallWindowPlacement.Mode.Remembered, null, [ScreenA], null, 0, new WindowRect(948, 37, 480, 410)),
            (CallWindowPlacement.Mode.ActiveScreen, null, [ScreenA, ScreenB], ScreenB, 1, new WindowRect(2844, 36, 480, 410)),
            // A screen smaller than the window: the size is limited to it.
            (CallWindowPlacement.Mode.ActiveScreen, null, [Small], Small, 0, new WindowRect(12, 12, 376, 276)),
            // No screens at all.
            (CallWindowPlacement.Mode.Remembered, null, [], null, 1, new WindowRect(24, 24, 480, 410)),
        ];
        for (int i = 0; i < vectors.Length; i++)
        {
            var v = vectors[i];
            WindowRect frame = CallWindowPlacement.Frame(v.Mode, v.Remembered, v.Screens, v.Active, v.Index);
            Check.Equal(v.Frame, frame, $"vector {i}");
        }
        Check.Equal("Where I left it", CallWindowPlacement.Mode.Remembered.Title(), "remembered title");
        Check.Equal("Top right of the active screen", CallWindowPlacement.Mode.ActiveScreen.Title(), "active screen title");
    }

    private static void RectFlip()
    {
        var r = new WindowRect(10, 20, 100, 50);
        Check.Equal(new WindowRect(10, 830, 100, 50), r.Flipped(900), "flipped");
        Check.Equal(r, r.Flipped(900).Flipped(900), "flipped twice");
        Check.True(CallWindowPlacement.IsOnScreen(new WindowRect(1200, 25, 480, 410), [ScreenA]), "on screen");
        Check.True(!CallWindowPlacement.IsOnScreen(new WindowRect(1300, 25, 480, 410), [ScreenA]), "off screen");
    }

    private static void QualityVectors()
    {
        // (drawn width, sub-stream width, asked for main before) -> main.
        (double Drawn, double? Sub, bool WasMain, bool Main)[] vectors =
        [
            (960, 640, false, true),
            (640, 640, false, false),
            (641, 640, false, true),
            (600, 640, true, true),
            (512, 640, true, true),
            (511, 640, true, false),
            (300, null, false, false),
            (700, null, false, true),
            (900, 0, false, true),
            (320, 352, false, false),
            (353, 352, false, true),
        ];
        for (int i = 0; i < vectors.Length; i++)
        {
            var v = vectors[i];
            Check.Equal(v.Main, CallWindowQuality.WantsMain(v.Drawn, v.Sub, v.WasMain), $"vector {i}");
        }
    }
}
