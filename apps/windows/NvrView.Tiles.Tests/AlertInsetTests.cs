// Tests of the alert insets' rules (NvrView.Tiles/AlertInsets.cs) on simulated time. The vectors
// are the macOS AlertInsetTests', so both apps order, swap, close and grid insets alike.

using NvrView.Tiles;

namespace NvrView.Tiles.Tests;

internal static class AlertInsetTests
{
    private static readonly CameraRef Door = new("dev-example", 3);
    private static readonly CameraRef Yard = new("dev-example", 1);
    private static readonly CameraRef Hall = new("dev-example", 4);
    private static readonly CameraRef Side = new("dev-example-2", 0);
    private static readonly CameraRef Shown = new("dev-example-2", 2);
    private static readonly DateTime T0 = new(2027, 1, 15, 8, 0, 0, DateTimeKind.Local);

    public static IEnumerable<(string, Action)> All() =>
    [
        ("alert insets: what opens an inset", WhatOpensAnInset),
        ("alert insets: a visible camera gets no inset", AVisibleCameraGetsNoInset),
        ("alert insets: newest on top up to the grid, then more", NewestOnTopUpToThreeThenMore),
        ("alert insets: a repeat on a drawn camera keeps its place", ARepeatOnADrawnCameraKeepsItsPlace),
        ("alert insets: a newer inset waits until the older has been up ten seconds", ANewerInsetWaits),
        ("alert insets: interleaved events on four cameras cause no churn", NoChurn),
        ("alert insets: a waiting camera can be brought in", AWaitingCameraCanBeBroughtIn),
        ("alert insets: the grid setting sets how many are drawn", TheGridSettingSetsHowManyAreDrawn),
        ("alert insets: the grid setting is saved clamped", TheGridSettingIsSaved),
        ("alert insets: the return inset shows the layout when the stage showed one", ReturnShowsTheLayout),
        ("alert insets: a single camera stage keeps the one-camera return inset", ReturnKeepsOneCamera),
        ("alert insets: the return inset always gets a place", ReturnAlwaysGetsAPlace),
        ("alert insets: a sound alarm outranks a detection on the same camera", SoundAlarmOutranks),
        ("alert insets: the header says reason, time and count", HeaderText),
        ("alert insets: a click swaps and one more click swaps back", ClickSwapsAndBack),
        ("alert insets: swapping another inset while swapped keeps the way back", SwapAnotherWhileSwapped),
        ("alert insets: Esc dismisses the swapped-in camera or the top inset", EscDismisses),
        ("alert insets: closing the return inset swaps back", ClosingTheReturnInset),
        ("alert insets: doorbell and detection insets close a minute after the event", CloseAMinuteAfter),
        ("alert insets: hover holds and use restarts the clock", HoverHoldsAndUseRestarts),
        ("alert insets: a lost stop still closes", ALostStopStillCloses),
        ("alert insets: a repeat keeps it open", ARepeatKeepsItOpen),
        ("alert insets: a swapped-in doorbell closes and the stage returns", ASwappedInDoorbellCloses),
        ("alert insets: a conversation inset closes a minute after its last use", ConversationInset),
        ("alert insets: a press during a conversation turns it into the doorbell inset", PressDuringConversation),
        ("alert insets: sound alarm insets stay until dismissed", SoundAlarmsStay),
        ("alert insets: dismissing the inset dismisses the alert and the other way round", InsetAndAlertLink),
        ("alert insets: a call in its own window is detached and not drawn", DetachedCalls),
        ("alert insets: the grid fills columns from the anchored edge", GridFillsColumns),
        ("alert insets: one column of three is the original stack", OneColumnIsTheOriginalStack),
        ("alert insets: columns that do not fit are dropped and the rest wait", ColumnsThatDoNotFit),
    ];

    private static DetectionEvent Event(string kind, int channel = 3, bool active = true, string source = DetectionSource.Device, bool alarm = false) =>
        new("dev-example", channel, kind, active, source, Alarm: alarm);

    private static IReadOnlySet<string> Kinds(params string[] kinds) => new HashSet<string>(kinds);

    private static string Names(IEnumerable<CameraRef> cameras) => string.Join(", ", cameras);

    private static string Sorted(IEnumerable<CameraRef> cameras) => string.Join(", ", cameras.OrderBy(c => c.ToString(), StringComparer.Ordinal));

    private static void Same(IReadOnlyList<CameraRef> expected, IEnumerable<InsetItem> actual, string what) =>
        Check.Equal(Names(expected), Names(actual.Select(i => i.Camera)), what);

    private static void WhatOpensAnInset()
    {
        Check.Equal<InsetKind?>(InsetKind.Doorbell, InsetTrigger.For(Event("visitor", alarm: true), false), "a press");
        Check.Equal<InsetKind?>(null, InsetTrigger.For(Event("other_alarm", alarm: true), true), "another alarm");
        Check.Equal<InsetKind?>(InsetKind.SoundAlarm("smoke_alarm"), InsetTrigger.For(Event("smoke_alarm", source: DetectionSource.LocalAudio), false), "smoke");
        Check.Equal<InsetKind?>(InsetKind.SoundAlarm("co_alarm"), InsetTrigger.For(Event("co_alarm", source: DetectionSource.LocalAudio), false), "CO");
        // People only on cameras marked as entrances, or turned on per camera.
        Check.Equal<InsetKind?>(InsetKind.Detection("people"), InsetTrigger.For(Event("people"), true), "a person at an entrance");
        Check.Equal<InsetKind?>(null, InsetTrigger.For(Event("people"), false), "a person elsewhere");
        Check.Equal<InsetKind?>(null, InsetTrigger.For(Event("vehicle"), true), "a vehicle at an entrance");
        Check.Equal<InsetKind?>(InsetKind.Detection("vehicle"), InsetTrigger.For(Event("vehicle"), false, Kinds("vehicle")), "a turned-on kind");
        Check.Equal<InsetKind?>(null, InsetTrigger.For(Event(DetectionEvent.MotionKind), true, Kinds("motion")), "motion never");
    }

    private static void AVisibleCameraGetsNoInset()
    {
        var board = new InsetBoard();
        Check.True(!board.Started(InsetKind.Doorbell, Door, true, 0, T0), "visible: no inset");
        Check.True(board.IsEmpty, "empty");
        Check.True(board.Started(InsetKind.Doorbell, Door, false, 0, T0), "not visible: an inset");
        Same([Door], board.Shown, "drawn");
    }

    private static void NewestOnTopUpToThreeThenMore()
    {
        var board = new InsetBoard();
        // Spaced past the minimum on-screen time, so the oldest gives way.
        CameraRef[] cameras = [Door, Yard, Hall, Side];
        for (int i = 0; i < cameras.Length; i++)
        {
            double t = i * 11;
            board.Started(InsetKind.Detection("people"), cameras[i], false, t, T0.AddSeconds(t));
        }
        Same([Side, Hall, Yard], board.Shown, "the three newest");
        Check.Equal(1, board.More, "one more");
        Same([Door], board.Waiting, "the oldest waits");
        // A repeat refreshes that camera's inset where it is (here: waiting).
        board.Started(InsetKind.Detection("people"), Door, false, 40, T0.AddSeconds(40));
        Check.Equal(4, board.Items.Count, "still four");
        Same([Side, Hall, Yard, Door], board.Items, "the order holds");
        Check.Equal(2, board.ItemFor(Door)!.Count, "counted");
        Check.Equal(T0.AddSeconds(40), board.ItemFor(Door)!.LastAt, "last time");
        Check.Equal(T0, board.ItemFor(Door)!.FirstAt, "first time");
    }

    private static void ARepeatOnADrawnCameraKeepsItsPlace()
    {
        var board = new InsetBoard();
        CameraRef[] cameras = [Door, Yard, Hall];
        for (int i = 0; i < cameras.Length; i++)
        {
            board.Started(InsetKind.Detection("people"), cameras[i], false, i, T0.AddSeconds(i));
        }
        Same([Hall, Yard, Door], board.Shown, "drawn");
        board.Started(InsetKind.Detection("people"), Door, false, 5, T0.AddSeconds(5));
        Same([Hall, Yard, Door], board.Shown, "the same places");
        Check.Equal(2, board.Shown[^1].Count, "counted");
    }

    private static void ANewerInsetWaits()
    {
        var board = new InsetBoard();
        CameraRef[] cameras = [Door, Yard, Hall];
        for (int i = 0; i < cameras.Length; i++)
        {
            board.Started(InsetKind.Detection("people"), cameras[i], false, i, T0.AddSeconds(i));
        }
        board.Started(InsetKind.Detection("people"), Side, false, 5, T0.AddSeconds(5));
        // All three are younger than 10 s: the newcomer waits.
        Same([Hall, Yard, Door], board.Shown, "the three");
        Same([Side], board.Waiting, "the newcomer waits");
        // The lowest one (the oldest) gives way at its tenth second.
        Check.Equal<double?>(InsetBoard.MinimumShownTime, board.Deadline, "the deadline");
        Check.Equal(0, board.Tick(9).Count, "nothing at 9 s");
        Same([Side], board.Waiting, "still waiting at 9 s");
        Check.Equal(0, board.Tick(10).Count, "nothing closes at 10 s");
        Same([Side, Hall, Yard], board.Shown, "the newcomer is in");
        Same([Door], board.Waiting, "the oldest waits");
    }

    private static void NoChurn()
    {
        var board = new InsetBoard();
        int changes = 0;
        string previous = "";
        CameraRef[] cameras = [Door, Yard, Hall, Side];
        for (int step = 0; step < 40; step++)
        {
            board.Started(InsetKind.Detection("people"), cameras[step % 4], false, step, T0.AddSeconds(step));
            board.Tick(step);
            string now = Names(board.Shown.Select(i => i.Camera));
            if (now != previous)
            {
                changes++;
            }
            previous = now;
        }
        // Three arrive, the fourth waits; the lowest makes room for it once it has been up 10 s,
        // and the order then holds: far fewer changes than the forty events.
        Check.True(changes <= 6, $"churn: {changes} changes");
        Check.Equal(4, board.Items.Count, "four items");
    }

    private static void AWaitingCameraCanBeBroughtIn()
    {
        var board = new InsetBoard();
        CameraRef[] cameras = [Door, Yard, Hall, Side];
        for (int i = 0; i < cameras.Length; i++)
        {
            board.Started(InsetKind.Detection("people"), cameras[i], false, i, T0.AddSeconds(i));
        }
        CameraRef waiting = board.Waiting[0].Camera;
        Check.True(!board.BringIn(board.Shown[0].Camera, 4), "a drawn one is not brought in");
        Check.True(board.BringIn(waiting, 4), "a waiting one is");
        Check.Equal(waiting, board.Shown[0].Camera, "on top");
        Check.Equal(3, board.Shown.Count, "three drawn");
        Check.Equal(1, board.More, "one waits");
    }

    private static void TheGridSettingSetsHowManyAreDrawn()
    {
        var grid = new InsetGridSetting(2, 3);
        Check.Equal(6, grid.Capacity, "2 x 3");
        Check.Equal(new InsetGridSetting(3, 1), InsetGridSetting.Standard, "the standard grid");
        Check.Equal(new InsetGridSetting(1, 6), new InsetGridSetting(0, 99), "clamped");
        var board = new InsetBoard(grid.Capacity);
        for (int i = 0; i < 7; i++)
        {
            board.Started(InsetKind.Detection("people"), new CameraRef("dev-example-9", i), false, i, T0.AddSeconds(i));
        }
        Check.Equal(6, board.Shown.Count, "six drawn");
        Check.Equal(1, board.More, "one more");
        // Shrinking it moves the lowest ones to the waiting list at once.
        board.SetCapacity(new InsetGridSetting(1, 2).Capacity);
        Check.Equal(2, board.Shown.Count, "two drawn");
        Check.Equal(5, board.More, "five wait");
        board.SetCapacity(6);
        Check.Equal(6, board.Shown.Count, "six again");
    }

    private static void TheGridSettingIsSaved()
    {
        Check.Equal<InsetGridSetting?>(new InsetGridSetting(2, 3), InsetGridSetting.FromJson(new InsetGridSetting(2, 3).ToJson()), "round trip");
        Check.Equal<InsetGridSetting?>(new InsetGridSetting(4, 6), InsetGridSetting.FromJson("{\"rows\":9,\"columns\":99}"), "clamped on reading");
        Check.True(InsetGridSetting.FromJson("{\"rows\":2}") is null, "columns missing");
        Check.True(InsetGridSetting.FromJson("[1]") is null, "not an object");
        Check.True(InsetGridSetting.FromJson("nonsense") is null, "not JSON");
        Check.True(InsetGridSetting.FromJson(null) is null, "nothing");
    }

    private static void ReturnShowsTheLayout()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Doorbell, Door, false, 0, T0);
        board.SwapIn(Door, Shown, 1, displacedLayout: true);
        Check.True(board.Swap!.DisplacedLayout, "the swap remembers the layout");
        Check.True(board.Items[0].IsReturn && board.Items[0].ShowsLayout, "a layout return inset");
        Check.True(board.Shown[0].ShowsLayout, "drawn as a layout");
        // A click on it restores the stage; nothing of the layout was kept here.
        board.SwapIn(Shown, null, 2);
        Check.True(board.Swap is null, "swapped back");
        Same([Door], board.Shown, "the doorbell's inset is back");
    }

    private static void ReturnKeepsOneCamera()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Doorbell, Door, false, 0, T0);
        board.SwapIn(Door, Shown, 1);
        Check.True(!board.Swap!.DisplacedLayout, "one camera");
        Check.True(!board.Items[0].ShowsLayout, "not a layout");
        board.SwapBack(2);
        board.SwapTop(Shown, 3, displacedLayout: true);
        Check.True(board.Swap!.DisplacedLayout, "Tab from a layout");
        board.SwapTop(null, 4);
        Check.True(board.Swap is null, "Tab swaps back");
    }

    private static void ReturnAlwaysGetsAPlace()
    {
        var board = new InsetBoard();
        CameraRef[] cameras = [Door, Yard, Hall];
        for (int i = 0; i < cameras.Length; i++)
        {
            board.Started(InsetKind.Detection("people"), cameras[i], false, i, T0.AddSeconds(i));
        }
        board.SwapIn(Hall, Shown, 3, displacedLayout: true);
        // Two others plus the return inset, though neither has been up 10 s.
        Check.Equal(Sorted([Door, Yard, Shown]), Sorted(board.Shown.Select(i => i.Camera)), "the drawn set");
    }

    private static void SoundAlarmOutranks()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Detection("people"), Hall, false, 0, T0);
        board.Started(InsetKind.SoundAlarm("smoke_alarm"), Hall, false, 1, T0.AddSeconds(1));
        board.Started(InsetKind.Detection("people"), Hall, false, 2, T0.AddSeconds(2));
        Check.Equal(InsetKind.SoundAlarm("smoke_alarm"), board.ItemFor(Hall)!.Kind, "the alarm stays");
        Check.Equal(3, board.ItemFor(Hall)!.Count, "all counted");
    }

    private static void HeaderText()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Doorbell, Door, false, 0, T0);
        string once = board.ItemFor(Door)!.Header(T0);
        Check.True(once.StartsWith("Doorbell · ", StringComparison.Ordinal), "reason first");
        Check.True(!once.Contains("times", StringComparison.Ordinal), "no count for one");
        board.Started(InsetKind.Doorbell, Door, false, 5, T0.AddSeconds(5));
        Check.True(board.ItemFor(Door)!.Header(T0).Contains("2 times, last", StringComparison.Ordinal), "the count and last time");
        Check.Equal("CO alarm (sound)", InsetKind.SoundAlarm("co_alarm").Text, "a sound alarm's reason");
    }

    private static void ClickSwapsAndBack()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Doorbell, Door, false, 0, T0);
        board.SwapIn(Door, Shown, 1);
        Check.Equal(Door, board.Swap!.Shown, "the doorbell on the stage");
        Same([Shown], board.Items, "the return inset");
        Check.True(board.Items[0].IsReturn, "a return inset");
        // The return inset: a click swaps back, the doorbell's inset returns.
        board.SwapIn(Shown, null, 2);
        Check.True(board.Swap is null, "swapped back");
        Same([Door], board.Items, "the doorbell's inset");
        Check.True(!board.Items[0].IsReturn, "an event inset again");
        // Tab does the same on the top inset.
        board.SwapTop(Shown, 3);
        Check.Equal(Door, board.Swap!.Shown, "Tab swaps in");
        board.SwapTop(null, 4);
        Check.True(board.Swap is null, "Tab swaps back");
        Same([Door], board.Shown, "drawn");
    }

    private static void SwapAnotherWhileSwapped()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Doorbell, Door, false, 0, T0);
        board.Started(InsetKind.SoundAlarm("smoke_alarm"), Hall, false, 0, T0);
        board.SwapIn(Hall, Shown, 1);
        board.SwapIn(Door, null, 2);
        Check.Equal(Door, board.Swap!.Shown, "the doorbell on the stage");
        Check.Equal<CameraRef?>(Shown, board.Swap.Displaced, "the way back");
        Check.Equal(Sorted([Hall, Shown]), Sorted(board.Items.Select(i => i.Camera)), "the others are insets");
        board.SwapBack(3);
        Check.True(board.Swap is null, "swapped back");
        Same([Door, Hall], board.Items, "both event insets");
    }

    private static void EscDismisses()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Doorbell, Door, false, 0, T0);
        board.Started(InsetKind.Detection("people"), Yard, false, 0, T0);
        board.SwapIn(Door, Shown, 1);
        Check.Equal<CameraRef?>(Door, board.DismissTop()?.Camera, "the swapped-in camera first");
        Check.True(board.Swap is null, "the stage returns");
        Same([Yard], board.Items, "the other inset remains");
        Check.Equal<CameraRef?>(Yard, board.DismissTop()?.Camera, "then the top inset");
        Check.True(board.IsEmpty, "empty");
        Check.True(board.DismissTop() is null, "nothing to dismiss");
    }

    private static void ClosingTheReturnInset()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Doorbell, Door, false, 0, T0);
        board.SwapIn(Door, Shown, 1);
        Check.True(board.Dismiss(Shown) is null, "a return inset is no event");
        Check.True(board.Swap is null, "swapped back");
        Check.Equal(0, board.Items.Count, "nothing left (the swapped-in item was dropped)");
    }

    private static void CloseAMinuteAfter()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Doorbell, Door, false, 0, T0);
        board.Started(InsetKind.Detection("people"), Yard, false, 0, T0);
        Check.Equal(0, board.Tick(100).Count, "still going: no stop yet");
        board.Stopped(InsetKind.Doorbell, Door, 100);
        board.Stopped(InsetKind.Detection("people"), Yard, 100);
        Check.Equal<double?>(160, board.Deadline, "a minute after the stop");
        Check.Equal(0, board.Tick(159.9).Count, "not yet");
        IReadOnlyList<InsetItem> due = board.Tick(160);
        Check.Equal(Sorted([Door, Yard]), Sorted(due.Select(i => i.Camera)), "both closed");
        Check.True(board.IsEmpty, "empty");
    }

    private static void HoverHoldsAndUseRestarts()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Doorbell, Door, false, 0, T0);
        board.Stopped(InsetKind.Doorbell, Door, 1);
        board.Hover(Door, true, 30);
        Check.Equal(0, board.Tick(500).Count, "hovered: held");
        board.Hover(Door, false, 500);
        Check.Equal(0, board.Tick(559).Count, "a minute after the hover");
        board.Use(Door, 559);
        Check.Equal(0, board.Tick(618).Count, "use restarts the minute");
        Check.Equal(Door, board.Tick(619).Single().Camera, "closed a minute after use");
    }

    private static void ALostStopStillCloses()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Detection("people"), Yard, false, 0, T0);
        Check.Equal(0, board.Tick(InsetBoard.LostStopAfter + InsetBoard.AutoCloseDelay - 1).Count, "not yet");
        Check.Equal(1, board.Tick(InsetBoard.LostStopAfter + InsetBoard.AutoCloseDelay).Count, "closed");
    }

    private static void ARepeatKeepsItOpen()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Doorbell, Door, false, 0, T0);
        board.Stopped(InsetKind.Doorbell, Door, 1);
        board.Started(InsetKind.Doorbell, Door, false, 50, T0.AddSeconds(50));
        Check.Equal(0, board.Tick(70).Count, "going again");
        board.Stopped(InsetKind.Doorbell, Door, 51);
        Check.Equal(0, board.Tick(110).Count, "a minute from the new stop");
        Check.Equal(1, board.Tick(111).Count, "closed");
    }

    private static void ASwappedInDoorbellCloses()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Doorbell, Door, false, 0, T0);
        board.Stopped(InsetKind.Doorbell, Door, 0);
        board.SwapIn(Door, Shown, 10);
        Check.Equal(0, board.Tick(69).Count, "a minute from the swap");
        Check.Equal(Door, board.Tick(70).Single().Camera, "closed");
        Check.True(board.Swap is null, "the stage returns");
        Check.Equal(0, board.Items.Count, "no return inset");
    }

    private static void ConversationInset()
    {
        Check.True(InsetKind.Conversation.IsCall && InsetKind.Doorbell.IsCall && !InsetKind.View.IsCall, "calls");
        Check.True(InsetKind.Conversation.ClosesByItself && !InsetKind.Conversation.IsCritical, "closes by itself");
        Check.Equal("Conversation", InsetKind.Conversation.Text, "reason");
        var board = new InsetBoard();
        // Started by the user, not an event: opened and "ended" at once.
        board.Started(InsetKind.Conversation, Door, false, 0, T0);
        board.Stopped(InsetKind.Conversation, Door, 0);
        board.Use(Door, 30);
        Check.Equal(0, board.Tick(89).Count, "a minute after the last use");
        Check.Equal(Door, board.Tick(90).Single().Camera, "closed");
    }

    private static void PressDuringConversation()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.Conversation, Door, false, 0, T0);
        board.Stopped(InsetKind.Conversation, Door, 0);
        board.Started(InsetKind.Doorbell, Door, false, 5, T0.AddSeconds(5));
        Check.True(board.ItemFor(Door)!.Kind == InsetKind.Doorbell && board.ItemFor(Door)!.Count == 2, "the doorbell's inset, counted");
    }

    private static void SoundAlarmsStay()
    {
        var board = new InsetBoard();
        board.Started(InsetKind.SoundAlarm("smoke_alarm"), Hall, false, 0, T0);
        board.Stopped(InsetKind.SoundAlarm("smoke_alarm"), Hall, 5);
        Check.True(board.Deadline is null, "no deadline");
        Check.Equal(0, board.Tick(10_000).Count, "nothing closes");
        Check.Equal(InsetKind.SoundAlarm("smoke_alarm"), board.Dismiss(Hall)!.Kind, "dismissed");
        Check.True(board.IsEmpty, "empty");
    }

    private static void InsetAndAlertLink()
    {
        var alerts = new AlertCenter();
        var board = new InsetBoard();
        DetectionEvent smoke = new("dev-example", 4, "smoke_alarm", true, DetectionSource.LocalAudio);
        alerts.Apply(smoke, T0);
        InsetKind kind = InsetTrigger.For(smoke, false)!.Value;
        board.Started(kind, Hall, false, 0, T0);
        // What the stage does when the inset is dismissed.
        if (board.Dismiss(Hall) is { } item && item.Kind.IsCritical)
        {
            alerts.Dismiss(item.Camera, T0.AddSeconds(1));
        }
        Check.True(!alerts.IsActive(Hall), "the alert goes with the inset");
        // Dismissed elsewhere (a tile header in another window): the inset goes.
        alerts.Apply(smoke, T0.AddSeconds(2));
        board.Started(kind, Hall, false, 2, T0.AddSeconds(2));
        alerts.Dismiss(Hall, T0.AddSeconds(3));
        IReadOnlyList<InsetItem> dropped = board.DropSoundAlarms(c => !alerts.IsActive(c));
        Check.Equal(Hall, dropped.Single().Camera, "dropped");
        Check.True(board.IsEmpty, "empty");
    }

    private static void DetachedCalls()
    {
        var board = new InsetBoard();
        // A call in its own window is recorded whether or not its camera is visible, but not drawn.
        Check.True(board.Started(InsetKind.Doorbell, Door, true, 0, T0, detached: true), "recorded though visible");
        Check.True(board.IsEmpty && board.Shown.Count == 0 && board.More == 0, "not drawn, not counted");
        Same([Door], board.Items.Where(i => i.Detached), "kept");
        // Popping in puts it on top.
        Check.True(board.SetDetached(Door, false, 5), "popped in");
        Same([Door], board.Shown, "drawn");
        // Popping out the swapped-in camera ends the swap first.
        board.SwapIn(Door, Shown, 6);
        Check.True(board.SetDetached(Door, true, 7), "popped out");
        Check.True(board.Swap is null, "the stage shows its own view again");
        Check.True(board.IsEmpty, "nothing drawn here");
    }

    private static void GridFillsColumns()
    {
        const double stageWidth = 1600, stageHeight = 900;
        double width = InsetGeometry.Width(stageWidth, 3);
        Check.Equal(480.0, width, "30% of the width; three columns fit");
        double[] heights = [150, 150, 150, 150, 150, 150, 150];
        IReadOnlyList<InsetFrame> right = InsetGeometry.Grid(heights, width, 2, 3, InsetAnchor.FromCorner(ChromeCorner.TopRight), stageWidth, stageHeight);
        Check.Equal(6, right.Count, "six fit");
        // The first column is at the right edge; the next ones grow leftward.
        Check.True(right[0].X == 1600 - 12 - 480 && right[1].X == right[0].X, "the first column at the right edge");
        Check.Equal(right[0].X - 8 - 480, right[2].X, "the second column");
        Check.Equal(right[0].X - 2 * (8 + 480), right[4].X, "the third column");
        Check.True(right[0].Y == 12 && right[1].Y == 12 + 150 + 8, "stacked downward from the top");
        IReadOnlyList<InsetFrame> left = InsetGeometry.Grid(heights, width, 2, 3, InsetAnchor.FromCorner(ChromeCorner.BottomLeft), stageWidth, stageHeight);
        Check.True(left[0].X == 12 && left[2].X == 12 + 480 + 8, "from the left edge");
        // Bottom anchored: the first inset is at the bottom of its column.
        Check.True(left[0].Y == 900 - 12 - 150 && left[1].Y == left[0].Y - 8 - 150, "upward from the bottom");
    }

    private static void OneColumnIsTheOriginalStack()
    {
        double width = InsetGeometry.Width(1000, 1);
        Check.Equal(InsetGeometry.Width(1000), width, "the same width");
        foreach (ChromeCorner corner in new[] { ChromeCorner.TopLeft, ChromeCorner.TopRight, ChromeCorner.BottomLeft, ChromeCorner.BottomRight })
        {
            InsetAnchor anchor = InsetAnchor.FromCorner(corner);
            IReadOnlyList<InsetFrame> grid = InsetGeometry.Grid([200, 200, 200], width, 3, 1, anchor, 1000, 900);
            IReadOnlyList<InsetFrame> old = InsetGeometry.Frames(3, width, [200, 200, 200], anchor, 1000, 900);
            Check.True(grid.Count == 3 && old.Count == 3, "three each");
            Check.True(grid.Zip(old).All(p => p.First.X == p.Second.X && p.First.Y == p.Second.Y), $"the same frames from {corner}");
        }
    }

    private static void ColumnsThatDoNotFit()
    {
        // 500 DIPs wide: two 220 columns fit; of six requested, two are used.
        Check.Equal(2, InsetGeometry.FittingColumns(500, 6), "two fit");
        Check.Equal(1, InsetGeometry.FittingColumns(200, 6), "at least one");
        double width = InsetGeometry.Width(500, 6);
        Check.True(width >= InsetGeometry.MinWidth && width <= (500 - 24 - 8) / 2.0, "the width");
        IReadOnlyList<InsetFrame> frames = InsetGeometry.Grid([.. Enumerable.Repeat(100.0, 8)], width, 2, 6, InsetAnchor.FromCorner(ChromeCorner.TopLeft), 500, 900);
        Check.Equal(4, frames.Count, "two columns of two");
        // A short stage: the second inset does not fit under the first, so it starts the next column.
        IReadOnlyList<InsetFrame> shortStage = InsetGeometry.Grid([150, 150, 150], 220, 3, 2, InsetAnchor.FromCorner(ChromeCorner.TopLeft), 700, 200);
        Check.True(shortStage.Count == 2 && shortStage[1].X > shortStage[0].X, "the next column");
    }
}
