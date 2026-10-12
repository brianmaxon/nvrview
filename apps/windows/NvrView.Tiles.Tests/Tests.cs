using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class Tests
{
    private const string Dev = "dev-example";
    private static readonly CameraRef Cam0 = new(Dev, 1);
    private static readonly CameraRef Lens1 = new(Dev, 1, 1);

    private static DetectionEvent Ev(string kind, bool active, int channel = 1, int lens = 0, string source = DetectionSource.Device) =>
        new(Dev, channel, kind, active, source, 0, lens);

    private static DateTime T(int h, int m, int s, int day = 10) => new(2026, 10, day, h, m, s);

    public static IEnumerable<(string, Action)> All() =>
    [
        ("tracker: start and stop per camera", TrackerStartStop),
        ("tracker: motion last, objects first", TrackerOrder),
        ("tracker: lens 1 is its own camera", TrackerLens),
        ("tracker: sound shows on every lens of the channel", TrackerSound),
        ("tracker: device loss and expiry", TrackerClearExpire),
        ("indicators: kinds and sources", IndicatorItems),
        ("indicators: hold keeps a kind 2 s after its stop", HoldTiming),
        ("indicators: a restart cancels the hold", HoldRestart),
        ("indicators: board per camera, channel, and device", BoardViews),
        ("indicators: board holds sound on both lenses", BoardSound),
        ("indicators: corner row origin and persistence", Corners),
        ("alerts: policy", AlertPolicyTest),
        ("alerts: raise, count, quiet, dismiss, re-raise", AlertLifecycle),
        ("alerts: multi-lens and per-channel", AlertScope),
        ("alerts: lines and date stamps", AlertLines),
        ("alerts: device lost, session log", AlertLog),
        ("alerts: border blinking and steady", Border),
        ("quality: level mapping", QualityLevels),
        ("quality: balanced only with a middle tier", QualityBalanced),
        ("quality: zoom hysteresis", ZoomHysteresisTest),
        ("quality: highest wins across tiles", HighestWins),
        ("quality: focus counts as high; fluent is an explicit choice", FocusAndFluent),
        ("quality: leaf member round trip", QualityMember),
        ("second lens: tiers mobile and extern", SecondLensTiers),
        ("second lens: start rejected falls back to extern", SecondLensStartRejected),
        ("second lens: failed mobile switch falls back, superseded does not", SecondLensTierFailed),
        ("auto-hide: chrome after 3 s idle", AutoHide),
        ("auto-hide: pointer only in full screen", AutoHidePointer),
        ("keys: enter, space, m, ctrl+l", Keys),
        ("clock: manual clock", ManualClock),
    ];

    // ---- tracker ----

    private static void TrackerStartStop()
    {
        var t = new DetectionTracker();
        t.Apply(Ev("people", true));
        Check.Equal(1, t.For(Cam0).Count, "one active");
        t.Apply(Ev("people", true));
        Check.Equal(1, t.For(Cam0).Count, "a repeated start does not duplicate");
        t.Apply(Ev("people", false));
        Check.Equal(0, t.For(Cam0).Count, "stop clears");
        Check.Equal(0, t.For(new CameraRef(Dev, 2)).Count, "another channel untouched");
    }

    private static void TrackerOrder()
    {
        var t = new DetectionTracker();
        t.Apply(Ev("motion", true));
        t.Apply(Ev("vehicle", true));
        t.Apply(Ev("people", true));
        Check.Equal("people,vehicle,motion", string.Join(",", t.For(Cam0).Select(a => a.Kind)), "order");
    }

    private static void TrackerLens()
    {
        var t = new DetectionTracker();
        t.Apply(Ev("dog_cat", true, lens: 1));
        Check.Equal(0, t.For(Cam0).Count, "lens 0 has none");
        Check.Equal(1, t.For(Lens1).Count, "lens 1 has it");
        Check.Equal(1, Ev("x", true, lens: 7).Camera.Lens, "an out-of-range lens clamps");
    }

    private static void TrackerSound()
    {
        var t = new DetectionTracker();
        t.Apply(Ev("smoke_alarm", true, source: DetectionSource.LocalAudio));
        Check.Equal(1, t.For(Cam0).Count, "lens 0 hears it");
        Check.Equal(1, t.For(Lens1).Count, "lens 1 hears it too");
        t.Apply(Ev("people", true));
        Check.Equal("people,smoke_alarm", string.Join(",", t.For(Cam0).Select(a => a.Kind)), "own, then sound");
        Check.Equal(1, t.For(Lens1).Count, "the lens-0 object is not on lens 1");
        t.Apply(Ev("smoke_alarm", false, source: DetectionSource.LocalAudio));
        Check.Equal(0, t.For(Lens1).Count, "the stop clears sound for every lens");
    }

    private static void TrackerClearExpire()
    {
        var t = new DetectionTracker();
        t.Apply(Ev("motion", true) with { AtNanos = 1_000 });
        t.Apply(Ev("people", true) with { AtNanos = 0 });
        t.Expire(nowNanos: 5_000_000_000, maxAgeNanos: 1_000_000_000);
        Check.Equal("people", string.Join(",", t.For(Cam0).Select(a => a.Kind)), "old one dropped, unknown age kept");
        t.Clear(Dev);
        Check.Equal(0, t.For(Cam0).Count, "device cleared");
    }

    // ---- indicators ----

    private static void IndicatorItems()
    {
        var t = new DetectionTracker();
        t.Apply(Ev("people", true));
        t.Apply(Ev("people", true, source: DetectionSource.LocalVideo));
        t.Apply(Ev("motion", true));
        t.Apply(Ev("co_alarm", true, source: DetectionSource.LocalAudio));
        Check.Items("Motion (camera AI), Person (camera AI and this PC), CO alarm (sound)", IndicatorItem.From(t.For(Cam0)), "items");
        Check.True(IndicatorKinds.FromDetection("co_alarm").IsSound(), "sound is accented");
        Check.True(!IndicatorKinds.FromDetection("people").IsSound(), "objects are monochrome");
        Check.Equal(IndicatorKind.Other, IndicatorKinds.FromDetection("cry"), "unknown kinds");
        Check.Equal("Smoke alarm · sound", DetectionTracker.SoundBadgeText("smoke_alarm"), "sound badge");
    }

    private static void HoldTiming()
    {
        var clock = new ManualViewerClock();
        var hold = new IndicatorHold();
        var person = new IndicatorItem(IndicatorKind.Person, "camera AI");
        hold.Update([person], clock.Now);
        Check.Equal(1, hold.Visible(clock.Now).Count, "shown while active");
        clock.Advance(10);
        hold.Update([], clock.Now);
        Check.Equal(1, hold.Visible(clock.Now).Count, "still shown right after the stop");
        Check.Equal(clock.Now + 2.0, hold.NextExpiry(clock.Now), "expiry is 2 s out");
        clock.Advance(1.9);
        Check.Equal(1, hold.Visible(clock.Now).Count, "held at 1.9 s");
        clock.Advance(0.1);
        Check.Equal(0, hold.Visible(clock.Now).Count, "gone at 2 s");
        hold.Update([], clock.Now);
        Check.True(hold.IsEmpty, "forgotten after the hold");
        Check.True(hold.NextExpiry(clock.Now) is null, "nothing to wait for");
    }

    private static void HoldRestart()
    {
        var clock = new ManualViewerClock();
        var hold = new IndicatorHold();
        var motion = new IndicatorItem(IndicatorKind.Motion, "camera AI");
        hold.Update([motion], clock.Now);
        clock.Advance(1);
        hold.Update([], clock.Now);
        clock.Advance(1);
        hold.Update([motion], clock.Now);
        clock.Advance(5);
        Check.Equal(1, hold.Visible(clock.Now).Count, "a restarted kind is active again, not held out");
    }

    private static void BoardViews()
    {
        var clock = new ManualViewerClock();
        var t = new DetectionTracker();
        var board = new IndicatorBoard();
        Check.True(!board.Update(t, clock.Now), "nothing changed");
        t.Apply(Ev("people", true, lens: 1));
        t.Apply(Ev("vehicle", true, channel: 3));
        Check.True(board.Update(t, clock.Now), "changed");
        Check.Items("Person (camera AI)", board.Items(Lens1), "lens 1");
        Check.Items("", board.Items(Cam0), "lens 0 empty (never an all clear)");
        Check.Items("Person (camera AI)", board.Items(Dev, 1), "the channel row summarizes its lenses");
        Check.Items("Person (camera AI), Vehicle (camera AI)", board.Items(Dev), "the device row is the union");
        Check.True(!board.Update(t, clock.Now), "no change on a repeat");
        t.Apply(Ev("people", false, lens: 1));
        clock.Advance(1);
        board.Update(t, clock.Now);
        Check.Items("Person (camera AI)", board.Items(Dev, 1), "held after its stop");
        Check.Equal(clock.Now + 2.0, board.NextExpiry(clock.Now), "next expiry");
        clock.Advance(1.9);
        board.Update(t, clock.Now);
        Check.Items("Person (camera AI)", board.Items(Dev, 1), "held at 1.9 s");
        clock.Advance(0.1);
        Check.True(board.Update(t, clock.Now), "the tick drops it");
        Check.Items("", board.Items(Dev, 1), "gone");
    }

    private static void BoardSound()
    {
        var t = new DetectionTracker();
        var board = new IndicatorBoard();
        t.Apply(Ev("smoke_alarm", true, source: DetectionSource.LocalAudio));
        board.Update(t, 0);
        Check.Items("Smoke alarm (sound)", board.Items(Cam0), "lens 0");
        Check.Items("Smoke alarm (sound)", board.Items(Lens1), "lens 1");
    }

    private static void Corners()
    {
        Check.Equal(ChromeCorner.TopRight, ChromeCorners.Default, "default corner");
        Check.Equal(ChromeCorner.TopRight, ChromeCorners.Parse(null), "missing reads as the default");
        Check.Equal(ChromeCorner.TopRight, ChromeCorners.Parse("sideways"), "unknown reads as the default");
        foreach (ChromeCorner c in Enum.GetValues<ChromeCorner>())
        {
            Check.Equal(c, ChromeCorners.Parse(c.Name()), $"round trip {c}");
        }
        (double x, double y) = ChromeCorner.TopRight.RowOrigin(3, 20, 5, 8, 400, 300);
        Check.Equal(400 - 8 - (3 * 20 + 2 * 5), x, "top right x grows leftward");
        Check.Equal(8.0, y, "top right y");
        (x, y) = ChromeCorner.BottomLeft.RowOrigin(2, 20, 5, 8, 400, 300);
        Check.Equal(8.0, x, "bottom left x");
        Check.Equal(300 - 8 - 20.0, y, "bottom left y");
    }

    // ---- alerts ----

    private static void AlertPolicyTest()
    {
        Check.True(AlertPolicy.Raises("smoke_alarm", DetectionSource.LocalAudio), "smoke from sound");
        Check.True(AlertPolicy.Raises("co_alarm", DetectionSource.LocalAudio), "CO from sound");
        Check.True(!AlertPolicy.Raises("people", DetectionSource.LocalAudio), "objects do not");
        Check.True(!AlertPolicy.Raises("smoke_alarm", DetectionSource.Device), "device smoke is not in the list yet");
    }

    private static DetectionEvent Smoke(bool active, int channel = 1, int lens = 0, string kind = "smoke_alarm") =>
        Ev(kind, active, channel, lens, DetectionSource.LocalAudio);

    private static void AlertLifecycle()
    {
        var c = new AlertCenter();
        Check.True(!c.IsActive(Cam0), "none at first");
        Check.True(c.Apply(Smoke(true), T(9, 0, 0)), "raised");
        Alert a = c.For(Cam0)!;
        Check.Equal(1, a.Count, "count 1");
        Check.True(a.Sounding, "sounding");
        Check.True(c.Apply(Smoke(false), T(9, 0, 30)), "quiet");
        Check.True(c.IsActive(Cam0), "a stop does not clear");
        Check.True(!a.Sounding, "not sounding");
        Check.True(!c.Apply(Smoke(false), T(9, 0, 31)), "a second stop changes nothing");
        c.Apply(Smoke(true), T(9, 5, 0));
        Check.Equal(2, a.Count, "second start counts");
        Check.Equal(T(9, 5, 0), a.LastAt, "last time");
        Check.Equal(T(9, 0, 0), a.FirstAt, "first time stays");
        Check.True(a.Sounding, "sounding again");
        Check.True(c.Dismiss(Cam0, T(9, 6, 0)) is { Count: 2 }, "dismissed with its count");
        Check.True(!c.IsActive(Cam0), "gone");
        Check.True(c.Dismiss(Cam0, T(9, 6, 1)) is null, "dismissing again does nothing");
        c.Apply(Smoke(true), T(9, 7, 0));
        Check.Equal(1, c.For(Cam0)!.Count, "a new detection raises a fresh alert with the count reset");
        Check.Equal(T(9, 7, 0), c.For(Cam0)!.FirstAt, "with a new first time");
    }

    private static void AlertScope()
    {
        var c = new AlertCenter();
        c.Apply(Smoke(true, channel: 1), T(9, 0, 0));
        Check.True(c.IsActive(Cam0), "lens 0 tile");
        Check.True(c.IsActive(Lens1), "lens 1 tile of the same camera");
        Check.True(!c.IsActive(new CameraRef(Dev, 2)), "another channel");
        Check.True(!c.IsActive(new CameraRef("dev-other", 1)), "another device");
        c.Apply(Smoke(true, channel: 2, kind: "co_alarm"), T(9, 1, 0));
        Check.Equal(2, c.All.Count, "two cameras");
        Check.Equal("Smoke alarm (sound)", c.All[0].Reason, "oldest first");
        c.Dismiss(Lens1, T(9, 2, 0));
        Check.True(!c.IsActive(Cam0) && !c.IsActive(Lens1), "dismissing on one lens clears every tile of the camera");
        Check.True(c.IsActive(new CameraRef(Dev, 2)), "the other camera keeps its alert");
        Check.True(!c.Apply(Ev("people", true), T(9, 3, 0)), "an ordinary detection raises nothing");
        Check.True(!c.Apply(Smoke(true) with { Alarm = true }, T(9, 3, 0)), "an alarm event raises nothing");
    }

    private static void AlertLines()
    {
        var c = new AlertCenter();
        c.Apply(Smoke(true), T(8, 30, 5));
        c.Apply(Smoke(false), T(8, 31, 0));
        c.Apply(Smoke(true), T(8, 45, 10));
        string[] lines = c.For(Cam0)!.Lines(T(9, 0, 0));
        Check.Equal("Smoke alarm (sound)", lines[0], "reason");
        Check.Equal("First detected 08:30:05", lines[1], "first time, today");
        Check.Equal("Detected 2 times, last 08:45:10", lines[2], "count and last");
        Check.Equal("Sounding now", lines[3], "state");
        string[] later = c.For(Cam0)!.Lines(T(9, 0, 0, day: 11));
        Check.Equal("First detected 2026-10-10 08:30:05", later[1], "the date when it is not today");
        c.Apply(Smoke(false), T(8, 46, 0));
        var one = new AlertCenter();
        one.Apply(Smoke(true), T(8, 0, 0));
        Check.Equal("Detected 1 time, last 08:00:00", one.For(Cam0)!.Lines(T(9, 0, 0))[2], "singular");
        Check.Equal("Not sounding now", c.For(Cam0)!.Lines(T(9, 0, 0))[3], "not sounding");
    }

    private static void AlertLog()
    {
        var c = new AlertCenter();
        c.Apply(Smoke(true), T(9, 0, 0));
        c.Apply(Smoke(true), T(9, 1, 0));
        c.DeviceLost(Dev);
        Check.True(!c.For(Cam0)!.Sounding, "a lost session is not sounding");
        Check.True(c.IsActive(Cam0), "but the alert stays");
        c.Dismiss(Cam0, T(9, 2, 0));
        Check.Equal(3, c.Log.Count, "raised, again, dismissed");
        Check.Equal("Alert raised: Smoke alarm (sound), channel 2", c.Log[0].Text, "raise line");
        Check.Equal("Alert again: Smoke alarm (sound), channel 2, detection 2", c.Log[1].Text, "again line");
        Check.Equal("Alert dismissed: Smoke alarm (sound), channel 2, 2 detections", c.Log[2].Text, "dismiss line");
    }

    private static void Border()
    {
        var m = AlertBorder.ModeFor(animationsEnabled: true);
        Check.Equal(AlertBorder.Mode.Blinking, m, "blinking with animations");
        Check.True(AlertBorder.Visible(m, 0.0) && AlertBorder.Visible(m, 0.49), "on for half a period");
        Check.True(!AlertBorder.Visible(m, 0.5) && !AlertBorder.Visible(m, 0.99), "off for the other half");
        Check.True(AlertBorder.Visible(m, 1.2), "and again");
        var steady = AlertBorder.ModeFor(animationsEnabled: false);
        Check.Equal(AlertBorder.Mode.Steady, steady, "steady without animations");
        Check.True(AlertBorder.Visible(steady, 0.7), "always drawn");
    }

    // ---- quality ----

    private static TierPlanner Planner(bool middle = false) => new() { HasMiddleTier = _ => middle };

    private static void QualityLevels()
    {
        var p = Planner(middle: true);
        var focusedAuto = new TierPlanner.Demand(TileQuality.Auto, true, false);
        Check.Equal(StreamLevel.Low, p.LevelOf(new(TileQuality.Auto, false, false), Cam0), "auto in the grid is low");
        Check.Equal(StreamLevel.High, p.LevelOf(focusedAuto, Cam0), "auto focused is high");
        Check.Equal(StreamLevel.High, p.LevelOf(new(TileQuality.Auto, false, true), Cam0), "auto zoomed is high");
        Check.Equal(StreamLevel.High, p.LevelOf(new(TileQuality.Clear, false, false), Cam0), "clear is high in the grid");
        Check.Equal(StreamLevel.Low, p.LevelOf(new(TileQuality.Fluent, true, true), Cam0), "fluent is low even focused and zoomed");
        Check.Equal(StreamLevel.Mid, p.LevelOf(new(TileQuality.Balanced, false, false), Cam0), "balanced is mid");
        Check.Equal(StreamTier.Sub, p.TierFor(StreamLevel.Low, Cam0), "low is sub");
        Check.Equal(StreamTier.Extern, p.TierFor(StreamLevel.Mid, Cam0), "mid is extern");
        Check.Equal(StreamTier.Main, p.TierFor(StreamLevel.High, Cam0), "high is main");
    }

    private static void QualityBalanced()
    {
        var p = Planner(middle: false);
        Check.True(!p.OffersBalanced(Cam0), "not offered without extern");
        Check.Equal(TileQuality.Auto, p.EffectiveQuality(TileQuality.Balanced, Cam0), "acts as auto");
        var m = Planner(middle: true);
        Check.True(m.OffersBalanced(Cam0), "offered for a single lens with extern");
        Check.True(!m.OffersBalanced(Lens1), "never for a second lens");
        Check.Equal(TileQuality.Auto, m.EffectiveQuality(TileQuality.Balanced, Lens1), "second lens acts as auto");
    }

    private static void ZoomHysteresisTest()
    {
        Check.True(!ZoomHysteresis.Next(false, 1.99), "below 2x stays low");
        Check.True(ZoomHysteresis.Next(false, 2.0), "2x goes high");
        Check.True(ZoomHysteresis.Next(true, 1.7), "between 1.6x and 2x stays high");
        Check.True(ZoomHysteresis.Next(true, 1.6), "1.6x still high");
        Check.True(!ZoomHysteresis.Next(true, 1.59), "below 1.6x goes low");
        var p = Planner();
        object tile = new();
        p.SetDemand(tile, Cam0, TileQuality.Auto, false, 1.8);
        Check.Equal(StreamTier.Sub, p.WantedTier(Cam0), "1.8x from the grid stays sub");
        p.SetDemand(tile, Cam0, TileQuality.Auto, false, 2.1);
        Check.Equal(StreamTier.Main, p.WantedTier(Cam0), "2.1x goes main");
        p.SetDemand(tile, Cam0, TileQuality.Auto, false, 1.8);
        Check.Equal(StreamTier.Main, p.WantedTier(Cam0), "1.8x stays main (hysteresis)");
        p.SetDemand(tile, Cam0, TileQuality.Auto, false, 1.5);
        Check.Equal(StreamTier.Sub, p.WantedTier(Cam0), "1.5x returns to sub");
    }

    private static void HighestWins()
    {
        var p = Planner(middle: true);
        object a = new(), b = new(), c = new();
        p.SetDemand(a, Cam0, TileQuality.Fluent, false, 1);
        p.SetDemand(b, Cam0, TileQuality.Balanced, false, 1);
        Check.Equal(StreamTier.Extern, p.WantedTier(Cam0), "balanced beats fluent");
        p.SetDemand(c, Cam0, TileQuality.Clear, false, 1);
        Check.Equal(StreamTier.Main, p.WantedTier(Cam0), "clear beats balanced");
        p.ClearDemands(c);
        Check.Equal(StreamTier.Extern, p.WantedTier(Cam0), "back to balanced when the clear tile goes");
        p.ClearDemands(b);
        Check.Equal(StreamTier.Sub, p.WantedTier(Cam0), "back to sub");
        Check.Equal(StreamTier.Sub, p.WantedTier(new CameraRef(Dev, 9)), "another camera is unaffected");
        IReadOnlyList<CameraRef> moved = p.SetDemand(a, new CameraRef(Dev, 2), TileQuality.Clear, false, 1);
        Check.Equal(2, moved.Count, "moving a tile re-plans both cameras");
        Check.Equal(StreamTier.Main, p.WantedTier(new CameraRef(Dev, 2)), "the new camera is raised");
        Check.Equal(StreamTier.Sub, p.WantedTier(Cam0), "the old one is released");
    }

    private static void FocusAndFluent()
    {
        var p = Planner();
        object tile = new();
        p.SetFocused(Cam0, true);
        Check.Equal(StreamTier.Main, p.WantedTier(Cam0), "focus without a demand is high");
        p.SetDemand(tile, Cam0, TileQuality.Fluent, true, 1);
        Check.Equal(StreamTier.Main, p.WantedTier(Cam0), "a focus anywhere still raises the camera");
        p.SetFocused(Cam0, false);
        Check.Equal(StreamTier.Sub, p.WantedTier(Cam0), "fluent alone stays low");
        p.SetFocused(Cam0, false);
        Check.True(!p.IsFocused(Cam0), "focus counts never go negative");
        p.SetFocused(Cam0, true);
        p.SetFocused(Cam0, true);
        p.SetFocused(Cam0, false);
        Check.True(p.IsFocused(Cam0), "two windows focus, one leaves: still focused");
    }

    private static void QualityMember()
    {
        foreach (TileQuality q in Enum.GetValues<TileQuality>())
        {
            Check.Equal(q, TileQualities.FromMember(q.ToMember()), $"round trip {q}");
        }
        Check.True(TileQuality.Auto.ToMember() is null, "auto is no member");
        Check.Equal(TileQuality.Auto, TileQualities.FromMember("ultra"), "a newer app's value reads as auto");
        Check.True(TileQuality.Auto.HeaderNote() is null, "no header note on auto");
        Check.Equal("Clear", TileQuality.Clear.HeaderNote(), "the header names the quality");
    }

    // ---- second lens ----

    private static void SecondLensTiers()
    {
        var p = Planner(middle: true);
        Check.Equal(StreamTier.Mobile, p.WantedTier(Lens1), "a second lens opens on mobile in the grid");
        p.SetFocused(Lens1, true);
        Check.Equal(StreamTier.Extern, p.WantedTier(Lens1), "extern when focused");
        p.SetFocused(Lens1, false);
        object tile = new();
        p.SetDemand(tile, Lens1, TileQuality.Clear, false, 1);
        Check.Equal(StreamTier.Extern, p.WantedTier(Lens1), "extern for Clear");
        p.SetDemand(tile, Lens1, TileQuality.Auto, false, 2.5);
        Check.Equal(StreamTier.Extern, p.WantedTier(Lens1), "extern when zoomed past 2x");
        p.SetDemand(tile, Lens1, TileQuality.Auto, false, 1);
        Check.Equal(StreamTier.Mobile, p.WantedTier(Lens1), "mobile again");
        Check.Equal((StreamTier.Mobile, StreamTier.Extern), p.Tiers(Lens1), "tiers of a second lens");
        Check.Equal((StreamTier.Sub, StreamTier.Main), p.Tiers(Cam0), "tiers of a first lens");
    }

    private static void SecondLensStartRejected()
    {
        var p = Planner();
        Check.True(!p.StartFailed(Cam0, StreamTier.Sub, "start_rejected"), "a first lens is left to reconnect handling");
        Check.True(!p.StartFailed(Lens1, StreamTier.Mobile, "timeout"), "other failures are left alone");
        Check.True(!p.StartFailed(Lens1, StreamTier.Extern, "start_rejected"), "a rejection of extern is not a mobile problem");
        Check.True(p.StartFailed(Lens1, StreamTier.Mobile, "start_rejected"), "mobile rejected: reopen");
        Check.Equal(StreamTier.Extern, p.WantedTier(Lens1), "extern for the rest of the run");
        Check.True(p.NoMobile(Lens1), "remembered");
        Check.Equal((StreamTier.Extern, StreamTier.Extern), p.Tiers(Lens1), "grid is extern now");
        Check.Equal(StreamTier.Mobile, p.WantedTier(new CameraRef(Dev, 1, 1) with { Channel = 5 }), "another second lens still tries mobile");
    }

    private static void SecondLensTierFailed()
    {
        var p = Planner();
        Check.True(!p.TierFailed(Lens1, StreamTier.Mobile, "superseded"), "superseded is not a failure");
        Check.True(!p.TierFailed(Lens1, StreamTier.Extern, "timeout"), "extern failing is not about mobile");
        Check.True(!p.TierFailed(Cam0, StreamTier.Mobile, "timeout"), "first lenses do not use mobile");
        Check.Equal(StreamTier.Mobile, p.WantedTier(Lens1), "still mobile");
        Check.True(p.TierFailed(Lens1, StreamTier.Mobile, "timeout"), "mobile timed out");
        Check.Equal(StreamTier.Extern, p.WantedTier(Lens1), "extern now");
    }

    // ---- auto-hide, keys, clock ----

    private static void AutoHide()
    {
        var clock = new ManualViewerClock(100);
        var idle = new ChromeIdle(clock);
        Check.True(!idle.ChromeShown, "hidden before any interaction");
        idle.Interact();
        Check.True(idle.ChromeShown, "shown on interaction");
        clock.Advance(2.9);
        Check.True(idle.ChromeShown, "still shown at 2.9 s");
        clock.Advance(0.1);
        Check.True(!idle.ChromeShown, "hidden at 3 s");
        idle.Interact();
        Check.True(idle.ChromeShown, "back on the next interaction");
        clock.Advance(2);
        idle.Interact();
        clock.Advance(2);
        Check.True(idle.ChromeShown, "an interaction restarts the 3 s");
    }

    private static void AutoHidePointer()
    {
        var clock = new ManualViewerClock();
        var idle = new ChromeIdle(clock);
        idle.Interact();
        clock.Advance(5);
        Check.True(idle.PointerHidden(fullScreen: true), "the pointer hides in full screen");
        Check.True(!idle.PointerHidden(fullScreen: false), "never out of full screen");
        idle.Interact();
        Check.True(!idle.PointerHidden(fullScreen: true), "returns on the next movement");
    }

    private static void Keys()
    {
        Check.Equal(TileAction.Dismiss, TileKeyMap.Map(TileKey.Enter, false, false, false, hasAlert: true), "enter dismisses an alert");
        Check.Equal(TileAction.ToggleFocus, TileKeyMap.Map(TileKey.Enter, false, false, false, hasAlert: false), "enter focuses without one");
        Check.Equal(TileAction.ToggleFocus, TileKeyMap.Map(TileKey.Space, false, false, false, hasAlert: true), "space always focuses");
        Check.Equal(TileAction.ToggleListening, TileKeyMap.Map(TileKey.M, false, false, false, false), "m listens");
        Check.Equal(TileAction.ToggleListening, TileKeyMap.Map(TileKey.L, true, false, false, false), "ctrl+l listens");
        Check.Equal(TileAction.None, TileKeyMap.Map(TileKey.L, false, false, false, false), "plain l does nothing");
        Check.Equal(TileAction.None, TileKeyMap.Map(TileKey.M, true, false, false, false), "ctrl+m is not m");
        Check.Equal(TileAction.None, TileKeyMap.Map(TileKey.M, false, true, false, false), "alt+m is a menu key");
        Check.Equal(TileAction.None, TileKeyMap.Map(TileKey.M, false, false, false, false, editingText: true), "text editing takes the key");
        Check.Equal(TileAction.None, TileKeyMap.Map(TileKey.Other, false, false, false, true), "other keys");
        Check.Equal(TileAction.SavePicture, TileKeyMap.Map(TileKey.P, false, false, false, false), "p saves the picture");
        Check.Equal(TileAction.SavePictureAs, TileKeyMap.Map(TileKey.P, false, false, true, false), "shift+p chooses where");
        Check.Equal(TileAction.SavePicture, TileKeyMap.Map(TileKey.P, true, false, true, false), "ctrl+shift+p saves in a call window");
        Check.Equal(TileAction.None, TileKeyMap.Map(TileKey.P, true, false, false, false), "ctrl+p is the system's");
        Check.Equal(TileAction.ToggleRecording, TileKeyMap.Map(TileKey.R, false, false, false, false), "r records");
        Check.Equal(TileAction.ToggleRecording, TileKeyMap.Map(TileKey.R, true, false, true, false), "ctrl+shift+r records the call");
        Check.Equal(TileAction.ToggleConversation, TileKeyMap.Map(TileKey.T, false, false, false, false), "t starts or ends a conversation");
        Check.Equal(TileAction.None, TileKeyMap.Map(TileKey.T, false, false, false, false, editingText: true), "t is typed while editing text");
    }

    private static void ManualClock()
    {
        var c = new ManualViewerClock(5);
        c.Advance(1.5);
        c.Advance(-3);
        Check.Equal(6.5, c.Now, "advances, never backward");
    }
}
