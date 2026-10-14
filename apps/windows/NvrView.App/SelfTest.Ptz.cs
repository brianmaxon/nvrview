// 11e. Camera motion (docs/UX.md "Pan, tilt, and zoom"), against the mock-nvr's cameras that move (never real
// hardware): the pad holds a move and renews it, an arrow key does too, the app going quiet lets the camera stop
// by itself, presets recall by menu and Alt+digit, the lens stays within its range, and a second lens is addressed
// as its own camera. The shell tests (NvrView.Shell.Tests PtzTests) check the same rules on a manual clock over a
// C# model of a camera; this runs the real core.

using NvrView.Shell;
using NvrView.Tiles;
using Windows.System;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    // The scan codes of "=" and "2", so the Alt keys are read by key position as they are on a keyboard.
    private const uint EqualsScan = 0x0D;
    private const uint DigitTwoScan = 0x03;

    private async Task PtzSteps(Viewer v, StageView stage)
    {
        PtzModel ptz = v.Ptz;
        StageTile? padTile = Tiles.FirstOrDefault(t => t.CameraOrNone is { Lens: 0 } c && v.ChannelOf(c)?.PtzOf(0).PanTilt == true);
        if (padTile is null)
        {
            _problems.Add("no camera that pans and tilts");
            return;
        }
        CameraRef camera = padTile.Camera;
        var clock = new ManualViewerClock(new SystemViewerClock().Now);
        v.UseClock(clock);
        try
        {
            await PtzOnCamera(v, stage, ptz, padTile, camera, clock);
        }
        finally
        {
            stage.ClosePtzFlyout();
            ptz.FrozenForTest = false;
            ptz.StopAll();
            v.UseClock(new SystemViewerClock());
        }
    }

    /// <summary>The stage's arrow keys as the window routes them: the camera first, then the selection.</summary>
    private static bool ArrowKey(StageView stage, VirtualKey key, bool alt)
    {
        bool handled = stage.HandlePtzKey(key, 0, alt, repeat: false);
        if (!handled)
        {
            handled = stage.HandleKey(key, ctrl: false, repeat: false);
        }
        stage.HandlePtzKeyUp(key);
        return handled;
    }

    /// <summary>Arrows move the selection where they do not steer a camera, and deactivating the window ends a held move.</summary>
    private void ArrowSelection(Viewer v, StageView stage, PtzModel ptz, StageTile padTile)
    {
        StageTile? right = stage.Neighbor(padTile, VirtualKey.Right);
        if (right is null)
        {
            _problems.Add("no tile to the right of the PTZ tile for the arrow keys");
            return;
        }
        int calls0 = ptz.Calls.Count;
        // parity: ptz-option-arrow-selection
        stage.Select(padTile.Path);
        ArrowKey(stage, VirtualKey.Right, alt: true);
        Say($"ptz Alt+arrow: selection {(stage.Selected == right ? "moved right" : "did not move")}, camera calls {ptz.Calls.Count - calls0}");
        Expect(stage.Selected == right, "Alt+arrow on a PTZ tile did not move the selection");
        Expect(ptz.Calls.Count == calls0 && ptz.Held is null, "Alt+arrow on a PTZ tile moved the camera");
        // parity: ptz-arrow-selection
        StageTile? plain = stage.Tiles.FirstOrDefault(t => !t.IsEmpty && t.CameraOrNone is { } c && v.ChannelOf(c)?.PtzOf(c.Lens).PanTilt != true);
        if (plain is not null && stage.Neighbor(plain, VirtualKey.Right) is { } plainRight)
        {
            stage.Select(plain.Path);
            ArrowKey(stage, VirtualKey.Right, alt: false);
            Say($"ptz arrow on a tile that does not move: selection {(stage.Selected == plainRight ? "moved right" : "did not move")}");
            Expect(stage.Selected == plainRight, "an arrow on a non-PTZ tile did not move the selection");
            Expect(ptz.Calls.Count == calls0 && ptz.Held is null, "an arrow on a non-PTZ tile moved a camera");
        }
        else
        {
            _problems.Add("no camera that does not pan and tilt, with a tile to its right, for the arrow-selection check");
        }
        // parity: ptz-arrow-deactivate
        stage.Select(padTile.Path);
        Expect(stage.HandlePtzKey(VirtualKey.Left, 0, alt: false, repeat: false), "Left was not taken for the camera");
        bool held = ptz.Held == padTile.Camera;
        stage.WindowDeactivated();
        Say($"ptz deactivation: held {held}, after {(ptz.Held is null ? "stopped" : "still held")}");
        Expect(held, "an arrow did not hold a move before the window was deactivated");
        Expect(ptz.Held is null && ptz.Calls[^1].Text == "stop", "deactivating the window did not stop the held move");
        stage.HandlePtzKeyUp(VirtualKey.Left);
    }

    private async Task PtzOnCamera(Viewer v, StageView stage, PtzModel ptz, StageTile padTile, CameraRef camera, ManualViewerClock clock)
    {
        int Sent(string action, string? reason = null) =>
            ptz.Events.Count(e => e.Action == action && e.State == PtzEventState.Sent && (reason is null || e.Reason == reason));
        int LensEvents() => ptz.Events.Count(e => e.Action == "lens");
        void Advance(double seconds)
        {
            for (double t = 0; t < seconds - 1e-9; t += 0.1)
            {
                clock.Advance(0.1);
                ptz.Tick();
            }
        }

        ptz.Load(camera);
        bool read = await Until(() => ptz.InfoFor(camera) is not null, 15);
        if (!read || ptz.InfoFor(camera) is not { } info || v.Leases.Tile(camera) is not { } tile)
        {
            _problems.Add("the camera's capabilities were not read");
            return;
        }
        stage.Select(padTile.Path);

        // parity: ptz-pad-hold
        stage.PerformHeaderItem(padTile, TileHeaderItem.PanTilt, null);
        if (stage.PtzContent?.Pad.GetValueOrDefault(PtzDirection.Left) is not { } pad)
        {
            _problems.Add("the pan and tilt pad did not open");
            return;
        }
        int calls0 = ptz.Calls.Count;
        int moves0 = Sent("move");
        pad.PressDown();
        Advance(1.2);
        (double At, string Text)[] held = [.. ptz.Calls.Skip(calls0)];
        double longestGap = held.Zip(held.Skip(1), (a, b) => b.At - a.At).DefaultIfEmpty(1).Max();
        int renewed = held.Count(c => c.Text.StartsWith("move left", StringComparison.Ordinal));
        pad.PressUp();
        Say($"ptz pad: {renewed} move calls over 1.2 s, longest gap {longestGap:0.00} s");
        Expect(renewed >= 4 && longestGap <= 0.5, "a held pad did not renew the move at least every 500 ms");
        Expect(ptz.Calls[^1].Text == "stop" && ptz.Held is null, "releasing the pad did not stop the camera");
        stage.ClosePtzFlyout();
        bool reported = await Until(() => Sent("move") > moves0 && Sent("stop", "released") >= 1, 10);
        Expect(reported, "the core did not report the pad's move and stop");
        // parity: ptz-arrow-hold
        // A plain arrow holds the move; its release stops it (a repeat is the same hold).
        Expect(stage.HandlePtzKey(VirtualKey.Right, 0, alt: false, repeat: false), "Right was not taken for the camera");
        Expect(stage.HandlePtzKey(VirtualKey.Right, 0, alt: false, repeat: true), "a repeat of Right was not taken for the camera");
        Advance(0.3);
        bool keyHeld = ptz.Held == camera;
        stage.HandlePtzKeyUp(VirtualKey.Right);
        Expect(keyHeld && ptz.Held is null && ptz.Calls[^1].Text == "stop", "an arrow did not move the camera while held and stop it on release");
        Expect(ptz.Calls.Count(c => c.Text.StartsWith("move right", StringComparison.Ordinal)) <= 2, "a key repeat sent another start");
        ArrowSelection(v, stage, ptz, padTile);

        // parity: ptz-deadman
        Expect(stage.HandlePtzKey(VirtualKey.Up, 0, alt: false, repeat: false), "Up was not taken for the camera");
        Advance(0.3);
        ptz.FrozenForTest = true;
        int deadman0 = Sent("stop", "deadman");
        bool stopped = await Until(() => Sent("stop", "deadman") > deadman0, 10);
        Say($"ptz dead-man: camera stopped by itself {stopped}");
        Expect(stopped, "the camera was not stopped by itself after the app went quiet");
        ptz.FrozenForTest = false;
        stage.HandlePtzKeyUp(VirtualKey.Up);

        // parity: ptz-preset-recall
        var presets = PresetsMenu.Items(info, camera, ptz).OfType<Microsoft.UI.Xaml.Controls.MenuFlyoutItem>().ToList();
        Expect(presets.Count == info.Presets.Count && presets.Count > 0, "the presets menu does not list the camera's presets");
        Expect(presets.Take(9).Select((p, i) => p.KeyboardAcceleratorTextOverride == $"Alt+{i + 1}").All(x => x), "the presets menu lacks the Alt+digit hints");
        int presets0 = Sent("preset");
        if (presets.Count > 0)
        {
            PresetsMenu.Choose(presets[0], camera, ptz);
        }
        int wanted = 1;
        if (info.Presets.Count > 1)
        {
            // Alt+2 is the second preset of the list (by position, whatever the layout).
            await Until(() => ptz.Events.Count(e => e.Action == "preset") >= 1, 5);
            Expect(stage.HandlePtzKey((VirtualKey)0x32, DigitTwoScan, alt: true, repeat: false), "Alt+2 was not taken for the camera");
            stage.HandlePtzAltUp();
            wanted = 2;
        }
        bool recalled = await Until(() => Sent("preset") >= presets0 + wanted, 10);
        Say($"ptz presets: {info.Presets.Count} listed, {Sent("preset") - presets0} recalled by menu and Alt+2");
        Expect(recalled, $"preset recalls by menu and key did not reach the camera ({Sent("preset") - presets0} of {wanted})");

        // parity: ptz-lens-clamp
        if (info.Zoom is { } zoom)
        {
            // Close to the top of the range, then Alt+= past it.
            ptz.DragLens(PtzLensAxis.Zoom, zoom.Max - 2, camera);
            ptz.EndLensDrag(PtzLensAxis.Zoom, camera);
            for (int i = 0; i < 4; i++)
            {
                int before = LensEvents();
                stage.HandlePtzKey((VirtualKey)187, EqualsScan, alt: true, repeat: false);
                await Until(() => LensEvents() > before, 5);
            }
            stage.HandlePtzAltUp();
            int? position = ptz.LensPosition(camera, PtzLensAxis.Zoom);
            Say($"ptz lens: zoom held at {position} of {zoom.Min}...{zoom.Max}");
            Expect(position == zoom.Max, $"Alt+= went past the lens range ({position})");
            // A position far past the range is clamped by the core, whatever the app sends.
            bool clamped = false;
            for (int attempt = 0; attempt < 20 && !clamped; attempt++)
            {
                int before = Sent("lens");
                v.Core.PtzLens(tile.Id, "zoom", zoom.Max + 5000);
                await Until(() => Sent("lens") > before, 1);
                clamped = ptz.Events.LastOrDefault(e => e.Action == "lens" && e.State == PtzEventState.Sent)?.Position == zoom.Max;
            }
            Expect(clamped, "the core did not clamp a position past the range");
            // A drag sends at most five positions a second and the last one at its end.
            stage.PerformHeaderItem(padTile, TileHeaderItem.Lens, null);
            if (stage.PtzContent?.LensRows.GetValueOrDefault(PtzLensAxis.Zoom) is { } row)
            {
                int events0 = LensEvents();
                for (int i = 0; i < 5; i++)
                {
                    row.Drag(zoom.Min + i * 2, ended: false);
                }
                row.Drag(zoom.Min + 10, ended: true);
                await Until(() => LensEvents() >= events0 + 2, 5);
                Expect(LensEvents() == events0 + 2, $"a lens drag sent {LensEvents() - events0} positions instead of the first and the last");
            }
            else
            {
                _problems.Add("the lens flyout has no zoom slider");
            }
            stage.ClosePtzFlyout();
        }

        // parity: ptz-lens1-addressing
        await PtzSecondLens(v, ptz);
    }

    private async Task PtzSecondLens(Viewer v, PtzModel ptz)
    {
        DeviceEntry device = v.Devices[0];
        CameraRef lens1 = default;
        bool found = false;
        foreach (ChannelInfo c in device.Channels.Values.OrderBy(c => c.Channel))
        {
            var candidate = new CameraRef(device.Key, c.Channel, 1);
            if (!found && v.ChannelOf(candidate)?.PtzOf(1).PanTilt == true)
            {
                lens1 = candidate;
                found = true;
            }
        }
        if (!found)
        {
            Say("ptz second lens: no camera whose second lens moves");
            return;
        }
        bool firstMoves = v.ChannelOf(lens1)?.PtzOf(0).PanTilt == true;
        CameraTile? tile = v.Leases.Acquire(lens1);
        try
        {
            bool open = tile is not null || await Until(() => v.Leases.Tile(lens1) is not null, 10);
            tile ??= v.Leases.Tile(lens1);
            if (!open || tile is null)
            {
                _problems.Add("the second lens's tile did not open");
                return;
            }
            ptz.Load(lens1);
            bool read = await Until(() => ptz.InfoFor(lens1) is not null, 15);
            Expect(read, "the second lens's capabilities were not read");
            ulong id = tile.Id;
            int base0 = ptz.Events.Count(e => e.Tile == id && e.Action == "move" && e.State == PtzEventState.Sent);
            ptz.Press(PtzDirection.Up, lens1);
            bool moved = await Until(() => ptz.Events.Count(e => e.Tile == id && e.Action == "move" && e.State == PtzEventState.Sent) > base0, 10);
            bool failed = ptz.Events.Any(e => e.Tile == id && e.State is PtzEventState.Failed or PtzEventState.Refused);
            ptz.Release();
            Say($"ptz second lens: moved {moved}, failed {failed}, first lens moves {firstMoves}");
            Expect(moved && !failed, "the second lens's move was not accepted");
            Expect(!firstMoves, "the first lens of the multi-lens camera claims to move");
        }
        finally
        {
            v.Leases.Release(lens1);
        }
    }
}
