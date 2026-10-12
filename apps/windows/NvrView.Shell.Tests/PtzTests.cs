using NvrView.Interop;
using NvrView.Tiles;

namespace NvrView.Shell.Tests;

/// <summary>
/// Camera motion (docs/UX.md "Pan, tilt, and zoom"): the pure pieces of Ptz.cs and the model over the
/// fake camera on a manual clock, the same rules the macOS tests and self-test check
/// (ptz-pad-hold, ptz-deadman, ptz-preset-recall, ptz-lens-clamp, ptz-lens1-addressing).
/// </summary>
internal static class PtzTests
{
    private static readonly CameraRef Camera = new("d-1", 1);
    private static readonly CameraRef Second = new("d-1", 5, 1);
    private const ulong TileA = 7;
    private const ulong TileB = 9;

    private sealed class Rig
    {
        public ManualViewerClock Clock { get; } = new(100);
        public FakePtzCore Core { get; }
        public PtzModel Model { get; }
        public List<bool> Timer { get; } = [];
        public int SavedSpeed { get; private set; }
        public Dictionary<CameraRef, ulong> Tiles { get; } = new() { [Camera] = TileA, [Second] = TileB };

        public Rig(bool lens1Moves = true)
        {
            Core = new FakePtzCore(Clock);
            Core.Add(TileA, new FakePtzCamera());
            Core.Add(TileB, new FakePtzCamera { Lens = 1, PanTilt = lens1Moves });
            Model = new PtzModel(
                Core, Clock, c => Tiles.TryGetValue(c, out ulong t) ? t : null,
                t => Tiles.Where(kv => kv.Value == t).Select(kv => (CameraRef?)kv.Key).FirstOrDefault(),
                saveSpeed: s => SavedSpeed = s, backupTimer: on => Timer.Add(on));
        }

        /// <summary>Time passes in steps, ticking as the stage does.</summary>
        public void Advance(double seconds, double step = 0.1)
        {
            for (double t = 0; t < seconds - 1e-9; t += step)
            {
                Clock.Advance(step);
                Model.Tick();
            }
        }

        public void Load(CameraRef camera)
        {
            Model.Load(camera);
        }
    }

    public static void Run()
    {
        T.Run("ptz: info parses", () =>
        {
            PtzInfo? info = PtzInfo.Parse("""{"pan_tilt":true,"zoom":{"min":0,"max":28,"cur":3},"focus":null,"presets":[{"id":0,"name":"Door"}],"speeds":[1,64]}""");
            T.NotNull(info);
            T.Check(info!.PanTilt);
            T.Equal(new PtzRange(0, 28, 3), info.Zoom);
            T.Check(info.Focus is null);
            T.Equal(1, info.Presets.Count);
            T.Equal(new PtzPreset(0, "Door"), info.Presets[0]);
            T.Equal(64, info.MaxSpeed);
            T.Check(PtzInfo.Parse("{") is null && PtzInfo.Parse("[]") is null);
            T.Check(!PtzInfo.Parse("{}")!.PanTilt && !PtzInfo.Parse("{}")!.HasLens);
        });

        T.Run("ptz: a held move renews well inside the core's limit", () =>
        {
            var hold = new PtzHold();
            T.Equal(PtzCall.Move(PtzDirection.Left, 20), hold.Press(PtzDirection.Left, 20, 0));
            T.Check(hold.Tick(0.1) is null);
            T.Equal(PtzCall.Move(PtzDirection.Left, 20), hold.Tick(0.25));
            T.Check(hold.Tick(0.4) is null);
            T.Check(PtzHold.RenewInterval < 0.5);
            T.Equal(PtzCall.Stop, hold.Release());
            T.Check(hold.Release() is null);
            T.Check(hold.Tick(5) is null);
        });

        T.Run("ptz: the lens stays in range", () =>
        {
            var r = new PtzRange(0, 28, 27);
            T.Equal(28, r.Stepped(27, 1));
            T.Equal(0, r.Stepped(1, -1));
            T.Equal(28, r.Clamped(99));
            T.Equal(28, r.PositionAt(2));
            T.Equal(0.5, r.Fraction(14));
            T.Equal(0, new PtzRange(5, 5, 5).Fraction(5));
        });

        T.Run("ptz: a lens drag sends five a second and the last", () =>
        {
            var t = new PtzLensThrottle();
            T.Equal(1, t.Moved(1, 0));
            T.Check(t.Moved(2, 0.05) is null);
            T.Check(t.Moved(3, 0.1) is null);
            T.Equal(3, t.Tick(0.2));
            T.Check(t.Moved(4, 0.25) is null);
            T.Equal(4, t.End());
            T.Check(t.End() is null);
        });

        T.Run("ptz: the Alt keys read by key position", () =>
        {
            static PtzKey? Key(int vk, int scan = 0, bool alt = true, bool ctrl = false, bool shift = false, bool rep = false, bool text = false) =>
                PtzKeys.Map(vk, scan, alt, ctrl, shift, rep, text);
            T.Equal(PtzKey.Move(PtzDirection.Left), Key(PtzKeys.VkLeft));
            T.Equal(PtzKey.Move(PtzDirection.Down), Key(PtzKeys.VkDown, rep: true));
            T.Equal(PtzKey.Zoom(1), Key(0xBB, PtzKeys.ScanEquals));
            T.Equal(PtzKey.Zoom(-1), Key(0xBD, PtzKeys.ScanMinus));
            T.Equal(PtzKey.Zoom(1), Key(0xDB, PtzKeys.ScanEquals), "another layout's character on the same position");
            T.Equal(PtzKey.Preset(1), Key(0x31, 0x02));
            T.Equal(PtzKey.Preset(9), Key(0, 0x0A));
            T.Equal(PtzKey.Preset(3), Key(0x33));
            T.Check(Key(0x39, 0x0A, rep: true) is null);
            T.Check(Key(0x30, 0x0B) is null, "0 is no preset");
            T.Check(Key(PtzKeys.VkLeft, alt: false) is null);
            T.Check(Key(PtzKeys.VkLeft, shift: true) is null);
            T.Check(Key(PtzKeys.VkLeft, ctrl: true) is null);
            T.Check(Key(PtzKeys.VkLeft, text: true) is null);
            T.Equal(PtzDirection.Up, PtzKeys.ArrowOf(PtzKeys.VkUp));
            T.Check(PtzKeys.ArrowOf(0x41) is null);
        });

        T.Run("ptz: events explain refusals", () =>
        {
            T.Check(new PtzEvent(1, "move", PtzEventState.Refused, "rate_limited").Problem is not null);
            T.Check(new PtzEvent(1, "move", PtzEventState.Sent).Problem is null);
            T.Equal("The camera refused the command (status 9)", new PtzEvent(1, "move", PtzEventState.Failed, "device_refused", 9).Problem);
            T.Equal(PtzEventState.Failed, PtzEvent.StateOf("failed"));
            T.Equal(PtzEventState.Other, PtzEvent.StateOf("later"));
        });

        // macOS step ptz-pad-hold
        T.Run("ptz-pad-hold: a held pad renews at least every 500 ms and stops on release", () =>
        {
            var rig = new Rig();
            rig.Load(Camera);
            T.NotNull(rig.Model.InfoFor(Camera));
            rig.Model.Press(PtzDirection.Left, Camera);
            T.Equal(true, rig.Timer.LastOrDefault(), "the backup timer runs while held");
            rig.Advance(1.2);
            T.Check(rig.Core.IsMoving(TileA), "the camera moves while the pad is held");
            (double At, string Text)[] held = [.. rig.Model.Calls];
            int renewed = held.Count(c => c.Text == "move left 20");
            double gap = held.Zip(held.Skip(1), (a, b) => b.At - a.At).Max();
            T.Check(renewed >= 4, $"{renewed} move calls over 1.2 s");
            T.Check(gap <= 0.5, $"longest gap {gap}");
            rig.Model.Release();
            T.Equal("stop", rig.Model.Calls[^1].Text);
            T.Check(rig.Model.Held is null && !rig.Core.IsMoving(TileA));
            T.Equal(false, rig.Timer.Last());
            int calls = rig.Model.Calls.Count;
            rig.Advance(1);
            T.Equal(calls, rig.Model.Calls.Count, "nothing is renewed after the release");
        });

        T.Run("ptz: the speed is kept and used", () =>
        {
            var rig = new Rig();
            rig.Load(Camera);
            rig.Model.Speed = 99;
            T.Equal(64, rig.Model.Speed);
            T.Equal(64, rig.SavedSpeed);
            rig.Model.Speed = 30;
            rig.Model.Press(PtzDirection.Up, Camera);
            T.Equal("move up 30", rig.Model.Calls[^1].Text);
            rig.Model.Release();
        });

        T.Run("ptz: an arrow key holds the move and its release stops it", () =>
        {
            var rig = new Rig();
            rig.Load(Camera);
            rig.Model.KeyDown(PtzDirection.Right, Camera);
            rig.Model.KeyDown(PtzDirection.Right, Camera);
            rig.Advance(0.3);
            T.Equal(Camera, rig.Model.Held);
            T.Equal(1, rig.Model.Calls.Count(c => c.Text == "move right 20" && c.At == 100), "a key repeat is not a new press");
            rig.Model.KeyUp(PtzDirection.Left);
            T.Equal(Camera, rig.Model.Held);
            rig.Model.KeyUp(PtzDirection.Right);
            T.Check(rig.Model.Held is null);
            T.Equal("stop", rig.Model.Calls[^1].Text);
            // Alt up while an arrow holds a move ends it too.
            rig.Model.KeyDown(PtzDirection.Up, Camera);
            rig.Model.ModifiersChanged(altDown: false);
            T.Check(rig.Model.Held is null && !rig.Core.IsMoving(TileA));
            // A button hold is not ended by Alt.
            rig.Model.Press(PtzDirection.Down, Camera);
            rig.Model.ModifiersChanged(altDown: false);
            T.Equal(Camera, rig.Model.Held);
            rig.Model.StopAll();
            T.Check(rig.Model.Held is null);
        });

        // macOS step ptz-deadman
        T.Run("ptz-deadman: the camera stops by itself when the app goes quiet", () =>
        {
            var rig = new Rig();
            rig.Load(Camera);
            rig.Model.Press(PtzDirection.Up, Camera);
            rig.Advance(0.3);
            T.Check(rig.Core.IsMoving(TileA));
            rig.Model.FrozenForTest = true;
            rig.Advance(1.2);
            T.Check(!rig.Core.IsMoving(TileA), "the camera stopped on its own");
            rig.Model.FrozenForTest = false;
            rig.Model.Release();
        });

        T.Run("ptz: a tile closing, a lost focus and the core's own stop end the hold", () =>
        {
            var rig = new Rig();
            rig.Load(Camera);
            rig.Model.Press(PtzDirection.Left, Camera);
            rig.Model.CameraClosed(Camera);
            T.Check(rig.Model.Held is null && rig.Model.InfoFor(Camera) is null);
            rig.Load(Camera);
            rig.Model.Press(PtzDirection.Left, Camera);
            rig.Model.StopAll();
            T.Equal("stop", rig.Model.Calls[^1].Text);
            rig.Model.Press(PtzDirection.Left, Camera);
            rig.Model.Handle(new PtzEvent(TileA, "stop", PtzEventState.Sent, "max_hold"));
            T.Check(rig.Model.Held is null);
            T.Equal("Stopped after 30 seconds; release and press again", rig.Model.ProblemFor(Camera));
            // A tile that closed: nothing is sent for it.
            rig.Tiles.Remove(Camera);
            int calls = rig.Model.Calls.Count;
            rig.Model.Press(PtzDirection.Left, Camera);
            T.Equal(calls, rig.Model.Calls.Count);
            T.Check(rig.Model.Held is null);
        });

        // macOS step ptz-preset-recall
        T.Run("ptz-preset-recall: presets recall in the camera's order", () =>
        {
            var rig = new Rig();
            rig.Load(Camera);
            rig.Model.Recall(0, Camera);
            T.Check(rig.Model.RecallNth(2, Camera));
            T.Check(!rig.Model.RecallNth(4, Camera) && !rig.Model.RecallNth(0, Camera));
            T.Check(rig.Core.Calls.Select(c => c.Text).Where(t => t.StartsWith("preset", StringComparison.Ordinal)).SequenceEqual(["preset 0", "preset 1"]));
            // The positions are read again once the camera has settled.
            int reads = rig.Model.InfoReads;
            rig.Advance(PtzModel.RereadDelay - 0.5);
            T.Equal(reads, rig.Model.InfoReads);
            rig.Advance(1);
            T.Equal(reads + 1, rig.Model.InfoReads);
            // A preset the camera no longer has is explained.
            rig.Core.FailNext = NvStatus.InvalidArgument;
            rig.Model.Recall(2, Camera);
            T.Check(rig.Model.ProblemFor(Camera)?.StartsWith("The camera was not moved", StringComparison.Ordinal) == true);
        });

        // macOS step ptz-lens-clamp
        T.Run("ptz-lens-clamp: the lens stays within its range", () =>
        {
            var rig = new Rig();
            rig.Load(Camera);
            for (int i = 0; i < 40; i++)
            {
                rig.Model.ZoomStep(1, Camera);
            }
            T.Equal(28, rig.Model.LensPosition(Camera, PtzLensAxis.Zoom));
            T.Equal(28, rig.Core.Camera(TileA).Zoom!.Value.Cur);
            for (int i = 0; i < 60; i++)
            {
                rig.Model.ZoomStep(-1, Camera);
            }
            T.Equal(0, rig.Model.LensPosition(Camera, PtzLensAxis.Zoom));
            rig.Model.StepLens(PtzLensAxis.Focus, 1, Camera);
            T.Equal(11, rig.Model.LensPosition(Camera, PtzLensAxis.Focus));
            // Past the range is clamped before it is sent.
            rig.Model.DragLens(PtzLensAxis.Zoom, 5000, Camera);
            T.Equal("lens zoom 28", rig.Core.Calls[^1].Text);
        });

        T.Run("ptz: a lens drag sends the first position and the last", () =>
        {
            var rig = new Rig();
            rig.Load(Camera);
            int before = rig.Core.Calls.Count;
            for (int i = 0; i < 5; i++)
            {
                rig.Model.DragLens(PtzLensAxis.Zoom, i * 5, Camera);
            }
            rig.Model.EndLensDrag(PtzLensAxis.Zoom, Camera);
            string[] sent = [.. rig.Core.Calls.Skip(before).Select(c => c.Text)];
            T.Check(sent.SequenceEqual(["lens zoom 0", "lens zoom 20"]), string.Join(", ", sent));
            // A slow drag sends five a second, with the held position going out on the tick.
            before = rig.Core.Calls.Count;
            rig.Model.DragLens(PtzLensAxis.Focus, 1, Camera);
            rig.Clock.Advance(0.1);
            rig.Model.DragLens(PtzLensAxis.Focus, 2, Camera);
            rig.Model.Tick();
            T.Equal(1, rig.Core.Calls.Count - before);
            rig.Clock.Advance(0.15);
            rig.Model.Tick();
            T.Check(rig.Core.Calls.Skip(before).Select(c => c.Text).SequenceEqual(["lens focus 1", "lens focus 2"]));
        });

        // macOS step ptz-lens1-addressing
        T.Run("ptz-lens1-addressing: a second lens is addressed as its own camera", () =>
        {
            var rig = new Rig();
            rig.Load(Second);
            T.Check(rig.Model.InfoFor(Second)!.PanTilt);
            rig.Model.Press(PtzDirection.Up, Second);
            rig.Model.Release();
            (ulong tile, string text, int lens) = rig.Core.Calls.Last(c => c.Text.StartsWith("move up", StringComparison.Ordinal));
            T.Equal(TileB, tile);
            T.Equal(1, lens);
            T.Equal("move up 20", text);
            T.Check(rig.Core.Calls.All(c => c.Tile == TileB), "nothing reached the first lens's tile");
        });

        T.Run("ptz: a failed call and an unanswered read are announced", () =>
        {
            var rig = new Rig();
            var heard = new List<PtzChange>();
            rig.Model.Changed += heard.Add;
            rig.Load(Camera);
            rig.Core.FailNext = NvStatus.NotPermitted;
            rig.Model.Press(PtzDirection.Left, Camera);
            T.Equal("The camera was not moved (status -7)", rig.Model.ProblemFor(Camera));
            T.Check(heard.Any(c => c.ProblemFor == Camera));
            rig.Model.Release();
            rig.Model.ClearProblem(Camera);
            T.Check(rig.Model.ProblemFor(Camera) is null);
            // A camera the core does not know answers with an error: the callers hear null.
            rig.Tiles[Camera] = 999;
            rig.Model.CameraClosed(Camera);
            PtzInfo? got = new PtzInfo();
            rig.Model.WithInfo(Camera, i => got = i);
            T.Check(got is null);
            T.Equal("The camera did not answer", rig.Model.ProblemFor(Camera));
        });
    }
}
