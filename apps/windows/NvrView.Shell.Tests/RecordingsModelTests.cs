using System.Text;
using NvrView.Interop;

namespace NvrView.Shell.Tests;

/// <summary>
/// The Recordings window's model (RecordingsService, RecordingsBrowser, RecordingsPlayer) against the fakes,
/// following the steps of the macOS self-test: cameras chosen, today held, the calendar and lanes, navigation,
/// the player and its transport, a lock, the downloads, and the window closing. Events are delivered at once
/// by the fakes; the model's own notifications are queued and run by Pump, as the app's dispatcher does.
/// </summary>
internal static class RecordingsModelTests
{
    private const string Device = "d-3f9a0c1b7e2d4a65";

    private sealed class Rig
    {
        public readonly Queue<Action> Queue = new();
        public readonly List<string> Trashed = [];
        public readonly List<IReadOnlyList<string>> Revealed = [];
        public long Now = 1_000;
        public readonly FakeRecordings Recordings;
        public readonly FakePlayback Playback;
        public readonly PlainPlaybackHost Host;
        public readonly RecordingsService Service;

        public Rig()
        {
            Recordings = new FakeRecordings(Sink) { RecordingWorkIsManual = true };
            Playback = new FakePlayback(Sink, Recordings, () => Now) { RecordingWorkIsManual = true };
            Host = new PlainPlaybackHost(Playback);
            Service = new RecordingsService(Recordings, Host)
            {
                Post = Queue.Enqueue,
                Now = () => DateTimeOffset.UtcNow,
                Trasher = Trashed.Add,
                Revealer = Revealed.Add,
            };
            Service.Start(Path.Combine(Path.GetTempPath(), "nvrview-shell-tests-" + Environment.ProcessId, "Incidents"));
        }

        private void Sink(string json) => Service.Route(Encoding.UTF8.GetBytes(json));

        public void Pump()
        {
            for (int i = 0; i < 100 && Queue.Count > 0; i++)
            {
                Queue.Dequeue()();
            }
        }

        public void Work()
        {
            Recordings.CompleteRecordingWork();
            Playback.CompletePlaybackWork();
            Pump();
        }
    }

    public static void Run()
    {
        T.Run("recordings window: cameras, today held, calendar, lanes, navigation", () =>
        {
            var rig = new Rig();
            RecordingsService service = rig.Service;
            T.Check(service.Available);
            RecordingCamera[] cams = [.. Enumerable.Range(0, 3).Select(c => new RecordingCamera(Device, c))];
            foreach (RecordingCamera c in cams)
            {
                service.SetEnabled(c, true);
            }
            T.Check(service.EnabledCameras.SequenceEqual(cams), "recordings-enable");

            var b = new RecordingsBrowser(service);
            rig.Pump();
            T.Check(b.Holding && rig.Recordings.Holds[b.HoldId].Count == 3, "today is held");
            T.Equal(3, b.Lanes.Count);
            T.Check(b.RecordedDays.Contains(b.Day), "the calendar marks today");
            T.Check(b.Lanes.All(l => l.Spans.Count > 0), "every lane has spans");
            T.Equal(6, b.DayIncidents.Count);
            T.Equal(6, b.Lanes.Sum(l => l.Events.Count));
            int reloads = b.Reloads;
            rig.Pump();
            T.Equal(reloads, b.Reloads, "no reload loop");

            // A kind filter and the Locked filter.
            b.SetKindFilter(KindFilter.People, true);
            T.Equal(3, b.Incidents.Count);
            b.SetKindFilter(KindFilter.People, false);
            T.Equal(6, b.Incidents.Count);

            // Yesterday is asked for but not held; back on today it is held again.
            long today = b.Day;
            b.PreviousDay();
            rig.Pump();
            T.Check(!b.Holding && !rig.Recordings.Holds.ContainsKey(b.HoldId), "yesterday is not held");
            T.Equal(6, b.DayIncidents.Count);
            b.NextDay();
            rig.Pump();
            T.Check(b.Day == today && rig.Recordings.Holds[b.HoldId].Count == 3, "today is held again");
            b.NextDay();
            T.Equal(today, b.Day);

            // Paging the calendar asks for the previous month alone, once.
            b.ShowMonth(-1);
            rig.Pump();
            T.Equal(1, rig.Recordings.MonthRequests);
            T.Equal(1, b.MonthRequests.Count);
            T.Check(b.RecordedDays.Count > 0 && b.RecordedDays.All(d => DeviceTime.Month(d).Start == b.Month));
            b.ShowMonth(1);
            b.ShowMonth(-1);
            T.Equal(1, rig.Recordings.MonthRequests, "a month already asked is not asked again");
            b.ShowMonth(1);
            T.Equal(DeviceTime.Month(today).Start, b.Month);

            // Names: the camera list's, else Channel N.
            T.Equal("Channel 1", b.Lanes[0].Name);
            service.CameraNamer = c => c.Channel == 0 ? "Front" : null;
            b.Reload();
            T.Equal("Front", b.Lanes[0].Name);

            // The window closes: the hold ends.
            b.Close();
            T.Check(!rig.Recordings.Holds.ContainsKey(b.HoldId), "the hold is released");
            T.Check(!b.Holding);
        });

        T.Run("recordings window: the player opens, plays, seeks, and closes", () =>
        {
            var rig = new Rig();
            RecordingsService service = rig.Service;
            RecordingCamera[] cams = [.. Enumerable.Range(0, 3).Select(c => new RecordingCamera(Device, c))];
            foreach (RecordingCamera c in cams)
            {
                service.SetEnabled(c, true);
            }
            var b = new RecordingsBrowser(service);
            rig.Pump();
            RecordingsPlayer p = b.Player;
            Incident first = b.DayIncidents.OrderBy(i => i.Start).First();
            b.Click(first.Key, extend: false);
            rig.Work();
            T.Check(p.Playback is not null && p.Opens == 1, "playback-open");
            T.Equal(1, p.Cells.Count);
            T.Equal((first.Start - first.Margin) * 1000, p.PositionMs);
            T.Check(p.Day == b.Day && p.IncidentKey == first.Key);
            rig.Work();
            T.Equal("playing", p.Cells[0].State);

            // Transport: play, pause, skip, speed.
            T.Check(p.Transport.IsPlaying, "selecting plays at once");
            rig.Now += 5_000;
            p.Tick();
            T.Equal((first.Start - first.Margin) * 1000 + 5_000, p.PositionMs);
            p.TogglePlay();
            rig.Work();
            T.Check(!p.Transport.IsPlaying && p.Transport.ShowsPlay, "playback-transport");
            long at = p.PositionMs;
            p.Skip(TransportState.SkipMs);
            T.Equal(at + 10_000, p.PositionMs);
            p.SetRate(4);
            T.Equal(4.0, p.Rate);
            T.Equal("Keyframes only, no sound", PlaybackSpeed.Hint(p.Rate));
            p.SetRate(3);
            T.Equal(4.0, p.Rate, "a rate the core does not take is ignored");
            p.SetRate(1);

            // Grid: add a camera, large and single views, the sound follows.
            p.Add(cams[2]);
            T.Equal(2, p.Cells.Count);
            p.Click(1);
            T.Equal<PlayerGridMode>(new PlayerGridMode.Emphasis(1), p.Mode);
            T.Equal(cams[2], p.AudibleCamera);
            p.DoubleClick(0);
            T.Equal<PlayerGridMode>(new PlayerGridMode.Single(0), p.Mode);
            T.Equal(p.Cells[0].Camera, p.AudibleCamera);
            p.Muted = true;
            T.Check(p.AudibleCamera is null);
            p.Muted = false;
            p.Mode = PlayerGridMode.Tiled;

            // Scrub: the range follows the scope; a lane click on a playing camera seeks.
            T.Equal(((first.Start - first.Margin) * 1000, (first.End + first.Margin + 1) * 1000), b.ScrubRange);
            b.ScrubScope = ScrubScope.Day;
            T.Equal((p.StartMs, p.EndMs), b.ScrubRange);
            int seeks = p.Seeks;
            b.LaneClick(p.Cells[0].Camera, first.Start + 3);
            T.Equal(seeks + 1, p.Seeks, "playback-scrub");
            T.Equal((first.Start + 3) * 1000, p.PositionMs);

            // Another incident of another camera reopens the playback; the same cameras only seek.
            Incident other = b.DayIncidents.First(i => i.Cameras[0].Camera != first.Cameras[0].Camera);
            b.Click(other.Key, extend: false);
            rig.Work();
            T.Equal(2, p.Opens);
            T.Check(p.Cells[0].Camera == other.Cameras[0].Camera);

            // Split Here is only offered inside the playing incident.
            T.Check(b.SplitSecond(other) is null || b.SplitSecond(other) > other.Start);

            // A lane click on a camera that is not playing plays it alone from there.
            RecordingCamera idle = cams.First(c => !p.Cameras.Contains(c));
            b.LaneClick(idle, b.Day + 1000);
            rig.Work();
            T.Check(p.Cameras.SequenceEqual([idle]) && b.Selection.Count == 0);

            // A day change closes the playback; so does the window.
            b.PreviousDay();
            rig.Pump();
            T.Check(p.Playback is null && p.Cells.Count == 0, "playback-close");
            b.Click(b.DayIncidents[0].Key, extend: false);
            rig.Work();
            T.Check(p.Playback is not null);
            ulong id = p.Playback!.Value;
            b.Close();
            T.Check(p.Playback is null);
            T.Equal(NvStatus.UnknownId, StatusOf(() => rig.Playback.PlaybackPosition(id)), "the core closed it too");
        });

        T.Run("recordings window: rename, lock, downloads, groups", () =>
        {
            var rig = new Rig();
            RecordingsService service = rig.Service;
            foreach (int c in new[] { 0, 1, 2 })
            {
                service.SetEnabled(new RecordingCamera(Device, c), true);
            }
            var b = new RecordingsBrowser(service);
            rig.Pump();
            Incident one = b.DayIncidents[0];
            b.Rename(one.Key, "Delivery");
            rig.Pump();
            T.Check(b.DayIncidents.Any(i => i.Name == "Delivery"), "recordings-edits");
            b.Revert(one.Key);
            rig.Pump();

            // Lock: the download is queued and the message says so; unlocking leaves it.
            Incident vehicle = b.DayIncidents.First(i => i.Kinds.SequenceEqual(["vehicle"]));
            b.SetLocked(vehicle.Key, true);
            rig.Pump();
            T.Check(service.Notice!.Contains("downloading", StringComparison.Ordinal), "recordings-lock");
            T.Check(b.DayIncidents.First(i => i.Key == vehicle.Key).Locked);
            T.Equal(1, service.Downloads.Count);
            b.LockedOnly = true;
            T.Check(b.Incidents.Select(i => i.Key).SequenceEqual([vehicle.Key]));
            b.LockedOnly = false;
            b.SetLocked(vehicle.Key, false);
            rig.Pump();
            T.Equal(1, service.Downloads.Count);

            // Downloads: pause, resume, done, Show in Explorer, forget, Recycle Bin.
            DownloadProgress d = service.Downloads[0];
            service.Pause(d.Download, true);
            T.Check(service.Downloads[0].Paused);
            rig.Work();
            T.Check(service.Downloads[0].State != "done", "a paused download does not finish");
            service.Pause(d.Download, false);
            rig.Work();
            T.Equal("done", service.Downloads[0].State);
            service.Reveal(service.Downloads[0]);
            T.Check(rig.Revealed.Count == 1 && rig.Revealed[0].SequenceEqual(service.Downloads[0].Outputs()), "recordings-downloads");
            service.Forget(d.Download);
            rig.Pump();
            T.Equal(0, service.Downloads.Count);
            Incident other = b.DayIncidents.First(i => i.Key != vehicle.Key);
            b.Download(other.Key);
            rig.Pump();
            T.Equal(1, service.Downloads.Count);
            string folder = service.Downloads[0].Folder!;
            service.Trash(service.Downloads[0].Download);
            rig.Pump();
            T.Check(rig.Trashed.SequenceEqual([folder]) && service.Downloads.Count == 0, "the Recycle Bin got the folder");

            // Groups round-trip through the core.
            var two = new IncidentGroups([new("Front", [new RecordingCamera(Device, 0), new RecordingCamera(Device, 1)])], []);
            T.Check(service.PutGroups(two));
            rig.Pump();
            T.Equal(1, service.Groups.Groups.Count);
            T.Equal("Front", service.Groups.Groups[0].Name);
            b.Scope = new IncidentScope.Group("Front");
            b.Scope = IncidentScope.Everything;
            b.Close();
        });

        T.Run("recordings window: thumbnails are asked for, decoded, and put back", () =>
        {
            var rig = new Rig();
            RecordingsService service = rig.Service;
            service.SetEnabled(new RecordingCamera(Device, 0), true);
            var b = new RecordingsBrowser(service);
            rig.Pump();
            ThumbnailKey key = b.DayIncidents[0].CoverKeys[0];
            service.RequestThumbnail(key);
            T.Check(service.Thumbnails[key] is ThumbnailState.Pending);
            Action<RecordedThumbnail?>? done = null;
            RecordedThumbnail? decoding = null;
            service.ThumbnailDecoder = (t, callback) =>
            {
                decoding = t;
                done = callback;
            };
            rig.Work();
            T.Check(service.Thumbnails[key] is ThumbnailState.Decoding && decoding is not null);
            T.Equal(NvThumbKind.H264Keyframe, decoding!.Kind);
            done!(new RecordedThumbnail(NvThumbKind.Image, 64, 36, 0, [0xff, 0xd8, 0xff, 0xd9]));
            rig.Pump();
            T.Check(service.Thumbnails[key] is ThumbnailState.Image);
            T.Equal(1, service.ThumbnailsPut);
            T.Equal(NvThumbKind.Image, rig.Recordings.ThumbnailGet(key.Json)!.Kind);
            // Without a decoder a keyframe shows as failed.
            ThumbnailKey other = b.DayIncidents[1].CoverKeys[0];
            service.ThumbnailDecoder = null;
            service.RequestThumbnail(other);
            rig.Work();
            T.Check(service.Thumbnails[other] is ThumbnailState.Failed);
            b.Close();
        });

        T.Run("recordings window: the cameras searched are kept between launches", () =>
        {
            string path = Path.Combine(Path.GetTempPath(), "nvrview-shell-tests-" + Environment.ProcessId, "recordings-cameras.json");
            var store = new RecordingsEnablementStore(path);
            var first = new Rig();
            var a = new RecordingsService(first.Recordings, first.Host, store) { Post = first.Queue.Enqueue };
            a.Start(Path.Combine(Path.GetTempPath(), "nvrview-shell-tests-" + Environment.ProcessId, "Incidents"));
            a.SetEnabled(new RecordingCamera(Device, 2), true);
            a.SetEnabled(new RecordingCamera(Device, 0), true);
            T.Check(a.EnabledCameras.SequenceEqual([new RecordingCamera(Device, 0), new RecordingCamera(Device, 2)]));
            var next = new Rig();
            var c = new RecordingsService(next.Recordings, next.Host, new RecordingsEnablementStore(path)) { Post = next.Queue.Enqueue };
            c.Start(Path.Combine(Path.GetTempPath(), "nvrview-shell-tests-" + Environment.ProcessId, "Incidents"));
            T.Check(c.EnabledCameras.SequenceEqual(a.EnabledCameras), "the choice came back");
            c.DeviceRemoved(Device);
            T.Equal(0, c.EnabledCameras.Count);
            c.Rekey(Device, "d-other");
            T.Equal(0, c.EnabledCameras.Count);
        });
    }

    private static NvStatus StatusOf(Action call)
    {
        try
        {
            call();
            return NvStatus.Ok;
        }
        catch (NvException e)
        {
            return e.Status;
        }
    }
}
