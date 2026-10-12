using System.Text.Json;
using NvrView.Interop;

namespace NvrView.Shell.Tests;

/// <summary>
/// The recordings screens' pure pieces (RecordingsBrowse.cs, PlayerRules.cs)
/// with the vectors of the macOS app's RecordingsBrowseTests and
/// PlayerRulesTests.
/// </summary>
internal static class RecordingsBrowseTests
{
    private const long Oct1 = 1_790_812_800; // 2026-10-01 00:00:00, a Thursday
    private const long Oct10 = 1_791_590_400;

    private static DownloadProgress Progress(string id, string state, int chunks = 0, bool paused = false)
    {
        using JsonDocument doc = JsonDocument.Parse($$"""
            {"download":"{{id}}","folder":"/x/{{id}}","state":"{{state}}","paused":{{(paused ? "true" : "false")}},"cameras":[
             {"camera":{"device":"d-1","channel":0},"state":"{{state}}","chunks_done":{{chunks}},"chunks_total":4,"bytes":0}]}
            """);
        return DownloadProgress.From(doc.RootElement)!;
    }

    public static void Run()
    {
        T.Run("browse: device days", () =>
        {
            T.Equal(Oct10, DeviceTime.DayStart(Oct10 + 3600));
            T.Equal(Oct10, DeviceTime.DayStart(Oct10));
            T.Equal(-86_400L, DeviceTime.DayStart(-1));
            // 23:30 UTC is already the next day two hours east of UTC.
            DateTimeOffset late = DateTimeOffset.FromUnixTimeSeconds(Oct10 + 23 * 3600 + 1800);
            T.Equal(Oct10, DeviceTime.Today(late, 0));
            T.Equal(Oct10 + 86_400, DeviceTime.Today(late, 7200));
            T.Equal("07:42:05", DeviceTime.Time(Oct10 + 7 * 3600 + 42 * 60 + 5));
            T.Equal("00:00:05–00:01:05", DeviceTime.Span(Oct10 + 5, Oct10 + 65));
            T.Equal(10, DeviceTime.DayOfMonth(Oct10));
        });

        T.Run("browse: months", () =>
        {
            T.Equal((Oct1, Oct1 + 31 * 86_400), DeviceTime.Month(Oct10));
            T.Equal(Oct1 - 30 * 86_400, DeviceTime.MonthAdding(Oct10, -1));
            T.Equal(1_798_761_600L, DeviceTime.MonthAdding(Oct10, 3)); // 2027-01-01
            List<long?[]> sundayFirst = DeviceTime.Weeks(Oct10, 1);
            T.Equal(5, sundayFirst.Count);
            T.Check(sundayFirst.All(w => w.Length == 7));
            T.Check(sundayFirst[0].Take(4).All(d => d is null) && sundayFirst[0][4] == Oct1);
            T.Equal(31, sundayFirst.SelectMany(w => w).Count(d => d is not null));
            T.Equal((long?)Oct1, DeviceTime.Weeks(Oct10, 2)[0][3]);
        });

        T.Run("browse: kind filters", () =>
        {
            foreach (string animal in new[] { "dog_cat", "dog", "cat", "bird", "animal", "wild_animal" })
            {
                T.Equal(KindFilter.Animal, KindFilters.Of(animal), animal);
            }
            T.Equal(KindFilter.Other, KindFilters.Of("dog_bark"));
            T.Equal(KindFilter.People, KindFilters.Of("people"));
            T.Equal(KindFilter.Vehicle, KindFilters.Of("vehicle"));
            T.Equal(KindFilter.Motion, KindFilters.Of("motion"));
            T.Equal(KindFilter.Other, KindFilters.Of("package"));
            T.Check(KindFilters.Passes(["people", "dog_cat"], new HashSet<KindFilter> { KindFilter.Animal }));
            T.Check(!KindFilters.Passes(["people"], new HashSet<KindFilter> { KindFilter.Animal, KindFilter.Vehicle }));
            T.Check(KindFilters.Passes(["anything"], new HashSet<KindFilter>()));
        });

        T.Run("browse: the contract's answers parse", () =>
        {
            IncidentsAnswer incidents = T.NotNull(IncidentsAnswer.Parse("""
                {"revision": 12, "incidents": [
                 {"key": "i-example-1", "id": "example-id-1",
                  "name": null, "group": "Group 1", "start": 1791618125, "end": 1791618160,
                  "margin_s": 5, "kinds": ["vehicle"], "growing": false, "edited": false, "locked": false,
                  "cameras": [
                   {"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0}, "name": "Camera 1",
                    "intervals": [[1791618125, 1791618141]], "download": [1791618120, 1791618146], "added": false}],
                  "events": [{"camera": {"device": "d-3f9a0c1b7e2d4a65", "channel": 3, "lens": 0},
                              "kind": "vehicle", "source": "device", "start": 1791618125, "end": 1791618141}],
                  "downloads": ["example-id-1"], "future": 1}]}
                """));
            Incident i = incidents.Incidents[0];
            T.Check(i.Group == "Group 1" && i.Name is null && i.Kinds.SequenceEqual(["vehicle"]));
            T.Equal(5L, i.Margin);
            T.Equal("07:42:05 Vehicle", i.Title);
            var cam = new RecordingCamera("d-3f9a0c1b7e2d4a65", 3);
            T.Equal(new ThumbnailKey(cam, "sub", 1_791_618_125, 1_791_618_141), i.CoverKeys[0]);
            SpansAnswer spans = T.NotNull(SpansAnswer.Parse("""
                {"revision": 41, "spans": [{"camera": {"device": "d-1", "channel": 3, "lens": 0},
                  "spans": [{"start": 1791590400, "end": 1791594000, "bytes": 1073741824, "causes": ["sched", "md"], "tier": "main"}]}]}
                """));
            T.Check(spans.Spans[0].Spans[0].Causes.SequenceEqual(["sched", "md"]));
            CalendarAnswer cal = T.NotNull(CalendarAnswer.Parse(
                """{"revision":41,"days":[{"camera":{"device":"d-1","channel":3},"days":[1,2]},{"camera":{"device":"d-1","channel":4},"days":[2,3]}]}"""));
            T.Check(cal.AllDays.SetEquals([1L, 2L, 3L]));
            LockAnswer lck = T.NotNull(LockAnswer.Parse(
                """{"revision":3,"keys":["i-a"],"download":null,"folder":null,"download_status":"not_configured"}"""));
            T.Check(lck.VideoNotKept && lck.Message!.Contains("not being kept"));
            IndexStatus status = T.NotNull(IndexStatus.Parse(
                """{"revision":4,"devices":[{"device":"d-1","state":"busy","pending_days":3,"revision":4}]}"""));
            T.Equal(3, status.Devices[0].PendingDays);
            IncidentGroups groups = T.NotNull(IncidentGroups.Parse("""
                {"revision": 3, "groups": [{"name": "Group 1", "cameras": [
                  {"device": "d-1", "channel": 3}, {"device": "d-1", "channel": 5, "lens": 1}]}],
                 "camera_names": [{"camera": {"device": "d-1", "channel": 3}, "name": "Camera 1"}]}
                """));
            T.Equal(new RecordingCamera("d-1", 5, 1), groups.Groups[0].Cameras[^1]);
            T.Equal("Camera 1", groups.CameraNames[0].Name);
            T.Check(IncidentsAnswer.Parse("[1]") is null && IncidentsAnswer.Parse("{") is null);
        });

        T.Run("browse: requests and edits", () =>
        {
            var c = new RecordingCamera("d-1", 3);
            T.Equal("""{"cameras":[{"device":"d-1","channel":3,"lens":0}],"days":[5,6],"force":true}""",
                RecordingsRequest.Want([c], [5, 6], force: true));
            T.Equal("""{"cameras":[{"device":"d-1","channel":3,"lens":0}],"months":[1788220800],"force":false}""",
                RecordingsRequest.WantMonths([c], [1_788_220_800]));
            T.Equal("""{"channels":[{"channel":3,"lens":0},{"channel":5,"lens":1}],"utc_offset_s":-3600}""",
                RecordingsRequest.Enable([c, new RecordingCamera("d-1", 5, 1)], -3600));
            T.Equal("""{"start":1,"end":2,"group":"Yard \"east\""}""", RecordingsRequest.Query(1, 2, group: "Yard \"east\""));
            T.Equal("""{"cameras":[{"device":"d-1","channel":3,"lens":0}]}""", RecordingsRequest.Hold([c]));
            T.Equal("""{"op":"merge","keys":["a","b"]}""", new IncidentEdit.Merge(["a", "b"]).Json);
            T.Equal("""{"op":"split","key":"a","at":9}""", new IncidentEdit.Split("a", 9).Json);
            T.Equal("""{"op":"rename","key":"a","name":"Delivery"}""", new IncidentEdit.Rename("a", "Delivery").Json);
            T.Equal("""{"op":"rename","key":"a","name":null}""", new IncidentEdit.Rename("a", "  ").Json);
            T.Equal("""{"op":"add_camera","key":"a","camera":{"device":"d-1","channel":3,"lens":0}}""", new IncidentEdit.AddCamera("a", c).Json);
            T.Equal("""{"op":"revert","key":"a"}""", new IncidentEdit.Revert("a").Json);
        });

        T.Run("browse: groups put drops empty groups and repeats", () =>
        {
            var a = new RecordingCamera("d-1", 0);
            var b = new RecordingCamera("d-1", 1);
            var groups = new IncidentGroups([new("Front", [a, b]), new(" ", [a]), new("Back", [b])], []);
            IncidentGroups back = T.NotNull(IncidentGroups.Parse(groups.PutJson));
            T.Equal(1, back.Groups.Count);
            T.Equal("Front", back.Groups[0].Name);
            T.Check(back.Groups[0].Cameras.SequenceEqual([a, b]));
            T.Check(!groups.PutJson.Contains("revision"));
        });

        T.Run("browse: downloads board", () =>
        {
            var board = new DownloadBoard();
            T.Check(board.Apply(Progress("a", "queued")));
            T.Check(board.Apply(Progress("a", "running", 1)));
            T.Check(!board.Apply(Progress("a", "running", 2)), "progress alone is not a state change");
            T.Equal(0.5, board.Downloads[0].Fraction());
            T.Check(board.Apply(Progress("b", "queued")));
            T.Check(board.Downloads.Select(d => d.Download).SequenceEqual(["a", "b"]));
            T.Check(board.Removed("b", false, null) is null);
            // Trash: gone from the list at once; a late event does not bring it back;
            // the folder goes to the Recycle Bin on download_removed.
            board.TrashRequested("a", "/x/a");
            T.Equal(0, board.Downloads.Count);
            T.Check(!board.Apply(Progress("a", "running", 3)));
            T.Equal(0, board.Downloads.Count);
            T.Equal("/x/a", board.Removed("a", true, "/x/a"));
            T.Equal(0, board.Trashing.Count);
            T.Equal("Failed", Progress("c", "failed").StateText());
            T.Check(Progress("c", "done").Fraction() == 1 && !Progress("c", "done").IsWorking());
            T.Check(!Progress("c", "running", paused: true).IsWorking());
            T.Equal("a", Progress("a", "queued").FolderName());
        });

        T.Run("browse: thumbnails board is bounded", () =>
        {
            var board = new ThumbnailBoard(3);
            ThumbnailKey[] keys = [.. Enumerable.Range(0, 5).Select(n => new ThumbnailKey(new RecordingCamera("d-1", 0), "sub", n, n + 1))];
            T.Check(board.NeedsRequest(keys[0]));
            board.Set(keys[0], new ThumbnailState.Pending());
            T.Check(!board.NeedsRequest(keys[0]));
            board.Set(keys[1], new ThumbnailState.Image([1]));
            board.Set(keys[2], new ThumbnailState.Image([2]));
            board.Touch(keys[1]);
            board.Set(keys[3], new ThumbnailState.Empty());
            // The pending one stays; the least recently used finished one goes.
            T.Equal(3, board.Count);
            T.Check(board[keys[0]] is ThumbnailState.Pending && board[keys[2]] is null && board[keys[1]] is not null);
            board.Set(keys[4], new ThumbnailState.Dropped());
            T.Check(board.NeedsRequest(keys[4]));
            board.ForgetPending();
            T.Check(board[keys[0]] is null && board.PendingKeys.Count == 0);
        });

        T.Run("player: speeds", () =>
        {
            T.Check(PlaybackSpeed.All.SequenceEqual([0.25, 0.5, 1, 2, 4, 8, 16]));
            T.Equal("0.25×", PlaybackSpeed.Label(0.25));
            T.Equal("1×", PlaybackSpeed.Label(1));
            T.Equal("16×", PlaybackSpeed.Label(16));
            T.Check(!PlaybackSpeed.KeyframesOnly(1) && PlaybackSpeed.KeyframesOnly(2));
            T.Check(PlaybackSpeed.HasSound(1) && !PlaybackSpeed.HasSound(0.5) && !PlaybackSpeed.HasSound(2));
            T.Check(PlaybackSpeed.Hint(1) is null);
            T.Equal("Keyframes only, no sound", PlaybackSpeed.Hint(8));
            T.Equal("No sound below 1×", PlaybackSpeed.Hint(0.5));
            T.Check(PlaybackSpeed.Faster(1) == 2 && PlaybackSpeed.Faster(16) is null);
            T.Check(PlaybackSpeed.Slower(1) == 0.5 && PlaybackSpeed.Slower(0.25) is null);
        });

        T.Run("player: transport", () =>
        {
            var t = new TransportState(1_000_000, 1_060_000, 1_055_000, NvPlaybackState.Paused, 1);
            T.Check(t.ShowsPlay && !t.IsPlaying);
            T.Equal(1_060_000L, t.Skipped(TransportState.SkipMs), "clamped to the end");
            T.Equal(1_045_000L, t.Skipped(-TransportState.SkipMs));
            T.Equal(1_000_000L, t.Clamp(0));
            TransportState playing = t with { State = NvPlaybackState.Buffering };
            T.Check(playing.IsPlaying && !playing.ShowsPlay, "buffering still counts as playing");
            T.Equal(0.5, TransportState.Fraction(1_030_000, 1_000_000, 1_060_000));
            T.Equal(0.0, TransportState.Fraction(5, 10, 10));
            T.Equal(15_000L, TransportState.TimeAtFraction(0.25, 0, 60_000));
            T.Equal(60_000L, TransportState.TimeAtFraction(2, 0, 60_000));
            T.Equal("07:42:05", TransportState.Clock((1_791_590_400L + 7 * 3600 + 42 * 60 + 5) * 1000 + 900));
            T.Equal(-1L, DeviceTime.FloorDiv(-1, 1000));
        });

        T.Run("player: grid cells and clicks", () =>
        {
            var b = new Rect(0, 0, 802, 402);
            T.Equal<Rect?>(b, PlayerGrid.Cells(1, PlayerGridMode.Tiled, b)[0]);
            Rect?[] two = PlayerGrid.Cells(2, PlayerGridMode.Tiled, b);
            T.Equal<Rect?>(new Rect(0, 0, 400, 402), two[0]);
            T.Equal<Rect?>(new Rect(402, 0, 400, 402), two[1]);
            Rect?[] four = PlayerGrid.Cells(4, PlayerGridMode.Tiled, b);
            T.Check(four.All(c => c is not null) && four[3] == new Rect(402, 202, 400, 200));
            T.Equal(3, PlayerGrid.Cells(3, PlayerGridMode.Tiled, b).Count(c => c is not null));
            Rect?[] big = PlayerGrid.Cells(3, new PlayerGridMode.Emphasis(1), b);
            T.Equal<Rect?>(new Rect(0, 0, 600, 402), big[1]);
            T.Check(big[0]!.Value.X == 602 && big[2]!.Value.X == 602);
            T.Check(big[0]!.Value.Height <= 402.0 / 3 + 0.001);
            T.Check(big[0]!.Value.Bottom <= big[2]!.Value.Y);
            Rect?[] alone = PlayerGrid.Cells(3, new PlayerGridMode.Single(2), b);
            T.Check(alone[0] is null && alone[1] is null && alone[2] == b);
            T.Equal(PlayerGridMode.Tiled, PlayerGrid.Normalized(new PlayerGridMode.Single(3), 2));
            T.Equal(PlayerGridMode.Tiled, PlayerGrid.Normalized(new PlayerGridMode.Emphasis(0), 1));
            T.Equal<PlayerGridMode>(new PlayerGridMode.Emphasis(2), PlayerGrid.Click(PlayerGridMode.Tiled, 2, 3));
            T.Equal(PlayerGridMode.Tiled, PlayerGrid.Click(new PlayerGridMode.Emphasis(2), 2, 3));
            T.Equal<PlayerGridMode>(new PlayerGridMode.Emphasis(0), PlayerGrid.Click(new PlayerGridMode.Emphasis(2), 0, 3));
            T.Equal<PlayerGridMode>(new PlayerGridMode.Single(1), PlayerGrid.Click(new PlayerGridMode.Single(1), 1, 3));
            T.Equal(PlayerGridMode.Tiled, PlayerGrid.Click(PlayerGridMode.Tiled, 0, 1));
            T.Equal<PlayerGridMode>(new PlayerGridMode.Single(1), PlayerGrid.DoubleClick(PlayerGridMode.Tiled, 1, 2));
            T.Equal(PlayerGridMode.Tiled, PlayerGrid.DoubleClick(new PlayerGridMode.Single(1), 1, 2));
            T.Equal<PlayerGridMode>(new PlayerGridMode.Single(1), PlayerGrid.DoubleClick(new PlayerGridMode.Emphasis(0), 1, 2));
            T.Equal<int?>(0, PlayerGrid.AudibleIndex(PlayerGridMode.Tiled, 3));
            T.Equal<int?>(2, PlayerGrid.AudibleIndex(new PlayerGridMode.Emphasis(2), 3));
            T.Equal<int?>(null, PlayerGrid.AudibleIndex(PlayerGridMode.Tiled, 0));
            T.Check(PlayerGrid.Cameras([1, 2, 3, 4, 5]).SequenceEqual([1, 2, 3, 4]));
        });

        T.Run("player: panes, split, strip", () =>
        {
            double both = RecordingsPanes.SidebarWidth + RecordingsPanes.InspectorWidth + RecordingsPanes.CenterMin;
            T.Equal((true, true), RecordingsPanes.Shown(both, true, true));
            T.Equal((true, false), RecordingsPanes.Shown(both - 1, true, true));
            T.Equal(RecordingsPanes.WindowMinWidth, RecordingsPanes.SidebarWidth + RecordingsPanes.CenterMin);
            T.Equal((true, false), RecordingsPanes.Shown(RecordingsPanes.WindowMinWidth, true, true));
            T.Equal((false, false), RecordingsPanes.Shown(600, true, true));
            T.Equal((false, true), RecordingsPanes.Shown(800, false, true));
            T.Equal((false, false), RecordingsPanes.Shown(2000, false, false));
            T.Equal(18.0 + 3 * 26 + 8, RecordingsPanes.LanesHeight(900, 3));
            T.Equal((600 - RecordingsPanes.TransportHeight) / 3, RecordingsPanes.LanesHeight(600, 16));
            T.Equal(18.0 + 2 * 26 + 8, RecordingsPanes.LanesHeight(200, 16));
            T.Equal<long?>(105, IncidentSplit.Second(100, 110, 105_400));
            T.Equal<long?>(null, IncidentSplit.Second(100, 110, 100_900));
            T.Equal<long?>(null, IncidentSplit.Second(100, 110, 110_000));
            T.Equal<long?>(null, IncidentSplit.Second(100, 110, 95_000));
            T.Equal((1, 0), IncidentRowStrip.Slots(1));
            T.Equal((2, 0), IncidentRowStrip.Slots(2));
            T.Equal((2, 2), IncidentRowStrip.Slots(4));
            T.Equal(2 * IncidentRowStrip.ThumbnailWidth + IncidentRowStrip.Spacing, IncidentRowStrip.Width);
        });

        T.Run("fake recordings: a month alone brings only its calendar", () =>
        {
            var fake = new FakeRecordings(_ => { });
            fake.RecordingsConfigure("{}");
            fake.RecordingsEnable("d-1", """{"channels":[{"channel":0}]}""");
            long today = DeviceTime.DayStart(DateTimeOffset.UtcNow.ToUnixTimeSeconds());
            long previous = DeviceTime.MonthAdding(DeviceTime.Month(today).Start, -1);
            var cam = new RecordingCamera("d-1", 0);
            fake.RecordingsWant(RecordingsRequest.WantMonths([cam], [previous + 86_400]));
            (long start, long next) = DeviceTime.Month(previous);
            CalendarAnswer cal = T.NotNull(CalendarAnswer.Parse(fake.RecordingsCalendar(RecordingsRequest.Query(start, next - 1, [cam]))));
            T.Equal((int)((next - start) / 86_400), cal.AllDays.Count);
            T.Equal(1, fake.MonthRequests);
            NvStatus status = NvStatus.Ok;
            try
            {
                fake.RecordingsWant(RecordingsRequest.WantMonths([cam], Enumerable.Range(0, 13).Select(n => n * 2_700_000L)));
            }
            catch (NvException e)
            {
                status = e.Status;
            }
            T.Equal(NvStatus.Limit, status);
        });
    }
}
