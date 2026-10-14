// Self-test area "tiles": tile features (alerts, indicators, quality, second lens) and fisheye views.

using System.Diagnostics;
using System.Text;
using Microsoft.UI.Windowing;
using NvrView.Shell;
using NvrView.Tiles;
using NvrView.Video;
using Windows.Graphics;
using NvThumbKind = NvrView.Interop.NvThumbKind;
using Point = Windows.Foundation.Point;
using RecordedThumbnail = NvrView.Interop.RecordedThumbnail;

namespace NvrView.App;

internal sealed partial class SelfTest
{
    // 10. The tile features.

    private async Task TileFeatures(Viewer v, StageView stage)
    {
        await HeaderModel(v, stage);
        if (Runs("capture"))
        {
            await SavePictures(v, stage);
        }

        if (!Runs("tiles"))
        {
            return;
        }

        // parity: corner-indicators
        // Camera AI detections from the mock reach the corner indicators.
        bool shown = await Until(() => Tiles.Any(t => stage.ChromeFor(t) is { Indicators.Count: > 0 }), 15);
        StageTile? withSymbols = Tiles.FirstOrDefault(t => stage.ChromeFor(t) is { Indicators.Count: > 0 });
        Say($"corner indicators: {(shown ? string.Join(", ", stage.ChromeFor(withSymbols!)!.Indicators.Select(i => i.Description)) : "none")}, " +
            $"corner {v.Settings.IndicatorCorner}");
        Expect(shown, "no tile showed a corner indicator from the mock's detections");
        Expect(v.Settings.IndicatorCorner == ChromeCorner.TopRight, "the indicator corner is not top right by default");
        if (withSymbols is not null)
        {
            // parity: corner-setting
            v.Settings.IndicatorCorner = ChromeCorner.BottomLeft;
            stage.SyncOverlays();
            Expect(stage.ChromeFor(withSymbols)!.Corner == ChromeCorner.BottomLeft, "the indicator corner setting did not move the symbols");
            v.Settings.IndicatorCorner = ChromeCorner.TopRight;
        }
        // parity: frame-rates-readout
        // View > Show frame rates: off by default, one readout per camera tile, remembered in the settings file.
        Expect(!v.Settings.ShowFrameRates && Tiles.Where(t => t.CameraOrNone is not null).All(t => stage.ChromeFor(t)!.RatesText is null), "frame rates are not off by default");
        stage.ToggleFrameRates();
        bool ratesShown = await Until(() => Tiles.Where(t => t.CameraOrNone is not null).All(t => stage.ChromeFor(t)!.RatesText?.StartsWith("Device ", StringComparison.Ordinal) == true), 15);
        Say($"frame rates: {stage.ChromeFor(Tiles.First(t => t.CameraOrNone is not null))!.RatesText}");
        Expect(ratesShown, "Show frame rates did not show a readout on every tile");
        string ratesFile = Path.Combine(Path.GetTempPath(), $"nvrview-selftest-view-settings-{Guid.NewGuid():N}.json");
        try
        {
            _ = new ViewSettings(ratesFile) { ShowFrameRates = true };
            Expect(new ViewSettings(ratesFile).ShowFrameRates, "the frame rate choice was not remembered");
        }
        finally
        {
            File.Delete(ratesFile);
        }
        stage.ToggleFrameRates();
        Expect(Tiles.Where(t => t.CameraOrNone is not null).All(t => stage.ChromeFor(t)!.RatesText is null), "Hide frame rates left a readout");
        // parity: indicator-strip-render
        await RenderStrip(stage);

        // Alerts, auto-hide, and the hold, on a clock that moves only when told to.
        var clock = new ManualViewerClock(1000);
        v.UseClock(clock);
        StageTile lens0 = Tiles.First(t => t.Channel == 1 && t.Lens == 0);
        DeviceEntry? device = lens0.Device;
        // parity: second-lens
        StageTile? lens1 = AddCamera(stage, new CameraRef(device!.Key, 1, 1));
        Expect(lens1 is not null, "the second-lens tile did not open");
        if (lens1 is null)
        {
            return;
        }
        bool lensPlays = await Until(() => (lens1.Video?.Counters.Decoded ?? 0) > 5 && lens1.PictureWidth > 0, 15);
        Say($"second lens: opened on {lens1.OpenedTier}, showing {lens1.ShownTier}, decoded {(lens1.Video?.Counters.Decoded ?? 0)}");
        Expect(lens1.OpenedTier == StreamTier.Mobile, "a second-lens tile did not open on mobile");
        Expect(lensPlays, "the second-lens tile did not play");
        TileChrome c0 = stage.ChromeFor(lens0)!, c1 = stage.ChromeFor(lens1)!;
        StageTile other = Tiles.First(t => t.Channel == 0);
        TileChrome cOther = stage.ChromeFor(other)!;

        // parity: alert-raise
        v.Apply(Detect(lens0, "smoke_alarm", "local_audio", true));
        stage.Tick();
        bool both = c0.AlertShown && c1.AlertShown && !cOther.AlertShown;
        Say($"alert raised: lens 0 {c0.AlertShown}, lens 1 {c1.AlertShown}, other camera {cOther.AlertShown}; " +
            $"border {(c0.AlertBlinking ? "blinking" : "steady")} (animations {(v.AnimationsEnabled() ? "on" : "off")}); " +
            $"\"{c0.AlertText.Replace('\n', '|')}\"");
        Expect(both, "a sound alarm did not raise the alert on both lens tiles of its camera only");
        // parity: alert-reduce-motion
        Expect(c0.AlertBlinking == v.AnimationsEnabled(), "the alert border does not follow the animation setting");
        Expect(c0.AlertText.Contains("Smoke alarm (sound)", StringComparison.Ordinal) && c0.AlertText.Contains("Detected 1 time", StringComparison.Ordinal) &&
            c0.AlertText.Contains("Sounding now", StringComparison.Ordinal), "the alert header lacks the reason, count, or state");
        Expect(c0.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm) && c1.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm),
            "the sound alarm is not an indicator on both lenses");
        Expect(!cOther.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm), "the sound alarm showed on another camera");

        // parity: fullscreen-chrome-autohide
        // Full screen: the chrome and pointer hide after 3 s idle; the alert and indicators stay.
        stage.FullScreenOverride = true;
        stage.Interact();
        c0.SetHover(true, true);
        clock.Advance(2.9);
        stage.Tick();
        Expect(stage.ChromeShown && !stage.CursorHidden, "the chrome hid before 3 s");
        clock.Advance(0.2);
        stage.Tick();
        Say($"full screen idle 3.1 s: chrome shown {stage.ChromeShown}, pointer hidden {stage.CursorHidden}, header {c0.HeaderShown}, alert {c0.AlertShown}, symbols {c0.Indicators.Count}");
        Expect(!stage.ChromeShown && stage.CursorHidden && !c0.HeaderShown, "the chrome and pointer did not hide after 3 s idle in full screen");
        Expect(c0.AlertShown && c0.Indicators.Count > 0, "the alert or the indicators hid with the chrome");
        stage.Interact();
        stage.Tick();
        Expect(stage.ChromeShown && !stage.CursorHidden, "an interaction did not bring the chrome and pointer back");
        clock.Advance(4);
        stage.Tick();
        Expect(stage.CursorHidden, "the pointer did not hide again");
        stage.FullScreenOverride = false;
        stage.Interact();
        clock.Advance(4);
        stage.Tick();
        Expect(!stage.CursorHidden, "the pointer hid outside full screen");
        stage.FullScreenOverride = null;

        // parity: alert-survives-stop
        // parity: indicator-hold
        // A stop does not clear the alert; the symbol is held 2 s.
        v.Apply(Detect(lens0, "smoke_alarm", "local_audio", false));
        stage.Tick();
        Expect(c0.AlertShown && c0.AlertText.Contains("Not sounding now", StringComparison.Ordinal), "the stop cleared the alert or did not say it stopped");
        Expect(c0.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm), "the symbol went the moment the sound stopped");
        clock.Advance(1.9);
        stage.Tick();
        bool held = c0.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm);
        clock.Advance(0.2);
        stage.Tick();
        bool gone = !c0.Indicators.Any(i => i.Kind == IndicatorKind.SmokeAlarm);
        Say($"indicator hold: after 1.9 s {(held ? "held" : "gone")}, after 2.1 s {(gone ? "gone" : "held")}");
        Expect(held && gone, "the symbol was not held for 2 s and then removed");

        // parity: alert-dismiss-enter
        // Again counts; Enter dismisses for every tile of the camera; a new one starts fresh.
        v.Apply(Detect(lens0, "smoke_alarm", "local_audio", true));
        stage.Tick();
        Expect(c1.AlertText.Contains("Detected 2 times", StringComparison.Ordinal), "a second detection was not counted");
        stage.ApplyAction(TileKeyMap.Map(TileKey.Enter, false, false, false, v.Alerts.IsActive(lens1.Camera)), lens1);
        stage.Tick();
        Say($"dismissed with Enter: alert on lens 0 {c0.AlertShown}, lens 1 {c1.AlertShown}, log: {string.Join(" / ", v.Alerts.Log.Select(l => l.Text))}");
        Expect(!c0.AlertShown && !c1.AlertShown && !v.Alerts.IsActive(lens0.Camera), "Enter did not dismiss the alert on every tile of the camera");
        Expect(v.Alerts.Log.Count >= 3, "the session log lacks the alert lines");
        v.Apply(Detect(lens0, "smoke_alarm", "local_audio", true));
        stage.Tick();
        Expect(c0.AlertShown && c0.AlertText.Contains("Detected 1 time", StringComparison.Ordinal), "a detection after the dismissal did not raise a fresh alert");
        v.Apply(Detect(lens0, "smoke_alarm", "local_audio", false));
        v.DismissAlert(lens0.Camera);
        stage.Tick();
        Expect(!c1.AlertShown, "the Dismiss path did not clear the alert");
        v.UseClock(new SystemViewerClock());

        // Quality: Clear, Balanced, and Auto switch the stream; zoom moves Auto.
        StageTile q = other;
        // parity: quality-menu-choices
        Expect(v.OffersBalanced(q), "Balanced is not offered for a single-lens channel that lists extern");
        Expect(!v.OffersBalanced(lens0), "Balanced is offered for a two-lens camera");
        Expect(QualityMenu.Choices(true).SequenceEqual(["Auto", "Clear", "Fluent", "Balanced"]) && QualityMenu.Choices(false).SequenceEqual(["Auto", "Clear", "Fluent"]),
            "the Quality menu lists the wrong choices");
        // parity: quality-clear-auto
        v.SetQuality(q, TileQuality.Clear);
        bool main = await Until(() => q.ShownTier == "main" && q.PictureWidth > 0, 10);
        Say($"quality Clear: showing {q.ShownTier} {q.PictureWidth}x{q.PictureHeight}, header \"{StageView.TitleOf(q)}\"");
        Expect(main, "Clear did not switch the tile to the main stream");
        Expect(StageView.TitleOf(q).Contains("Clear", StringComparison.Ordinal), "the header does not name the quality");
        // parity: quality-balanced
        v.SetQuality(q, TileQuality.Balanced);
        bool mid = await Until(() => q.ShownTier == "extern" && q.PictureWidth > 0, 10);
        Say($"quality Balanced: showing {q.ShownTier}");
        Expect(mid, "Balanced did not switch the tile to the extern stream");
        v.SetQuality(q, TileQuality.Auto);
        bool sub = await Until(() => q.ShownTier == "sub", 10);
        Say($"quality Auto: showing {q.ShownTier}");
        Expect(sub, "Auto did not return the tile to the sub stream");
        // parity: zoom-auto-quality
        q.Zoom = 2.5f;
        v.Tick();
        bool zoomedMain = await Until(() => q.ShownTier == "main", 10);
        q.Zoom = 1.7f;
        v.Tick();
        // The planner decides on the tick, so the tier asked for right after it is the decision (no wait); the
        // hysteresis itself (up past 2x, down below 1.6x) is ZoomHysteresis in NvrView.Tiles, tested there.
        bool stays = q.ShownTier == "main" && q.RequestedTier == StreamTier.Main && v.Planner.WantedTier(q.Camera) == StreamTier.Main;
        q.Zoom = 1.5f;
        v.Tick();
        bool back = await Until(() => q.ShownTier == "sub", 10);
        Say($"auto zoom: 2.5x {(zoomedMain ? "main" : q.ShownTier)}, 1.7x {(stays ? "still main" : "changed")}, 1.5x {(back ? "sub" : q.ShownTier)}");
        Expect(zoomedMain && stays && back, "Auto did not follow the zoom (main past 2x, still main at 1.7x, sub below 1.6x)");
        StageView.ResetZoom(q);
        v.Tick();
        await Until(() => q.ShownTier == "sub", 10);

        // parity: second-lens-refused
        // A camera whose device refuses the second lens's mobile stream: reopened on extern. The core ends
        // a refused start with tile_error start_rejected only after its 60 s wait (docs/FFI.md), so the event is
        // injected, as a fake core would.
        StageTile onCh2 = Tiles.First(t => t.Channel == 2 && t.Lens == 0);
        CameraRef camera = new(onCh2.Device!.Key, 2, 1);
        StageTile? refused = AddCamera(stage, camera);
        Expect(refused is { OpenedTier: StreamTier.Mobile }, "the refused second lens did not open on mobile first");
        ulong firstId = refused?.Id ?? 0;
        if (refused is not null)
        {
            v.Apply(new CoreEvent { Type = "tile_error", Tile = refused.Id, Reason = "start_rejected", Status = 400 });
        }
        StageTile? now = Tiles.FirstOrDefault(t => t.Camera == camera);
        bool reopened = now is { OpenedTier: StreamTier.Extern } && now.Id != firstId && v.Planner.NoMobile(camera);
        Say($"second lens refused on mobile: reopened on {now?.OpenedTier.ToString() ?? "n/a"} as a new tile {now is not null && now.Id != firstId} (no mobile for the run: {v.Planner.NoMobile(camera)}), " +
            $"a first lens is left alone: {!v.Planner.StartFailed(onCh2.Camera, StreamTier.Sub, "start_rejected")}");
        Expect(reopened, "a second lens whose mobile start was refused was not reopened on extern");
        while (Tiles.FirstOrDefault(t => t.Lens > 0) is { } t)
        {
            // The mock refuses extern too on this channel; that refusal is expected.
            v.Problems.RemoveAll(p => p.StartsWith($"tile_error tile {t.Id}:", StringComparison.Ordinal));
            RemoveTile(stage, t);
        }
        Expect(Tiles.All(t => t.Lens == 0), "a second-lens tile stayed open");
    }

    // 10b. Fisheye views (docs/UX.md "Fisheye cameras", docs/RENDERING.md "Fisheye dewarping").
    private async Task FisheyeViews(Viewer v, StageView stage, StageRenderer r)
    {
        StageTile? f = Tiles.FirstOrDefault(t => t.Channel == 2 && t.Lens == 0);
        if (f is null || options.MockChannels < 3)
        {
            Say("fisheye: skipped (needs 3 mock channels)");
            return;
        }
        // parity: fisheye-detected
        bool known = await Until(() => v.IsFisheye(f.Camera), 10);
        Expect(known, "the mock's fisheye channel was not reported as a fisheye camera");
        Expect(!v.IsFisheye(Tiles.First(t => t.Channel == 0).Camera), "an ordinary channel was reported as a fisheye camera");
        if (!known)
        {
            return;
        }
        stage.SetFisheyeMode(f, FisheyeViewMode.Panorama);
        // parity: fisheye-circle
        bool found = await Until(() => v.Circles.Circle(f.Camera) is not null, 10);
        FisheyeCircle circle = v.Circles.Circle(f.Camera) ?? FisheyeCircle.Inscribed(384, 384);
        Say($"fisheye circle: ({circle.Cx:F4}, {circle.Cy:F4}) r {circle.R:F4} (synthetic: 0.49, 0.505, 0.46), fell back {v.Circles.FellBack(f.Camera)}");
        Expect(found && !v.Circles.FellBack(f.Camera), "the fisheye circle was not found");
        Expect(Math.Abs(circle.Cx - 0.49) < 0.01 && Math.Abs(circle.Cy - 0.505) < 0.01 && Math.Abs(circle.R - 0.46) < 0.01,
            "the fisheye circle is more than 1% off the synthetic one");

        // parity: fisheye-view-pixels
        foreach (FisheyeViewMode mode in new[] { FisheyeViewMode.Panorama, FisheyeViewMode.DoublePanorama, FisheyeViewMode.Quad, FisheyeViewMode.Ptz })
        {
            stage.SetFisheyeMode(f, mode);
            long before = r.DewarpDraws;
            bool drawn = await Until(() => r.DewarpDraws > before + 3, 5);
            Expect(drawn, $"fisheye {mode.Title()}: the dewarp shader did not draw");
            Expect(StageView.TitleOf(f).Contains(mode.Title(), StringComparison.Ordinal), $"fisheye {mode.Title()}: the header does not name the view");
            // parity: fisheye-view-saved
            string? member = stage.State.Tree.Member(f.Path, FisheyeTileSetting.Member);
            Expect(FisheyeTileSetting.FromJson(member).Mode == mode, $"fisheye {mode.Title()}: the view was not kept in the layout leaf ({member})");
            await CheckColors(mode.Title(), f, r, circle);
        }

        if (Runs("capture"))
        {
            await FisheyePictures(v, stage, r, f, circle);
        }
        stage.SetFisheyeMode(f, FisheyeViewMode.Ptz);

        // parity: fisheye-ptz-zoom
        // Virtual PTZ zoom: eased on the viewer's clock, ends on the target, saved; 0 resets.
        var clock = new ManualViewerClock(2000);
        v.UseClock(clock);
        double fov = f.Fisheye?.Aim.Fov ?? 0;
        stage.ZoomFisheye(f, 2);
        clock.Advance(1.0 / 60);
        stage.StepFisheyeZoom(clock.Now);
        double mid = f.Fisheye?.Aim.Fov ?? 0;
        clock.Advance(2);
        stage.StepFisheyeZoom(clock.Now);
        double end = f.Fisheye?.Aim.Fov ?? 0;
        v.UseClock(new SystemViewerClock());
        FisheyeTileSetting saved = FisheyeTileSetting.FromJson(stage.State.Tree.Member(f.Path, FisheyeTileSetting.Member));
        Say($"fisheye PTZ zoom: {fov:0.#}° → {mid:0.#}° after one refresh → {end:0.#}°, saved {saved.Aim?.Fov:0.#}°");
        Expect(mid < fov && mid > fov / 2 && Math.Abs(end - fov / 2) < 1e-9 && !stage.IsEasingFisheyeZoom, "the Virtual PTZ zoom did not ease to its target");
        Expect(saved.Aim is { } a && Math.Abs(a.Fov - fov / 2) < 1e-9, "the Virtual PTZ zoom was not saved with the layout");
        await CheckColors("Virtual PTZ zoomed", f, r, circle);
        // parity: fisheye-reset-key
        stage.Select(f.Path);
        stage.HandleKey(Windows.System.VirtualKey.Number0, false, false);
        Expect(f.Fisheye?.Aim == FisheyeAim.Default(FisheyeMount.Ceiling), "0 did not reset the virtual camera");

        // parity: fisheye-mount-settings
        // Settings > fisheye mount (the picker, not the menu), then a synthetic pinch on the Virtual PTZ view.
        var mountHooks = new SettingsPagesHooks();
        SettingsWindow mountWindow = SettingsWindow.Open(v, development: true, SettingsSection.Cameras, hooks: mountHooks, background: true);
        await Until(() => mountWindow.IsLoaded, 5);
        var pickers = mountHooks.Mounts();
        Expect(pickers.Any(m => m.Camera == f.Camera.ChannelKey && m.Mount == FisheyeMount.Ceiling), "Settings does not list the fisheye camera with the default ceiling mount");
        Expect(pickers.All(m => v.IsFisheye(m.Camera)), "Settings lists a camera that is not a fisheye camera");
        mountHooks.ChooseMount(f.Camera.ChannelKey, FisheyeMount.Wall);
        Expect(v.Settings.Mount(f.Camera) == FisheyeMount.Wall, "choosing Wall in Settings did not set the mount");
        Expect(f.Fisheye is { Mount: FisheyeMount.Wall } onWall && onWall.Aim == FisheyeAim.Default(FisheyeMount.Wall), "the tile did not follow the mount chosen in Settings");
        Expect(mountHooks.Mounts().Any(m => m.Camera == f.Camera.ChannelKey && m.Mount == FisheyeMount.Wall), "the picker does not show the saved mount");
        // parity: fisheye-pinch
        var pinchClock = new ManualViewerClock(3000);
        v.UseClock(pinchClock);
        double pinchFrom = f.Fisheye?.Aim.Fov ?? 0;
        stage.PinchFisheye(f, 1.5);
        pinchClock.Advance(2);
        stage.StepFisheyeZoom(pinchClock.Now);
        double pinched = f.Fisheye?.Aim.Fov ?? 0;
        stage.PinchFisheye(f, 2);
        stage.PinchFisheye(f, 2);
        pinchClock.Advance(2);
        stage.StepFisheyeZoom(pinchClock.Now);
        double limit = f.Fisheye?.Aim.Fov ?? 0;
        stage.PinchFisheye(f, double.NaN);
        pinchClock.Advance(2);
        stage.StepFisheyeZoom(pinchClock.Now);
        v.UseClock(new SystemViewerClock());
        FisheyeTileSetting pinchSaved = FisheyeTileSetting.FromJson(stage.State.Tree.Member(f.Path, FisheyeTileSetting.Member));
        Say($"fisheye pinch (wall mount): {pinchFrom:0.#}° → {pinched:0.#}° (x1.5) → {limit:0.#}° (two x2), saved {pinchSaved.Aim?.Fov:0.#}°");
        Expect(Math.Abs(pinched - pinchFrom / 1.5) < 1e-9, "a pinch of 1.5 did not divide the field of view by 1.5");
        Expect(Math.Abs(limit - FisheyeAim.MinFov) < 1e-9 && (f.Fisheye?.Aim.Fov ?? 0) == limit, "pinching in did not stop at the narrowest view, or an unusable scale moved it");
        Expect(pinchSaved.Aim is { } pa && Math.Abs(pa.Fov - FisheyeAim.MinFov) < 1e-9, "the pinched view was not saved with the layout");
        mountHooks.ChooseMount(f.Camera.ChannelKey, FisheyeMount.Ceiling);
        Expect(v.Settings.Mount(f.Camera) == FisheyeMount.Ceiling, "choosing Ceiling in Settings did not restore the default mount");
        mountWindow.Close();
        stage.Select(f.Path);
        stage.HandleKey(Windows.System.VirtualKey.Number0, false, false);
        Expect(f.Fisheye?.Aim == FisheyeAim.Default(FisheyeMount.Ceiling), "0 did not reset the virtual camera after the pinch");

        // parity: fisheye-original
        // Back to the camera's own picture.
        stage.SetFisheyeMode(f, FisheyeViewMode.Original);
        Expect(f.Fisheye is null && stage.State.Tree.Member(f.Path, FisheyeTileSetting.Member) is null, "Original did not remove the leaf member");
    }

    // Samples the dewarped tile where the shared mapping says the synthetic sphere
    // (fixtures/synthetic/make-fisheye.sh) has a known color: red, green, blue, or
    // yellow by azimuth quadrant, away from the checkerboard's edges and the moving dot.
    private async Task CheckColors(string what, StageTile f, StageRenderer r, FisheyeCircle circle)
    {
        if (f.Fisheye is not { } fe)
        {
            _problems.Add($"fisheye {what}: the tile is not dewarped");
            return;
        }
        long drawn = r.DewarpDraws;
        await Until(() => r.DewarpDraws > drawn + 1, 5);
        Task<StageCapture> task = r.CaptureAsync();
        if (await Task.WhenAny(task, Task.Delay(5000)) != task)
        {
            _problems.Add($"fisheye {what}: no frame was drawn");
            return;
        }
        StageCapture c = await task;
        (StageTile _, float x0, float y0, float w, float h) = c.Rects.FirstOrDefault(rect => rect.Tile == f);
        int pw = Math.Max(f.PictureWidth, 1), ph = Math.Max(f.PictureHeight, 1);
        FisheyeDewarp d = FisheyeDewarp.Of(new FisheyeLens(circle, pw / (double)ph), fe.Mount, fe.Mode, fe.Aim,
            Math.Max((int)Math.Round(w), 1), Math.Max((int)Math.Round(h), 1));
        int tried = 0, right = 0;
        string? firstWrong = null;
        for (int j = 0; j < 32; j++)
        {
            for (int i = 0; i < 32; i++)
            {
                var p = new UnitPoint((i + 0.5) / 32, (j + 0.5) / 32);
                if (d.Direction(p) is not { } dir)
                {
                    continue;
                }
                (double pan, double tilt) = Fisheye.PanTilt(dir);
                double psi = ((pan % 360) + 360) % 360;
                static double Off(double a) => Math.Abs(a - Math.Round(a / 15) * 15);
                if (tilt < 5 || tilt > 80 || Off(psi) < 3 || Off(tilt) < 3 || Math.Abs(tilt - 20) < 8)
                {
                    continue;
                }
                int sx = (int)(x0 + p.X * w), sy = (int)(y0 + p.Y * h);
                if (sx < 0 || sy < 0 || sx >= c.Image.Width || sy >= c.Image.Height)
                {
                    continue;
                }
                int want = (int)(psi / 90) % 4;
                (int cr, int cg, int cb) = c.Image.Rgb(sx, sy);
                int got = cr > cb + 40 && cg > cb + 40 && cg > cr * 0.6 ? 3
                    : cr > cg + 25 && cr > cb + 25 ? 0
                    : cg > cr + 25 && cg > cb + 25 ? 1
                    : cb > cr + 25 && cb > cg + 25 ? 2
                    : -1;
                tried++;
                if (got == want)
                {
                    right++;
                }
                else
                {
                    firstWrong ??= $"at ({p.X:F2}, {p.Y:F2}) psi {psi:0} tau {tilt:0}: ({cr}, {cg}, {cb}), wanted quadrant {want}";
                }
            }
        }
        Say($"fisheye {what}: {right} of {tried} sampled pixels show the expected color{(firstWrong is null ? "" : $"; first miss {firstWrong}")}");
        Expect(tried >= 20 && right >= tried * 9 / 10, $"fisheye {what}: dewarped pixels do not match the synthetic scene ({right} of {tried})");
    }
}
